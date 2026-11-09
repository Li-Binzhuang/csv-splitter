//! End-to-end checks for the split pipeline.
//!
//! The most valuable property of a splitter is that it is *lossless*: merging the
//! chunks back must reproduce the input byte for byte. Each test here asserts
//! that invariant (or the intended transformation, for column filtering).

use csv_splitter_core::merge::merge_archive;
use csv_splitter_core::splitter::run_split;
use csv_splitter_core::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "csvs_it_{tag}_{}_{}",
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

fn base_config(input: &Path, output: &Path) -> SplitConfig {
    SplitConfig {
        input: input.to_path_buf(),
        output: Some(output.to_path_buf()),
        splits: 3,
        compress_format: CompressFormat::None,
        compress_level: 3,
        buf_mb: 1,
        columns_file: None,
        delimiter: Delimiter::Comma,
        sort_column: None,
        sort_descending: false,
    }
}

/// Run a split, returning the events so callers can assert on failures.
fn split(config: SplitConfig) -> Vec<ProgressEvent> {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || run_split(config, tx));
    let events: Vec<ProgressEvent> = rx.iter().collect();
    handle.join().unwrap();
    events
}

fn has_error(events: &[ProgressEvent]) -> bool {
    events.iter().any(|e| matches!(e, ProgressEvent::Error(_)))
}

fn roundtrip(content: &[u8], splits: usize, format: CompressFormat) {
    let dir = TempDir::new("rt");
    let input = dir.join("data.csv");
    let archive = dir.join("out.tar");
    let merged = dir.join("merged.csv");
    std::fs::write(&input, content).unwrap();

    let mut cfg = base_config(&input, &archive);
    cfg.splits = splits;
    cfg.compress_format = format;
    let events = split(cfg);
    assert!(!has_error(&events), "split failed: {events:?}");

    let (tx, _rx) = mpsc::channel();
    merge_archive(&archive, &merged, &tx).unwrap();
    assert_eq!(std::fs::read(&merged).unwrap(), content);
}

#[test]
fn roundtrip_plain() {
    roundtrip(
        b"id,name\n1,a\n2,b\n3,c\n4,d\n5,e\n",
        3,
        CompressFormat::None,
    );
}

#[test]
fn roundtrip_zstd() {
    roundtrip(b"id,name\n1,a\n2,b\n3,c\n4,d\n", 2, CompressFormat::Zstd);
}

#[test]
fn roundtrip_gzip() {
    roundtrip(b"id,name\n1,a\n2,b\n3,c\n", 2, CompressFormat::Gzip);
}

#[test]
fn roundtrip_zip() {
    roundtrip(
        b"id,name\n1,a\n2,b\n3,c\n4,d\n5,e\n",
        2,
        CompressFormat::Zip,
    );
}

#[test]
fn roundtrip_keeps_crlf_and_quoted_newlines() {
    let content = b"id,note\r\n1,\"line1\r\nline2\"\r\n2,plain\r\n3,\"a,b\"\r\n";
    roundtrip(content, 2, CompressFormat::None);
}

#[test]
fn roundtrip_keeps_bom() {
    let mut content = b"\xEF\xBB\xBFid,name\n1,a\n2,b\n".to_vec();
    content.extend_from_slice(b"3,c\n");
    roundtrip(&content, 2, CompressFormat::None);
}

#[test]
fn roundtrip_without_trailing_newline() {
    roundtrip(b"id,name\n1,a\n2,b\n3,c", 2, CompressFormat::None);
}

#[test]
fn roundtrip_more_splits_than_records() {
    roundtrip(b"id\n1\n2\n", 10, CompressFormat::None);
}

#[test]
fn single_uncompressed_split_is_a_plain_csv() {
    let dir = TempDir::new("single");
    let input = dir.join("data.csv");
    let output = dir.join("copy.csv");
    std::fs::write(&input, b"id\n1\n2\n").unwrap();

    let mut cfg = base_config(&input, &output);
    cfg.splits = 1;
    let events = split(cfg);
    assert!(!has_error(&events), "{events:?}");
    assert_eq!(std::fs::read(&output).unwrap(), b"id\n1\n2\n");
}

#[test]
fn column_filter_keeps_only_selected_columns() {
    let dir = TempDir::new("cols");
    let input = dir.join("data.csv");
    let cols = dir.join("cols.txt");
    let archive = dir.join("out.tar");
    let merged = dir.join("merged.csv");
    std::fs::write(&input, "id,name,age\n1,a,30\n2,b,40\n").unwrap();
    std::fs::write(&cols, "id\nage\n").unwrap();

    let mut cfg = base_config(&input, &archive);
    cfg.splits = 2;
    cfg.columns_file = Some(cols);
    let events = split(cfg);
    assert!(!has_error(&events), "{events:?}");

    let (tx, _rx) = mpsc::channel();
    merge_archive(&archive, &merged, &tx).unwrap();
    assert_eq!(
        std::fs::read_to_string(&merged).unwrap(),
        "id,age\n1,30\n2,40\n"
    );
}

#[test]
fn zero_splits_is_rejected_not_a_panic() {
    let dir = TempDir::new("zero");
    let input = dir.join("data.csv");
    let archive = dir.join("out.tar");
    std::fs::write(&input, b"id\n1\n").unwrap();

    let mut cfg = base_config(&input, &archive);
    cfg.splits = 0;
    let events = split(cfg);
    assert!(
        has_error(&events),
        "expected an Error event, got {events:?}"
    );
}

#[test]
fn empty_input_is_reported_as_error() {
    let dir = TempDir::new("empty");
    let input = dir.join("data.csv");
    let archive = dir.join("out.tar");
    std::fs::write(&input, b"").unwrap();

    let cfg = base_config(&input, &archive);
    let events = split(cfg);
    assert!(
        has_error(&events),
        "expected an Error event, got {events:?}"
    );
}
