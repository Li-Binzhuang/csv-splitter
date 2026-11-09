//! Coarse performance guard.
//!
//! `crates/core/benches/throughput.rs` reports real numbers; this file only
//! fails the build when something becomes *pathologically* slow (an accidental
//! O(n²), a lost buffer, a per-row syscall). The bounds are ~50x worse than
//! measured values on purpose: shared CI runners are noisy, and a flaky perf
//! test gets deleted rather than fixed.

use csv_splitter_core::index::SparseIndex;
use csv_splitter_core::splitter::run_split;
use csv_splitter_core::types::*;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

const ROWS: usize = 200_000;

struct Fixture {
    dir: PathBuf,
    csv: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "csvs_perf_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("perf.csv");
        let mut text = String::with_capacity(ROWS * 48);
        text.push_str("id,cust_no,city,amount,note\n");
        for i in 0..ROWS {
            text.push_str(&format!(
                "{i},C{:06},city{},{},ok\n",
                i % 50_000,
                i % 7,
                i % 90_000
            ));
        }
        std::fs::write(&csv, text).unwrap();
        Self { dir, csv }
    }

    fn bytes(&self) -> u64 {
        std::fs::metadata(&self.csv).unwrap().len()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

#[test]
fn index_build_is_streaming_fast() {
    let fx = Fixture::new();
    let bytes = fx.bytes();
    let started = Instant::now();
    let index = SparseIndex::build(&fx.csv, 1000).unwrap();
    let elapsed = started.elapsed();
    let mb_s = bytes as f64 / 1024.0 / 1024.0 / elapsed.as_secs_f64();

    assert_eq!(index.data_rows() as usize, ROWS);
    // Measured ~2 GB/s; anything under 20 MB/s means a buffer or a syscall per
    // row crept in.
    assert!(
        mb_s > 20.0,
        "index build too slow: {mb_s:.1} MB/s over {bytes} bytes in {elapsed:?}"
    );
    // 200k rows at interval 1000 => 201 checkpoints, 8 bytes each + header.
    assert!(
        index.size_bytes() < 4096,
        "index grew unexpectedly: {} bytes",
        index.size_bytes()
    );
}

#[test]
fn random_access_cost_does_not_depend_on_position() {
    let fx = Fixture::new();
    let index = SparseIndex::build(&fx.csv, 1000).unwrap();
    let total = index.data_rows();

    let head = Instant::now();
    index.read_rows(&fx.csv, 0, 20).unwrap();
    let head = head.elapsed();

    let tail = Instant::now();
    index.read_rows(&fx.csv, total - 20, 20).unwrap();
    let tail = tail.elapsed();

    // Generous in absolute terms (CI disks are slow), tight in relative terms:
    // the point of the index is that the last row is not slower than the first.
    assert!(tail.as_millis() < 100, "tail read took {tail:?}");
    assert!(
        tail.as_micros() < head.as_micros() * 20 + 5_000,
        "tail read ({tail:?}) is disproportionately slower than head ({head:?})"
    );
}

#[test]
fn split_keeps_a_flat_memory_and_time_profile() {
    let fx = Fixture::new();
    let out = fx.dir.join("perf.tar");
    let bytes = fx.bytes();

    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    drop(rx);
    run_split(
        SplitConfig {
            input: fx.csv.clone(),
            output: Some(out.clone()),
            splits: 4,
            max_bytes: None,
            compress_format: CompressFormat::None,
            compress_level: 0,
            buf_mb: 16,
            columns_file: None,
            delimiter: Delimiter::Comma,
            sort_column: None,
            sort_descending: false,
        },
        tx,
    );
    let elapsed = started.elapsed();
    let mb_s = bytes as f64 / 1024.0 / 1024.0 / elapsed.as_secs_f64();

    // Errors arrive as an event, not a panic; a failed run would be tiny and
    // fast, so check the artefact exists too.
    let produced = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    assert!(
        produced > bytes / 2,
        "output missing or truncated: {produced}"
    );
    assert!(mb_s > 10.0, "split too slow: {mb_s:.1} MB/s in {elapsed:?}");
}
