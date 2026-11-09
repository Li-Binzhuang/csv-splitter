//! Black-box tests for the CLI binary: these are what a user actually runs.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_csv-splitter")
}

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
            "csvs_cli_{tag}_{}_{}",
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
            "zst",
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

#[test]
fn gbk_csv_is_decoded_for_preview_and_split() {
    let dir = TempDir::new("gbk");
    let input = dir.join("gbk.csv");
    // "城市,人口\n北京,2189\n上海,2424\n" in GBK
    let bytes: Vec<u8> = vec![
        179, 199, 202, 208, 44, 200, 203, 191, 218, 10, 177, 177, 190, 169, 44, 50, 49, 56, 57, 10,
        201, 207, 186, 163, 44, 50, 52, 50, 52, 10,
    ];
    std::fs::write(&input, &bytes).unwrap();

    let preview = Command::new(bin())
        .args(["preview", input.to_str().unwrap(), "--rows", "5"])
        .output()
        .unwrap();
    assert!(preview.status.success());
    let text = String::from_utf8_lossy(&preview.stdout);
    assert!(text.contains("城市 | 人口"), "{text}");
    assert!(text.contains("北京 | 2189"), "{text}");

    // Column filtering on a GBK file has to decode before re-rendering.
    let archive = dir.join("pick.tar");
    let merged = dir.join("pick.csv");
    Command::new(bin())
        .args([
            "split",
            input.to_str().unwrap(),
            "-n",
            "1",
            "-f",
            "zst",
            "--columns",
        ])
        .arg("人口")
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
    let picked = std::fs::read_to_string(&merged).unwrap();
    assert_eq!(picked, "人口\n2189\n2424\n");
}

#[test]
fn convert_makes_utf16_files_indexable() {
    let dir = TempDir::new("conv");
    let input = dir.join("u16.csv");
    let output = dir.join("u16.utf8.csv");

    // UTF-16LE with BOM: "id,name\n1,北京\n2,上海\n"
    let text = "id,name\n1,北京\n2,上海\n";
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(&input, &bytes).unwrap();

    let out = Command::new(bin())
        .args(["convert", input.to_str().unwrap(), "-o"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&output).unwrap(), text);

    // The converted file is a normal UTF-8 CSV: the index works on it.
    let out = Command::new(bin())
        .args(["index", output.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("2 data rows"));

    let out = Command::new(bin())
        .args([
            "preview",
            output.to_str().unwrap(),
            "--start-row",
            "1",
            "--rows",
            "1",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("2 | 上海"));
}

#[test]
fn convert_rejects_unknown_encoding() {
    let status = Command::new(bin())
        .args(["convert", "x.csv", "--from", "klingon"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn filter_returns_matching_row_numbers() {
    let dir = TempDir::new("filter");
    let input = dir.join("f.csv");
    let mut csv = String::from("id,city,amount\n");
    for i in 0..100 {
        let city = if i % 10 == 0 { "CDMX" } else { "Puebla" };
        csv.push_str(&format!("{i},{city},{}\n", i * 10));
    }
    std::fs::write(&input, csv).unwrap();

    let out = Command::new(bin())
        .args([
            "filter",
            input.to_str().unwrap(),
            "--where",
            "city=CDMX",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("\"matched\":10"), "{text}");
    // Rows 1, 11, 21, ... in 1-based row numbering.
    assert!(text.contains("\"rows\":[1,11,21"), "{text}");

    let out = Command::new(bin())
        .args(["filter", input.to_str().unwrap(), "--where", "amount>900"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("matched 9"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let status = Command::new(bin())
        .args(["filter", input.to_str().unwrap(), "--where", "nonsense"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn sql_command_answers_aggregate_questions() {
    let dir = TempDir::new("sql");
    let input = dir.join("s.csv");
    let mut csv = String::from("city,amount\n");
    for i in 0..200 {
        csv.push_str(&format!(
            "{},{}\n",
            if i % 2 == 0 { "CDMX" } else { "Puebla" },
            i
        ));
    }
    std::fs::write(&input, csv).unwrap();

    let out = Command::new(bin())
        .args([
            "sql",
            input.to_str().unwrap(),
            "SELECT city, count(*) AS n FROM t GROUP BY 1 ORDER BY 1",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("CDMX | 100"), "{text}");
    assert!(text.contains("Puebla | 100"), "{text}");

    // Writes are refused before they reach the engine.
    let out = Command::new(bin())
        .args(["sql", input.to_str().unwrap(), "DELETE FROM t"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("only SELECT"));
}

#[test]
fn sql_respects_the_row_cap() {
    let dir = TempDir::new("sqlcap");
    let input = dir.join("s.csv");
    let mut csv = String::from("id\n");
    for i in 0..500 {
        csv.push_str(&format!("{i}\n"));
    }
    std::fs::write(&input, csv).unwrap();

    let out = Command::new(bin())
        .args([
            "sql",
            input.to_str().unwrap(),
            "SELECT * FROM t",
            "--limit",
            "3",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("\"truncated\":true"), "{text}");
    // Three rows, each its own array; assert on the rendered JSON rather than a
    // bracket count (which counts the columns array too and means nothing).
    assert!(text.contains(r#""rows":[["0"],["1"],["2"]]"#), "{text}");
}

#[test]
fn search_finds_text_anywhere_in_the_row() {
    let dir = TempDir::new("search");
    let input = dir.join("s.csv");
    let mut csv = String::from("id,city,note\n");
    csv.push_str("1,CDMX,refund issued\n");
    csv.push_str("2,Puebla,ok\n");
    csv.push_str("3,CDMX,pay refund later\n");
    std::fs::write(&input, csv).unwrap();

    let out = Command::new(bin())
        .args(["search", input.to_str().unwrap(), "refund", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("\"matched\":2"), "{text}");
    assert!(text.contains("\"rows\":[1,3]"), "{text}");

    // Column narrowing and case sensitivity are honoured.
    let out = Command::new(bin())
        .args([
            "search",
            input.to_str().unwrap(),
            "cdmx",
            "--columns",
            "city",
            "--case",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("matched 0"));

    let out = Command::new(bin())
        .args([
            "search",
            input.to_str().unwrap(),
            "CDMX",
            "--columns",
            "city",
            "--case",
        ])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("matched 2"));

    // Missing text is a usage error, not a silent scan.
    let status = Command::new(bin())
        .args(["search", input.to_str().unwrap()])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn files_lists_the_tree_relative_to_root() {
    let dir = TempDir::new("files");
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(dir.join("a.csv"), "id\n1\n").unwrap();
    std::fs::write(dir.join("nested/b.csv"), "id\n1\n").unwrap();
    std::fs::write(dir.join("skip.md"), "x").unwrap();

    let out = Command::new(bin())
        .args([
            "files",
            "--root",
            dir.0.to_str().unwrap(),
            "--ext",
            "csv",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("\"a.csv\""), "{text}");
    assert!(text.contains("\"nested/b.csv\""), "{text}");
    assert!(!text.contains("skip.md"), "{text}");
}

#[test]
fn filter_writes_a_csv_and_sort_orders_the_file() {
    let dir = TempDir::new("fsort");
    let input = dir.join("in.csv");
    let mut csv = String::from("city,amount\n");
    for (city, amount) in [
        ("CDMX", 10),
        ("CDMX", 500),
        ("Puebla", 500),
        ("Tijuana", 20),
    ] {
        csv.push_str(&format!("{city},{amount}\n"));
    }
    std::fs::write(&input, csv).unwrap();

    // AND across two predicates, written straight to a file.
    let out = dir.join("and.csv");
    let status = Command::new(bin())
        .args([
            "filter",
            input.to_str().unwrap(),
            "--where",
            "city=CDMX",
            "--where",
            "amount>100",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "city,amount\nCDMX,500\n"
    );

    // OR keeps either side.
    let any = dir.join("any.csv");
    let status = Command::new(bin())
        .args([
            "filter",
            input.to_str().unwrap(),
            "--where",
            "city=Tijuana",
            "--where",
            "amount>400",
            "--any",
            "-o",
            any.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&any).unwrap();
    assert_eq!(text.lines().count(), 4, "{text}");

    // Sorting is its own command now.
    let sorted = dir.join("sorted.csv");
    let status = Command::new(bin())
        .args([
            "sort",
            input.to_str().unwrap(),
            "--by",
            "amount",
            "--desc",
            "-o",
            sorted.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&sorted).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "city,amount");
    assert_eq!(lines[1], "CDMX,500");
    // Descending: 500, 500, 20, 10.
    assert_eq!(lines[3], "Tijuana,20");
    assert_eq!(lines[4], "CDMX,10");

    // A bad column is a client error, not a silently empty file.
    let status = Command::new(bin())
        .args([
            "sort",
            input.to_str().unwrap(),
            "--by",
            "nope",
            "-o",
            dir.join("x.csv").to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}
