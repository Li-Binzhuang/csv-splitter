//! Contract test for the built-in viewer.
//!
//! The HTML page cannot be opened in CI, so the risk is that it silently stops
//! working: an endpoint gains a required parameter, a response field gets renamed,
//! and the page white-screens for the only person who opens it.
//!
//! This test reads the page, finds every `api("/x")` call it makes, and replays
//! the same request sequence against a live server, asserting the exact fields the
//! JavaScript reads are present. It is a checked contract, not a browser: it
//! cannot catch CSS or layout problems, but it catches the failure that actually
//! happens — API drift.

use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

const PAGE: &str = include_str!("../src/web/index.html");

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "csvs_view_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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
    addr: String,
    child: std::process::Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(root: &std::path::Path) -> Server {
    // Bind an ephemeral port and read it from the banner. A pid-derived port
    // collided whenever two test binaries ran at once, and `cargo test` runs them
    // in parallel.
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
        .args(["serve", "--root", root.to_str().unwrap()])
        .args(["--addr", "127.0.0.1:0"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn serve");

    let stdout = child.stdout.take().unwrap();
    let mut line = String::new();
    std::io::BufReader::new(stdout)
        .read_line(&mut line)
        .unwrap();
    // "csv-splitter API on http://127.0.0.1:PORT  (root: ...)"
    let addr = line
        .split("http://")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .expect("address in banner")
        .to_string();
    Server { addr, child }
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
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

/// Endpoints the page calls, extracted from the HTML itself.
fn page_endpoints() -> Vec<String> {
    let mut out = Vec::new();
    for part in PAGE.split("api(\"").skip(1) {
        if let Some(end) = part.find('"') {
            out.push(part[..end].to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn the_page_only_calls_advertised_endpoints() {
    let dir = TempDir::new("endpoints");
    std::fs::write(dir.join("a.csv"), "id\n1\n").unwrap();
    let server = start(&dir.0);

    // The router advertises its own endpoints in the 404 body; compare against
    // that instead of a hand-kept list.
    let (status, body) = get(&server.addr, "/nope");
    assert_eq!(status, 404);
    let advertised = body;
    for endpoint in page_endpoints() {
        assert!(
            advertised.contains(&format!("\"{endpoint}\"")),
            "the viewer calls {endpoint}, which the router does not advertise in {advertised}"
        );
    }
}

#[test]
fn the_page_request_sequence_returns_the_fields_it_reads() {
    let dir = TempDir::new("sequence");
    let mut csv = String::from("city,amount,note\n");
    csv.push_str("CDMX,10,ok\n");
    csv.push_str("CDMX,500,\"a\nb\"\n");
    csv.push_str("Puebla,50,ok\n");
    std::fs::write(dir.join("orders.csv"), csv).unwrap();
    let server = start(&dir.0);

    // 1. loadFiles: `data.files[].path` and `.bytes`.
    let (status, body) = get(&server.addr, "/files?ext=csv%2Ctsv%2Ccsv.gz");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"files\":["), "{body}");
    assert!(body.contains("\"path\":\"orders.csv\""), "{body}");
    assert!(body.contains("\"bytes\":"), "{body}");

    // 2. openFile: `meta.rows`, `meta.bytes`, `meta.encoding`.
    let (status, body) = get(&server.addr, "/meta?path=orders.csv");
    assert_eq!(status, 200, "{body}");
    for field in ["\"rows\"", "\"bytes\"", "\"encoding\"", "\"headers\""] {
        assert!(body.contains(field), "meta misses {field}: {body}");
    }

    // 3. reloadRows: `columns`, `rows`, and `rowsSoFar` when filtered.
    let (status, body) = get(&server.addr, "/rows?path=orders.csv&start=0&count=100");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"columns\""), "{body}");
    assert!(body.contains("\"rows\""), "{body}");
    assert!(body.contains("\"total\""), "{body}");

    // 4. Filtering through the where box.
    let (status, body) = get(
        &server.addr,
        "/rows?path=orders.csv&start=0&count=100&where=city%3DCDMX",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"rowsSoFar\":2"), "{body}");
    assert!(body.contains("\"hasMore\""), "{body}");
    assert!(!body.contains("Puebla"), "filter leaked other rows: {body}");

    // 5. doSearch: `matched`, `rows`, `truncated`.
    let (status, body) = get(&server.addr, "/search?path=orders.csv&q=CDMX&limit=1000");
    assert_eq!(status, 200, "{body}");
    for field in ["\"matched\"", "\"rows\"", "\"truncated\""] {
        assert!(body.contains(field), "search misses {field}: {body}");
    }

    // 6. loadProfile: every field the table renders.
    let (status, body) = get(&server.addr, "/analyze?path=orders.csv");
    assert_eq!(status, 200, "{body}");
    for field in [
        "\"name\"",
        "\"rows\"",
        "\"empty\"",
        "\"distinct\"",
        "\"distinctTruncated\"",
        "\"numeric\"",
        "\"top\"",
        "\"ragged\"",
    ] {
        assert!(body.contains(field), "analyze misses {field}: {body}");
    }

    // 7. loadGroupby: `columns` and `rows`.
    let (status, body) = get(
        &server.addr,
        "/groupby?path=orders.csv&by=city&agg=amount:sum,amount:count&desc=1",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"columns\""), "{body}");
    assert!(body.contains("\"rows\""), "{body}");
    assert!(body.contains("CDMX"), "{body}");
}
