//! Black-box tests for the HTTP API: start the real binary and query it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

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
