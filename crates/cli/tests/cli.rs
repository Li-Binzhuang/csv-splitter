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

#[test]
fn analyze_profiles_columns_and_dedupe_removes_rows() {
    let dir = TempDir::new("prof");
    let input = dir.join("p.csv");
    let mut csv = String::from("id,city,amount\n");
    for i in 0..6 {
        csv.push_str(&format!("{},CDMX,{}\n", i % 3, 10 * (i + 1)));
    }
    std::fs::write(&input, csv).unwrap();

    let out = Command::new(bin())
        .args(["analyze", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("\"distinct\":3"), "{text}");
    assert!(text.contains("\"numeric\":{\"count\":6"), "{text}");
    // sum of 10,20,30,40,50,60
    assert!(text.contains("\"sum\":210"), "{text}");

    let deduped = dir.join("clean.csv");
    let out = Command::new(bin())
        .args([
            "dedupe",
            input.to_str().unwrap(),
            "--by",
            "id",
            "-o",
            deduped.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(&deduped).unwrap();
    assert_eq!(text.lines().count(), 4, "header + 3 distinct ids: {text}");
}

#[test]
fn sort_by_several_columns_and_regex_filter() {
    let dir = TempDir::new("multi");
    let input = dir.join("in.csv");
    let mut csv = String::from("city,amount,note\n");
    csv.push_str("CDMX,30,REFUND\n");
    csv.push_str("Puebla,10,ok\n");
    csv.push_str("CDMX,10,REFUND now\n");
    csv.push_str("Puebla,30,refund\n");
    std::fs::write(&input, csv).unwrap();

    let out = dir.join("sorted.csv");
    let status = Command::new(bin())
        .args([
            "sort",
            input.to_str().unwrap(),
            "--by",
            "city:asc,amount:desc",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[1], "CDMX,30,REFUND");
    assert_eq!(lines[2], "CDMX,10,REFUND now");
    assert_eq!(lines[3], "Puebla,30,refund");

    // Regex is case-sensitive unless the pattern says otherwise.
    let re_file = dir.join("re.csv");
    let status = Command::new(bin())
        .args([
            "filter",
            input.to_str().unwrap(),
            "--where",
            "note~re:^REFUND",
            "-o",
            re_file.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&re_file).unwrap(),
        "city,amount,note\nCDMX,30,REFUND\nCDMX,10,REFUND now\n"
    );

    // A broken pattern is a usage error, not an empty result.
    let status = Command::new(bin())
        .args([
            "filter",
            input.to_str().unwrap(),
            "--where",
            "note~re:[oops",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn columns_reshapes_and_in_filter_combines() {
    let dir = TempDir::new("cols");
    let input = dir.join("in.csv");
    let mut csv = String::from("internal_id,city,amount\n");
    csv.push_str("7,CDMX,10\n");
    csv.push_str("8,Puebla,500\n");
    csv.push_str("9,Tijuana,20\n");
    std::fs::write(&input, csv).unwrap();

    // Drop the private column, rename the Chinese-facing ones.
    let out = dir.join("share.csv");
    let status = Command::new(bin())
        .args([
            "columns",
            input.to_str().unwrap(),
            "--drop",
            "internal_id",
            "--rename",
            "city:城市",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().next().unwrap(), "城市,amount", "{text}");
    assert_eq!(text.lines().count(), 4);

    // `in (a,b)` behaves like an OR of equalities.
    let hits = dir.join("hits.csv");
    let status = Command::new(bin())
        .args([
            "filter",
            input.to_str().unwrap(),
            "--where",
            "city in CDMX,Puebla",
            "-o",
            hits.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&hits).unwrap();
    assert_eq!(text.lines().count(), 3, "{text}");
    assert!(!text.contains("Tijuana"), "{text}");

    // A conflicting spec is refused instead of silently dropping the column.
    let status = Command::new(bin())
        .args([
            "columns",
            input.to_str().unwrap(),
            "--keep",
            "city",
            "--drop",
            "city",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn partition_by_column_and_concat_files() {
    let dir = TempDir::new("parts");
    let input = dir.join("orders.csv");
    let mut csv = String::from("city,amount\n");
    for (city, amount) in [("CDMX", 10), ("Puebla", 500), ("CDMX", 20), ("Tijuana", 5)] {
        csv.push_str(&format!("{city},{amount}\n"));
    }
    std::fs::write(&input, csv).unwrap();

    // One file per city, packed into a tar.
    let archive = dir.join("by_city.tar");
    let status = Command::new(bin())
        .args([
            "partition",
            input.to_str().unwrap(),
            "--by",
            "city",
            "-o",
            archive.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let names = std::process::Command::new("tar")
        .args(["-tf", archive.to_str().unwrap()])
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&names.stdout);
    assert!(listing.contains("CDMX.csv"), "{listing}");
    assert!(listing.contains("Puebla.csv"), "{listing}");
    assert!(listing.contains("Tijuana.csv"), "{listing}");

    // Concat refuses a header mismatch, and accepts it when told to.
    let b = dir.join("more.csv");
    std::fs::write(&b, "city,amount\nMonterrey,7\n").unwrap();
    let out = dir.join("all.csv");
    let ok = Command::new(bin())
        .args([
            "concat",
            input.to_str().unwrap(),
            b.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(ok.code(), Some(0));
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().count(), 6, "one header + 5 rows: {text}");

    let bad = dir.join("bad.csv");
    std::fs::write(&bad, "city,total\nMonterrey,7\n").unwrap();
    let out2 = dir.join("bad_out.csv");
    let status = Command::new(bin())
        .args([
            "concat",
            input.to_str().unwrap(),
            bad.to_str().unwrap(),
            "-o",
            out2.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn groupby_writes_an_aggregated_file() {
    let dir = TempDir::new("gb");
    let input = dir.join("orders.csv");
    let mut csv = String::from("city,kind,amount\n");
    csv.push_str("CDMX,a,10\nCDMX,a,20\nCDMX,b,5\nPuebla,a,50\n");
    std::fs::write(&input, csv).unwrap();

    let out = dir.join("agg.csv");
    let status = Command::new(bin())
        .args([
            "groupby",
            input.to_str().unwrap(),
            "--by",
            "city",
            "--agg",
            "amount:sum,amount:mean,amount:max",
            "--order",
            "sum_amount",
            "--desc",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "city,sum_amount,mean_amount,max_amount");
    assert_eq!(lines[1], "Puebla,50,50,50");
    assert_eq!(lines[2], "CDMX,35,11.666667,20");

    // Multi-key grouping.
    let out2 = dir.join("agg2.csv");
    let status = Command::new(bin())
        .args([
            "groupby",
            input.to_str().unwrap(),
            "--by",
            "city,kind",
            "--agg",
            "amount:count",
            "-o",
            out2.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&out2).unwrap();
    assert_eq!(text.lines().count(), 4, "header + 3 groups: {text}");
}

/// The dirty-data path: GBK bytes, stray spaces in headers, ragged rows.
#[test]
fn handles_gbk_headers_with_stray_spaces_and_ragged_rows() {
    let dir = TempDir::new("dirty");
    let input = dir.join("messy.csv");
    // GBK bytes, CRLF endings, header names with trailing spaces, and one row
    // with an extra unquoted comma.
    let text = "id,城市 ,金额\r\n1,CDMX,10\r\n2,Puebla,20,x\r\n3,CDMX,30\r\n";
    let bytes = encoding_rs::GBK.encode(text).0;
    std::fs::write(&input, bytes.as_ref()).unwrap();

    // A column given without its trailing space still resolves.
    let out = Command::new(bin())
        .args([
            "filter",
            input.to_str().unwrap(),
            "--where",
            "城市=CDMX",
            "-o",
            dir.join("c.csv").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let filtered = std::fs::read_to_string(dir.join("c.csv")).unwrap();
    assert_eq!(filtered.lines().count(), 3, "{filtered}");

    // `analyze` reports the ragged row instead of silently shifting columns.
    let out = Command::new(bin())
        .args(["analyze", input.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("field count"), "{text}");
    assert!(text.contains("1 of 3 rows"), "{text}");

    // Partitioning a GBK file must decode, not mojibake the header.
    let archive = dir.join("parts.tar");
    let status = Command::new(bin())
        .args([
            "partition",
            input.to_str().unwrap(),
            "--by",
            "城市",
            "-o",
            archive.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(archive.exists());
}

/// Edge fixtures: zero bytes, header only, and Windows-1252 accents.
#[test]
fn edge_cases_do_not_error() {
    let dir = TempDir::new("edge");

    // 0 bytes: nothing to say, but not a failure.
    let empty = dir.join("empty.csv");
    std::fs::write(&empty, "").unwrap();
    let out = Command::new(bin())
        .args(["analyze", empty.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Header only: analysis reports zero rows, and split still round-trips.
    let header = dir.join("header.csv");
    std::fs::write(&header, "id,city\n").unwrap();
    let archive = dir.join("h.tar");
    let status = Command::new(bin())
        .args([
            "split",
            header.to_str().unwrap(),
            "-n",
            "2",
            "-o",
            archive.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0), "header-only split should succeed");
    let merged = dir.join("merged.csv");
    let status = Command::new(bin())
        .args([
            "merge",
            archive.to_str().unwrap(),
            "-o",
            merged.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(
        std::fs::read(&merged).unwrap(),
        b"id,city\n",
        "header-only file must round-trip byte for byte"
    );

    // Windows-1252 is not GBK: the accents must survive.
    let latin = dir.join("latin.csv");
    std::fs::write(&latin, b"id,caf\xe9\n1,r\xe9sum\xe9\n").unwrap();
    let out = Command::new(bin())
        .args(["preview", latin.to_str().unwrap(), "--rows", "5"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("café"), "{text}");
    assert!(text.contains("résumé"), "{text}");
}

#[test]
fn verify_flag_confirms_the_claims_of_each_operation() {
    let dir = TempDir::new("selfcheck");
    let input = dir.join("in.csv");
    let mut csv = String::from("id,city,amount\n");
    for i in 0..20 {
        csv.push_str(&format!("{},CDMX,{}\n", i % 5, i));
    }
    std::fs::write(&input, csv).unwrap();

    // filter: output holds exactly the matches.
    let filtered = dir.join("f.csv");
    let status = Command::new(bin())
        .args([
            "filter",
            input.to_str().unwrap(),
            "--where",
            "city=CDMX",
            "-o",
            filtered.to_str().unwrap(),
            "--verify",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    // columns: same rows, fewer columns.
    let reshaped = dir.join("c.csv");
    let status = Command::new(bin())
        .args([
            "columns",
            input.to_str().unwrap(),
            "--drop",
            "city",
            "-o",
            reshaped.to_str().unwrap(),
            "--verify",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    // dedupe: kept + removed must equal the source rows.
    let deduped = dir.join("d.csv");
    let status = Command::new(bin())
        .args([
            "dedupe",
            input.to_str().unwrap(),
            "--by",
            "id",
            "-o",
            deduped.to_str().unwrap(),
            "--verify",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&deduped).unwrap().lines().count(),
        6
    );
}

/// The documented tour must actually work on the documented sample.
#[test]
fn the_getting_started_tour_runs_end_to_end() {
    let dir = TempDir::new("tour");
    let demo_dir = dir.join("demo");

    let out = Command::new(bin())
        .args(["demo", "-o", demo_dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sample = demo_dir.join("orders.csv");
    assert!(sample.exists());

    // 1. analyze names the ragged rows (the guide's first lesson).
    let out = Command::new(bin())
        .args(["analyze", sample.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("city"), "{text}");
    assert!(
        text.contains("field count"),
        "ragged warning missing: {text}"
    );

    // 2. filter + verify.
    let filtered = dir.join("cdmx.csv");
    let status = Command::new(bin())
        .args([
            "filter",
            sample.to_str().unwrap(),
            "--where",
            "city=CDMX",
            "-o",
            filtered.to_str().unwrap(),
            "--verify",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    // 3. groupby writes the documented shape.
    let grouped = dir.join("per_city.csv");
    let status = Command::new(bin())
        .args([
            "groupby",
            sample.to_str().unwrap(),
            "--by",
            "city",
            "--agg",
            "amount:sum,amount:count",
            "--order",
            "sum_amount",
            "--desc",
            "-o",
            grouped.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&grouped).unwrap();
    assert_eq!(text.lines().next().unwrap(), "city,sum_amount,count_amount");

    // 4. verify proves the split is lossless.
    let status = Command::new(bin())
        .args(["verify", sample.to_str().unwrap(), "-n", "4"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0), "verify should pass on the sample");
}

/// Two shapes only: usage errors are `error:` + exit 2, runtime failures are
/// `✗ <command>:` + exit 1. Scripts key off this, and a hint line is allowed
/// underneath a runtime failure.
#[test]
fn failures_use_one_shape_and_one_exit_code() {
    let dir = TempDir::new("errors");
    let input = dir.join("in.csv");
    std::fs::write(&input, "a,b\n1,2\n").unwrap();

    let run = |args: &[&str]| -> (Option<i32>, String) {
        let out = Command::new(bin()).args(args).output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };

    // Usage problems: exit 2, `error:` prefix, no `✗`.
    let (code, err) = run(&["filter", input.to_str().unwrap()]);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.trim_start().starts_with("error:"), "{err}");

    let (code, err) = run(&["sort", input.to_str().unwrap()]);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.trim_start().starts_with("error:"), "{err}");

    // Runtime problems: exit 1, `✗ <command>:` prefix.
    let (code, err) = run(&["filter", input.to_str().unwrap(), "--where", "nope=1"]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("✗ filter:"), "{err}");

    let (code, err) = run(&["groupby", input.to_str().unwrap()]);
    assert_eq!(code, Some(2), "usage error expected: {err}");

    let (code, err) = run(&["search", input.to_str().unwrap(), "x", "--columns", "nope"]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("✗ search:"), "{err}");

    // A missing file is a runtime failure, and it comes with a hint.
    let (code, err) = run(&["preview", "/definitely/not/here.csv"]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("✗ preview:"), "{err}");
    assert!(err.contains("hint:"), "{err}");

    // The reporter's engine errors use the same shape (via a UTF-16 refusal).
    let utf16 = dir.join("wide.csv");
    std::fs::write(&utf16, b"\xff\xfei\x00d\x00\n\x00").unwrap();
    let archive = dir.join("o.tar");
    let (code, err) = run(&[
        "split",
        utf16.to_str().unwrap(),
        "-n",
        "2",
        "-o",
        archive.to_str().unwrap(),
    ]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("✗ split:"), "{err}");
    assert!(err.contains("hint:"), "{err}");
    // ...and it must be printed once, not twice.
    assert_eq!(
        err.matches("✗ split:").count(),
        1,
        "duplicated error: {err}"
    );
}

#[test]
fn sort_and_partition_verify_their_claims() {
    let dir = TempDir::new("verify2");
    let input = dir.join("in.csv");
    let mut csv = String::from("city,amount\n");
    for (city, amount) in [("CDMX", 10), ("Puebla", 500), ("CDMX", 20), ("Tijuana", 5)] {
        csv.push_str(&format!("{city},{amount}\n"));
    }
    std::fs::write(&input, csv).unwrap();

    // sort: same rows, same columns, just reordered.
    let sorted = dir.join("sorted.csv");
    let status = Command::new(bin())
        .args([
            "sort",
            input.to_str().unwrap(),
            "--by",
            "amount",
            "-o",
            sorted.to_str().unwrap(),
            "--verify",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    // partition: every source row is in exactly one partition — including when
    // the members are themselves compressed.
    for (tag, archive) in [("raw", dir.join("raw.tar")), ("zst", dir.join("zst.tar"))] {
        let mut cmd = Command::new(bin());
        cmd.args([
            "partition",
            input.to_str().unwrap(),
            "--by",
            "city",
            "-o",
            archive.to_str().unwrap(),
            "--verify",
        ]);
        if tag == "zst" {
            cmd.args(["-f", "zst"]);
        }
        let status = cmd.status().unwrap();
        assert_eq!(status.code(), Some(0), "{tag} partition verify failed");
        assert!(archive.exists(), "{tag} archive missing");
    }
}

/// Spreadsheet-style editing: the base file is never touched, edits live in a sidecar journal,
/// and exporting applies them.
#[test]
fn edit_journal_never_touches_the_base_file() {
    let dir = TempDir::new("edit");
    let input = dir.join("orders.csv");
    let original = "id,city,amount\n1,CDMX,10\n2,Puebla,20\n3,CDMX,30\n4,Tijuana,40\n";
    std::fs::write(&input, original).unwrap();

    // set → insert → delete, each recorded in the journal.
    for args in [
        vec!["set", "--row", "1", "--col", "city", "--value", "Monterrey"],
        vec!["insert", "--row", "2", "--value", "9,New,50"],
        vec!["delete", "--row", "4", "--count", "1"],
    ] {
        let mut cmd = Command::new(bin());
        cmd.arg("edit").arg(args[0]);
        cmd.arg(input.to_str().unwrap());
        cmd.args(&args[1..]);
        let status = cmd.status().unwrap();
        assert_eq!(status.code(), Some(0), "edit {} failed", args[0]);
    }

    // The source is byte-identical; the edits are in the sidecar.
    assert_eq!(std::fs::read_to_string(&input).unwrap(), original);
    let journal = dir.join("orders.csv.journal");
    assert!(
        journal.exists(),
        "journal should be written next to the data"
    );
    let text = std::fs::read_to_string(&journal).unwrap();
    assert_eq!(text.lines().count(), 3, "{text}");

    // Export applies them.
    let out = dir.join("fixed.csv");
    let status = Command::new(bin())
        .args([
            "edit",
            "export",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let fixed = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = fixed.lines().collect();
    assert_eq!(lines[0], "id,city,amount");
    assert_eq!(lines[2], "2,Monterrey,20", "{fixed}");
    assert_eq!(lines[3], "9,New,50", "{fixed}");
    assert_eq!(lines.len(), 5, "one row deleted: {fixed}");

    // Undo removes the last edit (the delete), and status reflects it.
    let status = Command::new(bin())
        .args(["edit", "undo", input.to_str().unwrap()])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let out2 = dir.join("fixed2.csv");
    let status = Command::new(bin())
        .args([
            "edit",
            "export",
            input.to_str().unwrap(),
            "-o",
            out2.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&out2).unwrap().lines().count(),
        6,
        "delete undone"
    );
}

/// A pivot turns one key into rows and another into columns.
#[test]
fn pivot_produces_a_matrix_and_refuses_to_explode() {
    let dir = TempDir::new("pivot");
    let input = dir.join("sales.csv");
    std::fs::write(
        &input,
        "city,month,amount\nCDMX,Jan,10\nCDMX,Feb,20\nPuebla,Jan,30\n",
    )
    .unwrap();

    let out = dir.join("m.csv");
    let status = Command::new(bin())
        .args([
            "pivot",
            input.to_str().unwrap(),
            "--rows",
            "city",
            "--cols",
            "month",
            "--values",
            "amount:sum",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().next().unwrap(), "city,Feb,Jan");
    assert_eq!(text.lines().nth(1).unwrap(), "CDMX,20,10");
    assert_eq!(text.lines().nth(2).unwrap(), "Puebla,0,30");

    // A column key with more distinct values than the cap is a clear failure,
    // not an out-of-memory.
    let wide = dir.join("wide.csv");
    let mut csv = String::from("id,k,v\n");
    for i in 0..600 {
        csv.push_str(&format!("{i},k{i},1\n"));
    }
    std::fs::write(&wide, csv).unwrap();
    let status = Command::new(bin())
        .args([
            "pivot",
            wide.to_str().unwrap(),
            "--rows",
            "id",
            "--cols",
            "k",
            "--values",
            "v:sum",
            "--max-columns",
            "100",
            "-o",
            dir.join("w.csv").to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1), "a wide column key must fail");
}

/// Conditional formatting: the server says which cells match, the caller paints.
#[test]
fn format_reports_matching_cells_and_a_summary() {
    let dir = TempDir::new("format");
    let input = dir.join("t.csv");
    std::fs::write(
        &input,
        "city,amount,status\nCDMX,1200,ok\nPuebla,900,late\nTijuana,30,ok\n",
    )
    .unwrap();

    let status = Command::new(bin())
        .args([
            "format",
            input.to_str().unwrap(),
            "--rule",
            "amount>1000:bg=red",
            "--rule",
            "status=late:fg=orange",
            "--json",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let status = Command::new(bin())
        .args([
            "format",
            input.to_str().unwrap(),
            "--rule",
            "amount>1000:bg=red",
            "--summary",
        ])
        .output()
        .unwrap();
    assert_eq!(status.status.code(), Some(0));
    let text = String::from_utf8_lossy(&status.stdout);
    // Whole-file census, so a too-broad rule is visible before anyone sees colour.
    assert!(text.contains("3 rows scanned"), "{text}");
    assert!(text.contains("amount>1000:bg=red"), "{text}");

    // A malformed rule is a usage error, not a silent no-op.
    let out = Command::new(bin())
        .args(["format", input.to_str().unwrap(), "--rule", "amount>1000"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// Chart data: categories, one array per series, nulls for missing points.
#[test]
fn chart_emits_json_with_categories_and_series() {
    let dir = TempDir::new("chart");
    let input = dir.join("s.csv");
    std::fs::write(
        &input,
        "month,city,amount\nJan,CDMX,10\nFeb,CDMX,20\nJan,Puebla,30\nFeb,Puebla,40\nJan,CDMX,5\n",
    )
    .unwrap();

    let out = Command::new(bin())
        .args([
            "chart",
            input.to_str().unwrap(),
            "--x",
            "month",
            "--y",
            "amount:sum",
            "--series",
            "city",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#""categories":["Jan","Feb"]"#), "{stdout}");
    assert!(
        stdout.contains(r#""name":"CDMX","points":[15,20]"#),
        "{stdout}"
    );
    assert!(
        stdout.contains(r#""name":"Puebla","points":[30,40]"#),
        "{stdout}"
    );

    // A gap in one series stays null rather than becoming a fake zero.
    let sparse = dir.join("sparse.csv");
    std::fs::write(&sparse, "month,city,amount\nJan,A,1\nFeb,B,2\n").unwrap();
    let out = Command::new(bin())
        .args([
            "chart",
            sparse.to_str().unwrap(),
            "--x",
            "month",
            "--y",
            "amount:sum",
            "--series",
            "city",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("null"), "{stdout}");
}

/// Display metadata lives in its own sidecar and never touches cell values.
#[test]
fn layout_sidecar_does_not_change_the_data() {
    let dir = TempDir::new("layout");
    let input = dir.join("t.csv");
    let original = "city,amount\nCDMX,1234.5\n";
    std::fs::write(&input, original).unwrap();

    let status = Command::new(bin())
        .args([
            "layout",
            input.to_str().unwrap(),
            "--set",
            "amount:format=#,##0.00",
            "--set",
            "city:width=160",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    // The CSV is byte-identical; only the view sidecar appeared.
    assert_eq!(std::fs::read_to_string(&input).unwrap(), original);
    assert!(dir.join("t.csv.view").exists());

    let out = Command::new(bin())
        .args(["layout", input.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"format\":\"#,##0.00\""), "{stdout}");
    assert!(stdout.contains(r#""width":160"#), "{stdout}");

    // A bad property is a usage error.
    let out = Command::new(bin())
        .args([
            "layout",
            input.to_str().unwrap(),
            "--set",
            "amount:colour=red",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// Read-only commands say when they are ignoring recorded edits, instead of
/// quietly answering with pre-edit bytes.
#[test]
fn read_commands_warn_about_pending_edits() {
    let dir = TempDir::new("warn");
    let input = dir.join("t.csv");
    std::fs::write(&input, "city,amount\nCDMX,10\n").unwrap();

    let before = Command::new(bin())
        .args(["filter", input.to_str().unwrap(), "--where", "city=CDMX"])
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&before.stderr).contains("pending edit"),
        "no warning without a journal"
    );

    let status = Command::new(bin())
        .args([
            "edit",
            "set",
            input.to_str().unwrap(),
            "--row",
            "0",
            "--col",
            "amount",
            "--value",
            "999",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let after = Command::new(bin())
        .args(["filter", input.to_str().unwrap(), "--where", "city=CDMX"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&after.stderr);
    assert!(stderr.contains("pending edit"), "{stderr}");
    assert!(
        stderr.contains("edit export"),
        "the warning names the fix: {stderr}"
    );
    // Exactly one line, not one per code path.
    assert_eq!(stderr.matches("pending edit").count(), 1, "{stderr}");

    // `edit export` is the documented escape hatch, and it works.
    let out = dir.join("fixed.csv");
    let status = Command::new(bin())
        .args([
            "edit",
            "export",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(std::fs::read_to_string(&out).unwrap().contains("999"));
}

/// Cross-table lookup: reference loaded, main file streamed, misses visible.
#[test]
fn lookup_enriches_rows_and_marks_misses() {
    let dir = TempDir::new("lookup");
    let main = dir.join("orders.csv");
    let reference = dir.join("customers.csv");
    std::fs::write(&main, "order,cust_no,amount\nA,1,100\nB,2,50\nC,9,10\n").unwrap();
    std::fs::write(&reference, "cust_no,name,city\n1,Ana,CDMX\n2,Beto,Puebla\n").unwrap();

    let out = dir.join("joined.csv");
    let status = Command::new(bin())
        .args([
            "lookup",
            main.to_str().unwrap(),
            "--key",
            "cust_no",
            "--from",
            reference.to_str().unwrap(),
            "--get",
            "name,city",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        text.lines().next().unwrap(),
        "order,cust_no,amount,name,city"
    );
    assert_eq!(text.lines().nth(1).unwrap(), "A,1,100,Ana,CDMX");
    // A miss is #N/A, not blank: blank would look like real data.
    assert_eq!(text.lines().nth(3).unwrap(), "C,9,10,#N/A,#N/A");

    // Missing required flags are usage errors.
    let out = Command::new(bin())
        .args(["lookup", main.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--key"), "{stderr}");
}

/// A formula column may read another sheet in the same directory.
#[test]
fn formulas_can_reference_another_sheet() {
    let dir = TempDir::new("sheetref");
    let orders = dir.join("orders.csv");
    let customers = dir.join("customers.csv");
    std::fs::write(&orders, "order,cust_no\nA,1\nB,9\n").unwrap();
    std::fs::write(&customers, "cust_no,name\n1,Ana\n").unwrap();

    let status = Command::new(bin())
        .args([
            "edit",
            "formula",
            orders.to_str().unwrap(),
            "--name",
            "customer",
            "--value",
            r#"ref("customers.csv","cust_no","name")"#,
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let out = dir.join("joined.csv");
    let status = Command::new(bin())
        .args([
            "edit",
            "export",
            orders.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().next().unwrap(), "order,cust_no,customer");
    assert_eq!(text.lines().nth(1).unwrap(), "A,1,Ana");
    assert_eq!(text.lines().nth(2).unwrap(), "B,9,#N/A");
}

/// A bulk edit is one journal entry however many rows it changes.
#[test]
fn bulk_edit_matches_a_filter_and_stays_one_operation() {
    let dir = TempDir::new("bulk");
    let input = dir.join("t.csv");
    std::fs::write(
        &input,
        "id,city,amount\n1,CDMX,10\n2,Puebla,20\n3,CDMX,30\n",
    )
    .unwrap();

    let status = Command::new(bin())
        .args([
            "edit",
            "set",
            input.to_str().unwrap(),
            "--where",
            "city=CDMX",
            "--col",
            "amount",
            "--value",
            "0",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    // Two rows changed, one operation, tiny journal.
    let journal = std::fs::read_to_string(dir.join("t.csv.journal")).unwrap();
    assert_eq!(journal.lines().count(), 1, "{journal}");
    assert!(journal.len() < 64, "journal stayed small: {journal}");

    let out = dir.join("out.csv");
    let status = Command::new(bin())
        .args([
            "edit",
            "export",
            input.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text, "id,city,amount\n1,CDMX,0\n2,Puebla,20\n3,CDMX,0\n");

    // row and where together is a usage error, not a guess.
    let out = Command::new(bin())
        .args([
            "edit",
            "set",
            input.to_str().unwrap(),
            "--row",
            "1",
            "--where",
            "city=CDMX",
            "--col",
            "amount",
            "--value",
            "5",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
