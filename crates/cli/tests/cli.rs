//! Black-box tests for the CLI binary: these are what a user actually runs.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_csv-splitter")
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "csvs_cli_{tag}_{}_{}",
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

fn sample_csv(dir: &TempDir) -> PathBuf {
    let path = dir.join("data.csv");
    std::fs::write(
        &path,
        "id,name,note\n1,a,\"line1\nline2\"\n2,b,plain\n3,c,\"x,y\"\n4,d,\n5,e,z\n",
    )
    .unwrap();
    path
}

#[test]
fn split_then_merge_is_lossless() {
    let dir = TempDir::new("rt");
    let input = sample_csv(&dir);
    let archive = dir.join("out.tar");
    let merged = dir.join("merged.csv");

    let status = Command::new(bin())
        .args([
            "split",
            input.to_str().unwrap(),
            "-n",
            "3",
            "-f",
            "zst",
            "-o",
        ])
        .arg(&archive)
        .arg("-q")
        .status()
        .unwrap();
    assert!(status.success());

    let status = Command::new(bin())
        .args(["merge"])
        .arg(&archive)
        .args(["-o"])
        .arg(&merged)
        .arg("-q")
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(
        std::fs::read(&input).unwrap(),
        std::fs::read(&merged).unwrap()
    );
}

#[test]
fn verify_reports_lossless() {
    let dir = TempDir::new("verify");
    let input = sample_csv(&dir);
    let out = Command::new(bin())
        .args(["verify", input.to_str().unwrap(), "-n", "4"])
        .arg("-q")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("lossless"));
}

#[test]
fn column_filter_produces_selected_headers() {
    let dir = TempDir::new("cols");
    let input = sample_csv(&dir);
    let archive = dir.join("cols.tar");
    let merged = dir.join("cols.csv");

    Command::new(bin())
        .args([
            "split",
            input.to_str().unwrap(),
            "-n",
            "2",
            "-f",
            "none",
            "--columns",
        ])
        .arg("id,note")
        .args(["-o"])
        .arg(&archive)
        .arg("-q")
        .status()
        .unwrap();

    Command::new(bin())
        .args(["merge"])
        .arg(&archive)
        .args(["-o"])
        .arg(&merged)
        .arg("-q")
        .status()
        .unwrap();

    let text = std::fs::read_to_string(&merged).unwrap();
    assert!(text.starts_with("id,note\n"), "unexpected header: {text}");
    assert!(!text.contains("name"));
}

#[test]
fn preview_and_stats_do_not_shift_columns() {
    let dir = TempDir::new("peek");
    let input = sample_csv(&dir);

    let preview = Command::new(bin())
        .args(["preview", input.to_str().unwrap(), "--rows", "5"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    let text = String::from_utf8_lossy(&preview.stdout);
    assert!(text.contains("id | name | note"), "{text}");

    let stats = Command::new(bin())
        .args([
            "stats",
            input.to_str().unwrap(),
            "--sample",
            "100",
            "--top",
            "2",
        ])
        .output()
        .unwrap();
    assert!(stats.status.success());
    let text = String::from_utf8_lossy(&stats.stdout);
    // Four of five rows have a note; a broken reader would miscount empties.
    assert!(text.contains("note"), "{text}");
}

#[test]
fn unknown_option_exits_with_usage_error() {
    let status = Command::new(bin())
        .args(["split", "whatever.csv", "--nope"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn missing_input_exits_nonzero() {
    let status = Command::new(bin()).args(["split"]).status().unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn by_size_controls_chunk_count() {
    let dir = TempDir::new("bysize");
    let input = dir.join("big.csv");
    let archive = dir.join("parts.tar");
    let merged = dir.join("merged.csv");

    let mut csv = String::from("id,payload\n");
    for i in 0..2000 {
        csv.push_str(&format!("{i},{}\n", "y".repeat(480)));
    }
    std::fs::write(&input, &csv).unwrap();

    let status = Command::new(bin())
        .args([
            "split",
            input.to_str().unwrap(),
            "--by-size",
            "100KB",
            "-f",
            "none",
            "-o",
        ])
        .arg(&archive)
        .arg("-q")
        .status()
        .unwrap();
    assert!(status.success());

    Command::new(bin())
        .args(["merge"])
        .arg(&archive)
        .args(["-o"])
        .arg(&merged)
        .arg("-q")
        .status()
        .unwrap();

    assert_eq!(std::fs::read_to_string(&merged).unwrap(), csv);
}

#[test]
fn by_size_rejects_garbage() {
    let status = Command::new(bin())
        .args(["split", "x.csv", "--by-size", "banana"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn by_column_writes_one_file_per_value() {
    let dir = TempDir::new("bycol");
    let input = dir.join("orders.csv");
    let archive = dir.join("parts.tar");
    std::fs::write(&input, "id,city\n1,CDMX\n2,Puebla\n3,CDMX\n").unwrap();

    let status = Command::new(bin())
        .args([
            "split",
            input.to_str().unwrap(),
            "--by-column",
            "city",
            "-f",
            "none",
            "-o",
        ])
        .arg(&archive)
        .arg("-q")
        .status()
        .unwrap();
    assert!(status.success());

    let listing = Command::new("tar")
        .arg("tf")
        .arg(&archive)
        .output()
        .unwrap();
    let names = String::from_utf8_lossy(&listing.stdout);
    assert!(names.contains("CDMX.csv"), "{names}");
    assert!(names.contains("Puebla.csv"), "{names}");
}

#[test]
fn by_column_and_by_size_conflict() {
    let status = Command::new(bin())
        .args(["split", "x.csv", "--by-column", "city", "--by-size", "10MB"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

fn indexed_csv(dir: &TempDir) -> PathBuf {
    let path = dir.join("rows.csv");
    let mut csv = String::from("id,note\n");
    for i in 0..1000 {
        if i % 97 == 0 {
            csv.push_str(&format!("{i},\"a\nb\"\n"));
        } else {
            csv.push_str(&format!("{i},ok\n"));
        }
    }
    std::fs::write(&path, csv).unwrap();
    path
}

#[test]
fn index_builds_a_sidecar_and_reports_rows() {
    let dir = TempDir::new("idx");
    let input = indexed_csv(&dir);

    let out = Command::new(bin())
        .args(["index", input.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("1000 data rows"), "{text}");
    assert!(input.with_file_name("rows.csv.idx").exists());
}

#[test]
fn preview_start_row_uses_the_index() {
    let dir = TempDir::new("jump");
    let input = indexed_csv(&dir);

    let out = Command::new(bin())
        .args([
            "preview",
            input.to_str().unwrap(),
            "--start-row",
            "500",
            "--rows",
            "2",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    // Rows are printed with a 1-based row number; the first cell is the id.
    assert!(text.contains("500 | ok") || text.contains("500,"), "{text}");
    assert!(text.contains("501"), "{text}");
}
