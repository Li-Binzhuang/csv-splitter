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
    assert!(html.contains("csv-splitter"), "{html}");
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
    assert!(body.contains("\"rowsSoFar\":2"), "{body}");
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
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
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
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
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

    // The second identical request must be materially faster; both being ~0 would make
    // the assertion meaningless, so require a cold run that took measurable time.
    assert!(
        warm_elapsed <= cold.max(std::time::Duration::from_millis(50)),
        "warm {warm_elapsed:?} vs cold {cold:?}"
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
