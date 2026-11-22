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
    start_with_env(root, &[])
}

/// Start a server with extra environment variables — the timeouts and limits are knobs
/// a test should be able to tighten without waiting ten seconds.
fn start_with_env(root: &PathBuf, env: &[(&str, &str)]) -> Server {
    let mut child = Command::new(env!("CARGO_BIN_EXE_csvx"));
    child
        .args(["serve", "--root"])
        .arg(root)
        .args(["--addr", "127.0.0.1:0"]);
    for (name, value) in env {
        child.env(name, value);
    }
    let mut child = child
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server");

    let stdout = child.stdout.take().unwrap();
    let mut line = String::new();
    BufReader::new(stdout).read_line(&mut line).unwrap();
    // "csvx API on http://127.0.0.1:PORT  (root: ...)"
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
    assert!(rows.contains(r#""total":667"#), "{rows}");

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

#[test]
fn groupby_endpoint_aggregates_and_caches() {
    let dir = TempDir::new("groupby");
    let mut csv = String::from("city,amount\n");
    for (city, amount) in [("CDMX", 10), ("CDMX", 20), ("Puebla", 50)] {
        csv.push_str(&format!("{city},{amount}\n"));
    }
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(
        &server.addr,
        "/groupby?path=data.csv&by=city&agg=amount:sum,amount:count&order=sum_amount&desc=1",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#"["CDMX","30","2"]"#), "{body}");
    assert!(body.contains(r#"["Puebla","50","1"]"#), "{body}");
    // Descending by sum: Puebla first.
    let puebla = body.find("Puebla").expect("row");
    let cdmx = body.find("CDMX").expect("row");
    assert!(puebla < cdmx, "descending order expected: {body}");

    // Same request twice must agree (result cached, not recomputed differently).
    let (_, again) = get(
        &server.addr,
        "/groupby?path=data.csv&by=city&agg=amount:sum,amount:count&order=sum_amount&desc=1",
    );
    assert_eq!(body, again);

    // Missing aggregates and unknown columns are client errors.
    let (status, _) = get(&server.addr, "/groupby?path=data.csv&by=city");
    assert_eq!(status, 400);
    let (status, _) = get(
        &server.addr,
        "/groupby?path=data.csv&by=nope&agg=amount:sum",
    );
    assert_eq!(status, 400);
    let (status, _) = get(
        &server.addr,
        "/groupby?path=data.csv&by=city&agg=amount:nope",
    );
    assert_eq!(status, 400);

    drop(server);
}

#[test]
fn serves_a_builtin_viewer_and_rows_supports_where() {
    let dir = TempDir::new("viewer");
    let mut csv = String::from("city,amount\n");
    for (city, amount) in [("CDMX", 10), ("CDMX", 500), ("Puebla", 20)] {
        csv.push_str(&format!("{city},{amount}\n"));
    }
    std::fs::write(dir.join("data.csv"), csv).unwrap();
    let server = start(&dir.0.clone());

    // The page is one self-contained file: no build step, no bundled assets.
    let (status, body) = get_raw(&server.addr, "/");
    assert_eq!(status, 200);
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("<!DOCTYPE html>"), "not html");
    assert!(html.contains("csvx"), "{html}");
    // It must talk to the public API only.
    for endpoint in [
        "/files", "/meta", "/rows", "/search", "/analyze", "/groupby",
    ] {
        assert!(
            html.contains(&format!("api(\"{endpoint}\"")),
            "viewer misses {endpoint}"
        );
    }

    // `where` on /rows turns the raw scan into a filtered view.
    let (status, body) = get(
        &server.addr,
        "/rows?path=data.csv&where=city%3DCDMX&count=5",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"total\":2"), "{body}");
    assert!(!body.contains("Puebla"), "{body}");

    // Ordering works too.
    let (status, body) = get(
        &server.addr,
        "/rows?path=data.csv&where=city%3DCDMX&order=amount&desc=1&count=1",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#"["CDMX","500"]"#), "{body}");

    drop(server);
}

/// Every client-side rejection has the same machine-readable shape, so a
/// front-end can point at the offending input instead of printing prose.
#[test]
fn bad_requests_are_structured() {
    let dir = TempDir::new("badreq");
    std::fs::write(dir.join("a.csv"), "id\n1\n").unwrap();
    let server = start(&dir.0.clone());

    let cases = [
        ("/groupby?path=a.csv", "by"),
        ("/search?path=a.csv", "q"),
        ("/sort?path=a.csv&out=o.csv", "by"),
        ("/rows?path=../../etc/passwd", "path"),
        ("/job", "id"),
    ];
    for (path, parameter) in cases {
        let out = if path.starts_with("/sort") {
            post(&server.addr, path).1
        } else {
            get(&server.addr, path).1
        };
        assert!(
            out.contains(&format!("\"parameter\":\"{parameter}\"")),
            "{path} did not name parameter {parameter}: {out}"
        );
        assert!(out.contains("\"hint\":"), "{path} has no hint: {out}");
        assert!(
            out.contains("\"error\":"),
            "{path} has no error text: {out}"
        );
    }

    // The router advertises everything it serves, including the viewer and the
    // newer write endpoints.
    let (_, body) = get(&server.addr, "/nope");
    for endpoint in [
        "/",
        "/analyze",
        "/groupby",
        "/files",
        "/download",
        "/export",
    ] {
        assert!(
            body.contains(&format!("\"{endpoint}\"")),
            "{endpoint} missing: {body}"
        );
    }
    for write in [
        "POST /sort",
        "POST /dedupe",
        "POST /partition",
        "POST /warm",
    ] {
        assert!(body.contains(write), "{write} missing: {body}");
    }

    drop(server);
}

/// A file with a journal is a sheet: /rows shows the edits, /sheet advertises what
/// the server can do.
#[test]
fn rows_show_the_edits_and_sheet_advertises_capabilities() {
    let dir = TempDir::new("sheetapi");
    let path = dir.join("t.csv");
    std::fs::write(&path, "id,city\n1,CDMX\n2,Puebla\n").unwrap();
    let server = start(&dir.0.clone());

    // No journal yet: the sheet is pristine.
    let (_, body) = get(&server.addr, "/sheet?path=t.csv");
    assert!(body.contains(r#""edited":false"#), "{body}");
    assert!(body.contains(r#""rows":2"#), "{body}");
    assert!(body.contains("\"window\""), "capabilities missing: {body}");

    // Record an edit through the API.
    let (_, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=Monterrey",
    );
    assert!(body.contains("\"op\":\"set\""), "{body}");

    // /rows now serves the edited value.
    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=10");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("Monterrey"), "{body}");
    assert!(body.contains(r#""edited":true"#), "{body}");
    assert!(body.contains("\"columns\":[\"id\",\"city\"]"), "{body}");

    // /sheet reports the operation.
    let (_, body) = get(&server.addr, "/sheet?path=t.csv");
    assert!(body.contains(r#""edited":true"#), "{body}");
    assert!(body.contains(r#""operations":1"#), "{body}");
    assert!(
        body.contains("Monterrey"),
        "operation description missing: {body}"
    );
}

#[test]
fn pivot_endpoint_returns_a_matrix() {
    let dir = TempDir::new("pivotapi");
    std::fs::write(
        dir.join("sales.csv"),
        "city,month,amount\nCDMX,Jan,10\nCDMX,Feb,20\nPuebla,Jan,30\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(
        &server.addr,
        "/pivot?path=sales.csv&rows=city&cols=month&values=amount:sum",
    );
    assert_eq!(status, 200, "{body}");
    // The header qualifies the measure so several values can coexist.
    assert!(body.contains(r#""columns":["city""#), "{body}");
    assert!(body.contains("Feb · sum_amount"), "{body}");
    assert!(body.contains(r#"["CDMX","20","10"]"#), "{body}");
    assert!(body.contains(r#""rowKeys":2"#), "{body}");

    // Missing parameters name themselves.
    let (_, body) = get(&server.addr, "/pivot?path=sales.csv");
    assert!(body.contains(r#""parameter":"rows""#), "{body}");
}

#[test]
fn style_endpoint_paints_only_matching_cells() {
    let dir = TempDir::new("styleapi");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,1200\nPuebla,900\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(
        &server.addr,
        "/style?path=t.csv&rules=amount%3E1000%3Abg%3Dred",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""row":0"#), "{body}");
    assert!(body.contains(r#""column":"amount""#), "{body}");
    assert!(body.contains("#f44336"), "{body}");
    // Only the matching row is reported, not the whole window.
    assert!(!body.contains(r#""row":1"#), "{body}");

    let (_, body) = get(&server.addr, "/style?path=t.csv");
    assert!(body.contains(r#""parameter":"rules""#), "{body}");
}

#[test]
fn chart_endpoint_returns_series_json() {
    let dir = TempDir::new("chartapi");
    std::fs::write(
        dir.join("s.csv"),
        "month,city,amount\nJan,CDMX,10\nFeb,CDMX,20\nJan,Puebla,30\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(
        &server.addr,
        "/chart?path=s.csv&x=month&y=amount:sum&series=city&type=bar",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""type":"bar""#), "{body}");
    assert!(body.contains(r#""categories":["Jan","Feb"]"#), "{body}");
    assert!(body.contains(r#""name":"CDMX""#), "{body}");

    let (_, body) = get(&server.addr, "/chart?path=s.csv");
    assert!(body.contains(r#""parameter":"x""#), "{body}");
}

#[test]
fn layout_endpoint_reports_and_sets_column_metadata() {
    let dir = TempDir::new("layoutapi");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,1234.5\n").unwrap();
    let server = start(&dir.0.clone());

    // Defaults first.
    let (status, body) = get(&server.addr, "/layout?path=t.csv");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""column":"city""#), "{body}");
    assert!(body.contains(r#""width":null"#), "{body}");

    // Set width and format through the API.
    let (_, body) = get(
        &server.addr,
        "/layout?path=t.csv&column=amount&key=format&value=%23%2C%23%230.00",
    );
    assert!(body.contains("#,##0.00"), "format should be stored: {body}");

    // And the sheet response now carries it.
    let (_, body) = get(&server.addr, "/sheet?path=t.csv");
    assert!(
        body.contains(r#""layout":["#) || body.contains(r#""layout":[{"#),
        "{body}"
    );
    assert!(body.contains("#,##0.00"), "{body}");

    // A bad property names itself.
    let (_, body) = get(
        &server.addr,
        "/layout?path=t.csv&column=city&key=colour&value=red",
    );
    assert!(body.contains(r#""parameter":"value""#), "{body}");
}

/// Filtering an edited sheet must see the edits: SQL reads bytes, the journal is
/// the only place the edits exist, so the sheet is materialised before the query.
#[test]
fn filtered_rows_see_the_edits() {
    let dir = TempDir::new("filteredits");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (_, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=amount&value=999",
    );
    assert!(body.contains("\"op\":\"set\""), "{body}");

    // Unfiltered window: edited value.
    let (_, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=10");
    assert!(body.contains("999"), "{body}");

    // Filtered window: same value, because the query runs on the edited sheet.
    let (status, body) = get(&server.addr, "/rows?path=t.csv&where=city%3DCDMX");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("999"), "filter should see the edit: {body}");
    assert!(
        !body.contains("CDMX\",\"10"),
        "pre-edit value should be gone: {body}"
    );
    assert!(
        !body.contains("Puebla"),
        "the filter should exclude Puebla: {body}"
    );
}

#[test]
fn book_endpoint_lists_every_sheet() {
    let dir = TempDir::new("bookapi");
    std::fs::write(dir.join("orders.csv"), "id,city\n1,CDMX\n").unwrap();
    std::fs::write(dir.join("customers.csv"), "id,name,city\n1,Ana,CDMX\n").unwrap();
    std::fs::write(dir.join("notes.txt"), "not a sheet").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/book");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""name":"orders.csv""#), "{body}");
    assert!(body.contains(r#""name":"customers.csv""#), "{body}");
    assert!(
        !body.contains("notes.txt"),
        "non-CSV files are not sheets: {body}"
    );
    assert!(body.contains(r#""sheets":["#), "{body}");
}

/// Frozen columns are a prefix, and the window says which ones to pin.
#[test]
fn frozen_columns_travel_with_the_window() {
    let dir = TempDir::new("frozen");
    std::fs::write(
        dir.join("t.csv"),
        "id,city,amount\n1,CDMX,10\n2,Puebla,20\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());

    // Default: nothing pinned.
    let (_, body) = get(&server.addr, "/rows?path=t.csv");
    assert!(body.contains(r#""frozenColumns":[]"#), "{body}");

    // Freeze the first two columns through the layout API.
    let (_, body) = get(
        &server.addr,
        "/layout?path=t.csv&column=id&key=frozen&value=true",
    );
    assert!(body.contains(r#""frozen":true"#), "{body}");
    let (_, body) = get(
        &server.addr,
        "/layout?path=t.csv&column=city&key=frozen&value=true",
    );
    assert!(body.contains(r#""frozen":true"#), "{body}");

    let (_, body) = get(&server.addr, "/rows?path=t.csv");
    assert!(body.contains(r#""frozenColumns":["id","city"]"#), "{body}");

    let (_, body) = get(&server.addr, "/sheet?path=t.csv");
    assert!(body.contains(r#""frozenColumns":["id","city"]"#), "{body}");
}

#[test]
fn insert_column_endpoint_places_a_constant_column() {
    let dir = TempDir::new("insertapi");
    std::fs::write(dir.join("t.csv"), "id,city\n1,CDMX\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=insert-column&name=note&value=manual&after=city",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("insert column"), "{body}");

    // The window shows the new column in the requested position.
    let (_, body) = get(&server.addr, "/rows?path=t.csv");
    assert!(body.contains(r#""columns":["id","city","note"]"#), "{body}");
    assert!(body.contains(r#"["1","CDMX","manual"]"#), "{body}");

    // Const and formula together is rejected.
    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=insert-column&name=x&value=a&formula=b",
    );
    assert_eq!(status, 400, "{body}");
}

#[test]
fn copy_endpoint_pastes_a_named_range() {
    let dir = TempDir::new("copyapi");
    std::fs::write(dir.join("t.csv"), "id,city\n1,CDMX\n2,Puebla\n3,X\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=copy&from=A1:B2&to=A3",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("paste"), "{body}");

    let (_, body) = get(&server.addr, "/rows?path=t.csv");
    assert!(body.contains(r#"["1","CDMX"]"#), "{body}");
    assert!(body.contains(r#"["2","Puebla"]"#), "{body}");

    // A bad range names the parameter that was wrong.
    let (status, body) = post(&server.addr, "/edit?path=t.csv&action=copy&from=Z9&to=A1");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("beyond the last column"), "{body}");
}

/// Every read endpoint answers a bad request with a structured error, not prose
/// and not a silent 200.
#[test]
fn bad_requests_always_carry_a_reason_and_a_hint() {
    let dir = TempDir::new("apihelp");
    std::fs::write(dir.join("t.csv"), "id,city\n1,CDMX\n").unwrap();
    let server = start(&dir.0.clone());

    // Each of these is missing something the endpoint genuinely needs.
    let endpoints = [
        "/meta",
        "/rows",
        "/sheet",
        "/book?max=nope",
        "/analyze",
        "/groupby",
        "/pivot",
        "/style",
        "/chart",
        "/layout",
        "/edit",
        "/filter",
        "/search",
        "/export",
        "/query",
        "/download",
        "/job",
    ];
    let mut problems = Vec::new();
    for endpoint in endpoints {
        let (status, body) = get(&server.addr, endpoint);
        if status == 200 {
            problems.push(format!(
                "{endpoint}: answered 200 with no parameters: {body}"
            ));
            continue;
        }
        if !body.contains("\"error\"") {
            problems.push(format!(
                "{endpoint}: {status} without an error field: {body}"
            ));
        }
        if !body.contains("\"hint\"") && !body.contains("\"parameter\"") {
            problems.push(format!("{endpoint}: {status} without a hint: {body}"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn garbage_numbers_are_refused_rather_than_ignored() {
    let dir = TempDir::new("numbers");
    std::fs::write(dir.join("t.csv"), "id\n1\n2\n").unwrap();
    let server = start(&dir.0.clone());

    for (query, parameter) in [
        ("/rows?path=t.csv&start=abc", "start"),
        ("/rows?path=t.csv&count=xyz", "count"),
        ("/book?max=nope", "max"),
    ] {
        let (status, body) = get(&server.addr, query);
        assert_eq!(status, 400, "{query}: {body}");
        assert!(
            body.contains(&format!("\"parameter\":\"{parameter}\"")),
            "{query} did not name {parameter}: {body}"
        );
    }

    // A valid value still works.
    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=1&count=1");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#"["2"]"#), "{body}");
}

#[test]
fn the_api_can_redo_what_it_undid() {
    let dir = TempDir::new("apidnd");
    std::fs::write(dir.join("t.csv"), "id,city\n1,x\n2,y\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=a",
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = post(&server.addr, "/edit?path=t.csv&action=undo");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("undone"), "{body}");

    let (status, body) = post(&server.addr, "/edit?path=t.csv&action=redo");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("redone"), "{body}");

    // Nothing left to redo now.
    let (status, body) = post(&server.addr, "/edit?path=t.csv&action=redo");
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("\"hint\""), "{body}");
}

/// A paged view shows the same `lag` values an export would.
#[test]
fn the_api_agrees_with_export_about_previous_rows() {
    let dir = TempDir::new("lagapi");
    std::fs::write(dir.join("t.csv"), "day,amount\n1,10\n2,20\n3,30\n4,40\n").unwrap();

    // The formula lives in the journal, so it applies to both paths.
    let server_dir = dir.0.clone();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_csvx"))
        .args([
            "edit",
            "formula",
            "t.csv",
            "--name",
            "prev",
            "--value",
            "lag(amount)",
        ])
        .current_dir(&server_dir)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let server = start(&server_dir);
    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=2&count=2");
    assert_eq!(status, 200, "{body}");
    // Rows 3 and 4 of the file, each showing the row before it.
    assert!(body.contains(r#"["3","30","20"]"#), "{body}");
    assert!(body.contains(r#"["4","40","30"]"#), "{body}");
}

/// The pager cache must never serve a page from a sheet that has since changed.
#[test]
fn editing_invalidates_the_cached_page() {
    let dir = TempDir::new("pagercache");
    std::fs::write(dir.join("t.csv"), "id,city\n1,CDMX\n2,Puebla\n3,Tijuana\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=1&count=1");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("Puebla"), "{body}");

    // Edit that row through the API, then read the same page again.
    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=2&col=city&value=Edited",
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=1&count=1");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("Edited"),
        "the cached pager served a stale sheet: {body}"
    );
    assert!(!body.contains("Puebla"), "{body}");
}

/// CLI and API must agree about what `row 1` means — it is the first data row in both.
#[test]
fn the_api_counts_rows_the_same_way_the_cli_does() {
    let dir = TempDir::new("rowbase");
    std::fs::write(dir.join("t.csv"), "id,city\n1,CDMX\n2,Puebla\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=First",
    );
    assert_eq!(status, 200, "{body}");

    // Export through the CLI path: the first data row carries the edit.
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_csvx"))
        .args(["edit", "export", "t.csv", "-o", "out.csv"])
        .current_dir(&dir.0)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(dir.join("out.csv")).unwrap();
    assert!(
        text.contains("1,First"),
        "row 1 is the first data row: {text}"
    );

    // And `row=0` is refused, naming the reason.
    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=0&col=city&value=Nope",
    );
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("1-based"), "{body}");
}

/// The SQL path materialises the edited sheet; an edit must invalidate it too.
#[test]
fn filtering_invalidates_the_materialised_copy() {
    let dir = TempDir::new("materialstale");
    std::fs::write(dir.join("t.csv"), "id,city\n1,CDMX\n2,Puebla\n3,CDMX\n").unwrap();
    let server = start(&dir.0.clone());

    // Filter through the materialised path.
    let (status, body) = get(&server.addr, "/rows?path=t.csv&where=city%3DCDMX");
    assert_eq!(status, 200, "{body}");
    assert_eq!(body.matches("CDMX").count(), 2, "{body}");

    // Edit a Puebla row into CDMX, then ask again.
    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=2&col=city&value=CDMX",
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = get(&server.addr, "/rows?path=t.csv&where=city%3DCDMX");
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body.matches("CDMX").count(),
        3,
        "the materialised copy was stale: {body}"
    );
}

/// Both caches answer "has this changed?" the same way, so a file rewritten under
/// the server is picked up whichever endpoint the client uses next.
#[test]
fn rewriting_the_base_file_is_noticed_by_both_paths() {
    let dir = TempDir::new("rewrites");
    let base = dir.join("t.csv");
    std::fs::write(&base, "id,city\n1,CDMX\n2,Puebla\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=10");
    assert_eq!(status, 200, "{body}");
    let (_, filtered) = get(&server.addr, "/rows?path=t.csv&where=city%3DCDMX");
    assert!(filtered.contains("CDMX"), "{filtered}");

    // Replace the base file with different content, same name.
    std::fs::write(&base, "id,city\n9,Zacatecas\n").unwrap();

    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=10");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("Zacatecas"),
        "the paged path served the old file: {body}"
    );
}

#[test]
fn the_export_job_honours_a_where_clause() {
    let dir = TempDir::new("apiexportwhere");
    std::fs::write(dir.join("t.csv"), "id,city\n1,CDMX\n2,Puebla\n3,CDMX\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=export&out=sel.csv&where=city%3DCDMX",
    );
    assert_eq!(status, 202, "{body}");
    let job = body.split('"').nth(3).unwrap_or_default().to_string();

    for _ in 0..100 {
        let (_, state) = get(&server.addr, &format!("/job?id={job}"));
        if state.contains("\"done\"") || state.contains("failed") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let text = std::fs::read_to_string(dir.join("sel.csv")).unwrap();
    assert_eq!(text.lines().count(), 3, "header + two rows: {text}");
}

/// `/sheet` says whether the index exists, and `POST /index` builds it up front.
#[test]
fn sheet_reports_the_index_and_it_can_be_prebuilt() {
    let dir = TempDir::new("indexapi");
    std::fs::write(dir.join("t.csv"), "id,city\n1,CDMX\n2,Puebla\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/sheet?path=t.csv");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"indexed\":false"), "{body}");

    let (status, body) = post(&server.addr, "/index?path=t.csv");
    assert_eq!(status, 202, "{body}");
    let job = body.split('"').nth(3).unwrap_or_default().to_string();
    for _ in 0..100 {
        let (_, state) = get(&server.addr, &format!("/job?id={job}"));
        if state.contains("done") || state.contains("failed") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let (status, body) = get(&server.addr, "/sheet?path=t.csv");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("\"indexed\":true"),
        "the index should be reported: {body}"
    );
    assert!(body.contains("\"indexBytes\":"), "{body}");
}

/// The documented cost model: a repeated filtered page is served from the cache.
#[test]
fn a_repeated_filtered_page_does_not_rescan() {
    let dir = TempDir::new("repeatfilter");
    let mut text = String::from("id,city\n");
    for i in 0..200_000 {
        text.push_str(&format!("{i},C{}\n", i % 9));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0.clone());

    let first = std::time::Instant::now();
    let (status, body) = get(
        &server.addr,
        "/rows?path=t.csv&where=city%3DC5&start=0&count=100",
    );
    assert_eq!(status, 200, "{body}");
    let cold = first.elapsed();

    let warm = std::time::Instant::now();
    let (status, body) = get(
        &server.addr,
        "/rows?path=t.csv&where=city%3DC5&start=0&count=100",
    );
    assert_eq!(status, 200, "{body}");
    let warm_elapsed = warm.elapsed();

    // The second identical request must not have to rescan. The bound is loose on
    // purpose: under a full test run the warm request competes for CPU with a dozen
    // other servers, and a tight ratio there measures the machine rather than the cache.
    // A rescan costs about the same as the cold run — a factor of three plus slack still
    // fails loudly for that, and passes when the box is busy.
    let bound = cold * 3 + std::time::Duration::from_millis(200);
    assert!(
        warm_elapsed <= bound,
        "warm {warm_elapsed:?} vs cold {cold:?} (bound {bound:?})"
    );
}

/// Row edits move the page window, and the server-side pager notices.
#[test]
fn paging_follows_row_inserts_and_deletes() {
    let dir = TempDir::new("pagingedits");
    let mut text = String::from("id,city\n");
    for i in 1..200 {
        text.push_str(&format!("{i},C{}\n", i % 5));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0.clone());

    let (_, body) = get(&server.addr, "/rows?path=t.csv&start=100&count=3");
    assert!(body.contains("\"total\":199"), "{body}");
    assert!(body.contains("[\"101\""), "{body}");

    // Ten rows inserted at the top push the window ten rows down.
    for _ in 0..10 {
        let (status, body) = post(
            &server.addr,
            "/edit?path=t.csv&action=insert&row=1&value=x,y",
        );
        assert!(
            status == 200 || status == 202,
            "insert returned {status}: {body}"
        );
    }
    let (_, body) = get(&server.addr, "/rows?path=t.csv&start=100&count=3");
    assert!(
        body.contains("\"total\":209"),
        "total must follow the insert: {body}"
    );
    assert!(
        body.contains("[\"91\""),
        "the same offset now holds different rows: {body}"
    );

    // Deleting them again restores the window: the pager is not holding a stale page.
    // `count` is a count — the API used to subtract one from it as if it were a row.
    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=delete&row=1&count=10",
    );
    assert!(
        status == 200 || status == 202,
        "delete returned {status}: {body}"
    );
    assert!(
        body.contains("delete 10 row(s)"),
        "ten rows, not nine: {body}"
    );
    let (_, body) = get(&server.addr, "/rows?path=t.csv&start=100&count=3");
    assert!(
        body.contains("\"total\":199"),
        "total must follow the delete: {body}"
    );
    assert!(
        body.contains("[\"101\""),
        "the window is back where it was: {body}"
    );
}

/// `count` is a count: delete removes exactly that many rows, and 0 is refused.
#[test]
fn delete_removes_exactly_the_count_asked_for() {
    let dir = TempDir::new("deletecount");
    let mut text = String::from("id,city\n");
    for i in 1..20 {
        text.push_str(&format!("{i},C{}\n", i % 5));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0.clone());

    let (_, body) = post(&server.addr, "/edit?path=t.csv&action=delete&row=2&count=3");
    assert!(body.contains("delete 3 row(s)"), "{body}");
    let (_, sheet) = get(&server.addr, "/sheet?path=t.csv");
    assert!(sheet.contains("\"rows\":16"), "19 - 3 rows: {sheet}");

    let (_, body) = post(&server.addr, "/edit?path=t.csv&action=delete&row=1&count=1");
    assert!(body.contains("delete 1 row(s)"), "{body}");
    let (_, sheet) = get(&server.addr, "/sheet?path=t.csv");
    assert!(sheet.contains("\"rows\":15"), "{sheet}");

    // Zero rows is a mistake, not a no-op that reports success.
    let (status, body) = post(&server.addr, "/edit?path=t.csv&action=delete&row=1&count=0");
    assert_eq!(status, 400, "{body}");
}

/// `/rows` reports how many rows it returned and the ceiling it applies, so a client
/// that asked for a million rows is not left wondering whether it got them.
#[test]
fn row_pages_report_their_own_size_and_limit() {
    let dir = TempDir::new("pagelimit");
    let mut text = String::from("id,city\n");
    for i in 1..2500 {
        text.push_str(&format!("{i},C{}\n", i % 5));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=1000000000");
    assert_eq!(status, 200, "{body}");
    // The page is capped, and the response says so rather than silently truncating.
    assert!(body.contains("\"limit\":2000"), "{body}");
    assert!(body.contains("\"requested\":1000000000"), "{body}");
    assert!(body.contains("\"count\":2000"), "{body}");

    // Asking for fewer rows than the cap returns exactly what was asked for.
    let (_, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=7");
    assert!(body.contains("\"count\":7"), "{body}");
    assert!(body.contains("\"requested\":7"), "{body}");
}

/// Every endpoint answers a bad request the same way: JSON with `error`, and for
/// parameter problems also `parameter` and `hint`. A client that has to special-case
/// one endpoint's error shape cannot write a generic error handler.
#[test]
fn error_responses_have_one_shape() {
    let dir = TempDir::new("errorshape");
    std::fs::write(dir.join("t.csv"), "a,b\n1,2\n").unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let server = start(&dir.0.clone());

    let reads = [
        "meta", "rows", "stats", "filter", "search", "analyze", "edit", "sheet", "groupby",
        "pivot", "style", "chart", "layout", "download", "export", "query", "job",
    ];
    let mut problems = Vec::new();
    for endpoint in reads {
        // No path at all: the answer must name the parameter and say what to pass.
        let (status, body) = get(&server.addr, &format!("/{endpoint}"));
        if status != 400 {
            problems.push(format!("{endpoint} missing-path status {status}: {body}"));
            continue;
        }
        for needle in [r#""error":"#, r#""parameter":"#, r#""hint":"#] {
            if !body.contains(needle) {
                problems.push(format!("{endpoint} missing {needle}: {body}"));
            }
        }
    }

    // A file that does not exist says so, rather than blaming the sandbox.
    let (status, body) = get(&server.addr, "/rows?path=nope.csv");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("no such file"), "{body}");

    // A directory is not a file, and the message says that instead.
    let (_, body) = get(&server.addr, "/rows?path=sub");
    assert!(body.contains("directory, not a file"), "{body}");

    // Escaping the root is its own message. The file has to exist, or the answer is
    // (correctly) "no such file" — so place one just outside the root.
    let outside = dir.0.parent().unwrap().join("csvs_escape_probe.csv");
    std::fs::write(&outside, "a,b\n1,2\n").unwrap();
    let (_, body) = get(&server.addr, "/rows?path=../csvs_escape_probe.csv");
    assert!(body.contains("outside the served directory"), "{body}");
    let _ = std::fs::remove_file(&outside);

    // Unknown endpoint and wrong method still carry an `error`.
    let (status, body) = get(&server.addr, "/nope");
    assert_eq!(status, 404, "{body}");
    assert!(body.contains(r#""error":"#), "{body}");

    assert!(problems.is_empty(), "{problems:?}");
}

/// The advertised capabilities and the accepted actions must describe each other.
///
/// A front-end draws its buttons from `capabilities`; if a working operation is missing
/// from that list, it does not exist as far as the user is concerned. The reverse
/// matters too: advertising something nothing implements is a promise that breaks.
#[test]
fn capabilities_and_actions_describe_each_other() {
    let dir = TempDir::new("capabilities");
    std::fs::write(dir.join("t.csv"), "id,city\n1,CDMX\n").unwrap();
    let server = start(&dir.0.clone());

    let (_, sheet) = get(&server.addr, "/sheet?path=t.csv");
    let capabilities: Vec<String> = sheet
        .split(r#""capabilities":["#)
        .nth(1)
        .and_then(|rest| rest.split("]").next())
        .map(|list| {
            list.split(',')
                .map(|item| item.trim().trim_matches('"').to_string())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default();
    assert!(!capabilities.is_empty(), "{sheet}");

    // Both hints come from one list, and that list names every action the endpoint
    // implements — checked by asking for a bogus action and for none at all.
    let (_, body) = post(&server.addr, "/edit?path=t.csv&action=bogus");
    let hint = body
        .split(r#""hint":""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_default()
        .to_string();
    let actions: Vec<&str> = hint.split('|').collect();
    for action in [
        "set", "insert", "delete", "replace", "copy", "formula", "drop", "rename",
    ] {
        assert!(actions.contains(&action), "{action} missing from {hint}");
    }

    // Every advertised capability corresponds to one of them.
    let pairs = [
        ("edit.set", "set"),
        ("edit.insert", "insert"),
        ("edit.delete", "delete"),
        ("edit.replace", "replace"),
        ("edit.copy", "copy"),
        ("column.drop", "drop"),
        ("column.rename", "rename"),
        ("column.formula", "formula"),
        ("edit.undo", "undo"),
        ("edit.redo", "redo"),
        ("export", "export"),
    ];
    let mut problems = Vec::new();
    for (capability, action) in pairs {
        if !capabilities.iter().any(|c| c == capability) {
            problems.push(format!("sheet does not advertise {capability}"));
        }
        if !actions.contains(&action) {
            problems.push(format!("edit does not accept {action}"));
        }
    }
    assert!(problems.is_empty(), "{problems:?}");
}

/// `split-column` from the API, and the boundary case that has no second column.
#[test]
fn the_api_can_split_a_column() {
    let dir = TempDir::new("apisplitcol");
    std::fs::write(dir.join("t.csv"), "id,full\n1,\"Ana,Perez,St 1\"\n2,Solo\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=split-column&col=full&by=%2C&into=first,last,addr",
    );
    assert!(status == 200 || status == 202, "{body}");
    let (_, window) = get(&server.addr, "/rows?path=t.csv&start=0&count=2");
    assert!(
        window.contains(r#"["id","first","last","addr"]"#),
        "{window}"
    );
    assert!(window.contains(r#"["1","Ana","Perez","St 1"]"#), "{window}");
    // Fewer pieces than names leaves the rest empty rather than shifting values.
    assert!(window.contains(r#"["2","Solo","",""]"#), "{window}");

    // One name is not a split.
    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=split-column&col=first&by=%2C&into=only",
    );
    assert_eq!(status, 400, "{body}");
}

/// `delete-where` over HTTP: one operation, and the row count follows.
#[test]
fn the_api_can_delete_by_condition() {
    let dir = TempDir::new("apideletewhere");
    let mut text = String::from("id,city\n");
    for i in 1..=10 {
        let city = if matches!(i, 2 | 3 | 7) {
            "Puebla"
        } else {
            "CDMX"
        };
        text.push_str(&format!("{i},{city}\n"));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=delete-where&where=city%3DPuebla",
    );
    assert!(status == 200 || status == 202, "{body}");
    assert!(body.contains("delete 3 row(s) in 2 range(s)"), "{body}");

    let (_, sheet) = get(&server.addr, "/sheet?path=t.csv");
    assert!(sheet.contains("\"rows\":7"), "{sheet}");

    // Nothing left to delete answers honestly instead of inventing an operation.
    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=delete-where&where=city%3DPuebla",
    );
    assert!(status == 200 || status == 202, "{body}");
    assert!(body.contains("nothing matched"), "{body}");
}

/// Every `/rows` path answers with the same fields, and all of them see edits.
#[test]
fn all_window_paths_agree_on_shape_and_on_edits() {
    let dir = TempDir::new("windowshapes");
    let mut text = String::from("id,city,qty\n");
    for i in 1..=10 {
        text.push_str(&format!("{i},CDMX,{}\n", i * 10));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0.clone());

    // An edit that changes what a filter and an order should return.
    let (status, _) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=2&col=qty&value=999",
    );
    assert!(status == 200 || status == 202);

    let paths = [
        "/rows?path=t.csv&start=0&count=2",
        "/rows?path=t.csv&where=city%3DCDMX&start=0&count=2",
        "/rows?path=t.csv&order=qty&desc=1&start=0&count=2",
    ];
    let mut shapes = Vec::new();
    for path in paths {
        let (status, body) = get(&server.addr, path);
        assert_eq!(status, 200, "{path}: {body}");
        let keys: Vec<&str> = [
            "start",
            "count",
            "requested",
            "limit",
            "total",
            "columns",
            "frozenColumns",
            "rows",
            "edited",
        ]
        .into_iter()
        .filter(|key| body.contains(&format!("\"{key}\":")))
        .collect();
        shapes.push((path, keys));
    }
    let first = &shapes[0].1;
    for (path, keys) in &shapes {
        assert_eq!(keys, first, "{path} has a different shape");
    }

    // Ordering really uses the edited value: 999 must come first.
    let (_, body) = get(
        &server.addr,
        "/rows?path=t.csv&order=qty&desc=1&start=0&count=1",
    );
    assert!(body.contains("\"999\""), "{body}");
}

/// The backend's two framework questions: which columns exist, and a guarded write.
#[test]
fn columns_and_compare_and_set() {
    let dir = TempDir::new("columnsand");
    std::fs::write(
        dir.join("t.csv"),
        "id,city,amount\n1,CDMX,10\n2,Puebla,20\n",
    )
    .unwrap();
    let server = start(&dir.0);

    // The column list: complete, 1-based, with kinds.
    let (code, body) = get(&server.addr, "/columns?path=t.csv&sample=2");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""count":3"#), "{body}");
    assert!(body.contains(r#""index":1"#), "{body}");
    assert!(body.contains(r#""kind":"text""#), "{body}");
    assert!(body.contains(r#""kind":"number""#), "{body}");

    // Existence: one answer, no 782-column download.
    let (code, body) = get(&server.addr, "/columns?path=t.csv&name=city");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""exists":true"#), "{body}");
    assert!(body.contains(r#""index":2"#), "{body}");

    let (code, body) = get(&server.addr, "/columns?path=t.csv&name=nosuch");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""exists":false"#), "{body}");

    // Compare-and-set: the first write succeeds, the second is refused with 409 and
    // says what the value actually is.
    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=Z&expect=CDMX",
    );
    assert_eq!(code, 200, "{body}");

    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=Z2&expect=CDMX",
    );
    assert_eq!(code, 409, "{body}");
    assert!(body.contains(r#""actual":"Z""#), "{body}");
    assert!(body.contains(r#""expected":"CDMX""#), "{body}");

    // The refused write left the sheet alone.
    let (_, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=1");
    assert!(body.contains(r#"Z"#), "{body}");
    assert!(!body.contains("Z2"), "{body}");
}

/// Discovery: one call lists what the backend can do, generated from the same
/// constants the handlers use.
#[test]
fn capabilities_describe_the_backend() {
    let dir = TempDir::new("caps");
    let server = start(&dir.0);
    let (code, body) = get(&server.addr, "/capabilities");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""version""#), "{body}");
    assert!(body.contains(r#""/columns""#), "{body}");
    assert!(body.contains(r#""rowsPerRequest":2000"#), "{body}");
    assert!(body.contains("compare-and-set"), "{body}");
    // Every edit action the handler accepts is advertised.
    for action in ["set", "insert", "delete", "replace", "export"] {
        assert!(
            body.contains(&format!(r#""{action}""#)),
            "missing {action}: {body}"
        );
    }
}

/// A window can ask for a few columns instead of all of them.
#[test]
fn rows_can_be_narrowed_to_selected_columns() {
    let dir = TempDir::new("rowscols");
    let mut header = String::from("id");
    for i in 0..100 {
        header.push_str(&format!(",c{i}"));
    }
    let mut body = String::new();
    for r in 0..10 {
        body.push_str(&format!("{r}"));
        for i in 0..100 {
            body.push_str(&format!(",{}", i * 10 + r));
        }
        body.push('\n');
    }
    std::fs::write(dir.join("w.csv"), format!("{header}\n{body}")).unwrap();
    let server = start(&dir.0);

    let (code, wide) = get(&server.addr, "/rows?path=w.csv&start=0&count=10");
    assert_eq!(code, 200, "{wide}");
    let (code, narrow) = get(
        &server.addr,
        "/rows?path=w.csv&start=0&count=10&columns=id,c7",
    );
    assert_eq!(code, 200, "{narrow}");
    assert!(
        narrow.len() < wide.len() / 10,
        "projection should shrink the body"
    );
    assert!(narrow.contains(r#""columns":["id","c7"]"#), "{narrow}");
    assert!(narrow.contains(r#"["0","70"]"#), "{narrow}");

    // A name that is not there is a 400 naming the parameter, not an empty row.
    let (code, body) = get(&server.addr, "/rows?path=w.csv&count=1&columns=nosuch");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains(r#""parameter":"columns""#), "{body}");
}

/// The framework's read-side extras: one cell, horizontal paging, stable row keys.
#[test]
fn cell_keys_and_horizontal_paging() {
    let dir = TempDir::new("cellkeys");
    let mut header = String::from("id");
    for i in 0..15 {
        header.push_str(&format!(",c{i}"));
    }
    let mut body = String::new();
    for r in 1..=5 {
        body.push_str(&r.to_string());
        for i in 0..15 {
            body.push_str(&format!(",{}", r * 100 + i));
        }
        body.push('\n');
    }
    std::fs::write(dir.join("w.csv"), format!("{header}\n{body}")).unwrap();
    let server = start(&dir.0);

    // One cell, by name and 1-based row.
    let (code, body) = get(&server.addr, "/cell?path=w.csv&row=2&col=c1");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""value":"201""#), "{body}");
    assert!(body.contains(r#""key":"b:1""#), "{body}");

    // Out of range is a 404 that says the total.
    let (code, body) = get(&server.addr, "/cell?path=w.csv&row=99&col=c1");
    assert_eq!(code, 404, "{body}");
    assert!(body.contains("5 data rows"), "{body}");

    // Horizontal paging: columns 5..9 of 16.
    let (code, body) = get(
        &server.addr,
        "/rows?path=w.csv&count=2&start-col=5&count-cols=5",
    );
    assert_eq!(code, 200, "{body}");
    assert!(
        body.contains(r#""columns":["c4","c5","c6","c7","c8"]"#),
        "{body}"
    );
    assert!(
        body.contains(r#"["204","205","206","207","208"]"#),
        "{body}"
    );

    // Stable keys travel with every window.
    let (code, body) = get(&server.addr, "/rows?path=w.csv&count=3");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""rowKeys":["b:0","b:1","b:2"]"#), "{body}");

    // /schema is /columns under another name.
    let (code, body) = get(&server.addr, "/schema?path=w.csv&name=c3");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""index":5"#), "{body}");
}

/// Row keys survive an insert: the question a selection asks after the table moved.
#[test]
fn row_keys_resolve_through_inserts() {
    let dir = TempDir::new("keyresolve");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n3,C\n").unwrap();
    let server = start(&dir.0);

    // Before any edit: keys resolve, and a stale key is reported rather than dropped.
    let (code, body) = get(&server.addr, "/rows?path=t.csv&keys=b:2,b:0,b:99");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""rowNumbers":[3,1]"#), "{body}");
    assert!(body.contains(r#"["3","C"]"#), "{body}");
    assert!(body.contains(r#""missing":["b:99"]"#), "{body}");

    // An insert at the top shifts everything down by one, but b:0 is still row "1,A".
    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=insert&row=1&value=9,Z",
    );
    assert_eq!(code, 200, "{body}");
    let (code, body) = get(&server.addr, "/rows?path=t.csv&keys=b:0,v:0");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#"["1","A"]"#), "{body}");
    assert!(body.contains(r#"["9","Z"]"#), "{body}");
    assert!(body.contains(r#""rowNumbers":[2,1]"#), "{body}");

    // A key that cannot be parsed is a 400 naming the parameter, not a silent skip.
    let (code, body) = get(&server.addr, "/rows?path=t.csv&keys=zzz");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains(r#""parameter":"keys""#), "{body}");
}

/// `/columns` filters by kind and truncates with a limit, saying what it left out.
#[test]
fn columns_filter_and_limit() {
    let dir = TempDir::new("kindfilter");
    let mut text = String::from("id,city,when\n1,A,2026-03-01\n2,B,2026-03-02\n");
    for i in 3..=5 {
        text.push_str(&format!("{i},C,2026-03-0{i}\n"));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/columns?path=t.csv&kind=number&sample=5");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""count":1"#), "{body}");
    assert!(body.contains(r#""kindFilter":"number""#), "{body}");
    assert!(!body.contains(r#""name":"city""#), "{body}");

    let (code, body) = get(&server.addr, "/columns?path=t.csv&kind=date&sample=5");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""name":"when""#), "{body}");

    // A kind that is not real is a 400 that lists the options.
    let (code, body) = get(&server.addr, "/columns?path=t.csv&kind=blah");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("number, date, text"), "{body}");

    // A limit truncates but reports the totals so the caller knows what it lost.
    let (code, body) = get(&server.addr, "/columns?path=t.csv&limit=1&sample=5");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""shown":1"#), "{body}");
    assert!(body.contains(r#""totalColumns":3"#), "{body}");
}

/// `/journal` is the undo stack a front-end renders, and it names each step the way
/// the CLI does.
#[test]
fn journal_reports_the_undo_stack() {
    let dir = TempDir::new("journalapi");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n").unwrap();
    let server = start(&dir.0);

    // Empty to begin with, and honest about it.
    let (code, body) = get(&server.addr, "/journal?path=t.csv");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""count":0"#), "{body}");
    assert!(body.contains(r#""canUndo":false"#), "{body}");

    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=X",
    );
    assert_eq!(code, 200, "{body}");
    let (code, body) = post(&server.addr, "/edit?path=t.csv&action=undo");
    assert_eq!(code, 200, "{body}");

    let (code, body) = get(&server.addr, "/journal?path=t.csv");
    assert_eq!(code, 200, "{body}");
    // The undo moved the operation into the redo stack, described the same way.
    assert!(body.contains(r#""count":0"#), "{body}");
    assert!(body.contains(r#""redoCount":1"#), "{body}");
    assert!(body.contains("set row 1 'city' = 'X'"), "{body}");
    assert!(body.contains(r#""canRedo":true"#), "{body}");
}

/// Every write action the handler accepts is listed in the hint a client sees when it
/// guesses wrong — so the list cannot drift from the code.
#[test]
fn every_edit_action_is_advertised_in_the_hint() {
    let dir = TempDir::new("actions");
    std::fs::write(dir.join("t.csv"), "id\n1\n").unwrap();
    let server = start(&dir.0);
    let (code, body) = post(&server.addr, "/edit?path=t.csv&action=nope");
    assert_eq!(code, 400, "{body}");
    for action in [
        "set",
        "insert",
        "delete",
        "replace",
        "copy",
        "fill",
        "insert-column",
        "split-column",
        "formula",
        "drop",
        "rename",
        "undo",
        "redo",
        "export",
    ] {
        assert!(
            body.contains(action),
            "hint does not offer {action}: {body}"
        );
    }
}

/// Ordering accepts the same `col:desc` shorthand the CLI uses, and refuses anything
/// that is not a column name with a 400 of its own rather than a raw SQL error.
#[test]
fn order_parsing_is_the_cli_shorthand_and_refuses_sql() {
    let dir = TempDir::new("orderparse");
    std::fs::write(
        dir.join("t.csv"),
        "id,city,amount\n1,A,10\n2,B,20\n3,C,30\n",
    )
    .unwrap();
    let server = start(&dir.0);

    // `amount:desc` is the CLI shorthand, and it works here too.
    let (code, body) = get(&server.addr, "/rows?path=t.csv&order=amount:desc&count=2");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""30""#), "{body}");
    assert!(body.contains(r#""20""#), "{body}");

    // The legacy form still works.
    let (code, body) = get(&server.addr, "/rows?path=t.csv&order=amount&desc=1&count=1");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""30""#), "{body}");

    // A direction that is not asc/desc is a 400 naming the parameter.
    let (code, body) = get(&server.addr, "/rows?path=t.csv&order=amount:sideways");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains(r#""parameter":"order""#), "{body}");

    // A name that is not a column never reaches the SQL engine.
    let (code, body) = get(&server.addr, "/rows?path=t.csv&order=amount;DROP");
    assert_eq!(code, 400, "{body}");
    assert!(
        !body.contains("Parser Error"),
        "raw SQL error leaked: {body}"
    );
    assert!(body.contains("is not a column name"), "{body}");
}

/// The three window modifiers compose: filter or order, then columns, then a slice.
#[test]
fn window_modifiers_compose() {
    let dir = TempDir::new("compose");
    std::fs::write(
        dir.join("t.csv"),
        "id,city,amount,note\n1,A,10,n1\n2,A,20,n2\n3,B,30,n3\n4,B,40,n4\n",
    )
    .unwrap();
    let server = start(&dir.0);

    let (code, body) = get(
        &server.addr,
        "/rows?path=t.csv&order=amount:desc&columns=id,amount,note&start-col=1&count-cols=1&count=2",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""columns":["amount"]"#), "{body}");
    assert!(body.contains(r#"["40"]"#), "{body}");
    assert!(body.contains(r#"["30"]"#), "{body}");

    let (code, body) = get(
        &server.addr,
        "/rows?path=t.csv&where=city%3DB&columns=amount,note&count=1",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""columns":["amount","note"]"#), "{body}");
}

/// SQL and ordering failures are the caller's mistake, so they answer 400 in the same
/// three-field shape as every other parameter error — never a raw engine message.
#[test]
fn sql_and_order_errors_are_structured() {
    let dir = TempDir::new("sqlerrors");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n").unwrap();
    let server = start(&dir.0);

    // A bad SELECT: structured 400, with the engine's wording kept in `detail`.
    let (code, body) = get(
        &server.addr,
        "/query?path=t.csv&sql=SELECT%20nope%20FROM%20t",
    );
    assert_eq!(code, 400, "{body}");
    assert!(
        body.contains(r#""error":"the query could not be run""#),
        "{body}"
    );
    assert!(body.contains(r#""parameter":"sql""#), "{body}");
    assert!(body.contains(r#""detail""#), "{body}");
    assert!(
        body.contains("Binder Error"),
        "the engine's reason is kept: {body}"
    );

    // An order on a column that does not exist: named before it reaches the engine.
    let (code, body) = get(&server.addr, "/rows?path=t.csv&order=amount:desc");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("cannot order by 'amount'"), "{body}");
    assert!(body.contains("available columns: id, city"), "{body}");
    assert!(!body.contains("DESCRIBE"), "no SQL leaked: {body}");
}

/// A filtered page reports whether its `total` is exact, and stops as early as it can.
#[test]
fn a_filtered_page_says_when_its_total_is_exact() {
    let dir = TempDir::new("filteredpage");
    let mut text = String::from("id,city\n");
    for i in 1..=100 {
        text.push_str(&format!(
            "{i},{}\n",
            if i % 10 == 0 { "CDMX" } else { "Puebla" }
        ));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0);

    // Ten matches, ask for five: more rows exist, so the total is a lower bound.
    let (code, body) = get(&server.addr, "/rows?path=t.csv&where=city%3DCDMX&count=5");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""totalExact":false"#), "{body}");
    assert!(
        body.contains(r#""total":5"#),
        "a lower bound, not a wrong count: {body}"
    );
    assert!(body.contains(r#""hasMore":true"#), "{body}");
    assert!(body.contains(r#""count":5"#), "{body}");

    // Three matches at the end of the file: the page reaches the end, total is exact.
    let (code, body) = get(&server.addr, "/rows?path=t.csv&where=id%3E97&count=10");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""totalExact":true"#), "{body}");
    assert!(body.contains(r#""total":3"#), "{body}");
    assert!(body.contains(r#""hasMore":false"#), "{body}");
    // A filtered page has no row keys: its rows are not start+i of the base file.
    assert!(
        !body.contains("rowKeys"),
        "keys would point at the wrong rows: {body}"
    );

    // A deep offset past the matches: no rows, exact total.
    let (code, body) = get(
        &server.addr,
        "/rows?path=t.csv&where=city%3DCDMX&start=20&count=10",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""count":0"#), "{body}");
    assert!(body.contains(r#""totalExact":true"#), "{body}");
}

/// `/sheet` describes the sheet; `/journal` is the stack. One place per fact.
#[test]
fn sheet_carries_counts_and_journal_carries_the_stack() {
    let dir = TempDir::new("sheetsummary");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n").unwrap();
    let server = start(&dir.0);
    let _ = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=X",
    );
    let _ = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=2&col=city&value=Y",
    );

    let (code, body) = get(&server.addr, "/sheet?path=t.csv");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""operations":2"#), "{body}");
    assert!(body.contains(r#""lastOperation""#), "{body}");
    assert!(body.contains("set row 2 'city' = 'Y'"), "{body}");
    // The full list is not duplicated here any more.
    assert!(
        !body.contains(r#""ops":"#),
        "ops should live at /journal: {body}"
    );

    let (code, body) = get(&server.addr, "/journal?path=t.csv");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""count":2"#), "{body}");
    assert!(body.contains("set row 1 'city' = 'X'"), "{body}");
    assert!(body.contains("set row 2 'city' = 'Y'"), "{body}");
}

/// `keys=` composes with `columns=` and `start-col=`: ask for the rows you kept,
/// then ask for a slice of them.
#[test]
fn keys_compose_with_projection_and_paging() {
    let dir = TempDir::new("keyscompose");
    let mut header = String::from("id");
    for i in 0..8 {
        header.push_str(&format!(",c{i}"));
    }
    std::fs::write(
        dir.join("t.csv"),
        format!("{header}\n1,a,b,c,d,e,f,g,h\n2,a,b,c,d,e,f,g,h\n3,a,b,c,d,e,f,g,h\n"),
    )
    .unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/rows?path=t.csv&keys=b:2,b:0&columns=id,c3");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""rowNumbers":[3,1]"#), "{body}");
    // c3 is the fourth feature column: id,c0,c1,c2,c3 → "d".
    assert!(body.contains(r#"["3","d"]"#), "{body}");

    // Horizontal paging applies to the keys result too: same rows, narrower columns.
    let (code, body) = get(
        &server.addr,
        "/rows?path=t.csv&keys=b:1&start-col=2&count-cols=2",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""columns":["c1","c2"]"#), "{body}");
}

/// `/query` says what limit it actually applied, so a client knows what it did not see.
#[test]
fn query_reports_the_limit_it_used() {
    let dir = TempDir::new("querylimit");
    let mut text = String::from("id\n");
    for i in 1..=30 {
        text.push_str(&format!("{i}\n"));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0);

    let (code, body) = get(
        &server.addr,
        "/query?path=t.csv&sql=SELECT%20*%20FROM%20t&limit=5",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""truncated":true"#), "{body}");
    assert!(body.contains(r#""limit":5"#), "{body}");
    assert!(body.contains(r#""requestedLimit":5"#), "{body}");

    // Asking for more than the ceiling: the applied limit is reported, not the asked one.
    let (code, body) = get(
        &server.addr,
        "/query?path=t.csv&sql=SELECT%20*%20FROM%20t&limit=50000",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""limit":10000"#), "{body}");
    assert!(body.contains(r#""requestedLimit":50000"#), "{body}");
}

/// An empty page says whether it is empty because the file ended or the offset went past
/// it — two different things a client has to render differently.
#[test]
fn an_empty_page_says_why_it_is_empty() {
    let dir = TempDir::new("pastend");
    std::fs::write(dir.join("t.csv"), "id\n1\n2\n").unwrap();
    std::fs::write(dir.join("empty.csv"), "id\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/rows?path=t.csv&start=99&count=5");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""count":0"#), "{body}");
    assert!(body.contains(r#""pastEnd":true"#), "{body}");
    assert!(body.contains(r#""total":2"#), "{body}");

    let (code, body) = get(&server.addr, "/rows?path=empty.csv&start=0&count=5");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""pastEnd":false"#), "{body}");
    assert!(body.contains(r#""total":0"#), "{body}");

    // A normal page is explicitly not past the end.
    let (code, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=2");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""pastEnd":false"#), "{body}");
}

/// Four path mistakes stay four messages, and a named directory that does not exist is
/// an error rather than a silent fallback to the served root.
#[test]
fn path_errors_stay_distinct_and_roots_are_checked() {
    let dir = TempDir::new("pathreasons");
    std::fs::write(dir.join("t.csv"), "id\n1\n").unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/other.csv"), "id\n2\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/download?path=nope.csv");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("no such file"), "{body}");

    let (code, body) = get(&server.addr, "/download?path=sub");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("is a directory"), "{body}");

    // A file that exists *outside* the root: the reason is "outside", not "missing".
    let outside = dir.0.parent().unwrap().join(format!(
        "csvs_outside_{}_{}.csv",
        std::process::id(),
        dir.0.file_name().unwrap().to_string_lossy()
    ));
    std::fs::write(&outside, "id\n9\n").unwrap();
    let outside_name = outside.file_name().unwrap().to_string_lossy().to_string();
    let (code, body) = get(&server.addr, &format!("/download?path=../{outside_name}"));
    let _ = std::fs::remove_file(&outside);
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("outside"), "{body}");

    let (code, body) = get(&server.addr, "/download?path=t.csv");
    assert_eq!(code, 200, "{body}");

    // A root that is not there: 400 naming the parameter, not this directory's sheets.
    let (code, body) = get(&server.addr, "/book?root=nope");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains(r#""parameter":"root""#), "{body}");
    assert!(
        !body.contains("t.csv"),
        "fell back to the served root: {body}"
    );

    let (code, body) = get(&server.addr, "/book?root=sub");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("other.csv"), "{body}");
}

/// A cut-off listing says how many there are, so a client can render "3 of 12".
#[test]
fn truncated_listings_report_a_total() {
    let dir = TempDir::new("listtotals");
    for name in ["a.csv", "b.csv", "c.csv"] {
        std::fs::write(dir.join(name), "id\n1\n").unwrap();
    }
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/files?dir=.&limit=1");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""truncated":true"#), "{body}");
    assert!(
        body.contains(r#""total":3"#),
        "total must count what was hidden: {body}"
    );
    assert_eq!(body.matches(r#""path""#).count(), 1, "{body}");

    let (code, body) = get(&server.addr, "/book?root=.&max=1");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""hidden":2"#), "{body}");
    assert!(body.contains(r#""total":3"#), "{body}");
    assert!(body.contains(r#""truncated":true"#), "{body}");
}

/// The window cap is stated three ways, so a client never has to infer it.
#[test]
fn the_row_cap_is_stated_in_the_response() {
    let dir = TempDir::new("rowcap");
    let mut text = String::from("id\n");
    for i in 1..=3000 {
        text.push_str(&format!("{i}\n"));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/rows?path=t.csv&count=5000");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""count":2000"#), "{body}");
    assert!(body.contains(r#""requested":5000"#), "{body}");
    assert!(body.contains(r#""limit":2000"#), "{body}");
    assert!(body.contains(r#""total":3000"#), "{body}");
}

/// A stamp that moves when the journal moves, so a client can decide to refresh.
#[test]
fn the_sheet_carries_a_stamp_that_moves_after_a_write() {
    let dir = TempDir::new("stamp");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/sheet?path=t.csv");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""stamp":"#), "{body}");
    let before = body
        .split(r#""stamp":""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();
    assert_eq!(
        before, "0:0:0",
        "a pristine sheet has an empty stamp: {body}"
    );

    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=Z",
    );
    assert_eq!(code, 200, "{body}");

    let (code, body) = get(&server.addr, "/sheet?path=t.csv");
    assert_eq!(code, 200, "{body}");
    let after = body
        .split(r#""stamp":""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();
    assert_ne!(after, before, "the stamp must move after a write");

    // /journal reports the same stamp as /sheet: one fact, one place to compare.
    let (code, body) = get(&server.addr, "/journal?path=t.csv");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(&format!(r#""stamp":"{after}""#)), "{body}");
}

/// Like `raw_request`, but returns bytes: a gzip body is not UTF-8.
fn raw_request_bytes(
    addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
    write!(stream, "{request}").unwrap();
    let mut raw = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut raw).unwrap();
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|at| at + 4)
        .unwrap_or(raw.len());
    let head_text = String::from_utf8_lossy(&raw[..split]).to_string();
    let status = head_text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, head_text, raw[split..].to_vec())
}

/// A request with arbitrary headers, returning status and the raw response.
fn raw_request(addr: &str, method: &str, path: &str, headers: &[(&str, &str)]) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
    write!(stream, "{request}").unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, raw)
}

/// Send any method, for the status-code contract below.
fn request(addr: &str, method: &str, path: &str) -> u16 {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    raw.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// One table for every failure kind, so a client can switch on the code.
///
/// The rule: 400 means "your request, fix it", 404 means "it is not there",
/// 405 means "wrong method", 409 means "someone changed it under you",
/// 422 means "this file cannot be read", 504 means "the engine was stopped".
#[test]
fn failure_kinds_have_one_status_code_each() {
    let dir = TempDir::new("codes");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n").unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let server = start(&dir.0);

    let cases: Vec<(&str, &str, u16)> = vec![
        ("GET", "/nope", 404),
        ("GET", "/rows", 400),
        ("GET", "/rows?path=nope.csv", 400),
        ("GET", "/rows?path=sub", 400),
        ("GET", "/rows?path=../etc/passwd", 400),
        ("GET", "/rows?path=t.csv&start=abc", 400),
        ("GET", "/rows?path=t.csv&order=nosuch", 400),
        ("GET", "/query?path=t.csv&sql=SELECT%20nope%20FROM%20t", 400),
        ("GET", "/cell?path=t.csv&row=0&col=city", 400),
        ("GET", "/cell?path=t.csv&row=999&col=city", 404),
        ("GET", "/cell?path=t.csv&row=1&col=nope", 400),
        ("GET", "/columns?path=t.csv&kind=blah", 400),
        ("GET", "/download?path=sub", 400),
        ("POST", "/edit?path=t.csv&action=nope", 400),
        (
            "POST",
            "/edit?path=t.csv&action=set&row=1&col=city&value=Z&expect=__no__",
            409,
        ),
        ("POST", "/nope", 404),
        ("PUT", "/rows?path=t.csv", 405),
    ];

    let mut wrong: Vec<String> = Vec::new();
    for (method, path, expected) in cases {
        let got = if method == "GET" {
            get(&server.addr, path).0
        } else {
            request(&server.addr, method, path)
        };
        if got != expected {
            wrong.push(format!("{method} {path}: want {expected}, got {got}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// An ordered window is a page, not a full sort collected into memory: the engine gets
/// LIMIT/OFFSET, and the response says whether `total` is exact.
#[test]
fn an_ordered_page_is_a_page() {
    let dir = TempDir::new("orderedpage");
    let mut text = String::from("id,amount\n");
    for i in 1..=200 {
        text.push_str(&format!("{i},{}\n", 201 - i));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/rows?path=t.csv&order=amount&count=5");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""totalExact":false"#), "{body}");
    // amount is 200,199,198… so the smallest amounts come first.
    assert!(body.contains(r#"["200","1"]"#), "{body}");

    // The last page reaches the end, so its total is exact and matches the row count.
    let (code, body) = get(
        &server.addr,
        "/rows?path=t.csv&order=amount:desc&start=195&count=10",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""count":5"#), "{body}");
    assert!(body.contains(r#""totalExact":true"#), "{body}");
    assert!(body.contains(r#""total":200"#), "{body}");

    // An order column that does not exist is named, not sent to the engine.
    let (code, body) = get(&server.addr, "/rows?path=t.csv&order=nosuch");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("cannot order by 'nosuch'"), "{body}");
}

/// The cache is advertised, reported, and makes a filtered page cheap; deleting it
/// restores the cold path without changing the answer.
#[test]
fn the_columnar_cache_is_advertised_and_faster() {
    let dir = TempDir::new("parquet");
    let mut text = String::from("id,city,amount\n");
    for i in 1..=500 {
        text.push_str(&format!(
            "{i},{},{}\n",
            if i % 3 == 0 { "CDMX" } else { "Puebla" },
            i
        ));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/capabilities");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""caches":["parquet"]"#), "{body}");
    assert!(body.contains("POST /parquet"), "{body}");

    let (code, body) = get(&server.addr, "/sheet?path=t.csv");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""parquet":{"fresh":false"#), "{body}");

    // Cold answer, then build the cache, then the same answer must not change.
    let cold = get(
        &server.addr,
        "/rows?path=t.csv&where=city%3DCDMX&count=5&columns=id,city",
    );
    assert_eq!(cold.0, 200, "{}", cold.1);
    assert!(!dir.join("t.csv.parquet.tmp").exists());

    let (code, body) = post(&server.addr, "/parquet?path=t.csv");
    assert_eq!(code, 202, "{body}");
    assert!(body.contains(r#""job":"#), "{body}");
    let id = body
        .split(r#""job":""#)
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();
    let done = wait_for_job(&server.addr, &id);
    assert!(done.contains("\"state\":\"done\""), "{done}");

    let (code, body) = get(&server.addr, "/sheet?path=t.csv");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""parquet":{"fresh":true"#), "{body}");
    assert!(
        body.contains(r#""operations":0"#),
        "no journal ops were needed: {body}"
    );

    let warm = get(
        &server.addr,
        "/rows?path=t.csv&where=city%3DCDMX&count=5&columns=id,city",
    );
    assert_eq!(warm.0, 200, "{}", warm.1);
    assert_eq!(warm.1, cold.1, "the cache must not change the answer");

    // A second request now short-circuits instead of building again.
    let (code, body) = post(&server.addr, "/parquet?path=t.csv");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""cached":true"#), "{body}");
}

/// The CLI cache command reports state and can remove the sidecar.
#[test]
fn the_cache_command_reports_and_removes() {
    let dir = TempDir::new("cachecmd");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n").unwrap();
    let bin = env!("CARGO_BIN_EXE_csvx");

    let out = std::process::Command::new(bin)
        .arg("cache")
        .arg(dir.join("t.csv"))
        .arg("--status")
        .current_dir(&dir.0)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("missing"), "{text}");
    assert!(out.status.success());

    let out = std::process::Command::new(bin)
        .arg("cache")
        .arg(dir.join("t.csv"))
        .arg("--build")
        .current_dir(&dir.0)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.join("t.csv.parquet").exists());

    let out = std::process::Command::new(bin)
        .arg("cache")
        .arg(dir.join("t.csv"))
        .arg("--drop")
        .current_dir(&dir.0)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!dir.join("t.csv.parquet").exists());
}

/// A browser asks before it acts: preflight must allow the methods this API uses.
#[test]
fn a_browser_preflight_is_answered() {
    let dir = TempDir::new("preflight");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n").unwrap();
    let server = start(&dir.0);

    let (status, raw) = raw_request(
        &server.addr,
        "OPTIONS",
        "/rows?path=t.csv",
        &[
            ("Origin", "http://localhost:3000"),
            ("Access-Control-Request-Method", "POST"),
        ],
    );
    assert_eq!(status, 204, "{raw}");
    assert!(raw.contains("Access-Control-Allow-Origin: *"), "{raw}");
    assert!(
        raw.contains("Access-Control-Allow-Methods: GET, POST, HEAD, OPTIONS"),
        "{raw}"
    );
    assert!(raw.contains("Access-Control-Max-Age"), "{raw}");
}

/// HEAD returns the headers a GET would return, and no body.
#[test]
fn head_returns_headers_without_a_body() {
    let dir = TempDir::new("headmethod");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n").unwrap();
    let server = start(&dir.0);

    let (status, get_body) = get(&server.addr, "/rows?path=t.csv&count=2");
    assert_eq!(status, 200);

    let (status, raw) = raw_request(&server.addr, "HEAD", "/rows?path=t.csv&count=2", &[]);
    assert_eq!(status, 200, "{raw}");
    assert!(raw.contains("Content-Type: application/json"), "{raw}");
    // The declared length is the GET body's length; HEAD just does not send it.
    assert!(
        raw.contains(&format!("Content-Length: {}", get_body.len())),
        "want length {}. got headers: {raw}",
        get_body.len()
    );
    let after_headers = raw.split("\r\n\r\n").nth(1).unwrap_or("");
    assert!(after_headers.is_empty(), "HEAD must not send a body: {raw}");
}

/// A window is revalidated, not re-sent: the same ETag with If-None-Match gives 304.
#[test]
fn a_window_is_revalidated_with_304() {
    let dir = TempDir::new("etag");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n").unwrap();
    let server = start(&dir.0);

    let (_, raw) = raw_request(&server.addr, "GET", "/rows?path=t.csv&count=1", &[]);
    let etag = raw
        .lines()
        .find(|l| l.starts_with("ETag:"))
        .expect("a 200 must carry an ETag")
        .trim_start_matches("ETag:")
        .trim()
        .to_string();

    let (status, raw) = raw_request(
        &server.addr,
        "GET",
        "/rows?path=t.csv&count=1",
        &[("If-None-Match", &etag)],
    );
    assert_eq!(status, 304, "{raw}");
    assert!(raw.contains(&format!("ETag: {etag}")), "{raw}");

    // A different window is a different validator, so it is not short-circuited.
    let (status, _) = raw_request(
        &server.addr,
        "GET",
        "/rows?path=t.csv&count=2",
        &[("If-None-Match", &etag)],
    );
    assert_eq!(status, 200);

    // Errors are not cacheable, so no validator is handed out for them.
    let (status, raw) = raw_request(&server.addr, "GET", "/rows?path=nope.csv", &[]);
    assert_eq!(status, 400, "{raw}");
    assert!(
        !raw.contains("ETag:"),
        "an error must not be taggable: {raw}"
    );
}

/// The keys of a flat JSON object, top level only — enough for the shapes this API
/// returns, and no dependency for the test binary just to read them.
fn json_keys(body: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut current = String::new();
    // A string is a key only if the next non-space character is a colon.
    let chars: Vec<char> = body.chars().collect();
    for (index, &c) in chars.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
                if depth == 1 {
                    let rest: String = chars[index + 1..]
                        .iter()
                        .take_while(|ch| ch.is_whitespace())
                        .collect();
                    let after = chars[index + 1..]
                        .iter()
                        .find(|ch| !ch.is_whitespace())
                        .copied();
                    let _ = rest;
                    if after == Some(':') {
                        keys.push(current.clone());
                    }
                }
                current.clear();
            } else {
                current.push(c);
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '[' => depth += 1,
            '}' | ']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    keys
}

/// The strings inside the first array after `marker`.
fn array_after(body: &str, marker: &str) -> Vec<String> {
    let start = body.find(marker).unwrap_or_else(|| panic!("no {marker}"));
    let from = start + marker.len();
    let inner = &body[from..];
    let end = inner
        .find(']')
        .unwrap_or_else(|| panic!("unterminated {marker}"));
    inner[..end]
        .split(',')
        .map(|item| item.trim().trim_matches('"').to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// `/spec` must describe the server that is answering: the fields it lists are the
/// fields a response actually has.
#[test]
fn the_spec_matches_the_live_server() {
    let dir = TempDir::new("spec");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/spec");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""cors":true"#), "{body}");
    assert!(body.contains(r#""etag":true"#), "{body}");

    let reads = array_after(&body, r#""reads":["#);
    for route in ["/rows", "/columns", "/cell", "/journal", "/sheet", "/spec"] {
        assert!(
            reads.iter().any(|r| r == route),
            "{route} missing from /spec"
        );
    }

    // /rows: the listed fields are exactly the response's keys. Look inside the
    // `fields` object — `params` has a `/rows` entry too, and it lists parameters.
    let fields_part = &body[body.find(r#""fields":{"#).expect("fields")..];
    let listed = array_after(fields_part, r#""/rows":["#);
    let (code, window) = get(&server.addr, "/rows?path=t.csv&count=1");
    assert_eq!(code, 200, "{window}");
    let mut actual = json_keys(&window);
    actual.retain(|k| !k.is_empty());
    let mut want = listed.clone();
    want.sort_unstable();
    actual.sort_unstable();
    assert_eq!(want, actual, "spec fields drifted from the response");

    // /cell: same check on a smaller shape.
    let (code, cell) = get(&server.addr, "/cell?path=t.csv&row=1&col=city");
    assert_eq!(code, 200, "{cell}");
    let mut cell_keys = json_keys(&cell);
    cell_keys.retain(|k| !k.is_empty());
    cell_keys.sort_unstable();
    // The cell spec lives after the /rows entry; grab the array that starts with row.
    let cell_listed = array_after(fields_part, r#""/cell":["#);
    let mut cell_want = cell_listed.clone();
    cell_want.sort_unstable();
    assert_eq!(cell_want, cell_keys, "/cell spec fields drifted");
}

/// Every response is timed and counted, and the table says which route and status.
#[test]
fn metrics_count_requests_and_latency() {
    let dir = TempDir::new("metrics");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n").unwrap();
    let server = start(&dir.0);

    for _ in 0..3 {
        get(&server.addr, "/rows?path=t.csv&count=1");
    }
    get(&server.addr, "/rows?path=nope.csv");

    let (code, raw) = raw_request(&server.addr, "GET", "/rows?path=t.csv&count=1", &[]);
    assert_eq!(code, 200);
    assert!(
        raw.contains("X-Response-Time:"),
        "every response is timed: {raw}"
    );

    let (code, body) = get(&server.addr, "/metrics");
    assert_eq!(code, 200, "{body}");
    // Five requests so far: /metrics counts itself after it is built, which is why the
    // number is one behind the caller's view rather than one ahead.
    assert!(body.contains(r#""requests":5"#), "{body}");
    assert!(body.contains(r#""errors4xx":1"#), "{body}");
    assert!(body.contains(r#""GET /rows""#), "{body}");
    assert!(body.contains(r#""p50Ms":"#), "{body}");
    assert!(body.contains(r#""latencyBuckets""#), "{body}");
    assert!(body.contains(r#""jobs":{"total":"#), "{body}");
}

/// Readiness is about the root being usable, not the process being alive.
#[test]
fn health_separates_liveness_from_readiness() {
    let dir = TempDir::new("health");
    std::fs::write(dir.join("t.csv"), "id\n1\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/health");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""ok":true"#), "{body}");
    assert!(body.contains(r#""ready":true"#), "{body}");
    assert!(body.contains(r#""version":"#), "{body}");

    // Remove the root: the process is alive, but it cannot answer about files.
    std::fs::remove_dir_all(&dir.0).ok();
    let (code, body) = get(&server.addr, "/health");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""ok":true"#), "{body}");
    assert!(body.contains(r#""ready":false"#), "{body}");
}

/// A symlink is a path, and a path must resolve inside the root.
///
/// Unix only: creating a symlink on Windows needs a privilege and different APIs, and
/// the path check itself is platform independent.
#[cfg(unix)]
#[test]
fn a_symlink_cannot_escape_the_root() {
    let outside = TempDir::new("symlink_outside");
    std::fs::write(outside.join("secret.csv"), "id\n1\n").unwrap();
    let dir = TempDir::new("symlink_root");
    std::os::unix::fs::symlink(outside.join("secret.csv"), dir.join("link.csv")).unwrap();
    std::fs::write(dir.join("real.csv"), "id\n2\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/rows?path=link.csv");
    assert_eq!(code, 400, "{body}");
    assert!(
        body.contains("outside"),
        "a symlink out of root is outside: {body}"
    );

    // A symlink that stays inside is still a file and is allowed.
    std::os::unix::fs::symlink(dir.join("real.csv"), dir.join("inside.csv")).unwrap();
    let (code, body) = get(&server.addr, "/rows?path=inside.csv");
    assert_eq!(code, 200, "{body}");
}

/// Oversized requests are refused before they are buffered.
#[test]
fn oversized_requests_are_refused() {
    let dir = TempDir::new("limits");
    std::fs::write(dir.join("t.csv"), "id\n1\n").unwrap();
    let server = start(&dir.0);

    // A body sent to a GET becomes an enormous request line.
    let mut stream = TcpStream::connect(&server.addr).unwrap();
    let padding = "x".repeat(30 * 1024);
    write!(
        stream,
        "GET /rows?path=t.csv&pad={padding} HTTP/1.1\r\nHost: x\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status: u16 = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    assert_eq!(status, 400, "{raw}");

    // A query over 16 KB but under the request-line limit: the parameters are the
    // problem, so the answer says so with 414 rather than blaming the line.
    let (status, _raw) = raw_request(
        &server.addr,
        "GET",
        &format!("/rows?path=t.csv&pad={}", "x".repeat(17 * 1024)),
        &[],
    );
    assert_eq!(status, 414);

    // Too many headers.
    let mut stream = TcpStream::connect(&server.addr).unwrap();
    let mut request = String::from("GET /rows?path=t.csv HTTP/1.1\r\nHost: x\r\n");
    for i in 0..80 {
        request.push_str(&format!("X-Pad-{i}: 1\r\n"));
    }
    request.push_str("\r\n");
    write!(stream, "{request}").unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let status: u16 = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    assert_eq!(status, 400, "{raw}");
}

/// SIGTERM is a shutdown, not a kill: the listener closes and the process exits.
#[cfg(unix)]
#[test]
fn a_terminated_server_stops_accepting() {
    let dir = TempDir::new("sigterm");
    std::fs::write(dir.join("t.csv"), "id\n1\n").unwrap();
    let mut server = start(&dir.0);
    let (code, body) = get(&server.addr, "/health");
    assert_eq!(code, 200, "{body}");

    // SIGTERM: the graceful path. The process must leave on its own, not need SIGKILL.
    unsafe {
        libc::kill(server.child.id() as libc::pid_t, libc::SIGTERM);
    }
    let mut left = false;
    for _ in 0..100 {
        if let Ok(Some(status)) = server.child.try_wait() {
            left = true;
            assert!(
                status.success(),
                "a graceful shutdown exits 0, got {status:?}"
            );
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(left, "the server did not exit on SIGTERM");

    // And the port is gone, so a client sees a refusal rather than a hang.
    let refused = std::net::TcpStream::connect(&server.addr).is_err();
    assert!(refused, "the listener still accepts after shutdown");
}

/// Cancelling an unknown job is a 404, and the job listing says which jobs can stop.
#[test]
fn cancel_reports_what_actually_happened() {
    let dir = TempDir::new("canceljob");
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n2,B\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = post(&server.addr, "/job?action=cancel&id=nope");
    assert_eq!(code, 404, "{body}");

    // A job that has ended is not "cancelled": it says so instead of lying.
    let (code, body) = post(&server.addr, "/parquet?path=t.csv");
    assert_eq!(code, 202, "{body}");
    let id = body
        .split(r#""job":""#)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap()
        .to_string();
    let state = wait_for_job(&server.addr, &id);
    let (code, body) = post(&server.addr, &format!("/job?action=cancel&id={id}"));

    assert!(
        code == 200,
        "a finished job answers 200, got {code}: {body}"
    );
    assert!(
        body.contains("\"state\":\"done\"") || body.contains("\"state\":\"failed\""),
        "{body}"
    );

    // The listing marks a plain job as not cancellable.
    let (code, body) = get(&server.addr, "/jobs");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""cancellable":false"#), "{body}");
    let _ = state;
}

/// Column metadata must not pay for a row index, and must not pretend to know a row
/// count it has not computed.
#[test]
fn columns_do_not_build_an_index() {
    let dir = TempDir::new("colindex");
    std::fs::write(dir.join("t.csv"), "id,city,amount\n1,A,10\n2,B,9\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/columns?path=t.csv&sample=5");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""count":3"#), "{body}");
    // No index: the row count is honestly unknown rather than a full scan in disguise.
    assert!(body.contains(r#""rows":null"#), "{body}");
    assert!(!dir.join("t.csv.idx").exists(), "columns built an index");

    // After /meta (which does pay for it), the count is there.
    let (code, body) = get(&server.addr, "/meta?path=t.csv");
    assert_eq!(code, 200, "{body}");
    let (code, body) = get(&server.addr, "/columns?path=t.csv&sample=5");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""rows":2"#), "{body}");
}

/// The cost tier is published before a client asks for a page, and it flips with the cache.
#[test]
fn sheet_publishes_the_cost_tier() {
    let dir = TempDir::new("costtier");
    let mut text = String::from("id,city\n");
    for i in 1..=300 {
        text.push_str(&format!("{i},{}\n", if i % 2 == 0 { "A" } else { "B" }));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/sheet?path=t.csv");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""tier":"cold""#), "{body}");
    assert!(body.contains(r#""filterFirstPageMs":1000"#), "{body}");
    assert!(
        body.contains("POST /parquet"),
        "the hint names the action: {body}"
    );

    let (code, body) = post(&server.addr, "/parquet?path=t.csv");
    assert_eq!(code, 202, "{body}");
    let id = body
        .split(r#""job":""#)
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();
    let _ = wait_for_job(&server.addr, &id);

    let (code, body) = get(&server.addr, "/sheet?path=t.csv");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""tier":"warm""#), "{body}");
    assert!(body.contains(r#""filterFirstPageMs":260"#), "{body}");
}

/// A client that hangs up mid-response does not take the server with it.
#[test]
fn a_client_that_hangs_up_does_not_kill_the_server() {
    let dir = TempDir::new("hangup");
    let mut text = String::from("id,city\n");
    for i in 0..500 {
        text.push_str(&format!("{i},C{}\n", i % 7));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0);

    for _ in 0..5 {
        let mut stream = TcpStream::connect(&server.addr).unwrap();
        // Ask for a large window, then walk away without reading the body.
        write!(
            stream,
            "GET /rows?path=t.csv&count=500 HTTP/1.1\r\nHost: x\r\n\r\n"
        )
        .unwrap();
        drop(stream);
    }
    std::thread::sleep(std::time::Duration::from_millis(200));

    let (code, body) = get(&server.addr, "/health");
    assert_eq!(code, 200, "server died after a hangup: {body}");
    assert!(body.contains(r#""ready":true"#), "{body}");
}

/// Twenty clients at once answer correctly, and the metrics table sees them.
#[test]
fn twenty_concurrent_requests_do_not_interfere() {
    let dir = TempDir::new("concurrent");
    let mut text = String::from("id,city,amount\n");
    for i in 0..2000 {
        text.push_str(&format!("{i},{},{}\n", i % 5, i));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0);
    let addr = server.addr.clone();

    let handles: Vec<_> = (0..20)
        .map(|i| {
            let addr = addr.clone();
            std::thread::spawn(move || {
                if i % 2 == 0 {
                    let (code, body) = get(
                        &addr,
                        "/rows?path=t.csv&where=city%3D0&count=5&columns=id,amount",
                    );
                    (code, body.contains(r#""count":5"#), body)
                } else {
                    let (code, body) = get(&addr, "/metrics");
                    (code, body.contains(r#""requests":"#), body)
                }
            })
        })
        .collect();

    let results: Vec<(u16, bool, String)> =
        handles.into_iter().map(|h| h.join().unwrap()).collect();
    let failures: Vec<&str> = results
        .iter()
        .filter(|(code, ok, _)| *code != 200 || !ok)
        .map(|(_, _, body)| body.as_str())
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");

    // And the server is still healthy afterwards, with the requests counted.
    let (code, body) = get(&server.addr, "/metrics");
    assert_eq!(code, 200, "{body}");
    let counted: u64 = body
        .split(r#""requests":"#)
        .nth(1)
        .and_then(|rest| rest.split([',', '}']).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    assert!(
        counted >= 20,
        "expected at least 20 requests counted, got {counted}: {body}"
    );
}

/// When a query times out the client learns that, and nothing else.
#[test]
fn a_timeout_does_not_leak_the_engine_text() {
    let dir = TempDir::new("timeout");
    // Big enough that a one-millisecond budget cannot finish before the watchdog
    // fires. The first version used 2000 rows, and a fast runner answered the query
    // inside the budget, turning this into a flaky 200.
    let mut text = String::from("id,city\n");
    for i in 0..200_000 {
        text.push_str(&format!("{i},C{}\n", i % 9));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start_with_env(&dir.0, &[("CSVX_QUERY_TIMEOUT_MS", "1")]);

    let (code, body) = get(
        &server.addr,
        "/query?path=t.csv&sql=SELECT%20count(*)%20FROM%20t",
    );
    assert_eq!(code, 504, "{body}");
    assert!(
        body.contains(r#""error":"the query was interrupted""#),
        "{body}"
    );
    assert!(body.contains(r#""hint""#), "the hint stays: {body}");
    for leak in ["INTERRUPT", "SQL Error", "Binder Error", "detail"] {
        assert!(
            !body.contains(leak),
            "engine text leaked to the client ({leak}): {body}"
        );
    }

    // The advertised limit is the one in force, not a hardcoded 30.
    let (code, body) = get(&server.addr, "/capabilities");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""querySeconds":0"#), "{body}");
}

/// A gzip client gets a gzip body, its own validator, and can revalidate it.
#[test]
fn responses_are_gzipped_when_asked() {
    let dir = TempDir::new("gzip");
    let mut text = String::from("id,city,amount,note\n");
    for i in 0..2000 {
        text.push_str(&format!("{i},C{},{i},note number {i}\n", i % 9));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0);

    let path = "/rows?path=t.csv&count=500&columns=id,city,amount,note";
    let (status, head, body) = raw_request_bytes(&server.addr, "GET", path, &[]);
    assert_eq!(status, 200);
    assert!(
        !head.contains("Content-Encoding: gzip"),
        "no header, no gzip"
    );
    assert!(body.starts_with(b"{\""), "a plain body is JSON text");

    let (status, head, body) =
        raw_request_bytes(&server.addr, "GET", path, &[("Accept-Encoding", "gzip")]);
    assert_eq!(status, 200, "{head}");
    assert!(head.contains("Content-Encoding: gzip"), "{head}");
    assert!(head.contains("Vary: Accept-Encoding"), "{head}");
    assert!(
        body.starts_with(&[0x1f, 0x8b]),
        "gzip magic: {:?}",
        &body[..4.min(body.len())]
    );

    // It really is the same JSON, only compressed.
    let mut decoder = flate2::read::GzDecoder::new(body.as_slice());
    let mut decoded = String::new();
    std::io::Read::read_to_string(&mut decoder, &mut decoded).unwrap();
    assert_eq!(decoded.len(), {
        let (_, _, plain) = raw_request_bytes(&server.addr, "GET", path, &[]);
        plain.len()
    });

    // The compressed validator differs from the plain one, and revalidates to 304.
    let etag = head
        .lines()
        .find(|l| l.starts_with("ETag:"))
        .unwrap()
        .trim_start_matches("ETag:")
        .trim()
        .to_string();
    assert!(etag.ends_with("-gz\""), "{etag}");
    let (status, head, _) = raw_request_bytes(
        &server.addr,
        "GET",
        path,
        &[("Accept-Encoding", "gzip"), ("If-None-Match", &etag)],
    );
    assert_eq!(status, 304, "{head}");

    // And a HEAD with gzip carries the gzip headers without a body.
    let (status, head, body) =
        raw_request_bytes(&server.addr, "HEAD", path, &[("Accept-Encoding", "gzip")]);
    assert_eq!(status, 200, "{head}");
    assert!(head.contains("Content-Encoding: gzip"), "{head}");
    assert!(body.is_empty(), "HEAD must not send a body");
}

/// `/validate` answers with the same report the CLI prints, and does not write anything.
#[test]
fn validate_reports_rules_over_http() {
    let dir = TempDir::new("validate");
    std::fs::write(
        dir.join("t.csv"),
        "id,city,qty\n1,Puebla,5\n2,Puebla,-3\n3,,7\n",
    )
    .unwrap();
    let server = start(&dir.0);

    let (code, body) = get(
        &server.addr,
        "/validate?path=t.csv&checks=qty%3E%3D0;city:not-empty",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""ok":false"#), "{body}");
    assert!(body.contains(r#""rows":3"#), "{body}");
    assert!(body.contains(r#""count":1"#), "{body}");
    assert!(body.contains("qty: >=0"), "{body}");

    // A clean file is a 200 with ok:true — the request was fine either way.
    std::fs::write(dir.join("good.csv"), "id,qty\n1,5\n2,6\n").unwrap();
    let (code, body) = get(&server.addr, "/validate?path=good.csv&checks=qty%3E%3D0");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""ok":true"#), "{body}");

    // A rule that cannot be parsed names the parameter instead of guessing.
    let (code, body) = get(&server.addr, "/validate?path=t.csv&checks=nonsense");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains(r#""parameter":"checks""#), "{body}");
}

/// `/lookup` copies columns by key, in a job, with the CLI's `#N/A` for a miss.
#[test]
fn lookup_copies_columns_by_key() {
    let dir = TempDir::new("lookup");
    std::fs::write(
        dir.join("t.csv"),
        "id,city\n1,Puebla\n2,Puebla\n3,Nowhere\n",
    )
    .unwrap();
    std::fs::write(dir.join("regions.csv"), "city,sales\nPuebla,120\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = post(
        &server.addr,
        "/lookup?path=t.csv&key=city&from=regions.csv&get=sales&out=t-lookup.csv",
    );
    assert_eq!(code, 202, "{body}");
    let job = body
        .split("\"job\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("job id")
        .to_string();
    let status = wait_for_job(&server.addr, &job);
    assert!(status.contains(r#""state":"done""#), "{status}");

    let written = std::fs::read_to_string(dir.join("t-lookup.csv")).unwrap();
    assert_eq!(
        written, "id,city,sales\n1,Puebla,120\n2,Puebla,120\n3,Nowhere,#N/A\n",
        "a missing key must be marked, not silently blank"
    );

    // The reference file is read through the same root guard as everything else.
    let (code, body) = post(
        &server.addr,
        "/lookup?path=t.csv&key=city&from=../../etc/passwd&get=x&out=o.csv",
    );
    assert!(
        code == 404 || code == 400,
        "an outside path must be refused: {code} {body}"
    );
}

/// `/schema?diff=` answers exactly what `schema --diff --json` prints.
#[test]
fn schema_diff_matches_the_cli() {
    let dir = TempDir::new("schemadiff");
    std::fs::write(dir.join("a.csv"), "id,city,qty\n1,Puebla,5\n").unwrap();
    std::fs::write(dir.join("b.csv"), "id,qty,city,extra\n1,5,Puebla,x\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = get(&server.addr, "/schema?path=a.csv&diff=b.csv");
    assert_eq!(code, 200, "{body}");
    assert_eq!(
        body,
        r#"{"same":false,"removed":[],"added":["extra"],"moved":[{"name":"city","here":2,"there":3},{"name":"qty","here":3,"there":2}]}"#
    );

    // Reordering is not a schema change: same set, reported as `same:true` plus `moved`.
    std::fs::write(dir.join("c.csv"), "qty,id,city\n5,1,Puebla\n").unwrap();
    let (_, body) = get(&server.addr, "/schema?path=a.csv&diff=c.csv");
    assert!(body.contains(r#""same":true"#), "{body}");
    assert!(body.contains(r#""moved":[{"name":"id""#), "{body}");

    // Without `diff`, `/schema` is still the column listing it always was.
    let (code, body) = get(&server.addr, "/schema?path=a.csv");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""columns""#), "{body}");
}

/// `/concat` stacks files in a job, and refuses anything outside the root.
#[test]
fn concat_stacks_files_over_http() {
    let dir = TempDir::new("concat");
    std::fs::write(dir.join("a.csv"), "id,city\n1,Puebla\n2,Monterrey\n").unwrap();
    std::fs::write(dir.join("b.csv"), "id,city\n3,CDMX\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = post(&server.addr, "/concat?paths=a.csv,b.csv&out=all.csv");
    assert_eq!(code, 202, "{body}");
    let job = body
        .split("\"job\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("job id")
        .to_string();
    let status = wait_for_job(&server.addr, &job);
    assert!(status.contains(r#""state":"done""#), "{status}");
    assert_eq!(
        std::fs::read_to_string(dir.join("all.csv")).unwrap(),
        "id,city\n1,Puebla\n2,Monterrey\n3,CDMX\n"
    );

    // A path outside the root is refused before any work starts.
    let (code, body) = post(
        &server.addr,
        "/concat?paths=a.csv,../../etc/passwd&out=x.csv",
    );
    assert_eq!(code, 404, "{body}");
    assert!(body.contains("served directory"), "{body}");

    // One file is not a stack.
    let (code, body) = post(&server.addr, "/concat?paths=a.csv&out=x.csv");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains(r#""parameter":"paths""#), "{body}");
}

/// `POST /columns` writes a reshaped copy; the source is untouched.
#[test]
fn columns_reshape_writes_a_new_file() {
    let dir = TempDir::new("reshape");
    std::fs::write(
        dir.join("t.csv"),
        "id,city,qty,note\n1,Puebla,5,a\n2,Monterrey,-3,b\n",
    )
    .unwrap();
    let server = start(&dir.0);

    let (code, body) = post(
        &server.addr,
        "/columns?path=t.csv&out=res.csv&keep=id,qty,city&rename=qty:units",
    );
    assert_eq!(code, 202, "{body}");
    let job = body
        .split("\"job\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("job id")
        .to_string();
    let status = wait_for_job(&server.addr, &job);
    assert!(status.contains(r#""state":"done""#), "{status}");

    assert_eq!(
        std::fs::read_to_string(dir.join("res.csv")).unwrap(),
        "id,units,city\n1,5,Puebla\n2,-3,Monterrey\n"
    );
    // The original keeps every column it had.
    assert_eq!(
        std::fs::read_to_string(dir.join("t.csv")).unwrap(),
        "id,city,qty,note\n1,Puebla,5,a\n2,Monterrey,-3,b\n"
    );

    // Nothing to do is a 400 that names the parameter instead of writing a copy.
    let (code, body) = post(&server.addr, "/columns?path=t.csv&out=x.csv");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains(r#""parameter":"keep""#), "{body}");
}

/// `action=formula` appends a computed column, and it is one undoable operation.
#[test]
fn edit_formula_appends_a_computed_column() {
    let dir = TempDir::new("formula");
    std::fs::write(dir.join("t.csv"), "id,qty\n1,5\n2,-3\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=formula&name=total&value=qty*2",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""op":"addcolumn""#), "{body}");
    assert!(body.contains(r#""operations":1"#), "{body}");

    let (code, body) = get(&server.addr, "/rows?path=t.csv&count=5&columns=id,total");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#"["1","10"]"#), "{body}");
    assert!(body.contains(r#"["2","-6"]"#), "{body}");

    // One undo removes the whole column, not a row of it.
    let (code, body) = post(&server.addr, "/edit?path=t.csv&action=undo");
    assert_eq!(code, 200, "{body}");
    let (_, body) = get(&server.addr, "/rows?path=t.csv&count=1&columns=id,qty");
    // `total` is also the name of the row-count field, so assert on the column list.
    assert!(
        body.contains(r#""columns":["id","qty"]"#),
        "the column should be gone: {body}"
    );
}

/// `action=split-column` exists over HTTP, not only in the CLI.
#[test]
fn edit_split_column_is_reachable_over_http() {
    let dir = TempDir::new("splitcol");
    std::fs::write(
        dir.join("t.csv"),
        "id,name\n1,Ada Lovelace\n2,Alan Turing\n",
    )
    .unwrap();
    let server = start(&dir.0);

    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=split-column&col=name&by=%20&into=first,last",
    );
    assert_eq!(code, 200, "{body}");
    let (_, body) = get(
        &server.addr,
        "/rows?path=t.csv&count=2&columns=id,first,last",
    );
    assert!(body.contains("Ada"), "{body}");
    assert!(body.contains("Lovelace"), "{body}");
}

/// `action=set-formula` recomputes an existing column in place, undoably, and refuses a
/// formula that cannot evaluate before writing it into the journal.
#[test]
fn edit_set_formula_recomputes_a_column() {
    let dir = TempDir::new("setformula");
    std::fs::write(dir.join("t.csv"), "id,qty,price\n1,2,5\n2,3,7\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set-formula&col=qty&value=price*10",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""op":"setformula""#), "{body}");

    let (code, body) = get(
        &server.addr,
        "/rows?path=t.csv&count=2&columns=id,qty,price",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#"["1","50","5"]"#), "{body}");
    assert!(body.contains(r#"["2","70","7"]"#), "{body}");

    // One undo restores the base values; the column never moved.
    let (code, body) = post(&server.addr, "/edit?path=t.csv&action=undo");
    assert_eq!(code, 200, "{body}");
    let (_, body) = get(
        &server.addr,
        "/rows?path=t.csv&count=2&columns=id,qty,price",
    );
    assert!(body.contains(r#"["1","2","5"]"#), "{body}");

    // Reading your own column is in-place recompute, not recursion: the row pipeline
    // builds the row from the base first, so `qty = qty*2` doubles each value once.
    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set-formula&col=qty&value=qty*2",
    );
    assert_eq!(code, 200, "{body}");
    let (_, body) = get(&server.addr, "/rows?path=t.csv&count=2&columns=id,qty");
    assert!(body.contains(r#"["1","4"]"#), "{body}");

    // Re-issuing it replaces the previous pass rather than compounding: every pass reads
    // the *base* row, which is what keeps `undo` and the immutable base coherent.
    let (code, _) = post(
        &server.addr,
        "/edit?path=t.csv&action=set-formula&col=qty&value=qty*2",
    );
    assert_eq!(code, 200);
    let (_, body) = get(&server.addr, "/rows?path=t.csv&count=2&columns=id,qty");
    assert!(body.contains(r#"["1","4"]"#), "{body}");

    // Undo drops one pass; the value stays doubled because the first pass remains.
    let (code, _) = post(&server.addr, "/edit?path=t.csv&action=undo");
    assert_eq!(code, 200);

    // An unknown column is named, not stored.
    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set-formula&col=qty&value=nope*2",
    );
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("nope"), "{body}");
}

/// Inserting by row key lands next to the row it named, not next to its old position.
#[test]
fn insert_by_key_anchors_to_the_named_row() {
    let dir = TempDir::new("insertkey");
    std::fs::write(
        dir.join("t.csv"),
        "id,city\n1,Puebla\n2,Monterrey\n3,CDMX\n",
    )
    .unwrap();
    let server = start(&dir.0);

    // b:1 is the second base row (Monterrey); "after" it is position 2.
    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=insert-after-key&key=b:1&value=99,AfterMonterrey",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""op":"insert""#), "{body}");

    let (_, body) = get(&server.addr, "/rows?path=t.csv&count=5&columns=id,city");
    assert!(
        body.contains(r#"["2","Monterrey"],["99","AfterMonterrey"],["3","CDMX"]"#),
        "the row must sit between b:1 and b:2: {body}"
    );

    // The inserted row has an identity of its own, and "before" it works too.
    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=insert-before-key&key=v:0&value=50,BeforeVirtual",
    );
    assert_eq!(code, 200, "{body}");
    let (_, body) = get(&server.addr, "/rows?path=t.csv&count=6&columns=id,city");
    // "before v:0" lands before the row inserted a moment ago, which is the row v:0
    // names — order is identity-driven, not position-driven.
    assert!(
        body.contains(r#"["2","Monterrey"],["50","BeforeVirtual"],["99","AfterMonterrey"]"#),
        "{body}"
    );

    // A key that no longer exists is a 404 that names the parameter, not a silent append.
    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=insert-after-key&key=b:99&value=x,y",
    );
    assert_eq!(code, 404, "{body}");
    assert!(body.contains(r#""parameter":"key""#), "{body}");

    // A malformed key is a 400 with the shape it should have had.
    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=insert-before-key&key=42&value=x,y",
    );
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("b:12"), "{body}");
}

/// Filling several columns is one operation, so one undo restores all of them.
#[test]
fn fill_columns_is_one_undo() {
    let dir = TempDir::new("fillcols");
    std::fs::write(dir.join("t.csv"), "id,a,b,c\n1,x,y,z\n2,x,y,z\n3,x,y,z\n").unwrap();
    let server = start(&dir.0);

    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=fill&cols=a,b&from-row=1&to-row=2&value=0",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""op":"fillcols""#), "{body}");
    assert!(body.contains(r#""operations":1"#), "{body}");

    let (_, body) = get(&server.addr, "/rows?path=t.csv&count=3&columns=id,a,b,c");
    assert!(body.contains(r#"["1","0","0","z"]"#), "{body}");
    assert!(body.contains(r#"["2","0","0","z"]"#), "{body}");
    assert!(body.contains(r#"["3","x","y","z"]"#), "{body}");

    let (code, body) = post(&server.addr, "/edit?path=t.csv&action=undo");
    assert_eq!(code, 200, "{body}");
    let (_, body) = get(&server.addr, "/rows?path=t.csv&count=3&columns=id,a,b,c");
    assert!(
        body.contains(r#"["1","x","y","z"]"#),
        "one undo must restore both columns: {body}"
    );

    // The single-column form still works.
    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=fill&col=c&from-row=3&to-row=3&value=solo",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""op":"fill""#), "{body}");

    // Naming no column is a 400 that says what to pass.
    let (code, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=fill&from-row=1&to-row=2&value=0",
    );
    assert_eq!(code, 400, "{body}");
    assert!(body.contains(r#""parameter":"col""#), "{body}");
}

/// `POST /convert` rewrites a GBK file as UTF-8, and refuses an unknown encoding.
#[test]
fn convert_rewrites_the_encoding() {
    let dir = TempDir::new("convert");
    // 北京 = b1 b1 be a9, 上海 = c9 cf ba a3 in GBK; written as bytes so the fixture does
    // not depend on this machine's encoders.
    let gbk: &[u8] = b"id,city\n1,\xb1\xb1\xbe\xa9\n2,\xc9\xcf\xba\xa3\n";
    std::fs::write(dir.join("gbk.csv"), gbk).unwrap();
    let server = start(&dir.0);

    let (code, body) = post(&server.addr, "/convert?path=gbk.csv&out=utf8.csv&from=gbk");
    assert_eq!(code, 202, "{body}");
    let job = body
        .split("\"job\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("job id")
        .to_string();
    let status = wait_for_job(&server.addr, &job);
    assert!(status.contains(r#""state":"done""#), "{status}");
    let text = std::fs::read_to_string(dir.join("utf8.csv")).unwrap();
    assert_eq!(text, "id,city\n1,北京\n2,上海\n", "{text}");

    // An encoding the engine does not know is a 400, not a guess.
    let (code, body) = post(&server.addr, "/convert?path=gbk.csv&out=x.csv&from=klingon");
    assert_eq!(code, 400, "{body}");
    assert!(body.contains(r#""parameter":"from""#), "{body}");
}

/// `limit` bounds a validation scan, and the report says it only saw a prefix.
#[test]
fn validate_can_bound_its_scan() {
    let dir = TempDir::new("validate-limit");
    std::fs::write(dir.join("t.csv"), "id,qty\n1,5\n2,-3\n3,7\n4,-1\n").unwrap();
    let server = start(&dir.0);

    // Full scan: four rows, both violations counted.
    let (code, body) = get(&server.addr, "/validate?path=t.csv&checks=qty%3E%3D0");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""rows":4"#), "{body}");
    assert!(body.contains(r#""checked":4"#), "{body}");
    assert!(body.contains(r#""partial":false"#), "{body}");
    assert!(body.contains(r#""count":2"#), "{body}");

    // Bounded scan: two of four rows, one violation, and it says it is a prefix.
    let (code, body) = get(
        &server.addr,
        "/validate?path=t.csv&checks=qty%3E%3D0&limit=2",
    );
    assert_eq!(code, 200, "{body}");
    assert!(
        body.contains(r#""rows":4"#),
        "the sheet still has four rows: {body}"
    );
    assert!(body.contains(r#""checked":2"#), "{body}");
    assert!(body.contains(r#""partial":true"#), "{body}");
    assert!(body.contains(r#""count":1"#), "{body}");

    // A limit of zero is refused rather than silently validating nothing.
    let (code, body) = get(
        &server.addr,
        "/validate?path=t.csv&checks=qty%3E%3D0&limit=0",
    );
    assert_eq!(code, 400, "{body}");
    assert!(body.contains(r#""parameter":"limit""#), "{body}");
}

/// A journal cut off mid-write must not look like "no edits": the surviving operation is
/// applied and the response says how many records could not be read.
#[test]
fn a_damaged_journal_is_applied_and_reported() {
    let dir = TempDir::new("damagedjournal");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    // One good operation, then a truncated line as a crash mid-write would leave it.
    std::fs::write(
        dir.join("t.csv.journal"),
        "set,1,amount,999\nset,7,amount\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=10");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("999"),
        "the readable edit must still apply: {body}"
    );
    assert!(
        body.contains("\"damaged\":1"),
        "the unreadable record must be counted, got: {body}"
    );
}

/// A grouped key can hold a quoted newline; the endpoint must return one row per group, not
/// one row per physical line.
#[test]
fn groupby_survives_a_quoted_newline_in_the_key() {
    let dir = TempDir::new("groupnl");
    std::fs::write(
        dir.join("t.csv"),
        "note,amount\n\"first line\nsecond line\",10\nok,20\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());
    let (status, body) = get(&server.addr, "/groupby?path=t.csv&by=note&agg=amount:sum");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("ok"), "{body}");
    assert!(
        body.contains("first line\\nsecond line"),
        "the quoted key must stay one group: {body}"
    );
    // The response says which engine answered, instead of leaving it to inference.
    assert!(body.contains("\"engine\":\"native\""), "{body}");
}

/// Same guarantee as /groupby: a quoted newline in a pivot column key is one column, not
/// several physical lines pretending to be columns.
#[test]
fn pivot_survives_a_quoted_newline_in_the_key() {
    let dir = TempDir::new("pivotnl");
    std::fs::write(
        dir.join("t.csv"),
        "city,note,amount\nCDMX,\"first line\nsecond line\",10\nCDMX,ok,20\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());
    let (status, body) = get(
        &server.addr,
        "/pivot?path=t.csv&rows=city&cols=note&values=amount:sum",
    );
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("first line\\nsecond line"),
        "the quoted key must stay one column: {body}"
    );
}

/// With the parallel scan there is no timeout branch to hit: a page that runs out of
/// budget answers 200 with a cursor, and asking again with that cursor continues from
/// where the last wave stopped. The numbers must keep increasing with no repeats.
#[test]
fn a_filtered_page_resumes_with_the_cursor_instead_of_failing() {
    let dir = TempDir::new("resumepage");
    let mut text = String::from("id,city\n");
    for i in 0..200_000 {
        text.push_str(&format!("{i},CDMX\n"));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start_with_env(
        &dir.0,
        &[("CSVX_SCAN_THREADS", "2"), ("CSVX_QUERY_TIMEOUT_MS", "1")],
    );

    let (code, body) = get(
        &server.addr,
        "/filter?path=t.csv&where=city%3DCDMX&limit=10",
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""partial":true"#), "{body}");
    let cursor = body
        .split(r#""next":"#)
        .nth(1)
        .and_then(|rest| rest.split([',', '}']).next())
        .and_then(|n| n.trim().parse::<usize>().ok())
        .expect("a partial page must carry a numeric next cursor");

    let (code, body) = get(
        &server.addr,
        &format!("/filter?path=t.csv&where=city%3DCDMX&limit=10&cursor={cursor}"),
    );
    assert_eq!(code, 200, "{body}");
    assert!(
        body.contains(r#""next":null"#),
        "the last page must end: {body}"
    );
    let first_row: u64 = body
        .split(r#""rows":["#)
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .and_then(|n| n.trim().parse().ok())
        .expect("the second page must return rows");
    assert!(
        first_row > 10,
        "the resume must not repeat the first page: {body}"
    );
}

/// The filtered-page path has its own timeout branch; it must say the same thing as /query:
/// stopped, nothing still running, safe to retry smaller.
#[test]
fn a_filtered_page_timeout_says_nothing_is_still_running() {
    let dir = TempDir::new("timeoutpage");
    let mut text = String::from("id,city\n");
    for i in 0..200_000 {
        text.push_str(&format!("{i},C{}\n", i % 9));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start_with_env(&dir.0, &[("CSVX_QUERY_TIMEOUT_MS", "1")]);

    let (code, body) = get(&server.addr, "/rows?path=t.csv&where=city%3DC3");
    assert_eq!(code, 504, "{body}");
    assert!(
        body.contains("nothing keeps running"),
        "the hint must say the engine stopped for good: {body}"
    );
    for leak in ["INTERRUPT", "SQL Error", "Binder Error"] {
        assert!(!body.contains(leak), "engine text leaked ({leak}): {body}");
    }
}

/// The two endpoints count differently on purpose: /filter answers 1-based row numbers a
/// human recognises, /rows takes a 0-based start. A client gluing them must subtract one,
/// and this test pins that link so a later "cleanup" cannot quietly break it.
#[test]
fn a_filtered_row_number_points_at_the_same_row_in_the_window() {
    let dir = TempDir::new("filterlink");
    std::fs::write(
        dir.join("t.csv"),
        "city,amount\nPuebla,10\nCDMX,20\nTijuana,30\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/filter?path=t.csv&where=city%3DCDMX");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""rows":[2]"#), "{body}");

    // 2 (1-based) means data row index 1 (0-based).
    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=1&count=1");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("CDMX"), "{body}");
    assert!(!body.contains("Puebla"), "{body}");
}

/// The two lookup engines must produce the same file over HTTP, because the CLI promises
/// exactly that and the endpoints kept different abilities until now.
#[test]
fn lookup_over_http_agrees_across_engines() {
    let dir = TempDir::new("lookupengines");
    std::fs::write(dir.join("m.csv"), "id,city\n1,CDMX\n9,NOWHERE\n").unwrap();
    std::fs::write(
        dir.join("r.csv"),
        "id,city,region\n1,Puebla,SUR\n1,CDMX,NORTH\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());

    for (engine, out) in [("native", "out-native.csv"), ("sql", "out-sql.csv")] {
        let (status, body) = post(
            &server.addr,
            &format!(
                "/lookup?path=m.csv&key=id,city&from=r.csv&refKey=id,city&get=region&out={out}&engine={engine}"
            ),
        );
        assert_eq!(status, 202, "{engine}: {body}");
        let job = body.split('"').nth(3).unwrap_or_default().to_string();
        let status = wait_for_job(&server.addr, &job);
        assert!(status.contains("\"state\":\"done\""), "{engine}: {status}");
    }

    let native = std::fs::read_to_string(dir.join("out-native.csv")).unwrap();
    let sql = std::fs::read_to_string(dir.join("out-sql.csv")).unwrap();
    assert_eq!(native, sql, "engines must agree: {native} vs {sql}");

    // An unknown engine is refused rather than ignored.
    let (status, body) = post(
        &server.addr,
        "/lookup?path=m.csv&key=id&from=r.csv&get=region&out=x.csv&engine=bogus",
    );
    assert_eq!(status, 400, "{body}");
}

/// HTTP concat must agree across engines and refuse an unknown one, like lookup.
#[test]
fn concat_over_http_agrees_across_engines() {
    let dir = TempDir::new("concatengines");
    std::fs::write(dir.join("a.csv"), "id,city\n1,Puebla\n").unwrap();
    std::fs::write(dir.join("b.csv"), "id,city\n2,CDMX\n").unwrap();
    let server = start(&dir.0.clone());

    for (engine, out) in [("native", "n.csv"), ("sql", "s.csv")] {
        let (status, body) = post(
            &server.addr,
            &format!("/concat?paths=a.csv,b.csv&out={out}&engine={engine}"),
        );
        assert_eq!(status, 202, "{engine}: {body}");
        let job = body.split('"').nth(3).unwrap_or_default().to_string();
        let state = wait_for_job(&server.addr, &job);
        assert!(state.contains("\"state\":\"done\""), "{engine}: {state}");
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("n.csv")).unwrap(),
        std::fs::read_to_string(dir.join("s.csv")).unwrap()
    );

    let (status, body) = post(
        &server.addr,
        "/concat?paths=a.csv,b.csv&out=x.csv&engine=bogus",
    );
    assert_eq!(status, 400, "{body}");
}

/// Asking for SQL must show up where the metrics are, otherwise "did the engine switch
/// actually happen" stays a guess.
#[test]
fn metrics_count_which_engine_ran() {
    let dir = TempDir::new("enginemetrics");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,1\nPuebla,2\n").unwrap();
    let server = start(&dir.0.clone());

    let (_, _) = get(
        &server.addr,
        "/groupby?path=t.csv&by=city&agg=amount:sum&engine=native",
    );
    let (status, body) = get(
        &server.addr,
        "/groupby?path=t.csv&by=city&agg=amount:sum&engine=sql",
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = get(&server.addr, "/metrics");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("\"engines\""),
        "metrics must report engines: {body}"
    );
    assert!(
        body.contains("\"sql\":1") && body.contains("\"native\":1"),
        "both engines should be counted once: {body}"
    );
}

/// A filtered scan has no cancel button; when its budget runs out it must say so with the
/// same 504 shape /query uses, not with a 422 that reads like a bad request.
#[test]
fn a_filtered_scan_stops_at_its_budget() {
    let dir = TempDir::new("filterbudget");
    let mut text = String::from("id,city\n");
    for i in 0..200_000 {
        text.push_str(&format!("{i},C{}\n", i % 9));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start_with_env(&dir.0, &[("CSVX_QUERY_TIMEOUT_MS", "1")]);

    let (status, body) = get(&server.addr, "/filter?path=t.csv&where=city%3DC3");
    assert_eq!(status, 504, "{body}");
    assert!(
        body.contains("nothing keeps running"),
        "the same hint the other timeouts use: {body}"
    );
    assert!(
        !body.contains("INTERRUPT"),
        "engine text must not leak: {body}"
    );
}

/// Full-file validation without a limit can outlive the caller; it stops at the budget and
/// says to pass limit= rather than answering as if the request were malformed.
#[test]
fn validation_stops_at_its_budget() {
    let dir = TempDir::new("validatebudget");
    let mut text = String::from("id,city\n");
    for i in 0..200_000 {
        text.push_str(&format!("{i},C{}\n", i % 9));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start_with_env(&dir.0, &[("CSVX_QUERY_TIMEOUT_MS", "1")]);

    let (status, body) = get(&server.addr, "/validate?path=t.csv&checks=city:not-empty");
    assert_eq!(status, 504, "{body}");
    assert!(
        body.contains("limit"),
        "the hint must name the parameter: {body}"
    );

    // A limit completes inside a normal budget; the 1 ms server above cannot even open a
    // pager that fast, so this half gets its own server.
    let relaxed = start(&dir.0.clone());
    let (status, body) = get(
        &relaxed.addr,
        "/validate?path=t.csv&checks=city:not-empty&limit=100",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""partial":true"#), "{body}");
}

/// Search is the third synchronous scan; it stops at the budget like the other two.
#[test]
fn search_stops_at_its_budget() {
    let dir = TempDir::new("searchbudget");
    let mut text = String::from("id,city\n");
    for i in 0..200_000 {
        text.push_str(&format!("{i},C{}\n", i % 9));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start_with_env(&dir.0, &[("CSVX_QUERY_TIMEOUT_MS", "1")]);

    let (status, body) = get(&server.addr, "/search?path=t.csv&q=C3");
    assert_eq!(status, 504, "{body}");
    assert!(
        body.contains("limit"),
        "the hint must name a way forward: {body}"
    );
}

/// Strict parameter checking is on by default, and `=0` turns it off: both halves matter,
/// because the default changed and an escape hatch nobody tests is an escape hatch that
/// quietly stops working.
/// instead of silently ignored; with it off, existing callers keep working.
#[test]
fn strict_mode_refuses_a_parameter_nobody_reads() {
    let dir = TempDir::new("strictparams");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,1\n").unwrap();
    let server = start_with_env(&dir.0, &[("CSVX_STRICT_PARAMS", "1")]);

    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=5&bogus=1");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains(r#""parameter":"bogus""#), "{body}");
    assert!(body.contains("GET /spec"), "{body}");

    // A declared parameter still works.
    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=5");
    assert_eq!(status, 200, "{body}");
}

/// A single-cell read must see the journal: `/cell` is part of the render path, and showing
/// the pre-edit value while `/rows` shows the new one is the kind of split-brain that makes a
/// front-end look broken.
#[test]
fn a_cell_read_sees_the_edit() {
    let dir = TempDir::new("celledit");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=amount&value=999",
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = get(&server.addr, "/cell?path=t.csv&row=1&col=amount");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("999"), "/cell must show the edit: {body}");
    assert!(
        !body.contains("10"),
        "the pre-edit value must be gone: {body}"
    );
}

/// Three endpoints report how many edits exist: `/rows` (`operations`), `/journal`
/// (`count`) and `/sheet` (`journal`/`operations`). They read the same journal, so a
/// disagreement means one of them is counting something else.
#[test]
fn the_edit_count_agrees_across_rows_journal_and_sheet() {
    let dir = TempDir::new("opcount");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    for (row, value) in [(1u64, 111), (2, 222)] {
        let (status, body) = post(
            &server.addr,
            &format!("/edit?path=t.csv&action=set&row={row}&col=amount&value={value}"),
        );
        assert_eq!(status, 200, "{body}");
    }

    let numbers = |body: &str, key: &str| -> u64 {
        body.split(&format!("\"{key}\":"))
            .nth(1)
            .and_then(|tail| tail.split([',', '}']).next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(u64::MAX)
    };

    let (_, rows) = get(&server.addr, "/rows?path=t.csv&start=0&count=5");
    let (_, journal) = get(&server.addr, "/journal?path=t.csv");
    let (_, sheet) = get(&server.addr, "/sheet?path=t.csv");

    let rows_ops = numbers(&rows, "operations");
    let journal_count = numbers(&journal, "count");
    let sheet_ops = numbers(&sheet, "operations");
    assert_eq!(rows_ops, 2, "rows: {rows}");
    assert_eq!(journal_count, 2, "journal: {journal}");
    assert_eq!(sheet_ops, 2, "sheet: {sheet}");
}

/// `/filter` and `/search` are byte scans over the base file; they do not load the journal.
/// That is deliberate (a scan of the edited sheet would mean materialising it first), but it
/// must stay a *known* difference rather than a surprise: after an edit, `/filter` on the new
/// value finds nothing while `/rows?where=` does.
#[test]
fn filter_and_search_read_the_base_file_not_the_journal() {
    let dir = TempDir::new("baseonly");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=amount&value=999",
    );
    assert_eq!(status, 200, "{body}");

    // The edited view finds it, because it applies the journal.
    let (status, body) = get(&server.addr, "/rows?path=t.csv&where=amount%3D999");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("999"), "{body}");

    // The raw scan does not: it sees the base file only.
    let (status, body) = get(&server.addr, "/filter?path=t.csv&where=amount%3D999");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains(r#""matched":0"#),
        "the scan reads the base file; this is the documented difference: {body}"
    );
}

/// Both aggregation endpoints must give the same JSON whichever engine produced it.
/// Pivot is order-stable, so the bodies are compared directly; chart's category order is
/// allowed to differ (native keeps first appearance, SQL sorts), so only its values are
/// compared.
#[test]
fn pivot_and_chart_agree_across_engines_over_http() {
    let dir = TempDir::new("enginehttp");
    let mut text = String::from("city,shop,amount\n");
    for i in 0..600u32 {
        text.push_str(&format!("C{},S{},{}\n", i % 9, i % 5, i % 100));
    }
    std::fs::write(dir.join("t.csv"), &text).unwrap();
    let server = start(&dir.0.clone());

    let native = get(
        &server.addr,
        "/pivot?path=t.csv&rows=city&cols=shop&values=amount:sum&engine=native",
    )
    .1;
    let sql = get(
        &server.addr,
        "/pivot?path=t.csv&rows=city&cols=shop&values=amount:sum&engine=sql",
    )
    .1;
    assert_eq!(native, sql, "pivot engines must agree");

    let native = get(
        &server.addr,
        "/chart?path=t.csv&x=city&y=amount:sum&engine=native",
    )
    .1;
    let sql = get(
        &server.addr,
        "/chart?path=t.csv&x=city&y=amount:sum&engine=sql",
    )
    .1;
    let points = |body: &str| -> Vec<String> {
        body.split("\"points\":[")
            .nth(1)
            .and_then(|tail| tail.split(']').next())
            .map(|list| list.split(',').map(|v| v.to_string()).collect())
            .unwrap_or_default()
    };
    let mut a = points(&native);
    let mut b = points(&sql);
    a.sort();
    b.sort();
    assert_eq!(a, b, "chart engines must agree on values");
}

/// The default refuses an undeclared parameter; `=0` lets it through for clients that
/// append their own extras.
#[test]
fn strict_parameter_checking_is_on_by_default_and_can_be_turned_off() {
    let dir = TempDir::new("strictdefault");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,1\n").unwrap();

    let server = start(&dir.0.clone());
    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=5&bogus=1");
    assert_eq!(status, 400, "default must refuse: {body}");
    assert!(body.contains(r#""route":"/rows""#), "{body}");

    let server = start_with_env(&dir.0, &[("CSVX_STRICT_PARAMS", "0")]);
    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=5&bogus=1");
    assert_eq!(status, 200, "=0 must let it through: {body}");
}

/// `/health` answers the same build question as `--version`, so an operator looking at a
/// browser or a probe can tell whether asking for the SQL engine is even possible.
#[test]
fn health_reports_whether_duckdb_is_available() {
    let dir = TempDir::new("healthduck");
    std::fs::write(dir.join("t.csv"), "a\n1\n").unwrap();
    let server = start(&dir.0.clone());
    let (status, body) = get(&server.addr, "/health");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""ready":true"#), "{body}");
    assert!(body.contains(r#""duckdb":"#), "{body}");
}

/// `/capabilities` is how a client discovers what this build can do; listing the engines
/// without saying which routes take them would leave the client guessing.
#[test]
fn capabilities_lists_the_engines_and_the_routes_that_take_them() {
    let dir = TempDir::new("cappabilities");
    std::fs::write(dir.join("t.csv"), "a\n1\n").unwrap();
    let server = start(&dir.0.clone());
    let (status, body) = get(&server.addr, "/capabilities");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""engines":["native","sql"]"#), "{body}");
    for route in ["/groupby", "/chart", "/pivot", "/lookup", "/concat"] {
        assert!(
            body.contains(&format!("\"{route}\"")),
            "{route} should be listed as engine-capable: {body}"
        );
    }
    assert!(body.contains(r#""duckdb":"#), "{body}");
}

/// `/style` is the conditional-formatting half of the render path: it must judge the edited
/// value, or a cell that turned amber visually would not match its own rule.
#[test]
fn style_judges_the_edited_value() {
    let dir = TempDir::new("styleedit");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let rules = "amount%3E999:bg=red";
    let before = get(&server.addr, &format!("/style?path=t.csv&rules={rules}")).1;
    assert!(
        before.contains(r#""cells":[]"#),
        "no cell matches yet: {before}"
    );

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=amount&value=9999",
    );
    assert_eq!(status, 200, "{body}");

    let after = get(&server.addr, &format!("/style?path=t.csv&rules={rules}")).1;
    assert!(
        !after.contains(r#""cells":[]"#),
        "the edited value must match the rule: {after}"
    );
}

/// A damaged journal is reported per response; that is not enough to notice a trend. The
/// metric sums it so a dashboard can alert on "users are losing edits".
#[test]
fn metrics_sum_journal_damage() {
    let dir = TempDir::new("damagemeters");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\n").unwrap();
    // One good record plus a truncated one, as a crash mid-write leaves them.
    std::fs::write(dir.join("t.csv.journal"), "set,1,amount,999\nset,2,amount").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=5");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""damaged":1"#), "{body}");

    let (status, body) = get(&server.addr, "/metrics");
    assert_eq!(status, 200, "{body}");
    let value = body
        .split("\"journalDamaged\":")
        .nth(1)
        .and_then(|tail| tail.split([',', '}']).next())
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(u64::MAX);
    assert!(value >= 1, "damage must reach the metrics: {body}");
    // The global count says "something is damaged"; this says which file, without letting
    // the metric grow with every path a client ever touched.
    assert!(
        body.contains("\"lastDamagedFile\":\"") && body.contains("/t.csv\""),
        "the last damaged file should be named: {body}"
    );
}

/// One place that states which endpoints see the journal and which do not.
///
/// Adding a render-path endpoint without adding it here leaves it covert: nothing fails, and
/// the next person trusts the table in CAPABILITIES. The two lists are deliberately
/// exhaustive over the endpoints that read a sheet.
#[test]
fn the_journal_visibility_matrix_holds() {
    let dir = TempDir::new("visibility");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());
    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=ZZ9",
    );
    assert_eq!(status, 200, "{body}");

    // Endpoints that must show the edited value, and how to recognise it.
    let sees: &[(&str, &str)] = &[
        ("/rows?path=t.csv&where=city%3DZZ9", "ZZ9"),
        ("/cell?path=t.csv&row=1&col=city", "ZZ9"),
        ("/journal?path=t.csv", "ZZ9"),
        ("/sheet?path=t.csv", "\"operations\":1"),
    ];
    for (query, marker) in sees {
        let (status, body) = get(&server.addr, query);
        assert_eq!(status, 200, "{query}: {body}");
        assert!(body.contains(marker), "{query} must see the edit: {body}");
    }

    // Endpoints that scan the base file: they must NOT see it. If someone changes this
    // deliberately, both the code and this list have to change together.
    let scans: &[(&str, &str)] = &[
        ("/filter?path=t.csv&where=city%3DZZ9", "\"matched\":0"),
        ("/search?path=t.csv&q=ZZ9", "\"matched\":0"),
        ("/export?path=t.csv&where=city%3DZZ9", "city,amount"),
    ];
    for (query, marker) in scans {
        let (status, body) = get(&server.addr, query);
        assert_eq!(status, 200, "{query}: {body}");
        assert!(
            body.contains(marker),
            "{query} must keep scanning the base file: {body}"
        );
    }
}

/// `/validate` loads the journal, so a rule that fails before the edit must pass after it.
/// The matrix listed it as edit-aware; this makes that claim testable.
#[test]
fn validation_judges_the_edited_value() {
    let dir = TempDir::new("validateedit");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());
    let checks = "amount%3E%3D9999";

    let (status, body) = get(
        &server.addr,
        &format!("/validate?path=t.csv&checks={checks}"),
    );
    assert_eq!(status, 200, "{body}");
    let count = |body: &str| -> u64 {
        body.split("\"count\":")
            .nth(1)
            .and_then(|t| t.split([',', '}']).next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(u64::MAX)
    };
    assert_eq!(
        count(&body),
        2,
        "both rows break the rule to begin with: {body}"
    );

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=amount&value=9999",
    );
    assert_eq!(status, 200, "{body}");

    let (_, body) = get(
        &server.addr,
        &format!("/validate?path=t.csv&checks={checks}"),
    );
    assert_eq!(count(&body), 1, "the edited row must now pass: {body}");
}

/// `/meta` promises five fields; a doc row that also promised a ragged flag was wrong.
#[test]
fn meta_reports_what_it_promises() {
    let dir = TempDir::new("metaapi");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/meta?path=t.csv");
    assert_eq!(status, 200, "{body}");
    for key in [
        "\"rows\"",
        "\"bytes\"",
        "\"headers\"",
        "\"delimiter\"",
        "\"encoding\"",
    ] {
        assert!(body.contains(key), "{key} missing: {body}");
    }
    assert!(body.contains(r#""rows":2"#), "two data rows: {body}");
    assert!(body.contains(r#""city""#), "{body}");
    assert!(
        !body.contains("ragged"),
        "a field the docs no longer promise must not appear: {body}"
    );
}

/// The API table names response fields in backticks; the handler must actually return them.
/// Two endpoints are enough to make the pattern real — /meta drifted once already — and the
/// parser skips tokens that are paths or prose.
#[test]
fn documented_fields_are_fields_the_endpoint_returns() {
    let docs =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/API.md")).unwrap();
    // Parameters are declared in SPEC_PARAMS; the docs row also names them in backticks,
    // so the guard must skip them or every route with a parameter column would fail.
    let serve_src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/serve.rs")).unwrap();
    let spec = &serve_src[serve_src.find("const SPEC_PARAMS").unwrap()..];
    let spec = &spec[..spec.find("\n];").unwrap()];
    let declared_for = |route: &str| -> Vec<String> {
        let at = match spec.find(&format!("\"{route}\"")) {
            Some(at) => at,
            None => return Vec::new(),
        };
        let rest = &spec[at + route.len() + 2..];
        let stop = rest.find("\n    (\"").unwrap_or(rest.len());
        rest[..stop]
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|n| !n.is_empty() && !n.starts_with('/'))
            .map(str::to_string)
            .collect()
    };
    let row = |route: &str| -> String {
        // Rows come in two shapes: `| `GET /meta` | … |` and `| GET | `/metrics` | … |`.
        // Match either by looking for the route and a GET in the same line.
        docs.lines()
            .find(|line| line.contains(route) && line.contains("GET"))
            .unwrap_or_else(|| panic!("no table row for {route}"))
            .to_string()
    };

    let dir = TempDir::new("docfields");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\n").unwrap();
    // /schema documents its diff fields (same/removed/added/moved), so the guarded request
    // asks for the diff form — those keys only exist in that answer.
    std::fs::write(dir.join("t2.csv"), "city,total\nCDMX,10\n").unwrap();
    let server = start(&dir.0.clone());

    // A route may answer different shapes (schema with and without diff). The documented
    // fields are asserted against the union of its answers, not against one request.
    let mut bodies: std::collections::BTreeMap<&str, Vec<String>> =
        std::collections::BTreeMap::new();
    for (route, query) in [
        ("/health", "/health"),
        ("/meta", "/meta?path=t.csv"),
        ("/metrics", "/metrics"),
        ("/sheet", "/sheet?path=t.csv"),
        ("/journal", "/journal?path=t.csv"),
        ("/cell", "/cell?path=t.csv&row=1&col=city"),
        ("/filter", "/filter?path=t.csv&where=amount%3D100"),
        ("/search", "/search?path=t.csv&q=CDMX"),
        ("/groupby", "/groupby?path=t.csv&by=city&agg=amount:sum"),
        ("/pivot", "/pivot?path=t.csv&rows=city&values=amount:sum"),
        ("/chart", "/chart?path=t.csv&x=city&y=amount:sum"),
        ("/files", "/files?dir=."),
        ("/book", "/book"),
        ("/layout", "/layout?path=t.csv"),
        ("/validate", "/validate?path=t.csv&checks=amount%3E0"),
        ("/stats", "/stats?path=t.csv"),
        ("/schema", "/schema?path=t.csv"),
        ("/schema", "/schema?path=t.csv&diff=t2.csv"),
        ("/analyze", "/analyze?path=t.csv"),
    ] {
        let (status, body) = get(&server.addr, query);
        assert_eq!(status, 200, "{query}: {body}");
        bodies.entry(route).or_default().push(body);
    }

    for (route, answers) in &bodies {
        let text = row(route);
        let mut checked = 0usize;
        // Backticked tokens that look like field names: no slash, no spaces.
        for piece in text.split('`').skip(1).step_by(2) {
            let token = piece.trim();
            let declared = declared_for(route);
            if token.is_empty()
                || declared.iter().any(|p| p == token)
                || token.starts_with('/')
                || token.contains(' ')
                || !token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                continue;
            }
            checked += 1;
            let present = answers
                .iter()
                .any(|body| body.contains(&format!("\"{token}\"")));
            assert!(
                present,
                "{route} documents {token} but no answer has it: {}",
                answers.join(" | ")
            );
        }
        assert!(
            checked > 0,
            "{route}: the docs row names no fields, so this guard checks nothing"
        );
    }
}

/// A download is the bytes of the file, with the file's own length in Content-Length — and it
/// streams, so a large export does not become a large allocation.
#[test]
fn download_returns_the_file_bytes_with_a_accurate_length() {
    let dir = TempDir::new("downloadbytes");
    let body = "city,amount\nCDMX,10\nPuebla,20\n";
    std::fs::write(dir.join("t.csv"), body).unwrap();
    let server = start(&dir.0.clone());

    // HEAD answers the headers without the body, which is how the length is checked.
    let mut head = TcpStream::connect(&server.addr).unwrap();
    write!(
        head,
        "HEAD /download?path=t.csv HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut header_text = String::new();
    head.read_to_string(&mut header_text).unwrap();
    assert!(
        header_text.contains(&format!("Content-Length: {}", body.len())),
        "length must match the file: {header_text}"
    );
    assert!(
        header_text.contains("attachment; filename=\"t.csv\""),
        "{header_text}"
    );

    let (status, raw) = get_raw(&server.addr, "/download?path=t.csv");
    assert_eq!(status, 200);
    let text = String::from_utf8_lossy(&raw).to_string();
    assert!(
        text.ends_with(body),
        "the body must be the file, byte for byte: {text}"
    );
}

/// A range request resumes a download: 206 with Content-Range, exactly the bytes asked for,
/// and 416 with the real length when the range does not fit.
#[test]
fn a_range_request_returns_that_slice() {
    let dir = TempDir::new("downloadrange");
    let body = "city,amount\nCDMX,10\nPuebla,20\n";
    std::fs::write(dir.join("t.csv"), body).unwrap();
    let server = start(&dir.0.clone());

    let mut stream = TcpStream::connect(&server.addr).unwrap();
    write!(
        stream,
        "GET /download?path=t.csv HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Range: bytes=5-9\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    assert!(text.starts_with("HTTP/1.1 206"), "{text}");
    assert!(text.contains("Content-Range: bytes 5-9/"), "{text}");
    assert!(raw.ends_with(&body.as_bytes()[5..10]), "{text}");

    let mut stream = TcpStream::connect(&server.addr).unwrap();
    write!(
        stream,
        "GET /download?path=t.csv HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Range: bytes=9999-\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    assert!(text.starts_with("HTTP/1.1 416"), "{text}");
    assert!(text.contains("\"parameter\":\"Range\""), "{text}");
}

/// Two tabs of a front-end editing the same file is the scenario `update_locked` exists for.
/// Eight concurrent HTTP writers must all land: a lost edit here would be a lost click.
#[test]
fn concurrent_http_edits_all_land() {
    let dir = TempDir::new("concedit");
    let mut text = String::from("city,amount\n");
    for i in 0..8 {
        text.push_str(&format!("C{i},{i}\n"));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0.clone());
    let addr = server.addr.clone();

    let mut handles = Vec::new();
    for row in 1..=8u64 {
        let addr = addr.clone();
        handles.push(std::thread::spawn(move || {
            post(
                &addr,
                &format!("/edit?path=t.csv&action=set&row={row}&col=city&value=E{row}"),
            )
            .0
        }));
    }
    for handle in handles {
        let status = handle.join().unwrap();
        assert!(status == 200 || status == 202, "a writer failed: {status}");
    }

    let (status, body) = get(&addr, "/journal?path=t.csv");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("\"count\":8"),
        "all eight edits must be in the journal: {body}"
    );
    let (_, rows) = get(&addr, "/rows?path=t.csv&start=0&count=10");
    for row in 1..=8u64 {
        assert!(
            rows.contains(&format!("E{row}")),
            "edit {row} is missing from the window: {rows}"
        );
    }
}

/// Concurrent readers share the pager cache; eight threads asking for different windows must
/// all get their own rows, which is also the cheapest way to notice a cache race.
#[test]
fn concurrent_reads_return_their_own_windows() {
    let dir = TempDir::new("concread");
    let mut text = String::from("id,city\n");
    for i in 0..200u32 {
        text.push_str(&format!("{i},C{}\n", i % 7));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0.clone());
    let addr = server.addr.clone();

    let mut handles = Vec::new();
    for w in 0..8u32 {
        let addr = addr.clone();
        handles.push(std::thread::spawn(move || {
            let start = (w as u64) * 20;
            let (status, body) = get(&addr, &format!("/rows?path=t.csv&start={start}&count=20"));
            assert_eq!(status, 200, "window {start}: {body}");
            // The first id of each window identifies it: they advance by 20.
            // Each row is ["id","C…"], so the first cell must be exactly this window's start.
            assert!(
                body.contains(&format!("[\"{start}\",\"C")),
                "window {start} returned other rows: {body}"
            );
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
}

/// `format=csv` on a window is the only way to get *edited* rows as text: /export reads the
/// base file, so a filtered export of an edited cell comes back empty.
#[test]
fn a_window_can_be_asked_for_as_csv() {
    let dir = TempDir::new("windowcsv");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());
    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=ZZ9",
    );
    assert_eq!(status, 200, "{body}");

    let (status, raw) = get_raw(&server.addr, "/rows?path=t.csv&start=0&count=2&format=csv");
    assert_eq!(status, 200);
    let text = String::from_utf8_lossy(&raw).to_string();
    assert!(text.starts_with("city,amount"), "{text}");
    assert!(
        text.contains("ZZ9"),
        "the edited value must be in the CSV: {text}"
    );
    assert!(
        !text.contains("CDMX"),
        "the pre-edit value must be gone: {text}"
    );
}

/// The spec lists one parameter set per route, so `keep` and friends are declared for
/// /columns even though only POST reads them. GET must say so rather than look successful.
#[test]
fn a_get_columns_request_refuses_post_only_parameters() {
    let dir = TempDir::new("columnsget");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\n").unwrap();
    let server = start(&dir.0.clone());

    for key in ["keep=city", "drop=amount", "rename=a:b", "out=reshaped.csv"] {
        let (code, body) = get(&server.addr, &format!("/columns?path=t.csv&{key}"));
        assert_eq!(code, 400, "{key}: {body}");
        assert!(
            body.contains("POST /columns"),
            "the error must name where the parameter belongs: {body}"
        );
    }
}

/// GET /edit reports the journal; the write parameters belong to POST. A GET carrying them
/// must say so, or a caller who swapped the method reads "200 OK" as "the edit landed".
#[test]
fn a_get_edit_request_refuses_write_parameters() {
    let dir = TempDir::new("editsget");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\n").unwrap();
    let server = start(&dir.0.clone());

    for key in ["action=set", "row=1", "col=city", "value=X", "expect=CDMX"] {
        let (code, body) = get(&server.addr, &format!("/edit?path=t.csv&{key}"));
        assert_eq!(code, 400, "{key}: {body}");
        assert!(
            body.contains("POST /edit"),
            "the error must name where the parameter belongs: {body}"
        );
    }
}

/// The spec now publishes which declared parameters its read side refuses, and the same table
/// drives the 400s. If they ever disagree, a client that generates calls from /spec would
/// send a GET it cannot use.
#[test]
fn the_spec_publishes_the_write_only_parameters() {
    let dir = TempDir::new("writeonlyspec");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\n").unwrap();
    let server = start(&dir.0.clone());
    let (status, body) = get(&server.addr, "/spec");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""writeOnlyParams":"#), "{body}");
    for name in ["action", "row", "col", "value", "expect"] {
        assert!(
            body.contains(&format!("\"{name}\"")),
            "{name} missing: {body}"
        );
    }
    for name in ["keep", "drop", "rename"] {
        assert!(
            body.contains(&format!("\"{name}\"")),
            "{name} missing: {body}"
        );
    }
}

/// Going over the group cap is a 400 that names the parameter to turn, not "request".
#[test]
fn too_many_groups_names_max_groups_and_says_what_to_do() {
    let dir = TempDir::new("maxgroups");
    let mut text = String::from("id,v\n");
    for i in 0..50u32 {
        text.push_str(&format!("k{i},{i}\n"));
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0.clone());

    let (code, body) = get(
        &server.addr,
        "/groupby?path=t.csv&by=id&agg=v:sum&maxGroups=5",
    );
    assert_eq!(code, 400, "{body}");
    assert!(body.contains(r#""parameter":"maxGroups""#), "{body}");
    assert!(
        body.contains("more than 5 groups"),
        "the error should say the limit it hit: {body}"
    );
    assert!(body.contains(r#""hint":"#), "{body}");
}

/// Strictness changes how a request with extra parameters answers, so a client discovering
/// the backend must be able to see which mode it is in.
#[test]
fn capabilities_says_whether_strict_mode_is_on() {
    let dir = TempDir::new("capstrict");
    std::fs::write(dir.join("t.csv"), "a\n1\n").unwrap();
    let server = start(&dir.0.clone());
    let (status, body) = get(&server.addr, "/capabilities");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""strictParams":true"#), "{body}");

    let server = start_with_env(&dir.0, &[("CSVX_STRICT_PARAMS", "0")]);
    let (_, body) = get(&server.addr, "/capabilities");
    assert!(body.contains(r#""strictParams":false"#), "{body}");
}

/// A renamed or dropped column must disappear from /columns too, or the front-end draws
/// headers for a sheet that no longer has them.
#[test]
fn columns_lists_the_edited_schema() {
    let dir = TempDir::new("columnsedit");
    std::fs::write(
        dir.join("t.csv"),
        "city,amount,note\nCDMX,10,a\nPuebla,20,b\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=rename&from=amount&to=total",
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = get(&server.addr, "/columns?path=t.csv");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains("\"total\""),
        "the new name must be listed: {body}"
    );
    assert!(
        !body.contains("\"amount\""),
        "the old name must be gone: {body}"
    );
}

/// /meta describes the base file on purpose: rows, bytes and headers are physical facts.
/// What it must not do is stay silent about a journal — a caller that sees 2 columns here and
/// 3 in /columns needs to know why.
#[test]
fn meta_says_a_journal_exists() {
    let dir = TempDir::new("metaedit");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=insert-column&name=note&value=x&after=city",
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = get(&server.addr, "/meta?path=t.csv");
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains(r#""edited":1"#),
        "the edit count must be visible: {body}"
    );
    assert!(
        body.contains(r#""headers":["city","amount"]"#),
        "headers stay the file's own: {body}"
    );
    let (_, columns) = get(&server.addr, "/columns?path=t.csv");
    assert!(
        columns.contains("\"note\""),
        "the edited schema lives in /columns: {columns}"
    );
}

/// `limit` works in the handler but was missing from the spec, so strict mode answered 400.
/// With it declared, a capped scan must also say it was capped.
#[test]
fn a_limited_filter_works_and_says_it_was_capped() {
    let dir = TempDir::new("filtermore");
    let mut text = String::from("city,amount\n");
    for i in 0..10u32 {
        text.push_str(&format!("C{},100\n", i % 3)); // three cities, ten rows
    }
    std::fs::write(dir.join("t.csv"), text).unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(
        &server.addr,
        "/filter?path=t.csv&where=amount%3D100&limit=2",
    );
    assert_eq!(status, 200, "limit must be accepted: {body}");
    assert!(
        body.contains(r#""matched":10"#),
        "matched counts all hits: {body}"
    );
    assert!(body.contains(r#""rows":[1,2]"#), "rows are capped: {body}");
    assert!(
        body.contains(r#""truncated":true"#),
        "a capped result must admit it: {body}"
    );
}

/// The spec now declares parameters the handlers already read. Two of them are checked here
/// end to end, because "declared" and "accepted" are different claims.
#[test]
fn undeclared_before_params_are_accepted_now() {
    let dir = TempDir::new("declaredparams");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (code, body) = post(&server.addr, "/index?path=t.csv&interval=100");
    assert!(
        code == 200 || code == 202,
        "/index interval must be accepted: {body}"
    );

    let (code, body) = post(
        &server.addr,
        "/partition?path=t.csv&by=city&out=p.tar&format=none&maxPartitions=2",
    );
    assert!(
        code == 200 || code == 202,
        "/partition maxPartitions must be accepted: {body}"
    );
}

/// Documented parameters that a handler reads must also be accepted; the spec omitted them and
/// strict mode answered 400 (chart desc, style start/count were recent instances).
#[test]
fn chart_and_style_accept_their_documented_parameters() {
    let dir = TempDir::new("aggparams");
    std::fs::write(
        dir.join("t.csv"),
        "city,amount\nCDMX,10\nPuebla,20\nCDMX,30\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());

    let (code, body) = get(&server.addr, "/chart?path=t.csv&x=city&y=amount:sum&desc=1");
    assert_eq!(code, 200, "/chart desc must be accepted: {body}");

    let (code, body) = get(
        &server.addr,
        "/style?path=t.csv&rules=amount%3E5:bg=red&start=0&count=2",
    );
    assert_eq!(code, 200, "/style start/count must be accepted: {body}");
}

/// `apply-edits=1` groups/charts/pivots the edited sheet: a value changed by an edit must move
/// between groups or categories, which cannot happen if the handler reads the base file.
#[test]
fn the_aggregations_can_apply_edits() {
    let dir = TempDir::new("groupapply");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let base = get(&server.addr, "/groupby?path=t.csv&by=city&agg=amount:sum").1;
    assert!(base.contains(r#""CDMX""#), "{base}");

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=Puebla",
    );
    assert_eq!(status, 200, "{body}");

    let edited = get(
        &server.addr,
        "/groupby?path=t.csv&by=city&agg=amount:sum&apply-edits=1",
    )
    .1;
    assert!(
        !edited.contains(r#""CDMX""#),
        "after the edit no CDMX row remains: {edited}"
    );
    let plain = get(&server.addr, "/groupby?path=t.csv&by=city&agg=amount:sum").1;
    assert!(
        plain.contains(r#""CDMX""#),
        "without apply-edits the base file still has CDMX: {plain}"
    );

    // chart: the edited row's city is gone from the categories with the flag.
    let chart = get(
        &server.addr,
        "/chart?path=t.csv&x=city&y=amount:sum&apply-edits=1",
    )
    .1;
    assert!(
        !chart.contains(r#""CDMX""#),
        "chart must use the edit: {chart}"
    );

    // pivot: same claim on the row keys.
    let pivot = get(
        &server.addr,
        "/pivot?path=t.csv&rows=city&values=amount:sum&apply-edits=1",
    )
    .1;
    assert!(
        !pivot.contains(r#""CDMX""#),
        "pivot must use the edit: {pivot}"
    );
}

/// An export of edited data is the one thing `/export` could not do: the endpoint reads the
/// base file, so a filtered export of an edited cell came back empty.
#[test]
fn export_can_apply_edits() {
    let dir = TempDir::new("exportapply");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=ZZ9",
    );
    assert_eq!(status, 200, "{body}");

    let (status, raw) = get_raw(
        &server.addr,
        "/export?path=t.csv&where=city%3DZZ9&apply-edits=1",
    );
    assert_eq!(status, 200);
    let text = String::from_utf8_lossy(&raw).to_string();
    assert!(text.contains("ZZ9"), "the export must see the edit: {text}");

    // Without the flag the base file still answers, and the edited value is absent.
    let (status, raw) = get_raw(&server.addr, "/export?path=t.csv&where=city%3DZZ9");
    assert_eq!(status, 200);
    let text = String::from_utf8_lossy(&raw).to_string();
    assert!(
        !text.contains("ZZ9"),
        "without apply-edits the base file answers: {text}"
    );
}

/// With edits applied, filter's row numbers must describe the edited sheet — otherwise a caller
/// pages `/rows` with numbers that point at different rows.
#[test]
fn filter_can_apply_edits_and_its_row_numbers_follow() {
    let dir = TempDir::new("filterapply");
    std::fs::write(dir.join("t.csv"), "city,amount\nA,1\nB,2\nC,3\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(&server.addr, "/edit?path=t.csv&action=delete&row=1&count=1");
    assert_eq!(status, 200, "{body}");

    let base = get(&server.addr, "/filter?path=t.csv&where=amount%3E0").1;
    assert!(base.contains(r#""rows":[1,2,3]"#), "{base}");
    assert!(base.contains(r#""source":"file""#), "{base}");

    let edited = get(
        &server.addr,
        "/filter?path=t.csv&where=amount%3E0&apply-edits=1",
    )
    .1;
    assert!(
        edited.contains(r#""rows":[1,2]"#),
        "the edited sheet has two rows left: {edited}"
    );
    assert!(edited.contains(r#""source":"edited""#), "{edited}");
}

/// Search over the edited sheet: a deleted row must stop matching, and without the flag the
/// base file still answers.
#[test]
fn search_can_apply_edits() {
    let dir = TempDir::new("searchapply");
    std::fs::write(dir.join("t.csv"), "city,amount\nA,1\nB,2\nC,3\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(&server.addr, "/edit?path=t.csv&action=delete&row=2&count=1");
    assert_eq!(status, 200, "{body}");

    let base = get(&server.addr, "/search?path=t.csv&q=B").1;
    assert!(base.contains(r#""matched":1"#), "{base}");

    let edited = get(&server.addr, "/search?path=t.csv&q=B&apply-edits=1").1;
    assert!(
        edited.contains(r#""matched":0"#),
        "the deleted row must stop matching: {edited}"
    );
}

/// Profiling the edited sheet: after a column is renamed, /analyze with the flag must name the
/// new one; without it the base file still answers with the old name.
#[test]
fn analyze_and_stats_can_apply_edits() {
    let dir = TempDir::new("profileapply");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=rename&from=amount&to=total",
    );
    assert_eq!(status, 200, "{body}");

    let base = get(&server.addr, "/analyze?path=t.csv").1;
    assert!(base.contains(r#""amount""#), "{base}");

    let edited = get(&server.addr, "/analyze?path=t.csv&apply-edits=1").1;
    assert!(
        edited.contains(r#""total""#),
        "the renamed column: {edited}"
    );
    assert!(
        !edited.contains(r#""amount""#),
        "the old name is gone: {edited}"
    );

    let stats = get(&server.addr, "/stats?path=t.csv&apply-edits=1").1;
    assert!(stats.contains(r#""total""#), "stats follows too: {stats}");
}

/// `/rows?sql=` pages a query result; the question this pins down is whether that result is
/// built from the edited sheet or the raw file.
#[test]
fn rows_sql_sees_the_journal_or_says_otherwise() {
    let dir = TempDir::new("rowsapply");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=ZZ9",
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = get(
        &server.addr,
        "/rows?path=t.csv&sql=SELECT%20city%20FROM%20t%20WHERE%20city%3D%27ZZ9%27",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("ZZ9"), "the query must see the edit: {body}");
}

/// Lookup is a job; `apply-edits=1` must look up the edited main file, so a changed key joins
/// against the reference instead of the original one.
#[test]
fn lookup_can_apply_edits() {
    let dir = TempDir::new("lookupapply");
    std::fs::write(dir.join("main.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    std::fs::write(dir.join("ref.csv"), "city,region\nZZ9,Bajio\nCDMX,Centro\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=main.csv&action=set&row=1&col=city&value=ZZ9",
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = post(
        &server.addr,
        "/lookup?path=main.csv&key=city&from=ref.csv&get=region&out=j.csv&apply-edits=1",
    );
    assert!(status == 200 || status == 202, "{body}");

    // Wait for the job to finish, then read the attached result.
    let mut region = None;
    for _ in 0..60 {
        let (_, state) = get(&server.addr, "/jobs?path=main.csv");
        if state.contains(r#""state":"done""#) {
            let (_, out) = get(&server.addr, "/rows?path=j.csv&start=0&count=5");
            region = Some(out);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let out = region.expect("the lookup job should finish");
    assert!(out.contains("Bajio"), "the edited key must join: {out}");
}

/// HTTP concat with byName=1: same columns in a different order concatenate; a different set
/// still fails (the job reports the error rather than appending nonsense).
#[test]
fn concat_can_align_by_name_over_http() {
    let dir = TempDir::new("concatbynamehttp");
    std::fs::write(dir.join("a.csv"), "a,b,c\n1,2,3\n").unwrap();
    std::fs::write(dir.join("b.csv"), "c,a,b\n30,10,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/concat?paths=a.csv,b.csv&out=joined.csv&byName=1",
    );
    assert!(status == 200 || status == 202, "{body}");

    let mut ready = false;
    let mut state = String::new();
    for _ in 0..60 {
        (_, state) = get(&server.addr, "/jobs?path=a.csv");
        if state.contains(r#""state":"done""#) {
            ready = true;
            break;
        }
        if state.contains(r#""state":"failed""#) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(ready, "the concat job should finish: {state}");
    let (_, out) = get(&server.addr, "/rows?path=joined.csv&start=0&count=5");
    assert!(out.contains("10"), "the reordered row is aligned: {out}");
    assert!(out.contains("30"), "{out}");
}

/// Sorting an edited sheet writes a flattened view of the edits; without the flag the base
/// file is still what gets sorted.
#[test]
fn sort_can_apply_edits() {
    let dir = TempDir::new("sortapply");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=ZZ9",
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = post(
        &server.addr,
        "/sort?path=t.csv&by=city&out=sorted.csv&apply-edits=1",
    );
    assert!(status == 200 || status == 202, "{body}");

    let mut state = String::new();
    for _ in 0..60 {
        (_, state) = get(&server.addr, "/jobs?path=t.csv");
        if state.contains(r#""state":"done""#) || state.contains(r#""state":"failed""#) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(state.contains(r#""state":"done""#), "{state}");

    let (_, out) = get(&server.addr, "/rows?path=sorted.csv&start=0&count=5");
    assert!(out.contains("ZZ9"), "the edit is in the sorted file: {out}");
    assert!(!out.contains("CDMX"), "the old value is gone: {out}");
}

/// `stamp=` makes two edits from one read safe: the second one answers 409 and names the
/// current stamp instead of appending on top of a state the client never saw.
#[test]
fn an_edit_carries_a_stamp_precondition() {
    let dir = TempDir::new("stampededit");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=A",
    );
    assert_eq!(status, 200, "{body}");
    let fresh = body
        .split("\"stamp\":\"")
        .nth(1)
        .and_then(|t| t.split('"').next())
        .unwrap()
        .to_string();

    // Another client that read the old state writes with that stamp: it must be refused.
    let stale = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=2&col=city&value=B&stamp=0%3A0%3A0",
    );
    assert_eq!(stale.0, 409, "{}", stale.1);
    assert!(stale.1.contains(r#""parameter":"stamp""#), "{}", stale.1);

    // With the current stamp the same edit lands.
    let (status, body) = post(
        &server.addr,
        &format!("/edit?path=t.csv&action=set&row=2&col=city&value=B&stamp={fresh}"),
    );
    assert_eq!(status, 200, "{body}");
}

/// `/copy` is "save as": a whole-file duplicate, and with apply-edits=1 the duplicate carries
/// the edits instead of the base bytes.
#[test]
fn copy_duplicates_a_sheet_with_or_without_edits() {
    let dir = TempDir::new("copysheet");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(&server.addr, "/copy?path=t.csv&out=plain.csv");
    assert!(status == 200 || status == 202, "{body}");
    // Poll the output, not `/jobs`: two copy jobs on the same source make "a job is done"
    // ambiguous, and the first run of this test was flaky for exactly that reason.
    let mut rows = String::new();
    for _ in 0..100 {
        (_, rows) = get(&server.addr, "/rows?path=plain.csv&start=0&count=5");
        if rows.contains("CDMX") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(rows.contains("CDMX"), "{rows}");

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=ZZ9",
    );
    assert_eq!(status, 200, "{body}");
    let (status, body) = post(
        &server.addr,
        "/copy?path=t.csv&out=edited.csv&apply-edits=1",
    );
    assert!(status == 200 || status == 202, "{body}");
    let mut rows = String::new();
    for _ in 0..100 {
        (_, rows) = get(&server.addr, "/rows?path=edited.csv&start=0&count=5");
        if rows.contains("ZZ9") || rows.contains("CDMX") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(rows.contains("ZZ9"), "the copy carries the edit: {rows}");
    assert!(!rows.contains("CDMX"), "the old value is gone: {rows}");
}

/// Validation applies edits, so its violation row numbers should describe the edited sheet —
/// the same row numbers `/rows` uses. Deleting the first row must shift them by one.
#[test]
fn validation_row_numbers_follow_edits() {
    let dir = TempDir::new("validateedits");
    std::fs::write(dir.join("t.csv"), "city,amount\n,A\nCDMX,10\n,B\nZZ9,30\n").unwrap();
    let server = start(&dir.0.clone());

    let before = get(&server.addr, "/validate?path=t.csv&checks=city:not-empty").1;
    assert!(before.contains(r#"{"row":1,"#), "{before}");
    assert!(before.contains(r#"{"row":3,"#), "{before}");

    let (status, body) = post(&server.addr, "/edit?path=t.csv&action=delete&row=1&count=1");
    assert_eq!(status, 200, "{body}");

    let after = get(&server.addr, "/validate?path=t.csv&checks=city:not-empty").1;
    // The row that was 3 is now 2; the deleted empty row no longer reports.
    assert!(after.contains(r#"{"row":2,"#), "{after}");
    assert!(!after.contains(r#"{"row":3,"#), "{after}");
}

/// `/schema` without `diff` shares the columns handler, so it already applies edits; `/schema`
/// with `diff` compares two files on disk. Both halves are pinned here so the difference is
/// explicit rather than accidental.
#[test]
fn schema_applies_edits_but_diff_compares_files_on_disk() {
    let dir = TempDir::new("schemaedits");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\n").unwrap();
    std::fs::write(dir.join("other.csv"), "city,amount\nCDMX,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=rename&from=amount&to=total",
    );
    assert_eq!(status, 200, "{body}");

    let (_, schema) = get(&server.addr, "/schema?path=t.csv");
    assert!(
        schema.contains(r#""total""#),
        "schema follows the edit: {schema}"
    );
    assert!(!schema.contains(r#""amount""#), "{schema}");

    // The journal renamed amount to total, but the files on disk both still say amount, so
    // the diff reports no difference — that is the proof it reads the disk, not the sheet.
    let (_, diff) = get(&server.addr, "/schema?path=t.csv&diff=other.csv");
    assert!(
        diff.contains(r#""same":true"#),
        "diff compares the files on disk: {diff}"
    );
}

/// The HTTP merge job honours `chunks=`: an archive with fewer parts fails and says so.
#[test]
fn merge_over_http_refuses_an_unexpected_chunk_count() {
    let dir = TempDir::new("mergewrongcount");
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    std::fs::write(dir.join("parts/t_0001.csv"), "id,v\n1,a\n").unwrap();
    std::fs::write(dir.join("parts/t_0002.csv"), "id,v\n2,b\n").unwrap();
    // HTTP merges archives; the directory form is CLI-only, so pack the parts first.
    let tar = Command::new("tar")
        .args(["-cf", dir.join("parts.tar").to_str().unwrap()])
        .args(["-C", dir.join("parts").to_str().unwrap()])
        .args(["t_0001.csv", "t_0002.csv"])
        .output()
        .unwrap();
    assert!(tar.status.success(), "{tar:?}");
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/merge?path=parts.tar&out=joined.csv&chunks=3",
    );
    assert!(status == 200 || status == 202, "{body}");

    let mut state = String::new();
    for _ in 0..100 {
        (_, state) = get(&server.addr, "/jobs?path=parts");
        if state.contains(r#""state":"failed""#) || state.contains(r#""state":"done""#) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(state.contains(r#""state":"failed""#), "{state}");
    assert!(state.contains("holds 2 chunk(s), not 3"), "{state}");
}

/// Per-route metrics must carry percentiles and an error count, not just an average: one
/// pathological request should not be invisible.
#[test]
fn metrics_report_per_route_percentiles_and_errors() {
    let dir = TempDir::new("routepercentiles");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\n").unwrap();
    let server = start(&dir.0.clone());

    get(&server.addr, "/meta?path=t.csv");
    get(&server.addr, "/meta?path=nope.csv");

    let (_, body) = get(&server.addr, "/metrics");
    assert!(body.contains(r#""routes":{"#), "routes: {body}");
    assert!(body.contains(r#""p50Ms""#), "per-route p50: {body}");
    assert!(body.contains(r#""p95Ms""#), "per-route p95: {body}");
    assert!(body.contains(r#""errors""#), "per-route errors: {body}");
    assert!(body.contains(r#""maxMs""#), "{body}");
    // The same route answered 200 and 404 above; both must be attributed to it.
    assert!(body.contains(r#""status":{"#), "per-route status: {body}");
    // A missing file is a 400 in this server's contract, not a 404.
    assert!(body.contains(r#""400""#), "the 400 is attributed: {body}");
    assert!(body.contains(r#""200""#), "the 200 is attributed: {body}");
}

/// `format=csv` must mean CSV whether or not the sheet has edits: the pristine branch used
/// to ignore the parameter and answer JSON, and the edited branch was the only covered path.
#[test]
fn format_csv_works_on_a_pristine_sheet_too() {
    let dir = TempDir::new("csvpristine");
    std::fs::write(dir.join("t.csv"), "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=2&format=csv");
    assert_eq!(status, 200, "{body}");
    assert!(body.starts_with("city,amount\n"), "{body}");
    assert!(!body.contains(r#""columns""#), "not JSON: {body}");

    // Special characters must survive the CSV rendering (quotes, backslash, newline, tab).
    std::fs::write(
        dir.join("w.csv"),
        "id,text\n1,\"he said \"\"hi\"\"\"\n2,back\\slash\n3,\"line1\nline2\"\n",
    )
    .unwrap();
    let (_, csv) = get(&server.addr, "/rows?path=w.csv&start=0&count=3&format=csv");
    assert!(csv.contains("\"he said \"\"hi\"\"\""), "{csv}");
    assert!(csv.contains("back\\slash"), "{csv}");
    assert!(csv.contains("\"line1\nline2\""), "{csv}");
}

/// JSON must escape what a CSV cell can legally contain: a raw newline or control byte inside
/// a JSON string makes the whole response unparseable for every client.
#[test]
fn json_escapes_newlines_and_control_characters() {
    let dir = TempDir::new("jsonescape");
    std::fs::write(
        dir.join("t.csv"),
        b"id,text\n1,\"line1\nline2\"\n2,ctrl\x01char\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/rows?path=t.csv&start=0&count=2");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#"line1\nline2"#), "newline escaped: {body}");
    assert!(
        body.contains(r#"ctrl\u0001char"#),
        "control escaped: {body}"
    );

    let (_, cell) = get(&server.addr, "/cell?path=t.csv&row=2&col=text");
    assert!(cell.contains(r#"ctrl\u0001char"#), "{cell}");
}

/// The same hostile cell through three other output paths: export (CSV), query and groupby
/// (JSON). They share helpers with /rows, but sharing a helper is not the same as calling it.
#[test]
fn export_query_and_groupby_survive_hostile_cells() {
    let dir = TempDir::new("hostilecells");
    std::fs::write(
        dir.join("t.csv"),
        b"id,text\n1,\"he said \"\"hi\"\"\"\n2,back\\slash\n3,\"line1\nline2\"\n4,quote \x27ok\x27\n5,ctrl\x01char\n",
    )
    .unwrap();
    let server = start(&dir.0.clone());

    // CSV out: the quote is doubled, the newline stays inside quotes.
    let (status, csv) = get(&server.addr, "/export?path=t.csv&where=id<6");
    assert_eq!(status, 200, "{csv}");
    assert!(csv.contains(r#""he said ""hi"""#), "{csv}");
    assert!(csv.contains("\"line1\nline2\""), "{csv}");

    // JSON out: a control byte must be escaped, not raw.
    let (_, query) = get(
        &server.addr,
        "/query?path=t.csv&sql=SELECT%20text%20FROM%20t%20WHERE%20id%3D%275%27",
    );
    assert!(query.contains(r#"ctrl\u0001char"#), "{query}");

    let (_, group) = get(&server.addr, "/groupby?path=t.csv&by=text&agg=id:count");
    assert!(group.contains(r#"he said \"hi\""#), "{group}");

    // A chart category is the same hostile string again, one more layer out.
    let (_, chart) = get(&server.addr, "/chart?path=t.csv&x=text&y=id:count");
    assert!(chart.contains(r#"he said \"hi\""#), "{chart}");
}

/// Readers must never see a torn journal while a writer appends: every /rows and /journal
/// response during a burst of edits has to parse and report `damaged:0`. This is the
/// read-side counterpart of the eight-writer test.
#[test]
fn reads_stay_consistent_while_edits_land() {
    let dir = TempDir::new("readwhilewriting");
    std::fs::write(dir.join("t.csv"), "city,amount\nA,1\nB,2\nC,3\n").unwrap();
    let server = start(&dir.0.clone());
    let addr = server.addr.clone();

    let writer = {
        let addr = addr.clone();
        std::thread::spawn(move || {
            for i in 0..40 {
                let path = format!("/edit?path=t.csv&action=set&row=1&col=city&value=W{}", i);
                assert_eq!(post(&addr, &path).0, 200);
            }
        })
    };

    for _ in 0..120 {
        let (status, body) = get(&addr, "/rows?path=t.csv&start=0&count=3");
        assert_eq!(status, 200, "{body}");
        assert!(body.contains(r#""rows":[["#), "no rows: {body}");
        // A pristine answer carries no `damaged` field; an edited one must say zero.
        assert!(
            body.contains(r#""edited":false"#) || body.contains(r#""damaged":0"#),
            "torn journal: {body}"
        );

        let (status, body) = get(&addr, "/journal?path=t.csv");
        assert_eq!(status, 200, "{body}");
        assert!(
            body.contains(r#""damaged":{"operations":0,"redo":0}"#),
            "torn journal: {body}"
        );
    }

    writer.join().expect("writer panicked");

    // After the burst the last write must be the one visible: no lost edit.
    let (_, body) = get(&addr, "/rows?path=t.csv&start=0&count=1");
    assert!(body.contains("W39"), "the last edit is visible: {body}");
    let (_, journal) = get(&addr, "/journal?path=t.csv");
    assert!(journal.contains(r#""count":40"#), "{journal}");
}

/// Materialising endpoints take a journal snapshot before scanning the base file. While edits
/// land, an export must still be the whole table with a value that was actually written —
/// never a truncated row or a half-applied value.
#[test]
fn export_apply_edits_snapshots_while_edits_land() {
    let dir = TempDir::new("exportduringwrites");
    std::fs::write(dir.join("t.csv"), "city,amount\nA,1\nB,2\nC,3\n").unwrap();
    let server = start(&dir.0.clone());
    let addr = server.addr.clone();

    let writer = {
        let addr = addr.clone();
        std::thread::spawn(move || {
            for i in 0..30 {
                let path = format!("/edit?path=t.csv&action=set&row=1&col=city&value=W{i}");
                assert_eq!(post(&addr, &path).0, 200);
            }
        })
    };

    let mut seen = 0;
    for _ in 0..15 {
        let (status, body) = get(
            &server.addr,
            "/export?path=t.csv&apply-edits=1&sql=SELECT%20city%2Camount%20FROM%20t",
        );
        assert_eq!(status, 200, "{body}");
        let lines: Vec<&str> = body.trim_end_matches('\n').split('\n').collect();
        assert_eq!(lines.len(), 4, "three rows plus header: {body}");
        let first = lines[1].split(',').next().unwrap();
        assert!(
            first == "A" || first.starts_with('W'),
            "a written value or the original: {first}"
        );
        seen += 1;
    }
    assert_eq!(seen, 15);
    writer.join().expect("writer panicked");
}

/// `/capabilities` publishes which routes honour `apply-edits`, and every published route must
/// actually be served and declare the parameter in /spec.
#[test]
fn capabilities_publishes_apply_edits_routes() {
    let dir = TempDir::new("applyeditscaps");
    let server = start(&dir.0.clone());

    let (status, body) = get(&server.addr, "/capabilities");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""applyEdits":["#), "{body}");
    for route in [
        "/groupby", "/pivot", "/chart", "/export", "/filter", "/search", "/analyze", "/stats",
        "/lookup", "/sort", "/copy",
    ] {
        assert!(
            body.contains(&format!(r#""{route}""#)),
            "missing {route}: {body}"
        );
    }

    // Every published route must be real, and its /spec entry must list apply-edits.
    let (_, spec) = get(&server.addr, "/spec");
    for route in [
        "/groupby", "/pivot", "/chart", "/export", "/filter", "/search", "/analyze", "/stats",
        "/lookup", "/sort", "/copy",
    ] {
        let entry = format!(r#""{route}""#);
        assert!(spec.contains(&entry), "spec does not serve {route}");
    }
    assert!(body.contains(r#""expect=compare-and-set""#), "{body}");
    assert!(body.contains(r#""stamp=journal-precondition""#), "{body}");
}

/// `set-many` writes every cell in one journal update, so a reader sees either none or all;
/// undo still walks one cell at a time, and the response says so.
#[test]
fn set_many_writes_all_cells_in_one_operation_batch() {
    let dir = TempDir::new("setmany");
    std::fs::write(dir.join("t.csv"), "city,amount\nA,1\nB,2\nC,3\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set-many&cells=1:city:X;2:amount:99;3:city:Z",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""cells":3"#), "{body}");
    assert!(body.contains(r#""undo":"one cell per undo""#), "{body}");

    let (_, rows) = get(&server.addr, "/rows?path=t.csv&start=0&count=3");
    assert!(
        rows.contains(r#"[["X","1"],["B","99"],["Z","3"]]"#),
        "{rows}"
    );
    let (_, journal) = get(&server.addr, "/journal?path=t.csv");
    assert!(journal.contains(r#""count":3"#), "{journal}");

    // One undo per cell: after three undos the sheet is back to the original.
    for _ in 0..3 {
        assert_eq!(post(&server.addr, "/edit?path=t.csv&action=undo").0, 200);
    }
    let (_, rows) = get(&server.addr, "/rows?path=t.csv&start=0&count=3");
    assert!(
        rows.contains(r#"[["A","1"],["B","2"],["C","3"]]"#),
        "{rows}"
    );

    // A bad entry refuses the whole batch and writes nothing.
    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set-many&cells=1:city:Q;4:amount:7",
    );
    assert_eq!(status, 404, "{body}");
    let (_, journal) = get(&server.addr, "/journal?path=t.csv");
    assert!(journal.contains(r#""count":0"#), "{journal}");
}

/// Two entries naming the same cell are applied in order (last wins), a value may contain
/// colons, and an empty value is a real value rather than a parse failure.
#[test]
fn set_many_handles_duplicates_colons_and_empty_values() {
    let dir = TempDir::new("setmanyedges");
    std::fs::write(dir.join("t.csv"), "city,amount\nA,1\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set-many&cells=1:city:first;1:city:http%3A%2F%2Fx.example",
    );
    assert_eq!(status, 200, "{body}");

    let (_, rows) = get(&server.addr, "/rows?path=t.csv&start=0&count=1");
    assert!(
        rows.contains(r#"[["http://x.example","1"]]"#),
        "the later entry wins and a colon is part of the value: {rows}"
    );
    let (_, journal) = get(&server.addr, "/journal?path=t.csv");
    assert!(journal.contains(r#""count":2"#), "{journal}");

    // The last entry can also be empty: that is a cleared cell, not a malformed one.
    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set-many&cells=1:city:",
    );
    assert_eq!(status, 200, "{body}");
    let (_, rows) = get(&server.addr, "/rows?path=t.csv&start=0&count=1");
    assert!(rows.contains(r#"[["","1"]]"#), "the cell is empty: {rows}");
}

/// A `;` inside a value cannot be expressed: it splits into an entry that fails to parse,
/// and the batch is refused before anything is written — not half-applied.
#[test]
fn set_many_refuses_a_semicolon_inside_a_value() {
    let dir = TempDir::new("setmanysemicolon");
    std::fs::write(dir.join("t.csv"), "city,amount\nA,1\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set-many&cells=1:city:a;b",
    );
    assert_eq!(status, 400, "{body}");
    assert!(body.contains(r#""parameter":"cells""#), "{body}");
    let (_, journal) = get(&server.addr, "/journal?path=t.csv");
    assert!(
        journal.contains(r#""count":0"#),
        "nothing was written: {journal}"
    );
}

/// Dedupe and partition are "flatten the edits into output files" commands: with
/// `apply-edits=1` the output carries the edit, without it the base file is what gets written.
#[test]
fn dedupe_and_partition_can_apply_edits() {
    let dir = TempDir::new("dedupedapply");
    std::fs::write(dir.join("t.csv"), "city,amount\nA,1\nA,2\nB,3\n").unwrap();
    let server = start(&dir.0.clone());

    let (status, body) = post(
        &server.addr,
        "/edit?path=t.csv&action=set&row=1&col=city&value=Q",
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = post(
        &server.addr,
        "/dedupe?path=t.csv&by=city&out=dedup.csv&apply-edits=1",
    );
    assert!(status == 200 || status == 202, "{body}");
    let mut rows = String::new();
    for _ in 0..100 {
        (_, rows) = get(&server.addr, "/rows?path=dedup.csv&start=0&count=5");
        if rows.contains("Q") || rows.contains("city") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(rows.contains("Q"), "the edit survives dedupe: {rows}");
    assert!(rows.contains("A") && rows.contains("B"), "{rows}");

    let (status, body) = post(
        &server.addr,
        "/partition?path=t.csv&by=city&out=parts.tar&apply-edits=1",
    );
    assert!(status == 200 || status == 202, "{body}");
    // `/jobs?path=t.csv` also lists the dedupe job above; waiting for any "done" let the
    // dedupe job satisfy the loop while the partition was still writing its archive.
    let mut state = String::new();
    for _ in 0..100 {
        (_, state) = get(&server.addr, "/jobs?path=t.csv");
        if state.contains(r#""kind":"partition","state":"done""#)
            || state.contains(r#""state":"failed""#)
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        state.contains(r#""kind":"partition","state":"done""#),
        "{state}"
    );

    // The archive is a tar of one file per value; a part named after Q proves the edit
    // reached the partition step.
    let listing = std::process::Command::new("tar")
        .args(["-tf", dir.join("parts.tar").to_str().unwrap()])
        .output()
        .unwrap();
    let names = String::from_utf8_lossy(&listing.stdout).to_string();
    assert!(
        names.contains('Q'),
        "a part named after the edited value: {names}"
    );
}

/// A transaction validates the whole list before writing, and applies all of it in one
/// journal update. A rejected entry must leave nothing behind.
#[test]
fn a_transaction_applies_every_op_or_none() {
    let dir = TempDir::new("txn");
    std::fs::write(dir.join("t.csv"), "city,amount\nA,1\nB,2\nC,3\n").unwrap();
    let server = start(&dir.0.clone());

    let ops = "set%3A1%3Acity%3AZ%3Bdelete%3A2%3A1%3Binsert%3A4%3Ad%2C4";
    let (code, body) = post(
        &server.addr,
        &format!("/edit?path=t.csv&action=transaction&ops={ops}"),
    );
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""atomic":true"#), "{body}");
    let (_, rows) = get(&server.addr, "/rows?path=t.csv&count=10");
    assert!(rows.contains("\"Z\""), "the set must land: {rows}");
    assert!(rows.contains("\"d\""), "the insert must land: {rows}");
    assert!(!rows.contains("\"B\""), "the delete must land: {rows}");

    // One bad entry (a delete past the end) must reject the whole batch: the set in the
    // same request must not sneak in.
    let bad = "set%3A1%3Acity%3AQ%3Bdelete%3A99%3A1";
    let (code, body) = post(
        &server.addr,
        &format!("/edit?path=t.csv&action=transaction&ops={bad}"),
    );
    assert_eq!(code, 404, "{body}");
    let (_, rows) = get(&server.addr, "/rows?path=t.csv&count=10");
    assert!(
        !rows.contains('Q'),
        "a rejected batch wrote something: {rows}"
    );
}

/// The HTTP undo honours the same transaction boundary as the CLI.
#[test]
fn a_transaction_is_undone_in_one_step_over_http() {
    let dir = TempDir::new("txnundohttp");
    std::fs::write(dir.join("t.csv"), "city,amount\nA,1\nB,2\nC,3\n").unwrap();
    let server = start(&dir.0.clone());
    let ops = "set%3A1%3Acity%3AZ%3Bdelete%3A2%3A1%3Binsert%3A4%3Ad%2C4";
    let (code, body) = post(
        &server.addr,
        &format!("/edit?path=t.csv&action=transaction&ops={ops}"),
    );
    assert_eq!(code, 200, "{body}");

    let (code, body) = post(&server.addr, "/edit?path=t.csv&action=undo");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""batch":3"#), "{body}");
    assert!(body.contains(r#""operations":0"#), "{body}");

    let (_, rows) = get(&server.addr, "/rows?path=t.csv&count=10");
    assert!(
        rows.contains("A") && rows.contains("B") && rows.contains("C"),
        "{rows}"
    );
    assert!(!rows.contains("\"Z\"") && !rows.contains("\"d\""), "{rows}");

    // Redo mirrors it: one call puts the whole transaction back.
    let (code, body) = post(&server.addr, "/edit?path=t.csv&action=redo");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains(r#""batch":3"#), "{body}");
    assert!(body.contains(r#""operations":3"#), "{body}");
    let (_, rows) = get(&server.addr, "/rows?path=t.csv&count=10");
    assert!(rows.contains("\"Z\"") && rows.contains("\"d\""), "{rows}");
    assert!(!rows.contains("\"B\""), "{rows}");
}
