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
    // 1-based rows: `set --row 1` is the first data row, `insert --row 2` puts the
    // new row before what was the second row, `delete --row 4` removes the row
    // that sits at position 4 after the insert.
    assert_eq!(lines[0], "id,city,amount");
    assert_eq!(lines[1], "1,Monterrey,10", "{fixed}");
    assert_eq!(lines[2], "9,New,50", "{fixed}");
    assert_eq!(lines[3], "2,Puebla,20", "{fixed}");
    assert_eq!(lines[4], "4,Tijuana,40", "{fixed}");
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
            "--json",
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
            "--json",
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
            "1",
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

/// A column can be inserted at a chosen position, as a constant or a formula.
#[test]
fn insert_column_places_it_where_asked() {
    let dir = TempDir::new("insertcol");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,city,amount\n1,CDMX,10\n2,Puebla,20\n").unwrap();

    for args in [
        vec!["--after", "city", "--name", "note", "--value", "manual"],
        vec![
            "--after",
            "id",
            "--name",
            "double",
            "--formula",
            "amount * 2",
        ],
    ] {
        let mut cmd = Command::new(bin());
        cmd.args(["edit", "insert-column", input.to_str().unwrap()]);
        cmd.args(&args);
        let status = cmd.status().unwrap();
        assert_eq!(status.code(), Some(0), "{args:?}");
    }

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
    assert_eq!(text.lines().next().unwrap(), "id,double,city,note,amount");
    assert_eq!(text.lines().nth(1).unwrap(), "1,20,CDMX,manual,10");

    // value and formula together is a usage error.
    let out = Command::new(bin())
        .args([
            "edit",
            "insert-column",
            input.to_str().unwrap(),
            "--name",
            "x",
            "--value",
            "a",
            "--formula",
            "b",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// Copy a rectangular block by A1-style range.
#[test]
fn copy_pastes_a_block_by_range() {
    let dir = TempDir::new("copy");
    let input = dir.join("t.csv");
    std::fs::write(
        &input,
        "id,city,amount\n1,CDMX,10\n2,Puebla,20\n3,Tijuana,30\n4,Monterrey,40\n",
    )
    .unwrap();

    let status = Command::new(bin())
        .args([
            "edit",
            "copy",
            input.to_str().unwrap(),
            "--from",
            "A1:B2",
            "--to",
            "B3",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

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
    let lines: Vec<&str> = text.lines().collect();
    // The 2×2 block from A1:B2 landed at B3.
    assert_eq!(lines[3], "3,1,CDMX", "{text}");
    assert_eq!(lines[4], "4,2,Puebla", "{text}");

    // A range beyond the sheet is refused with a usable message.
    let out = Command::new(bin())
        .args([
            "edit",
            "copy",
            input.to_str().unwrap(),
            "--from",
            "Z1:Z2",
            "--to",
            "A5",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("beyond the last column"), "{stderr}");
}

/// `row()` makes per-row values possible without storing anything.
#[test]
fn row_function_builds_a_sequence() {
    let dir = TempDir::new("rowfn");
    let input = dir.join("t.csv");
    std::fs::write(&input, "order,amount\nA,10\nB,20\nC,30\n").unwrap();

    for (name, expr) in [
        ("seq", r#"order & "-" & row()"#),
        ("parity", r#"if(row() % 2 = 1, "odd", "even")"#),
    ] {
        let status = Command::new(bin())
            .args([
                "edit",
                "formula",
                input.to_str().unwrap(),
                "--name",
                name,
                "--value",
                expr,
            ])
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(0), "{name}");
    }

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
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "order,amount,seq,parity");
    assert_eq!(lines[1], "A,10,A-1,odd");
    assert_eq!(lines[2], "B,20,B-2,even");
    assert_eq!(lines[3], "C,30,C-3,odd");
}

/// Filtering can order its output, and sorting takes several keys.
#[test]
fn filter_can_order_and_sort_takes_several_keys() {
    let dir = TempDir::new("order");
    let input = dir.join("t.csv");
    std::fs::write(
        &input,
        "city,amount\nCDMX,10\nPuebla,30\nCDMX,20\nTijuana,5\nCDMX,40\n",
    )
    .unwrap();

    let out = dir.join("filtered.csv");
    let status = Command::new(bin())
        .args([
            "filter",
            input.to_str().unwrap(),
            "--where",
            "amount>6",
            "--order",
            "city,amount:desc",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&out).unwrap();
    // Only rows above 6, ordered by city then amount descending.
    assert_eq!(
        text, "city,amount\nCDMX,40\nCDMX,20\nCDMX,10\nPuebla,30\n",
        "{text}"
    );

    // Multi-key sort on its own file.
    let sorted = dir.join("sorted.csv");
    let status = Command::new(bin())
        .args([
            "sort",
            input.to_str().unwrap(),
            "--by",
            "city,amount:desc",
            "-o",
            sorted.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&sorted).unwrap();
    assert_eq!(
        text, "city,amount\nCDMX,40\nCDMX,20\nCDMX,10\nPuebla,30\nTijuana,5\n",
        "{text}"
    );
}

/// Sampling trades accuracy for speed, and says so on the screen.
#[test]
fn analyze_sample_stops_early_and_labels_itself() {
    let dir = TempDir::new("sample");
    let input = dir.join("t.csv");
    let mut csv = String::from("city,amount\n");
    for i in 0..500 {
        csv.push_str(&format!("C{},{}\n", i % 5, i));
    }
    std::fs::write(&input, csv).unwrap();

    let full = Command::new(bin())
        .args(["analyze", input.to_str().unwrap()])
        .output()
        .unwrap();
    let full_out = String::from_utf8_lossy(&full.stdout);
    assert!(!full_out.contains("not a random sample"), "{full_out}");

    let sampled = Command::new(bin())
        .args(["analyze", input.to_str().unwrap(), "--sample", "100"])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&sampled.stdout);
    assert!(
        out.contains("not a random sample"),
        "the report must not look like a full count: {out}"
    );
    // The sampled run reports fewer rows than the full one.
    assert!(out.contains("100"), "{out}");
}

/// A backslash-escaped file parses wrong by default, warns, and can be fixed.
#[test]
fn quote_style_is_detected_and_can_be_overridden() {
    let dir = TempDir::new("quotes");
    let input = dir.join("t.csv");
    std::fs::write(&input, "name,note\nAna,\"has a \\\"quote\\\" inside\"\n").unwrap();

    let default = Command::new(bin())
        .args(["preview", input.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&default.stderr);
    assert!(stderr.contains("backslash"), "should warn: {stderr}");
    assert!(
        stderr.contains("--quotes backslash"),
        "should name the fix: {stderr}"
    );

    let fixed = Command::new(bin())
        .args(["preview", input.to_str().unwrap(), "--quotes", "backslash"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&fixed.stdout);
    assert!(
        stdout.contains("has a \\\"quote\\\" inside") || stdout.contains("has a \"quote\" inside"),
        "{stdout}"
    );

    // A plain RFC file must not warn.
    let plain = dir.join("plain.csv");
    std::fs::write(&plain, "a,b\n1,\"x \"\"y\"\" z\"\n").unwrap();
    let out = Command::new(bin())
        .args(["preview", plain.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("backslash"),
        "RFC file should not warn: {stderr}"
    );
}

/// `--quotes` changes what a filter matches, not just how it prints.
#[test]
fn filter_reads_the_quote_style_it_is_given() {
    let dir = TempDir::new("quotefilter");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,note\n1,\"a \\\"b,c\"\n").unwrap();

    // Read as RFC, the note field ends at the escaped quote, so it holds `a \b`
    // and no `c` — the row does not match.
    let out = dir.join("rfc.csv");
    let status = Command::new(bin())
        .args([
            "filter",
            input.to_str().unwrap(),
            "--where",
            "note~c",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&out).unwrap().lines().count(),
        1,
        "header only"
    );

    // Read with backslash escaping, note is `a "b,c` and the row matches.
    let out = dir.join("bs.csv");
    let status = Command::new(bin())
        .args([
            "filter",
            input.to_str().unwrap(),
            "--where",
            "note~c",
            "--quotes",
            "backslash",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(std::fs::read_to_string(&out).unwrap().lines().count(), 2);
}

/// A backslash file keeps its record boundaries: the row count is right, not just
/// the field count.
#[test]
fn backslash_files_keep_their_records() {
    let dir = TempDir::new("bsrecords");
    let input = dir.join("t.csv");
    std::fs::write(&input, "city,note\nCDMX,\"a \\\"b,c\"\nPuebla,plain\n").unwrap();

    let out = dir.join("fixed.csv");
    let status = Command::new(bin())
        .args([
            "edit",
            "export",
            input.to_str().unwrap(),
            "--quotes",
            "backslash",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "two data rows, not one merged record: {text}"
    );
    assert_eq!(lines[1], "CDMX,\"a \"\"b,c\"", "{text}");
    assert_eq!(lines[2], "Puebla,plain", "{text}");
}

/// A failing command must say why, on stderr, without the caller's help.
#[test]
fn every_failure_prints_a_reason() {
    let dir = TempDir::new("reasons");
    let input = dir.join("t.csv");
    std::fs::write(&input, "city,amount\nCDMX,10\n").unwrap();

    // Engine-returned errors used to exit 1 silently (the reporter only prints
    // errors that arrive through the progress channel).
    let cases: Vec<Vec<&str>> = vec![
        vec![
            "pivot",
            input.to_str().unwrap(),
            "--rows",
            "nope",
            "--values",
            "amount:sum",
        ],
        vec![
            "groupby",
            input.to_str().unwrap(),
            "--by",
            "nope",
            "--agg",
            "amount:sum",
        ],
        vec![
            "chart",
            input.to_str().unwrap(),
            "--x",
            "nope",
            "--y",
            "amount:sum",
        ],
        vec![
            "lookup",
            input.to_str().unwrap(),
            "--key",
            "nope",
            "--from",
            input.to_str().unwrap(),
            "--get",
            "amount",
        ],
        vec!["columns", input.to_str().unwrap(), "--drop", "nope"],
    ];
    for args in cases {
        let out = Command::new(bin()).args(&args).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?} should fail");
        assert!(
            stderr.contains('✗'),
            "{args:?} failed silently: stderr={stderr:?}"
        );
        assert!(
            stderr.contains("nope"),
            "{args:?} should name the column: {stderr}"
        );
    }
}

/// Every sub-command that needs arguments says so when called without them.
///
/// A command that exits non-zero and prints nothing leaves the user with no way
/// forward except guessing. This checks the whole surface at once.
#[test]
fn usage_errors_are_never_silent() {
    // `demo` writes a sample to the current directory, so it is deliberately
    // argument-free; `--help` and `serve` are excluded for the same reason.
    let exempt = ["demo", "serve"];
    let help = Command::new(bin()).arg("--help").output().unwrap();
    let help_text = String::from_utf8_lossy(&help.stdout);
    let commands: Vec<&str> = help_text
        .lines()
        .skip_while(|line| !line.starts_with("COMMANDS"))
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .filter_map(|line| line.split_whitespace().next())
        .filter(|word| word.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
        .collect();
    assert!(
        commands.len() > 15,
        "expected the help to list commands: {commands:?}"
    );

    let dir = TempDir::new("usage");
    let mut silent = Vec::new();
    for command in commands {
        if exempt.contains(&command) {
            continue;
        }
        let out = Command::new(bin())
            .arg(command)
            .current_dir(&dir.0)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status.code() != Some(0) && stderr.trim().is_empty() {
            silent.push(command.to_string());
        }
    }
    assert!(
        silent.is_empty(),
        "these commands fail without saying why: {silent:?}"
    );
}

/// `preview` must not read the whole file to show a window.
///
/// This is the regression that made "memory does not grow with file size" false:
/// the quote-style detector used read_to_end and pulled the entire file into RAM.
/// The check is relative — a 10× bigger file must not take 10× longer — because
/// wall-clock alone is noise when the test suite runs in parallel.
#[test]
fn a_window_read_does_not_scale_with_the_file() {
    let dir = TempDir::new("windowread");
    // 6 MB and 60 MB of identical rows.
    let make = |name: &str, target: usize| {
        let path = dir.join(name);
        let mut text = String::from("id,city,amount\n");
        while text.len() < target {
            text.push_str("1,CDMX,10\n");
        }
        std::fs::write(&path, text).unwrap();
        path
    };
    let small = make("small.csv", 6 * 1024 * 1024);
    let big = make("big.csv", 60 * 1024 * 1024);

    let time = |path: &std::path::Path| {
        let start = std::time::Instant::now();
        let out = Command::new(bin())
            .args(["preview", path.to_str().unwrap(), "--rows", "5"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
        start.elapsed().as_millis()
    };

    // Warm the binary and the page cache so the comparison is fair.
    time(&small);
    let small_ms = time(&small);
    let big_ms = time(&big);
    assert!(
        big_ms < small_ms * 4 + 200,
        "a 10x bigger file took {big_ms} ms against {small_ms} ms, which suggests a full read"
    );
}

/// Undo takes a step back; redo puts it back; a new edit drops the redo branch.
#[test]
fn undo_and_redo_are_symmetric() {
    let dir = TempDir::new("redocl");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,city\n1,x\n2,y\n").unwrap();

    let run = |args: &[&str]| {
        let out = Command::new(bin()).args(args).output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let path = input.to_str().unwrap();
    run(&[
        "edit", "set", path, "--row", "1", "--col", "city", "--value", "a",
    ]);
    run(&[
        "edit", "set", path, "--row", "2", "--col", "city", "--value", "b",
    ]);

    let export = |name: &str| {
        let out = dir.join(name);
        run(&["edit", "export", path, "-o", out.to_str().unwrap()]);
        std::fs::read_to_string(&out).unwrap()
    };
    assert_eq!(export("both.csv"), "id,city\n1,a\n2,b\n");

    let (code, stdout, _) = run(&["edit", "undo", path]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("can be redone"), "{stdout}");
    assert_eq!(export("undone.csv"), "id,city\n1,a\n2,y\n");

    let (code, stdout, _) = run(&["edit", "redo", path]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("redone"), "{stdout}");
    assert_eq!(export("redone.csv"), "id,city\n1,a\n2,b\n");

    // Undo once more, then make a different edit: redo is no longer available.
    run(&["edit", "undo", path]);
    run(&[
        "edit", "set", path, "--row", "2", "--col", "city", "--value", "c",
    ]);
    let (code, _, stderr) = run(&["edit", "redo", path]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("nothing to redo"), "{stderr}");
}

/// Rows are 1-based, like A1 references and like a spreadsheet.
#[test]
fn rows_are_one_based_for_the_user() {
    let dir = TempDir::new("onebased");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,city\n1,x\n2,y\n").unwrap();
    let path = input.to_str().unwrap();

    let status = Command::new(bin())
        .args([
            "edit", "set", path, "--row", "1", "--col", "city", "--value", "a",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let out = dir.join("o.csv");
    Command::new(bin())
        .args(["edit", "export", path, "-o", out.to_str().unwrap()])
        .status()
        .unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("1,a"), "row 1 is the first data row: {text}");

    // Row 0 is refused rather than quietly meaning "the first row".
    let out = Command::new(bin())
        .args([
            "edit", "set", path, "--row", "0", "--col", "city", "--value", "q",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("1-based"), "{stderr}");
}

/// Regex replacement: one operation cleans a whole column at export time.
#[test]
fn replace_takes_a_regex() {
    let dir = TempDir::new("regex");
    let input = dir.join("t.csv");
    std::fs::write(
        &input,
        "id,note\n1,\"too   many    spaces\"\n2,\"a1b22c\"\n",
    )
    .unwrap();
    let path = input.to_str().unwrap();

    let status = Command::new(bin())
        .args([
            "edit", "replace", path, "--from", "[ ]+", "--to", " ", "--regex", "--col", "note",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let status = Command::new(bin())
        .args([
            "edit", "replace", path, "--from", "[0-9]", "--to", "#", "--regex", "--col", "note",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let out = dir.join("o.csv");
    Command::new(bin())
        .args(["edit", "export", path, "-o", out.to_str().unwrap()])
        .status()
        .unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.contains("too many spaces"), "{text}");
    assert!(text.contains("a#b##c"), "{text}");

    // A pattern outside the supported subset is refused before it is recorded.
    let out = Command::new(bin())
        .args([
            "edit", "replace", path, "--from", "[", "--to", "x", "--regex",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not a supported pattern"), "{stderr}");
}

/// A1 references work the way a filled-down spreadsheet formula does.
#[test]
fn formula_accepts_a1_references() {
    let dir = TempDir::new("a1refs");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,amount,qty\n1,10,3\n2,20,4\n3,30,5\n").unwrap();
    let path = input.to_str().unwrap();

    let status = Command::new(bin())
        .args([
            "edit", "formula", path, "--name", "total", "--value", "B2 * C2",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let out = dir.join("o.csv");
    Command::new(bin())
        .args(["edit", "export", path, "-o", out.to_str().unwrap()])
        .status()
        .unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "id,amount,qty,total");
    assert_eq!(lines[1], "1,10,3,30", "{text}");
    assert_eq!(lines[2], "2,20,4,80", "{text}");
    assert_eq!(lines[3], "3,30,5,150", "{text}");
}

/// A formula can look at the row before it, and the cost does not grow with rows.
#[test]
fn formulas_can_look_at_the_previous_row() {
    let dir = TempDir::new("lag");
    let input = dir.join("t.csv");
    std::fs::write(&input, "day,amount\n1,10\n2,20\n3,30\n4,40\n").unwrap();
    let path = input.to_str().unwrap();

    for (name, formula) in [
        ("prev", "lag(amount)"),
        ("delta", "amount - lag(amount)"),
        ("running", "running_total(amount)"),
    ] {
        let status = Command::new(bin())
            .args(["edit", "formula", path, "--name", name, "--value", formula])
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(0), "{name}");
    }

    let out = dir.join("o.csv");
    Command::new(bin())
        .args(["edit", "export", path, "-o", out.to_str().unwrap()])
        .status()
        .unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "day,amount,prev,delta,running");
    // The first row has no previous value, and the total includes the current row.
    assert_eq!(lines[1], "1,10,#VALUE!,#VALUE!,10", "{text}");
    assert_eq!(lines[2], "2,20,10,10,30", "{text}");
    assert_eq!(lines[3], "3,30,20,10,60", "{text}");
    assert_eq!(lines[4], "4,40,30,10,100", "{text}");
}

/// `view` shows the edited sheet, filters it, and pages it.
#[test]
fn view_applies_edits_and_filters() {
    let dir = TempDir::new("viewcmd");
    let input = dir.join("t.csv");
    std::fs::write(
        &input,
        "id,city,amount\n1,CDMX,10\n2,Puebla,20\n3,CDMX,30\n4,Tijuana,40\n",
    )
    .unwrap();
    let path = input.to_str().unwrap();

    // No edits yet: the first page.
    let out = Command::new(bin())
        .args(["view", path, "--count", "2"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    // The columns are padded, so check the header names and the values separately.
    assert!(text.contains("id"), "{text}");
    assert!(text.contains("city"), "{text}");
    assert!(text.contains("amount"), "{text}");
    assert!(text.contains("CDMX"), "{text}");
    assert!(text.contains("10"), "{text}");

    // Edit one row, then look again: the value must be the edited one.
    Command::new(bin())
        .args([
            "edit", "set", path, "--row", "2", "--col", "amount", "--value", "999",
        ])
        .status()
        .unwrap();
    let out = Command::new(bin())
        .args(["view", path, "--where", "city=Puebla"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Puebla"), "{text}");
    assert!(text.contains("999"), "edits must be visible: {text}");
    assert!(text.contains("1 edit(s) applied"), "{text}");

    // Paging past the end says so, and says how far the file goes, rather than failing.
    let out = Command::new(bin())
        .args(["view", path, "--start", "10", "--count", "5"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("past the end"), "{text}");
    assert!(text.contains("4 data rows"), "{text}");

    // JSON is machine-readable.
    let out = Command::new(bin())
        .args(["view", path, "--count", "1", "--json"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with(r#"{"columns":["id","city","amount"]"#),
        "{text}"
    );
}

/// Tables line up even with CJK column values, and `--order` pages sorted rows.
#[test]
fn view_aligns_columns_and_can_order() {
    let dir = TempDir::new("vieworder");
    let input = dir.join("t.csv");
    std::fs::write(&input, "城市,金额\nCDMX,10\nPuebla,30\n蒂华纳,20\n").unwrap();
    let path = input.to_str().unwrap();

    let out = Command::new(bin())
        .args(["view", path, "--count", "3"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    // 城市 is two terminal columns per character, so the separator sits three cells
    // after "CDMX" rather than one.
    let header = text.lines().next().unwrap_or("");
    assert!(header.contains("城市"), "{header}");
    // Every row's first cell must occupy the same number of terminal columns.
    let mut widths = Vec::new();
    for line in text.lines().filter(|l| l.contains(" | ")) {
        let first = line.split(" | ").next().unwrap();
        let columns: usize = first
            .chars()
            .map(|c| if c as u32 >= 0x2E80 { 2 } else { 1 })
            .sum();
        widths.push((first.to_string(), columns));
    }
    let expected = widths.first().map(|(_, w)| *w).unwrap_or(0);
    for (cell, width) in &widths {
        assert_eq!(
            *width, expected,
            "'{cell}' is {width} columns, expected {expected}"
        );
    }

    // Ordered view: the smallest amount comes first, and the page is sorted too.
    let out = Command::new(bin())
        .args(["view", path, "--order", "金额", "--count", "2"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let rows: Vec<&str> = text.lines().skip(2).collect();
    assert!(rows[0].contains("CDMX"), "10 should be first: {text}");
    assert!(rows[1].contains("蒂华纳"), "20 should be second: {text}");
}

/// Paging deep into a file stays fast: the index is built once, not per page.
#[test]
fn view_pages_deep_without_rebuilding_the_sheet() {
    let dir = TempDir::new("deepview");
    let input = dir.join("big.csv");
    let mut text = String::from("id,city,amount\n");
    for i in 0..100_000 {
        text.push_str(&format!("{i},C{},{}\n", i % 7, i % 100));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    // First run builds the index; it must still answer a deep page quickly.
    let started = std::time::Instant::now();
    let out = Command::new(bin())
        .args(["view", path, "--start", "90000", "--count", "10"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("90000"), "should land on row 90000: {text}");
    assert!(text.contains("of 100000"), "{text}");
    // Well under a full scan; a rebuild-per-page version was seconds here.
    assert!(
        started.elapsed().as_secs() < 5,
        "deep paging took {:?}",
        started.elapsed()
    );
}

/// `view --find` centres on the match, so nobody has to copy a row number between
/// two commands.
#[test]
fn view_can_jump_to_a_match() {
    let dir = TempDir::new("viewfind");
    let input = dir.join("t.csv");
    let mut text = String::from("id,city\n");
    for i in 0..100 {
        let city = if i == 40 { "Zacatecas" } else { "CDMX" };
        text.push_str(&format!("{i},{city}\n"));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    let out = Command::new(bin())
        .args(["view", path, "--find", "Zacatecas", "--context", "2"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("Zacatecas"),
        "the match should be on screen: {text}"
    );
    assert!(
        text.contains("row 41 of 100"),
        "the match row is reported: {text}"
    );
    // The page is centred: two rows of context above it, so row 38 is visible.
    assert!(text.contains("38"), "context rows should show: {text}");

    // No match is a normal answer, not a failure.
    let out = Command::new(bin())
        .args(["view", path, "--find", "not-there"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("no row contains"), "{text}");
}

/// `--nth` walks through repeated matches without re-running the search.
#[test]
fn view_can_jump_to_the_second_match() {
    let dir = TempDir::new("viewnth");
    let input = dir.join("t.csv");
    let mut text = String::from("id,city\n");
    for i in 0..30 {
        let city = if i % 10 == 5 { "CDMX" } else { "Other" };
        text.push_str(&format!("{i},{city}\n"));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    let out = Command::new(bin())
        .args([
            "view",
            path,
            "--find",
            "CDMX",
            "--nth",
            "2",
            "--context",
            "1",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    // The second CDMX is row 16 (ids 5 and 15).
    assert!(text.contains("15"), "{text}");
    assert!(text.contains("(match 2)"), "should say which match: {text}");

    // Asking past the last match explains itself.
    let out = Command::new(bin())
        .args(["view", path, "--find", "CDMX", "--nth", "9"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("only 3 row(s)"), "{text}");

    // `--nth 0` is a usage error.
    let out = Command::new(bin())
        .args(["view", path, "--find", "CDMX", "--nth", "0"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// The input file is protected; overwriting anything else is announced.
#[test]
fn writing_over_the_input_is_refused() {
    let dir = TempDir::new("overwrite");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,city\n1,CDMX\n2,Puebla\n").unwrap();
    let path = input.to_str().unwrap();

    // `-o` pointing at the input is data loss waiting to happen: refuse it.
    let cases: Vec<Vec<&str>> = vec![
        vec!["filter", path, "--where", "city=CDMX", "-o", path],
        vec!["sort", path, "--by", "city", "-o", path],
        vec!["columns", path, "--keep", "id", "-o", path],
        vec!["edit", "export", path, "-o", path],
    ];
    for args in cases {
        let out = Command::new(bin()).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("refusing"), "{args:?} said: {stderr}");
    }
    // And the input survived every attempt.
    assert_eq!(
        std::fs::read_to_string(&input).unwrap(),
        "id,city\n1,CDMX\n2,Puebla\n"
    );

    // Writing over a different existing file is allowed, but said out loud.
    let out_path = dir.join("out.csv");
    std::fs::write(&out_path, "old\n").unwrap();
    let out = Command::new(bin())
        .args([
            "filter",
            path,
            "--where",
            "city=CDMX",
            "-o",
            out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("overwriting existing"), "{stderr}");
}

/// Minimal stored-entry zip writer: enough to exercise the extractor's path checks.
fn build_zip(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, body) in entries {
        let offset = out.len() as u32;
        let crc = crc32(body.as_bytes());
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0u16.to_le_bytes()); // flags
        out.extend_from_slice(&0u16.to_le_bytes()); // stored
        out.extend_from_slice(&0u16.to_le_bytes()); // time
        out.extend_from_slice(&0u16.to_le_bytes()); // date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(body.as_bytes());

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // made by
        central.extend_from_slice(&20u16.to_le_bytes()); // needed
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(body.len() as u32).to_le_bytes());
        central.extend_from_slice(&(body.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u32.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// An archive member named `../x` must not be written outside the output directory.
#[test]
fn extraction_refuses_to_escape_the_output_directory() {
    let dir = TempDir::new("zipslip");
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();

    // Build a zip with one good entry and one that points outside. The test crate
    // has no zip dependency, so the archive is written by hand: a stored (uncompressed)
    // entry is a local header, the data, then a central directory.
    let zip_path = dir.join("evil.zip");
    std::fs::write(
        &zip_path,
        build_zip(&[("good.txt", "ok"), ("../escaped.txt", "pwned")]),
    )
    .unwrap();

    let status = Command::new(bin())
        .args([
            "extract",
            zip_path.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1), "the archive should be refused");

    // Nothing escaped, and nothing was extracted either: validation runs first.
    assert!(
        !dir.join("escaped.txt").exists(),
        "zip slip escaped the output dir"
    );
    let entries: Vec<_> = std::fs::read_dir(&out).unwrap().collect();
    assert!(
        entries.is_empty(),
        "partial extraction should not be left behind"
    );
}

/// Splitting a column in place: one column becomes several, the rest is the last piece.
#[test]
fn a_column_can_be_split_into_several() {
    let dir = TempDir::new("splitcol");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,full\n1,\"Ana,Perez,Calle 1\"\n2,\"Solo\"\n").unwrap();
    let path = input.to_str().unwrap();

    let status = Command::new(bin())
        .args([
            "edit",
            "split-column",
            path,
            "--col",
            "full",
            "--by",
            ",",
            "--into",
            "first,last,addr",
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let out = dir.join("o.csv");
    Command::new(bin())
        .args(["edit", "export", path, "-o", out.to_str().unwrap()])
        .status()
        .unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "id,first,last,addr");
    // The last piece keeps the remainder, so an address containing commas survives.
    assert_eq!(lines[1], "1,Ana,Perez,Calle 1", "{text}");
    // Fewer pieces than columns leaves the rest empty rather than shifting values.
    assert_eq!(lines[2], "2,Solo,,", "{text}");
}

/// `edit export --where` keeps the filtered rows, edits included.
#[test]
fn export_can_be_filtered() {
    let dir = TempDir::new("filteredexport");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,city\n1,CDMX\n2,Puebla\n3,CDMX\n").unwrap();
    let path = input.to_str().unwrap();

    let out = dir.join("sel.csv");
    let status = Command::new(bin())
        .args([
            "edit",
            "export",
            path,
            "--where",
            "city=CDMX",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        text.lines().count(),
        3,
        "header + two matching rows: {text}"
    );

    // An edit that turns a Puebla row into CDMX makes it part of the export: the
    // predicate sees the finished row, not the raw bytes.
    Command::new(bin())
        .args([
            "edit", "set", path, "--row", "2", "--col", "city", "--value", "CDMX",
        ])
        .status()
        .unwrap();
    let out = dir.join("sel2.csv");
    Command::new(bin())
        .args([
            "edit",
            "export",
            path,
            "--where",
            "city=CDMX",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().count(), 4, "all three rows now match: {text}");
}

/// Conditional formatting is visible in the terminal, not only in JSON.
#[test]
fn view_marks_cells_that_match_a_formatting_rule() {
    let dir = TempDir::new("viewstyled");
    let input = dir.join("t.csv");
    std::fs::write(
        &input,
        "id,city,amount\n1,CDMX,10\n2,Puebla,2000\n3,CDMX,3000\n",
    )
    .unwrap();
    let path = input.to_str().unwrap();

    // Piped output has no colour, so a match is shown in brackets.
    let out = Command::new(bin())
        .args(["view", path, "--rule", "amount>1000:bg=red"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("[2000]"), "{text}");
    assert!(text.contains("[3000]"), "{text}");
    assert!(
        !text.contains("[10]"),
        "the untouched cell must stay plain: {text}"
    );
    assert!(text.contains("2 cell(s) matched"), "{text}");

    // A rule that matches nothing says nothing.
    let out = Command::new(bin())
        .args(["view", path, "--rule", "amount>99999:bg=red"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("matched the formatting"), "{text}");
}

/// A pivot with no `-o` prints the matrix instead of writing a file next to the input.
#[test]
fn pivot_prints_the_matrix_by_default() {
    let dir = TempDir::new("pivotprint");
    let input = dir.join("t.csv");
    std::fs::write(
        &input,
        "city,status,amount\nCDMX,paid,10\nCDMX,free,20\nPuebla,paid,30\n",
    )
    .unwrap();
    let path = input.to_str().unwrap();

    let out = Command::new(bin())
        .args([
            "pivot",
            path,
            "--rows",
            "city",
            "--cols",
            "status",
            "--values",
            "amount:sum",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("city"), "{text}");
    assert!(text.contains("paid"), "{text}");
    assert!(text.contains("Puebla"), "{text}");
    assert!(text.contains("2 row(s) × 2 column(s)"), "{text}");
    // Nothing was written next to the input.
    assert!(
        !dir.join("t.pivot.csv").exists(),
        "no file should be created"
    );
}

/// A chart in the terminal: bars you can read, and JSON when a machine asks.
#[test]
fn chart_draws_in_the_terminal() {
    let dir = TempDir::new("chartdraw");
    let input = dir.join("t.csv");
    std::fs::write(&input, "city,amount\nCDMX,10\nPuebla,30\nCDMX,5\n").unwrap();
    let path = input.to_str().unwrap();

    let out = Command::new(bin())
        .args(["chart", path, "--x", "city", "--y", "amount:sum"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Puebla"), "{text}");
    assert!(text.contains('█'), "bars should be drawn: {text}");
    assert!(text.contains("2 categories"), "{text}");

    // The machine-readable form is still available.
    let out = Command::new(bin())
        .args(["chart", path, "--x", "city", "--y", "amount:sum", "--json"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with('{'), "{text}");
    assert!(text.contains("\"points\""), "{text}");
}

/// A big file without an index is announced, and the index it builds is kept.
#[test]
fn the_first_page_says_it_is_building_an_index() {
    let dir = TempDir::new("indexnote");
    let large = dir.join("big.csv");
    let mut text = String::from("id,city\n");
    while text.len() < 60 * 1024 * 1024 {
        text.push_str("1,CDMX\n");
    }
    std::fs::write(&large, text).unwrap();
    let path = large.to_str().unwrap();
    let index = dir.join("big.csv.idx");
    assert!(!index.exists());

    let out = Command::new(bin())
        .args(["view", path, "--count", "5"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("building a row index"),
        "the wait should be explained: {stderr}"
    );

    // The index is written, so the next run is quiet and fast.
    assert!(index.exists(), "the built index should be saved");
    let out = Command::new(bin())
        .args(["view", path, "--count", "5"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("building a row index"), "{stderr}");
}

/// A backslash-escaped file survives `columns` and `search` when told about it.
#[test]
fn columns_and_search_read_the_quote_style() {
    let dir = TempDir::new("quotesmore");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,note\n1,\"a \\\"b,c\"\n").unwrap();
    let path = input.to_str().unwrap();

    let out = dir.join("cols.csv");
    let status = Command::new(bin())
        .args([
            "columns",
            path,
            "--keep",
            "note",
            "--quotes",
            "backslash",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&out).unwrap();
    // Re-written as RFC 4180, so the value is quoted and doubled.
    assert!(text.contains("\"a \"\"b,c\""), "{text}");

    // Search finds the value that only exists when read with backslash escaping.
    let status = Command::new(bin())
        .args(["search", path, "b,c", "--quotes", "backslash"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
}

/// A stack of edits and column-layout changes must land where a person expects.
#[test]
fn edits_survive_column_changes() {
    let dir = TempDir::new("combop");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,name,qty\n1,a,10\n2,b,20\n3,c,30\n").unwrap();
    let path = input.to_str().unwrap();

    let run = |args: &[&str]| {
        let status = Command::new(bin()).args(args).status().unwrap();
        assert_eq!(status.code(), Some(0), "{args:?}");
    };

    // Change a cell, then move columns around it.
    run(&[
        "edit", "set", path, "--row", "2", "--col", "qty", "--value", "99",
    ]);
    run(&[
        "edit",
        "insert-column",
        path,
        "--after",
        "id",
        "--name",
        "tag",
        "--value",
        "x",
    ]);
    run(&["edit", "drop", path, "--name", "name"]);
    run(&["edit", "rename", path, "--from", "qty", "--to", "amount"]);

    let out = dir.join("o.csv");
    run(&["edit", "export", path, "-o", out.to_str().unwrap()]);
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "id,tag,amount");
    assert_eq!(lines[1], "1,x,10");
    // The edit follows the column through both the drop and the rename.
    assert_eq!(lines[2], "2,x,99", "{text}");
    assert_eq!(lines[3], "3,x,30");

    // A formula written before the rename keeps pointing at the column.
    let formula_file = dir.join("f.csv");
    std::fs::write(&formula_file, "id,qty\n1,10\n").unwrap();
    run(&[
        "edit",
        "formula",
        formula_file.to_str().unwrap(),
        "--name",
        "total",
        "--value",
        "qty*2",
    ]);
    run(&[
        "edit",
        "rename",
        formula_file.to_str().unwrap(),
        "--from",
        "qty",
        "--to",
        "amount",
    ]);
    let fout = dir.join("f_out.csv");
    run(&[
        "edit",
        "export",
        formula_file.to_str().unwrap(),
        "-o",
        fout.to_str().unwrap(),
    ]);
    let text = std::fs::read_to_string(&fout).unwrap();
    assert_eq!(text.lines().next().unwrap(), "id,amount,total");
    assert_eq!(text.lines().nth(1).unwrap(), "1,10,20", "{text}");
}

/// Editing one split piece must not erase its siblings.
#[test]
fn editing_a_split_piece_keeps_the_other_pieces() {
    let dir = TempDir::new("splitedit");
    let input = dir.join("t.csv");
    std::fs::write(
        &input,
        "id,full\n1,\"Ana,Perez,St 1\"\n2,\"Beto,Calle 2\"\n",
    )
    .unwrap();
    let path = input.to_str().unwrap();

    let run = |args: &[&str]| {
        assert_eq!(
            Command::new(bin()).args(args).status().unwrap().code(),
            Some(0),
            "{args:?}"
        );
    };

    run(&[
        "edit",
        "split-column",
        path,
        "--col",
        "full",
        "--by",
        ",",
        "--into",
        "first,last,addr",
    ]);
    run(&[
        "edit", "set", path, "--row", "1", "--col", "first", "--value", "Z",
    ]);

    let out = dir.join("o.csv");
    run(&["edit", "export", path, "-o", out.to_str().unwrap()]);
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "id,first,last,addr");
    // The edit lands on one piece; the neighbours still come from the base text.
    assert_eq!(lines[1], "1,Z,Perez,St 1", "{text}");
    assert_eq!(lines[2], "2,Beto,Calle 2,", "{text}");
}

/// A derived value must be computed from the *current* row, edits included.
///
/// This family produced three bugs in a row (split pieces, renames, split edits), so the
/// semantics are pinned here rather than rediscovered: whatever reads a cell — a
/// formula, `lag`, `running_total` — sees the edited value, not the base file's.
#[test]
fn derived_values_see_edited_values() {
    let dir = TempDir::new("derived");
    let run = |args: &[&str]| {
        assert_eq!(
            Command::new(bin()).args(args).status().unwrap().code(),
            Some(0),
            "{args:?}"
        );
    };
    let export = |path: &std::path::Path, name: &str| {
        let out = path.with_file_name(name);
        run(&[
            "edit",
            "export",
            path.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ]);
        std::fs::read_to_string(&out).unwrap()
    };

    // A formula reads an edited cell.
    let plain = dir.join("plain.csv");
    std::fs::write(&plain, "id,qty\n1,10\n2,20\n").unwrap();
    run(&[
        "edit",
        "set",
        plain.to_str().unwrap(),
        "--row",
        "2",
        "--col",
        "qty",
        "--value",
        "99",
    ]);
    run(&[
        "edit",
        "formula",
        plain.to_str().unwrap(),
        "--name",
        "total",
        "--value",
        "qty*2",
    ]);
    let text = export(&plain, "plain_out.csv");
    assert!(text.contains("2,99,198"), "{text}");

    // `lag` and `running_total` read the edited row, not the one on disk.
    let hist = dir.join("hist.csv");
    std::fs::write(&hist, "id,qty\n1,10\n2,20\n3,30\n").unwrap();
    run(&[
        "edit",
        "set",
        hist.to_str().unwrap(),
        "--row",
        "1",
        "--col",
        "qty",
        "--value",
        "100",
    ]);
    run(&[
        "edit",
        "formula",
        hist.to_str().unwrap(),
        "--name",
        "prev",
        "--value",
        "lag(qty)",
    ]);
    run(&[
        "edit",
        "formula",
        hist.to_str().unwrap(),
        "--name",
        "cum",
        "--value",
        "running_total(qty)",
    ]);
    let text = export(&hist, "hist_out.csv");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "id,qty,prev,cum");
    assert_eq!(lines[1], "1,100,#VALUE!,100", "{text}");
    // The previous row's edited value, and a running total that includes it.
    assert_eq!(lines[2], "2,20,100,120", "{text}");
    assert_eq!(lines[3], "3,30,20,150", "{text}");

    // A formula reading a split piece sees the piece, edits included.
    let split = dir.join("split.csv");
    std::fs::write(&split, "id,full\n1,\"Ana,Perez\"\n").unwrap();
    run(&[
        "edit",
        "split-column",
        split.to_str().unwrap(),
        "--col",
        "full",
        "--by",
        ",",
        "--into",
        "first,last",
    ]);
    run(&[
        "edit",
        "formula",
        split.to_str().unwrap(),
        "--name",
        "who",
        "--value",
        r#"first & " " & last"#,
    ]);
    let text = export(&split, "split_out.csv");
    assert!(text.contains("1,Ana,Perez,Ana Perez"), "{text}");
    run(&[
        "edit",
        "set",
        split.to_str().unwrap(),
        "--row",
        "1",
        "--col",
        "first",
        "--value",
        "Zoe",
    ]);
    let text = export(&split, "split_out2.csv");
    assert!(text.contains("1,Zoe,Perez,Zoe Perez"), "{text}");
}

/// Empty screens say which kind of empty they are.
#[test]
fn an_empty_page_says_why_it_is_empty() {
    let dir = TempDir::new("emptypage");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,city\n1,CDMX\n2,Puebla\n").unwrap();
    let path = input.to_str().unwrap();

    let out = Command::new(bin())
        .args(["view", path, "--start", "50", "--count", "5"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("past the end"), "{text}");

    let out = Command::new(bin())
        .args(["view", path, "--where", "city=Nowhere"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("no rows matched"), "{text}");
    assert!(text.contains("2 data rows"), "{text}");

    let empty = dir.join("empty.csv");
    std::fs::write(&empty, "id,city\n").unwrap();
    let out = Command::new(bin())
        .args(["view", empty.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("no data rows"), "{text}");
}

/// `--regex` searches with the same pattern subset conditional formatting uses.
#[test]
fn search_and_find_accept_a_pattern() {
    let dir = TempDir::new("regexfind");
    let input = dir.join("t.csv");
    // 100 rows over 20 cities, so `C1[0-9]` (C10..C19) has something to match.
    let mut text = String::from("id,city\n");
    for i in 1..=100 {
        text.push_str(&format!("{i},C{}\n", i % 20));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    // A range matches several cities; the literal text matches none.
    let out = Command::new(bin())
        .args(["search", path, "C1[0-9]", "--regex"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("matched 50"),
        "ten cities × five rows: {text}"
    );

    let out = Command::new(bin())
        .args(["search", path, "C1[0-9]"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("matched 0"),
        "without --regex it is literal: {text}"
    );

    // Anchors work, and `--find` agrees with `search` on the same pattern.
    let out = Command::new(bin())
        .args(["search", path, "^C3$", "--regex"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("matched 5"), "{text}");

    let out = Command::new(bin())
        .args(["view", path, "--find", "C1[0-9]", "--regex"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    // The first city in C10..C19 is C10, on data row 10.
    assert!(text.contains("row 10"), "{text}");
}

/// `validate` reports what is wrong and exits 1; it never edits anything.
#[test]
fn validate_reports_bad_rows_without_changing_them() {
    let dir = TempDir::new("validate");
    let input = dir.join("t.csv");
    let original =
        "id,city,qty,date\n1,CDMX,10,2026-01-01\n2,,abc,2026-13-01\n3,Puebla,30,2026-02-30\n";
    std::fs::write(&input, original).unwrap();
    let path = input.to_str().unwrap();

    let out = Command::new(bin())
        .args([
            "validate",
            path,
            "--check",
            "city:not-empty",
            "--check",
            "qty>=0",
            "--check",
            "date:date",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "violations should fail the command"
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("row 2"), "{text}");
    assert!(text.contains("3 violation(s)"), "{text}");

    // The file is untouched — validation is a read, not a cleanup.
    assert_eq!(std::fs::read_to_string(&input).unwrap(), original);

    // A clean file exits 0.
    let clean = dir.join("ok.csv");
    std::fs::write(&clean, "id,city,qty\n1,CDMX,10\n2,Puebla,20\n").unwrap();
    let out = Command::new(bin())
        .args([
            "validate",
            clean.to_str().unwrap(),
            "--check",
            "city:not-empty",
            "--check",
            "qty>=0",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("0 violation(s)"), "{text}");
}

/// `edit fill` writes one value across a row range, and an explicit edit still wins.
#[test]
fn fill_writes_a_row_range() {
    let dir = TempDir::new("fill");
    let input = dir.join("t.csv");
    let mut text = String::from("id,qty\n");
    for i in 1..=20 {
        text.push_str(&format!("{i},{}\n", i * 10));
    }
    std::fs::write(&input, &text).unwrap();
    let path = input.to_str().unwrap();

    let run = |args: &[&str]| {
        assert_eq!(
            Command::new(bin()).args(args).status().unwrap().code(),
            Some(0),
            "{args:?}"
        );
    };
    run(&[
        "edit",
        "fill",
        path,
        "--col",
        "qty",
        "--from-row",
        "5",
        "--to-row",
        "10",
        "--value",
        "0",
    ]);
    let out = dir.join("o.csv");
    run(&["edit", "export", path, "-o", out.to_str().unwrap()]);
    let text = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[1], "1,10", "outside the range is untouched");
    assert_eq!(lines[5], "5,0", "the range starts at row 5");
    assert_eq!(lines[10], "10,0", "and ends at row 10");
    assert_eq!(lines[11], "11,110", "{text}");

    // A precise edit beats the bulk fill that covers it.
    run(&[
        "edit", "set", path, "--row", "7", "--col", "qty", "--value", "999",
    ]);
    let out2 = dir.join("o2.csv");
    run(&["edit", "export", path, "-o", out2.to_str().unwrap()]);
    let text = std::fs::read_to_string(&out2).unwrap();
    assert!(text.lines().any(|l| l == "7,999"), "{text}");

    // An inverted range is a usage error, not a silent no-op.
    let out = Command::new(bin())
        .args([
            "edit",
            "fill",
            path,
            "--col",
            "qty",
            "--from-row",
            "10",
            "--to-row",
            "5",
            "--value",
            "1",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// An export can pick columns and order rows, in one pass over the data.
#[test]
fn export_can_select_columns_and_order_rows() {
    let dir = TempDir::new("exportselect");
    let input = dir.join("t.csv");
    let mut text = String::from("id,city,qty\n");
    for i in 1..=10 {
        text.push_str(&format!("{i},C{},{}\n", i % 3, i * 10));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    // Column order is the order asked for.
    let out = dir.join("cols.csv");
    assert_eq!(
        Command::new(bin())
            .args([
                "edit",
                "export",
                path,
                "--columns",
                "qty,id",
                "-o",
                out.to_str().unwrap()
            ])
            .status()
            .unwrap()
            .code(),
        Some(0)
    );
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().next().unwrap(), "qty,id");
    assert_eq!(text.lines().nth(1).unwrap(), "10,1");

    // Ordering, filtering and selecting together.
    let out2 = dir.join("sorted.csv");
    assert_eq!(
        Command::new(bin())
            .args([
                "edit",
                "export",
                path,
                "--columns",
                "city,qty",
                "--order",
                "qty:desc",
                "-o",
                out2.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .code(),
        Some(0)
    );
    let text = std::fs::read_to_string(&out2).unwrap();
    let first = text.lines().nth(1).unwrap();
    assert!(first.ends_with(",100"), "{text}");

    let out3 = dir.join("filtered.csv");
    assert_eq!(
        Command::new(bin())
            .args([
                "edit",
                "export",
                path,
                "--where",
                "qty>50",
                "--order",
                "qty",
                "-o",
                out3.to_str().unwrap()
            ])
            .status()
            .unwrap()
            .code(),
        Some(0)
    );
    let text = std::fs::read_to_string(&out3).unwrap();
    assert_eq!(text.lines().count(), 6, "header + five rows: {text}");

    // A column that is not in the sheet is refused, not silently skipped.
    let out = Command::new(bin())
        .args([
            "edit",
            "export",
            path,
            "--columns",
            "nope",
            "-o",
            dir.join("x.csv").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot export column 'nope'"), "{stderr}");
}

/// `edit set --find` locates the row by content, and the message numbers rows 1-based.
#[test]
fn edit_can_locate_a_row_by_content() {
    let dir = TempDir::new("editfind");
    let input = dir.join("t.csv");
    let mut text = String::from("id,city,qty\n");
    for i in 1..=20 {
        let city = if i == 7 { "Puebla" } else { "CDMX" };
        text.push_str(&format!("{i},{city},{}\n", i * 10));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    let run = |args: &[&str]| {
        assert_eq!(
            Command::new(bin()).args(args).status().unwrap().code(),
            Some(0),
            "{args:?}"
        );
    };

    // Change the row that says Puebla, without knowing its number.
    let out = Command::new(bin())
        .args([
            "edit",
            "set",
            path,
            "--find",
            "Puebla",
            "--col",
            "city",
            "--value",
            "Puebla, MX",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("set row 7"),
        "rows are 1-based here: {stdout}"
    );

    // The second match is a different CDMX row, and `--nth` says which.
    run(&[
        "edit", "set", path, "--find", "CDMX", "--nth", "2", "--col", "qty", "--value", "999",
    ]);
    let exported = dir.join("o.csv");
    run(&["edit", "export", path, "-o", exported.to_str().unwrap()]);
    let text = std::fs::read_to_string(&exported).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    // The value with a comma survived the journal round-trip.
    assert!(lines.iter().any(|l| l.contains("Puebla, MX")), "{text}");
    // `--nth 2` on CDMX is the second data row: row 2, not row 3.
    assert_eq!(lines[2], "2,CDMX,999", "{text}");
    assert_eq!(lines[3], "3,CDMX,30", "{text}");

    // Not found is an error that says so, not a no-op.
    let out = Command::new(bin())
        .args([
            "edit", "set", path, "--find", "Nowhere", "--col", "city", "--value", "x",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no row contains 'Nowhere'"), "{stderr}");
}

/// `edit delete --where` removes every matching row as one undoable operation.
#[test]
fn delete_by_condition_is_one_operation() {
    let dir = TempDir::new("deletewhere");
    let input = dir.join("t.csv");
    let mut text = String::from("id,city,qty\n");
    for i in 1..=20 {
        let city = if matches!(i, 3 | 4 | 5 | 12) {
            "Puebla"
        } else {
            "CDMX"
        };
        text.push_str(&format!("{i},{city},{}\n", i * 10));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    let run = |args: &[&str]| {
        assert_eq!(
            Command::new(bin()).args(args).status().unwrap().code(),
            Some(0),
            "{args:?}"
        );
    };
    let rows = |name: &str| {
        let out = dir.join(name);
        run(&["edit", "export", path, "-o", out.to_str().unwrap()]);
        std::fs::read_to_string(&out).unwrap().lines().count()
    };

    run(&["edit", "delete", path, "--where", "city=Puebla"]);
    // Two stretches (rows 3-5 and row 12) become one operation.
    assert_eq!(rows("after.csv"), 17, "header + 16 rows");

    // One undo brings all four rows back — that is the point of one operation.
    run(&["edit", "undo", path]);
    assert_eq!(rows("undo.csv"), 21, "all four rows are back");

    // And redo takes them away again.
    run(&["edit", "redo", path]);
    assert_eq!(rows("redo.csv"), 17);

    // The journal format survives a reload: the ranges are data, not a closure.
    let journal = std::fs::read_to_string(dir.join("t.csv.journal")).unwrap();
    assert!(journal.contains("dels"), "{journal}");
}

/// A fill follows its rows: inserting or deleting elsewhere must not move the values.
#[test]
fn a_fill_sticks_to_the_rows_it_covered() {
    let dir = TempDir::new("fillidentity");
    let run = |args: &[&str]| {
        assert_eq!(
            Command::new(bin()).args(args).status().unwrap().code(),
            Some(0),
            "{args:?}"
        );
    };

    // Fill rows 6-8, then delete the first three rows (by condition).
    let input = dir.join("t.csv");
    let mut text = String::from("id,city,qty\n");
    for i in 1..=10 {
        let city = if i <= 3 { "Puebla" } else { "CDMX" };
        text.push_str(&format!("{i},{city},0\n"));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();
    run(&[
        "edit",
        "fill",
        path,
        "--col",
        "qty",
        "--from-row",
        "6",
        "--to-row",
        "8",
        "--value",
        "999",
    ]);
    run(&["edit", "delete", path, "--where", "city=Puebla"]);
    let out = dir.join("deleted.csv");
    run(&["edit", "export", path, "-o", out.to_str().unwrap()]);
    let text = std::fs::read_to_string(&out).unwrap();
    // ids 6, 7, 8 keep the fill; ids 9 and 10 do not.
    assert!(text.contains("6,CDMX,999"), "{text}");
    assert!(text.contains("8,CDMX,999"), "{text}");
    assert!(text.contains("9,CDMX,0"), "{text}");
    assert!(!text.contains("10,CDMX,999"), "{text}");

    // Fill rows 2-3, then insert two rows at the top: the fill stays with its rows.
    let other = dir.join("u.csv");
    let mut text = String::from("id,qty\n");
    for i in 1..=5 {
        text.push_str(&format!("{i},0\n"));
    }
    std::fs::write(&other, text).unwrap();
    let opath = other.to_str().unwrap();
    run(&[
        "edit",
        "fill",
        opath,
        "--col",
        "qty",
        "--from-row",
        "2",
        "--to-row",
        "3",
        "--value",
        "555",
    ]);
    run(&["edit", "insert", opath, "--row", "1", "--value", "x,0"]);
    run(&[
        "edit",
        "export",
        opath,
        "-o",
        dir.join("inserted.csv").to_str().unwrap(),
    ]);
    let text = std::fs::read_to_string(dir.join("inserted.csv")).unwrap();
    assert!(text.contains("2,555"), "{text}");
    assert!(text.contains("3,555"), "{text}");
    assert!(
        !text.contains("x,555"),
        "the inserted row was not filled: {text}"
    );
}

/// Filtering and ordering in `view` must use the edited values, not the file's.
#[test]
fn view_filters_and_orders_on_edited_values() {
    let dir = TempDir::new("viewedits");
    let input = dir.join("t.csv");
    let mut text = String::from("id,city,qty\n");
    for i in 1..=10 {
        let city = if i == 5 { "Puebla" } else { "CDMX" };
        text.push_str(&format!("{i},{city},{}\n", i * 10));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    let run = |args: &[&str]| {
        assert_eq!(
            Command::new(bin()).args(args).status().unwrap().code(),
            Some(0),
            "{args:?}"
        );
    };

    // Turn the one Puebla row into CDMX: now every row matches the filter.
    run(&[
        "edit", "set", path, "--row", "5", "--col", "city", "--value", "CDMX",
    ]);
    let out = Command::new(bin())
        .args(["view", path, "--where", "city=CDMX", "--count", "20"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("10 row(s)"), "all ten rows match now: {text}");

    // Raise one value: ordering must put it first.
    run(&[
        "edit", "set", path, "--row", "1", "--col", "qty", "--value", "999",
    ]);
    let out = Command::new(bin())
        .args(["view", path, "--order", "qty:desc", "--count", "2"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let first_row = text
        .lines()
        .find(|line| line.contains("999"))
        .unwrap_or_default();
    assert!(first_row.starts_with('1'), "999 must sort first: {text}");

    // A computed column can be filtered on, the same way a rule can match it.
    run(&["edit", "formula", path, "--name", "big", "--value", "qty*2"]);
    let out = Command::new(bin())
        .args(["view", path, "--where", "big>1000", "--count", "5"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("row(s) where big>1000"), "{text}");
}

/// A user journey: dirty file → clean it → add a column → summarise → export.
///
/// Written as one test because the steps have to work *in sequence*: each piece can be
/// correct alone and still fail when the output of one feeds the next.
#[test]
fn the_journey_from_dirty_data_to_a_delivered_file() {
    let dir = TempDir::new("journey");

    // 1. A file with the failure real CSVs have: unquoted commas inside a field, so the
    //    row has an extra column and the rest shifts.
    let input = dir.join("orders.csv");
    let mut text = String::from("id,city,amount,ts\n");
    for i in 1..=50 {
        if i % 10 == 0 {
            text.push_str(&format!(
                "{i},Puebla,{},\"stray,comma\",2026-03-0{}\n",
                i * 100,
                i % 9
            ));
        } else {
            text.push_str(&format!("{i},Puebla,{},2026-03-0{}\n", i * 100, i % 9));
        }
    }
    // Two rows with the extra field, and one with a value where a date should be.
    text.push_str("51,CDMX,5100,not-a-date\n");
    text.push_str("52,Tijuana,5200,\"stray,comma\",2026-03-02\n");
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    let run = |args: &[&str]| {
        assert_eq!(
            Command::new(bin()).args(args).status().unwrap().code(),
            Some(0),
            "{args:?}"
        );
    };

    // 2. The profile warns about the shifted rows rather than pretending they are fine.
    let out = Command::new(bin())
        .args(["analyze", path])
        .output()
        .unwrap();
    let text_stdout = String::from_utf8_lossy(&out.stdout);
    assert!(text_stdout.contains("field count"), "{text_stdout}");

    // 3. Validation names the rows that cannot be trusted.
    let out = Command::new(bin())
        .args(["validate", path, "--check", "ts:date"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "bad rows must fail validation");
    let text_stdout = String::from_utf8_lossy(&out.stdout);
    assert!(text_stdout.contains("violation"), "{text_stdout}");

    // 4. Remove them, in one operation.
    run(&["edit", "delete", path, "--invalid", "ts:date"]);

    // 5. Add a computed column, then summarise by it — without materialising by hand.
    run(&[
        "edit",
        "formula",
        path,
        "--name",
        "vat",
        "--value",
        "amount*0.16",
    ]);
    let grouped = dir.join("by_city.csv");
    run(&[
        "groupby",
        path,
        "--by",
        "city",
        "--agg",
        "vat:sum",
        "--apply-edits",
        "-o",
        grouped.to_str().unwrap(),
    ]);
    let text = std::fs::read_to_string(&grouped).unwrap();
    assert!(
        text.contains("vat"),
        "the formula column reached groupby: {text}"
    );

    // 6. Deliver only the columns that are wanted, ordered.
    let delivered = dir.join("delivered.csv");
    run(&[
        "edit",
        "export",
        path,
        "--columns",
        "id,city,amount",
        "--order",
        "amount:desc",
        "-o",
        delivered.to_str().unwrap(),
    ]);
    let text = std::fs::read_to_string(&delivered).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "id,city,amount");
    // 52 rows went in; the 12 whose `ts` could not be read were removed by `--invalid`.
    assert_eq!(lines.len(), 41, "header + 40 rows: {text}");
    assert!(lines.iter().any(|l| l.contains("Puebla")), "{text}");
    // Row 52 had a stray comma, so its `ts` shifted and validation rejected it.
    assert!(
        !lines.iter().any(|l| l.starts_with("52,")),
        "row 52 should have been removed: {text}"
    );
}

/// Isolates the journey's last step: export with columns and order, on a plain file.
#[test]
fn export_with_order_keeps_every_row() {
    let dir = TempDir::new("exportorderiso");
    let input = dir.join("t.csv");
    let mut text = String::from("id,city,amount\n");
    for i in 1..=40 {
        text.push_str(&format!("{i},Puebla,{}\n", i * 100));
    }
    std::fs::write(&input, text).unwrap();
    let out = dir.join("out.csv");
    let status = Command::new(bin())
        .args([
            "edit",
            "export",
            input.to_str().unwrap(),
            "--columns",
            "id,city,amount",
            "--order",
            "amount:desc",
            "-o",
            out.to_str().unwrap(),
        ])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().count(), 41, "{text}");
}

/// A page far from the start must not cost a scan of everything before it.
///
/// This is a timing test, which is usually a smell — but the regression it guards was
/// "one page at row 3.9 M took 5 s on a 77 MB file instead of 0.3 s", a 15× difference
/// that no threshold in between mistakes. The bound is deliberately loose.
#[test]
fn a_deep_page_costs_the_same_as_a_shallow_one() {
    let dir = TempDir::new("deepPage");
    let input = dir.join("big.csv");
    let mut text = String::from("id,city\n");
    for i in 0..200_000 {
        text.push_str(&format!("{i},C{}\n", i % 20));
    }
    std::fs::write(&input, &text).unwrap();
    let path = input.to_str().unwrap();

    // Warm the index once so the measurement is paging, not index building.
    Command::new(bin())
        .args(["view", path, "--start", "0", "--count", "20"])
        .output()
        .unwrap();

    let shallow = std::time::Instant::now();
    Command::new(bin())
        .args(["view", path, "--start", "0", "--count", "20"])
        .output()
        .unwrap();
    let shallow = shallow.elapsed();

    let deep = std::time::Instant::now();
    Command::new(bin())
        .args(["view", path, "--start", "190000", "--count", "20"])
        .output()
        .unwrap();
    let deep = deep.elapsed();

    // The regression this guards was 5 s against 0.3 s — a factor of fifteen, on a
    // machine running the whole test suite. A factor of four leaves room for scheduling
    // noise while still failing loudly if paging goes back to scanning.
    let bound = shallow * 4 + std::time::Duration::from_millis(300);
    assert!(deep <= bound, "deep {deep:?} vs shallow {shallow:?}");
}

/// `verify` compares by streaming and reports where the two files diverge.
#[test]
fn verify_streams_and_reports_the_first_difference() {
    let dir = TempDir::new("verifydiff");
    // A file with embedded newlines and quotes, so the round-trip is not trivial.
    let input = dir.join("t.csv");
    let mut text = String::from("id,note\n");
    for i in 1..=200 {
        text.push_str(&format!("{i},\"line one\nline two,{i}\"\n"));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    let out = Command::new(bin())
        .args(["verify", path, "-n", "3"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("lossless"), "{stdout}");

    // A file that cannot round-trip identically: two rows with an unquoted comma make
    // the split/merge byte comparison fail, and the message must say where.
    let broken = dir.join("broken.csv");
    std::fs::write(&broken, "a,b\n1,x,y\n2,p,q\n").unwrap();
    let out = Command::new(bin())
        .args(["verify", broken.to_str().unwrap(), "-n", "2"])
        .output()
        .unwrap();
    if out.status.code() != Some(0) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("mismatch at byte"), "{stderr}");
    }
}

/// The base file is never modified — by any command, on success or on failure.
///
/// This is the project's central promise, and it is cheap to check: hash and mtime
/// before, the same after. The commands that write are exercised on a file they must
/// only read.
#[test]
fn no_command_modifies_the_input_file() {
    let dir = TempDir::new("immutable");
    let input = dir.join("t.csv");
    let mut text = String::from("id,city,amount\n");
    for i in 1..=200 {
        text.push_str(&format!("{i},C{},{}\n", i % 7, i * 10));
    }
    std::fs::write(&input, &text).unwrap();
    let path = input.to_str().unwrap();

    let stamp = || {
        let bytes = std::fs::read(&input).unwrap();
        let modified = std::fs::metadata(&input).unwrap().modified().ok();
        // A cheap content digest: length plus a rolling checksum over the bytes.
        let sum: u64 = bytes.iter().enumerate().fold(0u64, |acc, (i, b)| {
            acc.wrapping_add((i as u64 + 1) * (*b as u64))
        });
        (bytes.len(), sum, modified)
    };
    let before = stamp();

    let out = dir.join("out.csv");
    let mut runs: Vec<Vec<String>> = vec![
        vec![
            "filter".into(),
            path.into(),
            "--where".into(),
            "city=C3".into(),
            "-o".into(),
            out.display().to_string(),
        ],
        vec![
            "sort".into(),
            path.into(),
            "--by".into(),
            "amount".into(),
            "-o".into(),
            out.display().to_string(),
        ],
        vec![
            "groupby".into(),
            path.into(),
            "--by".into(),
            "city".into(),
            "--agg".into(),
            "amount:sum".into(),
            "-o".into(),
            out.display().to_string(),
        ],
        vec![
            "validate".into(),
            path.into(),
            "--check".into(),
            "city:not-empty".into(),
        ],
        vec![
            "edit".into(),
            "set".into(),
            path.into(),
            "--row".into(),
            "2".into(),
            "--col".into(),
            "city".into(),
            "--value".into(),
            "X".into(),
        ],
        vec![
            "edit".into(),
            "export".into(),
            path.into(),
            "-o".into(),
            out.display().to_string(),
        ],
        vec![
            "split".into(),
            path.into(),
            "-n".into(),
            "2".into(),
            "-o".into(),
            dir.join("parts.tar").display().to_string(),
        ],
    ];
    // A failing command must not touch it either.
    runs.push(vec![
        "sort".into(),
        path.into(),
        "--by".into(),
        "nosuch".into(),
        "-o".into(),
        out.display().to_string(),
    ]);

    for args in runs {
        let _ = Command::new(bin()).args(&args).status().unwrap();
    }

    let after = stamp();
    assert_eq!(
        before, after,
        "the input file changed: {before:?} vs {after:?}"
    );
}

/// A typo in a formula is caught when it is written, not after an export.
#[test]
fn a_formula_is_checked_before_it_is_recorded() {
    let dir = TempDir::new("formulacheck");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,amount\n1,10\n2,20\n").unwrap();
    let path = input.to_str().unwrap();

    // Unknown column: refused, with the columns that do exist.
    let out = Command::new(bin())
        .args([
            "edit", "formula", path, "--name", "bad", "--value", "amonut*2",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown column"), "{stderr}");
    assert!(stderr.contains("id, amount"), "{stderr}");

    // Missing reference sheet: refused too, because the export would refuse it later.
    let out = Command::new(bin())
        .args([
            "edit",
            "formula",
            path,
            "--name",
            "ext",
            "--value",
            r#"ref("nosuch.csv","id","amount")"#,
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not found"), "{stderr}");
    assert!(stderr.contains("hint:"), "{stderr}");

    // A good formula still works, and its journal entry survives.
    let out = Command::new(bin())
        .args([
            "edit", "formula", path, "--name", "twice", "--value", "amount*2",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
}

/// A journal cut off mid-write loses a record — and says so instead of pretending.
#[test]
fn a_truncated_journal_is_reported_not_ignored() {
    let dir = TempDir::new("truncatedJournal");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,city\n1,A\n2,B\n3,C\n").unwrap();
    let path = input.to_str().unwrap();

    for row in ["2", "3"] {
        assert_eq!(
            Command::new(bin())
                .args(["edit", "set", path, "--row", row, "--col", "city", "--value", "Z"])
                .status()
                .unwrap()
                .code(),
            Some(0)
        );
    }

    // Cut the journal in the middle of its last record, as an interrupted write would.
    let journal = dir.join("t.csv.journal");
    let full = std::fs::read_to_string(&journal).unwrap();
    let cut = &full[..full.len() - 8];
    std::fs::write(&journal, cut).unwrap();

    let out = Command::new(bin())
        .args(["view", path, "--count", "3"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("could not be read"),
        "a lost edit must be reported: {stderr}"
    );
    // The readable operations still apply: the first edit survived, the cut one did not.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains('Z'), "{stdout}");
}

/// A damaged index or layout sidecar is rebuilt, not trusted.
#[test]
fn damaged_sidecars_are_rebuilt() {
    let dir = TempDir::new("damagedSidecars");
    let input = dir.join("t.csv");
    let mut text = String::from("id,city\n");
    for i in 1..=100 {
        text.push_str(&format!("{i},C{}\n", i % 5));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    assert_eq!(
        Command::new(bin())
            .args(["index", path])
            .status()
            .unwrap()
            .code(),
        Some(0)
    );
    let index = dir.join("t.csv.idx");
    std::fs::write(&index, b"garbage").unwrap();

    // The page still has the right rows: a rebuilt index, not an error and not a wrong
    // answer.
    let out = Command::new(bin())
        .args(["preview", path, "--start-row", "90", "--rows", "2"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("91"), "{stdout}");

    // A layout file full of nonsense falls back to defaults instead of refusing to open
    // the file.
    let layout = dir.join("t.csv.view");
    std::fs::write(&layout, b"\x00garbage\n").unwrap();
    let out = Command::new(bin()).args(["layout", path]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("width"), "{stdout}");
}

/// A UTF-8 BOM is preserved, because Excel reads a BOM-less file as the system code page.
#[test]
fn a_bom_survives_every_output() {
    let dir = TempDir::new("bom");
    let input = dir.join("t.csv");
    std::fs::write(&input, "\u{feff}id,city,amount\n1,CDMX,10\n2,Puebla,20\n").unwrap();
    let path = input.to_str().unwrap();
    let run = |args: &[&str]| {
        assert_eq!(
            Command::new(bin()).args(args).status().unwrap().code(),
            Some(0),
            "{args:?}"
        );
    };

    let filtered = dir.join("f.csv");
    run(&[
        "filter",
        path,
        "--where",
        "city=CDMX",
        "-o",
        filtered.to_str().unwrap(),
    ]);
    let kept = dir.join("k.csv");
    run(&[
        "columns",
        path,
        "--keep",
        "id,city",
        "-o",
        kept.to_str().unwrap(),
    ]);
    let exported = dir.join("e.csv");
    run(&["edit", "export", path, "-o", exported.to_str().unwrap()]);

    for output in [filtered, kept, exported] {
        let bytes = std::fs::read(&output).unwrap();
        assert_eq!(
            &bytes[..3],
            &[0xEF, 0xBB, 0xBF],
            "{} lost its BOM",
            output.display()
        );
    }

    // And a file without a BOM does not gain one.
    let plain = dir.join("plain.csv");
    std::fs::write(&plain, "id,city\n1,CDMX\n").unwrap();
    let out = dir.join("plain_out.csv");
    run(&[
        "edit",
        "export",
        plain.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(&bytes[..3], b"id,", "a BOM was invented");
}

/// A wide file is shown twenty columns at a time, and `--columns` picks the rest.
#[test]
fn a_wide_file_is_not_printed_column_by_column() {
    let dir = TempDir::new("wide");
    let input = dir.join("wide.csv");
    let mut header: Vec<String> = (0..300).map(|i| format!("c{i}")).collect();
    header.insert(0, "id".to_string());
    let mut text = header.join(",") + "\n";
    text.push_str(&format!("{},", 1));
    text.push_str(&vec!["7"; 300].join(","));
    text.push('\n');
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    let out = Command::new(bin())
        .args(["preview", path, "--rows", "1"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("more column"), "{stdout}");
    // 301 columns: twenty shown, 281 hidden.
    assert!(stdout.contains("+281"), "{stdout}");

    // Selecting the columns explicitly shows exactly those.
    let out = Command::new(bin())
        .args(["preview", path, "--rows", "1", "--columns", "id,c299"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("id"), "{stdout}");
    assert!(stdout.contains("c299"), "{stdout}");
    assert!(!stdout.contains("more column"), "{stdout}");

    // A name that is not there is an error, not an empty table.
    let out = Command::new(bin())
        .args(["preview", path, "--rows", "1", "--columns", "nope"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// `schema` answers "is this column here?" and "what is in it?".
#[test]
fn schema_lists_columns_and_answers_existence() {
    let dir = TempDir::new("schema");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,city,amount\n1,CDMX,10\n2,Puebla,20\n3,CDMX,\n").unwrap();
    let path = input.to_str().unwrap();

    // A column that exists: exit 0, and it says where.
    let out = Command::new(bin())
        .args(["schema", path, "--has", "city"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("position 2"), "{stdout}");

    // A column that does not: exit 1, so a script can branch on it.
    let out = Command::new(bin())
        .args(["schema", path, "--has", "nope"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("2 of 3") || stderr.contains("no column"),
        "{stderr}"
    );

    // The full list: every column, with kinds sampled from the data.
    let out = Command::new(bin())
        .args(["schema", path, "--json", "--sample", "3"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#""count":3"#), "{stdout}");
    assert!(stdout.contains(r#""name":"city""#), "{stdout}");
    assert!(stdout.contains(r#""kind":"number""#), "{stdout}");
    assert!(stdout.contains(r#""index":3"#), "{stdout}");
}

/// `--expect` makes a write conditional, and refuses instead of guessing.
#[test]
fn expect_turns_a_set_into_compare_and_set() {
    let dir = TempDir::new("expect");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,city\n1,CDMX\n2,Puebla\n").unwrap();
    let path = input.to_str().unwrap();

    // Correct expectation: written.
    let out = Command::new(bin())
        .args([
            "edit", "set", path, "--row", "1", "--col", "city", "--expect", "CDMX", "--value", "Z",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{:?}", out);

    // The old value is gone: the same expectation now fails, and says what it found.
    let out = Command::new(bin())
        .args([
            "edit", "set", path, "--row", "1", "--col", "city", "--expect", "CDMX", "--value", "Z2",
        ])
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not \"CDMX\""), "{stderr}");
    assert!(stderr.contains("\"Z\""), "{stderr}");
    assert!(stderr.contains("nothing was written"), "{stderr}");

    // And the refused write really did nothing.
    let out = Command::new(bin())
        .args(["view", path, "--count", "1", "--columns", "city"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains('Z'), "{stdout}");
    assert!(!stdout.contains("Z2"), "{stdout}");
}

/// Two writers on the same file both survive: the journal append is locked.
#[test]
fn concurrent_edits_do_not_lose_each_other() {
    let dir = TempDir::new("concurrent");
    let input = dir.join("t.csv");
    let mut text = String::from("id,city\n");
    for i in 1..=40 {
        text.push_str(&format!("{i},C{i}\n"));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap().to_string();

    let mut children = Vec::new();
    for row in 1..=10 {
        let child = Command::new(bin())
            .args([
                "edit",
                "set",
                &path,
                "--row",
                &row.to_string(),
                "--col",
                "city",
                "--value",
                &format!("W{row}"),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        children.push(child);
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }

    // Every edit is in the journal: nothing was overwritten by a racing rename.
    let journal = std::fs::read_to_string(dir.join("t.csv.journal")).unwrap();
    assert_eq!(journal.lines().count(), 10, "{journal}");

    // And each landed on its own row.
    let out = Command::new(bin())
        .args(["view", &path, "--count", "10", "--columns", "city"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    for row in 1..=10 {
        assert!(
            stdout.contains(&format!("W{row}")),
            "row {row} missing: {stdout}"
        );
    }
}

/// `--require` asserts several columns at once, and says which are missing.
#[test]
fn schema_require_asserts_a_column_set() {
    let dir = TempDir::new("require");
    let input = dir.join("t.csv");
    std::fs::write(&input, "id,city,amount\n1,CDMX,10\n").unwrap();
    let path = input.to_str().unwrap();

    let out = Command::new(bin())
        .args(["schema", path, "--require", "id,city,amount"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("3 required column(s) present"), "{stdout}");

    let out = Command::new(bin())
        .args(["schema", path, "--require", "id,nosuch"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("missing 1 of 2"), "{stderr}");
    assert!(stderr.contains("nosuch"), "{stderr}");
}

/// Sampled column metadata carries what a front-end needs to pick an editor.
#[test]
fn schema_reports_kind_empty_and_distinct() {
    let dir = TempDir::new("kinds");
    let input = dir.join("t.csv");
    let mut text = String::from("id,city,when\n");
    for i in 1..=20 {
        text.push_str(&format!("{i},C{},2026-03-{:02}\n", i % 2, (i % 27) + 1));
    }
    std::fs::write(&input, text).unwrap();
    let path = input.to_str().unwrap();

    let out = Command::new(bin())
        .args(["schema", path, "--json", "--sample", "20"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#""name":"city""#), "{stdout}");
    assert!(stdout.contains(r#""kind":"text""#), "{stdout}");
    assert!(stdout.contains(r#""distinct":2"#), "{stdout}");
    assert!(stdout.contains(r#""name":"when""#), "{stdout}");
    assert!(stdout.contains(r#""kind":"date""#), "{stdout}");
    assert!(stdout.contains("distinctCapped"), "{stdout}");
}

/// `schema --diff` reports added, removed and moved columns between two files.
#[test]
fn schema_diff_reports_column_set_changes() {
    let dir = TempDir::new("schemadiff");
    let a = dir.join("a.csv");
    let b = dir.join("b.csv");
    std::fs::write(&a, "id,city,amount\n1,A,10\n").unwrap();
    std::fs::write(&b, "id,note,city,amount\n1,x,A,10\n").unwrap();
    let a_path = a.to_str().unwrap();
    let b_path = b.to_str().unwrap();

    let out = Command::new(bin())
        .args(["schema", a_path, "--diff", b_path, "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "differences must be visible in the exit code"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#""added":["note"]"#), "{stdout}");
    assert!(stdout.contains(r#""removed":[]"#), "{stdout}");
    assert!(
        stdout.contains(r#""name":"amount","here":3,"there":4"#),
        "{stdout}"
    );

    // Same set: exit 0 and "same".
    let out = Command::new(bin())
        .args(["schema", a_path, "--diff", a_path])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("same column set"), "{stdout}");

    // A column that only the first file has is "removed", not "moved".
    let c = dir.join("c.csv");
    std::fs::write(&c, "id,city\n1,A\n").unwrap();
    let out = Command::new(bin())
        .args(["schema", a_path, "--diff", c.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(r#""removed":["amount"]"#), "{stdout}");
}

/// `serve --check` is the one command a deployment can gate on.
#[test]
fn serve_check_reports_the_configuration() {
    let dir = TempDir::new("servecheck");
    std::fs::write(dir.join("t.csv"), "id\n1\n").unwrap();

    let out = std::process::Command::new(bin())
        .args(["serve", "--check", "--root"])
        .arg(&dir.0)
        // Port 0: the check binds a free port, so the test never races a real service.
        .args(["--addr", "127.0.0.1:0"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("version"), "{text}");
    assert!(text.contains("root readable"), "{text}");
    assert!(text.contains("bind"), "{text}");
    assert!(text.contains("configuration ok"), "{text}");

    // A root that is not there must fail, and say which check failed.
    let out = std::process::Command::new(bin())
        .args(["serve", "--check", "--root", "/nope"])
        .args(["--addr", "127.0.0.1:0"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "a missing root must fail: {text}");
    assert!(text.contains("FAIL"), "{text}");
    assert!(text.contains("check(s) failed"), "{text}");
    std::fs::remove_dir_all(&dir.0).ok();
}
