//! Throughput benchmarks for the operations that matter.
//!
//! Run with `cargo bench -p csv-splitter-core`. These are informational, not
//! assertions — shared CI runners are too noisy for hard thresholds. The
//! regression guard that *does* fail the build lives in
//! `crates/core/tests/performance.rs` with deliberately generous bounds.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use csv_splitter_core::index::SparseIndex;
use csv_splitter_core::splitter::run_split;
use csv_splitter_core::types::*;
use std::path::PathBuf;
use std::sync::mpsc;

struct Fixture {
    dir: PathBuf,
    csv: PathBuf,
}

impl Fixture {
    fn new(rows: usize) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "csvs_bench_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("bench.csv");
        let mut text = String::with_capacity(rows * 64);
        text.push_str("id,cust_no,city,amount,note\n");
        for i in 0..rows {
            text.push_str(&format!(
                "{i},C{:06},city{},{}",
                i % 50_000,
                i % 7,
                (i % 90_000) as f64 / 1.0
            ));
            // Every 10th row carries a quoted newline so the record scanner is
            // exercised rather than the trivial case.
            if i % 10 == 0 {
                text.push_str(",\"a\nb\"\n");
            } else {
                text.push_str(",ok\n");
            }
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

fn bench_index_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("index_build");
    group.sample_size(10);
    for rows in [100_000usize, 1_000_000] {
        let fx = Fixture::new(rows);
        group.throughput(Throughput::Bytes(fx.bytes()));
        group.bench_with_input(BenchmarkId::from_parameter(rows), &fx, |b, fx| {
            b.iter(|| SparseIndex::build(&fx.csv, 1000).unwrap())
        });
    }
    group.finish();
}

fn bench_random_access(c: &mut Criterion) {
    let fx = Fixture::new(1_000_000);
    let index = SparseIndex::build(&fx.csv, 1000).unwrap();
    let total = index.data_rows();

    let mut group = c.benchmark_group("random_access");
    group.sample_size(50);
    for label in ["head", "middle", "tail"] {
        let row = match label {
            "head" => 10u64,
            "middle" => total / 2,
            _ => total - 20,
        };
        group.bench_function(label, |b| {
            b.iter(|| index.read_rows(&fx.csv, row, 20).unwrap())
        });
    }
    group.finish();
}

fn bench_split(c: &mut Criterion) {
    let mut group = c.benchmark_group("split");
    group.sample_size(10);
    let fx = Fixture::new(500_000);
    group.throughput(Throughput::Bytes(fx.bytes()));
    for chunks in [1usize, 4] {
        group.bench_with_input(
            BenchmarkId::from_parameter(chunks),
            &chunks,
            |b, &chunks| {
                b.iter(|| {
                    let out = fx.dir.join(format!("bench_{chunks}.tar"));
                    let (tx, rx) = mpsc::channel();
                    drop(rx);
                    run_split(
                        SplitConfig {
                            input: fx.csv.clone(),
                            output: Some(out.clone()),
                            splits: chunks,
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
                    std::fs::remove_file(&out).ok();
                })
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_index_build, bench_random_access, bench_split);
criterion_main!(benches);
