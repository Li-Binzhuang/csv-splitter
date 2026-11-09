//! Black-box tests for the HTTP API: start the real binary and query it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// Per-process unique suffix for temp directories.
///
/// `SystemTime::now()` only has microsecond resolution on macOS, so a dozen
/// tests starting at once can read the same value and share a directory — one
/// test's cleanup then deletes the other's files mid-run.
fn unique_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "csvs_srv_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                + unique_seq() as u128
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

struct Server {
    child: Child,
    addr: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(root: &PathBuf) -> Server {
    let mut child = Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
        .args(["serve", "--root"])
        .arg(root)
        .args(["--addr", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");

    let stdout = child.stdout.take().unwrap();
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    // "csv-splitter API on http://127.0.0.1:PORT  (root: ...)"
    let addr = line
        .split("http://")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .expect("address in banner")
        .to_string();
    Server { child, addr }
}

fn get(addr: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status: u16 = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

fn sample(dir: &TempDir) -> PathBuf {
    let p = dir.join("data.csv");
    let mut csv = String::from("id,city\n");
    for i in 0..500 {
        csv.push_str(&format!("{i},C{}\n", i % 7));
    }
    std::fs::write(&p, csv).unwrap();
    p
}

#[test]
fn health_meta_and_rows() {
    let dir = TempDir::new("api");
    let file = sample(&dir);
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/health");
    assert_eq!(status, 200);
    assert!(body.contains("\"ok\":true"), "{body}");

    let (status, body) = get(&server.addr, "/meta?path=data.csv");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"rows\":500"), "{body}");
    assert!(body.contains("\"headers\":[\"id\",\"city\"]"), "{body}");

    let (status, body) = get(&server.addr, "/rows?path=data.csv&start=497&count=3");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("[\"497\",\"C0\"]"), "{body}");
    assert!(body.contains("[\"499\","), "{body}");

    let (status, _) = get(&server.addr, "/rows?path=../../etc/passwd");
    assert_eq!(status, 400);

    let (status, _) = get(&server.addr, "/nope");
    assert_eq!(status, 404);

    drop(server);
    drop(file);
}

#[test]
fn stats_endpoint_reports_columns() {
    let dir = TempDir::new("stats");
    sample(&dir);
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/stats?path=data.csv&sample=500");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"name\":\"city\""), "{body}");
    assert!(body.contains("\"sampled\":500"), "{body}");

    drop(server);
}

#[test]
fn filter_endpoint_counts_matches() {
    let dir = TempDir::new("flt");
    let mut csv = String::from("id,city\n");
    for i in 0..50 {
        csv.push_str(&format!(
            "{i},{}\n",
            if i % 5 == 0 { "CDMX" } else { "Puebla" }
        ));
    }
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/filter?path=data.csv&where=city%3DCDMX");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"matched\":10"), "{body}");
    assert!(body.contains("\"rows\":[1,6,11"), "{body}");

    let (status, _) = get(&server.addr, "/filter?path=data.csv&where=bogus");
    assert_eq!(status, 400);
}

#[test]
fn query_endpoint_runs_sql_and_hides_writes() {
    let dir = TempDir::new("sql");
    let mut csv = String::from("city,amount\n");
    for i in 0..50 {
        csv.push_str(&format!(
            "{},{}\n",
            if i % 2 == 0 { "CDMX" } else { "Puebla" },
            i
        ));
    }
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    let sql = "SELECT city, count(*) AS n FROM t GROUP BY 1 ORDER BY 1";
    let encoded = sql
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('*', "%2A")
        .replace('(', "%28")
        .replace(')', "%29")
        .replace(',', "%2C");
    let (status, body) = get(&server.addr, &format!("/query?path=data.csv&sql={encoded}"));
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"columns\":[\"city\",\"n\"]"), "{body}");
    assert!(body.contains("[\"CDMX\",\"25\"]"), "{body}");

    let (status, body) = get(&server.addr, "/query?path=data.csv&sql=DROP%20TABLE%20t");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("only SELECT"), "{body}");

    drop(server);
}

fn post(addr: &str, path: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status: u16 = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

fn wait_for_job(addr: &str, id: &str) -> String {
    let mut last = String::new();
    for _ in 0..200 {
        let (status, body) = get(addr, &format!("/job?id={id}"));
        assert_eq!(status, 200, "{body}");
        if body.contains("\"state\":\"done\"") || body.contains("\"state\":\"failed\"") {
            return body;
        }
        last = body;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("job {id} did not finish; last status: {last}");
}

#[test]
fn split_runs_as_a_job() {
    let dir = TempDir::new("jobsplit");
    let mut csv = String::from("id,city\n");
    for i in 0..5_000 {
        csv.push_str(&format!("{i},C{}\n", i % 7));
    }
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/split?path=data.csv&splits=3&format=none&out=parts.tar",
    );
    assert_eq!(status, 202, "{body}");
    let id = body
        .split("\"job\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("job id")
        .to_string();

    let done = wait_for_job(&server.addr, &id);
    assert!(done.contains("\"state\":\"done\""), "{done}");
    assert!(done.contains("parts.tar"), "{done}");
    assert!(dir.join("parts.tar").exists());

    drop(server);
}

#[test]
fn merge_job_round_trips_the_file() {
    let dir = TempDir::new("jobmerge");
    let mut csv = String::from("id,city\n");
    for i in 0..2_000 {
        csv.push_str(&format!("{i},C{}\n", i % 5));
    }
    std::fs::write(dir.join("data.csv"), csv.clone()).unwrap();
    let server = start(&dir.0.clone());

    let (_, body) = post(
        &server.addr,
        "/split?path=data.csv&splits=2&format=none&out=p.tar",
    );
    let id = body
        .split("\"job\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap()
        .to_string();
    let done = wait_for_job(&server.addr, &id);
    assert!(done.contains("\"state\":\"done\""), "{done}");

    let (status, body) = post(&server.addr, "/merge?path=p.tar&out=restored.csv");
    assert_eq!(status, 202, "{body}");
    let mid = body
        .split("\"job\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap()
        .to_string();
    let done = wait_for_job(&server.addr, &mid);
    assert!(done.contains("\"state\":\"done\""), "{done}");

    assert_eq!(
        std::fs::read_to_string(dir.join("restored.csv")).unwrap(),
        csv
    );
    drop(server);
}

#[test]
fn write_endpoints_refuse_paths_outside_root() {
    let dir = TempDir::new("jobescape");
    std::fs::write(dir.join("data.csv"), "id\n1\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, _) = post(&server.addr, "/split?path=data.csv&out=../../tmp/evil.tar");
    assert_eq!(status, 400);

    let (status, _) = post(&server.addr, "/split?path=../../etc/passwd&out=x.tar");
    assert_eq!(status, 400);

    let (status, _) = get(&server.addr, "/job?id=nope");
    assert_eq!(status, 404);

    drop(server);
}

#[test]
fn rows_can_page_a_query_result() {
    let dir = TempDir::new("sqlrows");
    let mut csv = String::from("city,amount\n");
    for i in 0..200 {
        csv.push_str(&format!(
            "{},{}\n",
            if i % 2 == 0 { "CDMX" } else { "Puebla" },
            i
        ));
    }
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    // Sorted view, second page of 5 rows.
    let sql = "SELECT city, amount FROM t WHERE city='CDMX' ORDER BY amount";
    let encoded = sql
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('\'', "%27")
        .replace('=', "%3D");
    let (status, body) = get(
        &server.addr,
        &format!("/rows?path=data.csv&sql={encoded}&start=5&count=5"),
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"start\":5"), "{body}");
    assert!(body.contains("\"hasMore\":true"), "{body}");
    assert!(body.contains("\"columns\":[\"city\",\"amount\"]"), "{body}");
    // Row 5 (0-based) of the CDMX subset is amount 10 (evens only).
    assert!(body.contains("[\"CDMX\",\"10\"]"), "{body}");

    // Writes are refused through this path too.
    let (status, body) = get(&server.addr, "/rows?path=data.csv&sql=DROP%20TABLE%20t");
    assert_eq!(status, 400, "{body}");

    drop(server);
}
