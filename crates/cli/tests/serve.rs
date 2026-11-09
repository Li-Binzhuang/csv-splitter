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

    // Second page comes from the cache: rowsSoFar stays at the materialised
    // size instead of growing, and the answer is identical.
    let (status, again) = get(
        &server.addr,
        &format!("/rows?path=data.csv&sql={encoded}&start=5&count=5"),
    );
    assert_eq!(status, 200);
    assert_eq!(again, body, "cached page should be identical");

    // Writes are refused through this path too.
    let (status, body) = get(&server.addr, "/rows?path=data.csv&sql=DROP%20TABLE%20t");
    assert_eq!(status, 400, "{body}");

    drop(server);
}

#[test]
fn warm_preloads_the_cache_for_the_first_scroll() {
    let dir = TempDir::new("warm");
    let mut csv = String::from("id,city\n");
    for i in 0..2_000 {
        csv.push_str(&format!(
            "{i},{}\n",
            if i % 3 == 0 { "CDMX" } else { "Puebla" }
        ));
    }
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    let sql = "SELECT id, city FROM t WHERE city='CDMX' ORDER BY id";
    let encoded = sql
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('\'', "%27")
        .replace('=', "%3D");

    // Warm runs as a job; polling it to completion is enough.
    let (status, body) = post(&server.addr, &format!("/warm?path=data.csv&sql={encoded}"));
    assert_eq!(status, 202, "{body}");
    let id = body
        .split("\"job\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("job id")
        .to_string();
    let done = wait_for_job(&server.addr, &id);
    assert!(done.contains("\"state\":\"done\""), "{done}");
    assert!(done.contains("materialised"), "{done}");

    // The window is served from the warmed cache.
    let (status, rows) = get(
        &server.addr,
        &format!("/rows?path=data.csv&sql={encoded}&start=0&count=5"),
    );
    assert_eq!(status, 200, "{rows}");
    assert!(rows.contains(r#""rowsSoFar":667"#), "{rows}");

    // Warming again is a no-op, not another job.
    let (status, again) = post(&server.addr, &format!("/warm?path=data.csv&sql={encoded}"));
    assert_eq!(status, 200, "{again}");
    assert!(again.contains("\"cached\":true"), "{again}");

    // A bad query is rejected before any job starts.
    let (status, body) = post(&server.addr, "/warm?path=data.csv&sql=DROP%20TABLE%20t");
    assert_eq!(status, 400, "{body}");

    drop(server);
}

#[test]
fn search_and_files_endpoints() {
    let dir = TempDir::new("basic");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let mut csv = String::from("id,city,note\n");
    csv.push_str("1,CDMX,\"refund issued\"\n");
    csv.push_str("2,Puebla,ok\n");
    csv.push_str("3,CDMX,refund again\n");
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    std::fs::write(dir.join("sub/other.csv"), "id\n1\n").unwrap();
    let server = start(&dir.0.clone());

    // Search matches rows, not columns; quoted field with a space still matches.
    let (status, body) = get(&server.addr, "/search?path=data.csv&q=refund");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"matched\":2"), "{body}");
    assert!(body.contains("\"rows\":[1,3]"), "{body}");

    // Narrowing to a column that has no match returns zero, not an error.
    let (status, body) = get(&server.addr, "/search?path=data.csv&q=refund&columns=city");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"matched\":0"), "{body}");

    // Unknown column is a client error.
    let (status, body) = get(&server.addr, "/search?path=data.csv&q=x&columns=nope");
    assert_eq!(status, 400, "{body}");

    // File listing walks the tree and keeps paths relative.
    let (status, body) = get(&server.addr, "/files?ext=csv");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"path\":\"data.csv\""), "{body}");
    assert!(body.contains("\"path\":\"sub/other.csv\""), "{body}");

    // Escaping the root is refused.
    let (status, _) = get(&server.addr, "/files?dir=../../etc&ext=csv");
    assert_eq!(status, 400);

    drop(server);
}

fn get_raw(addr: &str, path: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, raw[split..].to_vec())
}

#[test]
fn download_returns_the_exact_bytes() {
    let dir = TempDir::new("dl");
    // Include non-UTF8 bytes and a quoted newline to prove nothing is mangled.
    let mut content = b"id,note\n1,\"a\nb\"\n".to_vec();
    content.extend_from_slice(&[0xFF, 0xFE]);
    std::fs::write(dir.join("data.csv"), &content).unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get_raw(&server.addr, "/download?path=data.csv");
    assert_eq!(status, 200);
    assert_eq!(body, content, "bytes must round-trip untouched");

    let (status, _) = get_raw(&server.addr, "/download?path=../../etc/passwd");
    assert_eq!(status, 400);

    drop(server);
}

#[test]
fn export_returns_csv_text_of_a_query() {
    let dir = TempDir::new("exp");
    let mut csv = String::from("city,amount,note\n");
    csv.push_str("CDMX,10,ok\n");
    csv.push_str("Puebla,20,\"a,b\"\n");
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    let sql = "SELECT city, note FROM t WHERE amount > 5".replace(' ', "%20");
    let (status, body) = get_raw(&server.addr, &format!("/export?path=data.csv&sql={sql}"));
    assert_eq!(status, 200);
    let text = String::from_utf8_lossy(&body);
    assert_eq!(text, "city,note\nCDMX,ok\nPuebla,\"a,b\"\n", "{text}");

    let (status, _) = get_raw(&server.addr, "/export?path=data.csv&sql=DROP%20TABLE%20t");
    assert_eq!(status, 400);

    drop(server);
}

#[test]
fn filtered_export_and_sort_job() {
    let dir = TempDir::new("exp2");
    let mut csv = String::from("city,amount\n");
    for (city, amount) in [
        ("CDMX", 10),
        ("CDMX", 500),
        ("Puebla", 500),
        ("Tijuana", 20),
    ] {
        csv.push_str(&format!("{city},{amount}\n"));
    }
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    // `where` with `|` means AND: only the CDMX row above 100 survives.
    let (status, body) = get_raw(
        &server.addr,
        "/export?path=data.csv&where=city%3DCDMX%7Camount%3E100",
    );
    assert_eq!(status, 200);
    assert_eq!(
        String::from_utf8_lossy(&body),
        "city,amount\nCDMX,500\n",
        "AND export"
    );

    // whereAny means OR.
    let (status, body) = get_raw(
        &server.addr,
        "/export?path=data.csv&whereAny=city%3DTijuana%7Camount%3E400",
    );
    assert_eq!(status, 200);
    let text = String::from_utf8_lossy(&body);
    assert_eq!(
        text, "city,amount\nCDMX,500\nPuebla,500\nTijuana,20\n",
        "{text}"
    );

    // Sorting runs as a job and lands inside the root.
    let (status, body) = post(
        &server.addr,
        "/sort?path=data.csv&by=amount&desc=1&out=sorted.csv",
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
    let sorted = std::fs::read_to_string(dir.join("sorted.csv")).unwrap();
    let lines: Vec<&str> = sorted.lines().collect();
    assert_eq!(lines[0], "city,amount");
    assert_eq!(lines[1], "CDMX,500");
    assert_eq!(lines[3], "Tijuana,20");

    // Missing `by` and an output outside the root are client errors.
    let (status, _) = post(&server.addr, "/sort?path=data.csv&out=x.csv");
    assert_eq!(status, 400);
    let (status, _) = post(
        &server.addr,
        "/sort?path=data.csv&by=amount&out=../escape.csv",
    );
    assert_eq!(status, 400);

    drop(server);
}

#[test]
fn analyze_endpoint_and_dedupe_job() {
    let dir = TempDir::new("prof2");
    let mut csv = String::from("id,city\n");
    for i in 0..6 {
        csv.push_str(&format!("{},CDMX\n", i % 3));
    }
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/analyze?path=data.csv");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"distinct\":3"), "{body}");
    assert!(body.contains("\"city\""), "{body}");

    let (status, body) = post(&server.addr, "/dedupe?path=data.csv&by=id&out=clean.csv");
    assert_eq!(status, 202, "{body}");
    let id = body
        .split("\"job\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("job id")
        .to_string();
    let done = wait_for_job(&server.addr, &id);
    assert!(done.contains("\"state\":\"done\""), "{done}");
    let text = std::fs::read_to_string(dir.join("clean.csv")).unwrap();
    assert_eq!(text.lines().count(), 4, "{text}");

    drop(server);
}

#[test]
fn export_understands_in_lists() {
    let dir = TempDir::new("inlist");
    let mut csv = String::from("city,amount\n");
    csv.push_str("CDMX,10\nPuebla,500\nTijuana,20\n");
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get_raw(
        &server.addr,
        "/export?path=data.csv&where=city%20in%20CDMX,Puebla",
    );
    assert_eq!(status, 200);
    let text = String::from_utf8_lossy(&body);
    assert_eq!(text, "city,amount\nCDMX,10\nPuebla,500\n", "{text}");

    drop(server);
}

#[test]
fn partition_runs_as_a_job() {
    let dir = TempDir::new("part");
    let mut csv = String::from("city,amount\n");
    for (city, amount) in [("CDMX", 10), ("Puebla", 500), ("CDMX", 20)] {
        csv.push_str(&format!("{city},{amount}\n"));
    }
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/partition?path=data.csv&by=city&out=by_city.tar&format=none",
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
    assert!(dir.join("by_city.tar").exists());

    // Missing `by` is a client error, not an empty archive.
    let (status, _) = post(&server.addr, "/partition?path=data.csv&out=x.tar");
    assert_eq!(status, 400);

    drop(server);
}
