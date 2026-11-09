//! Partition-by-column behaviour: one file per distinct value, nothing lost.

use csv_splitter_core::decompress::decompress_file;
use csv_splitter_core::partition::{run_partition, PartitionConfig};
use csv_splitter_core::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "csvs_part_{tag}_{}_{}",
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

fn run(cfg: PartitionConfig) -> Vec<ProgressEvent> {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || run_partition(cfg, tx));
    let events: Vec<ProgressEvent> = rx.iter().collect();
    handle.join().unwrap();
    events
}

fn base(input: &Path, output: &Path, column: &str) -> PartitionConfig {
    PartitionConfig {
        input: input.to_path_buf(),
        output: Some(output.to_path_buf()),
        column: column.to_string(),
        compress_format: CompressFormat::None,
        compress_level: 3,
        buf_mb: 1,
        delimiter: Delimiter::Comma,
        max_partitions: 512,
    }
}

#[test]
fn one_file_per_distinct_value() {
    let dir = TempDir::new("basic");
    let input = dir.join("orders.csv");
    let archive = dir.join("parts.tar");
    let out_dir = dir.join("out");
    std::fs::write(
        &input,
        "id,city,amount\n1,CDMX,10\n2,Puebla,20\n3,CDMX,30\n4,\"New York\",40\n5,Puebla,50\n",
    )
    .unwrap();

    let events = run(base(&input, &archive, "city"));
    assert!(
        !events.iter().any(|e| matches!(e, ProgressEvent::Error(_))),
        "{events:?}"
    );

    std::fs::create_dir_all(&out_dir).unwrap();
    let (tx, _rx) = mpsc::channel();
    let files = decompress_file(&archive, &out_dir, &tx).unwrap();
    assert_eq!(files.len(), 3, "expected 3 partitions: {files:?}");

    let mut all = String::new();
    for f in &files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let text = std::fs::read_to_string(f).unwrap();
        // Each partition repeats the header and only its own value.
        assert!(text.starts_with("id,city,amount\n"), "{name}: {text}");
        let value = name.split('.').next().unwrap().to_string();
        let expected = match value.as_str() {
            "CDMX" => 2,
            "Puebla" => 2,
            "New_York" => 1,
            other => panic!("unexpected partition file {other}"),
        };
        assert_eq!(text.lines().count() - 1, expected, "{name}: {text}");
        all.push_str(&text);
    }

    // Nothing was dropped: 5 data records across the partitions.
    let records = all.lines().filter(|l| !l.starts_with("id,")).count();
    assert_eq!(records, 5);
}

#[test]
fn empty_value_gets_its_own_partition() {
    let dir = TempDir::new("empty");
    let input = dir.join("d.csv");
    let archive = dir.join("p.tar");
    let out_dir = dir.join("out");
    std::fs::write(&input, "id,city\n1,\n2,CDMX\n").unwrap();

    let events = run(base(&input, &archive, "city"));
    assert!(!events.iter().any(|e| matches!(e, ProgressEvent::Error(_))));

    std::fs::create_dir_all(&out_dir).unwrap();
    let (tx, _rx) = mpsc::channel();
    let files = decompress_file(&archive, &out_dir, &tx).unwrap();
    let names: Vec<String> = files
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert!(names.iter().any(|n| n.starts_with("_empty")), "{names:?}");
}

#[test]
fn high_cardinality_column_is_rejected() {
    let dir = TempDir::new("card");
    let input = dir.join("d.csv");
    let archive = dir.join("p.tar");
    let mut csv = String::from("id,uid\n");
    for i in 0..20 {
        csv.push_str(&format!("{i},u{i}\n"));
    }
    std::fs::write(&input, csv).unwrap();

    let mut cfg = base(&input, &archive, "uid");
    cfg.max_partitions = 5;
    let events = run(cfg);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ProgressEvent::Error(msg) if msg.contains("distinct values"))),
        "{events:?}"
    );
}

#[test]
fn unknown_column_is_an_error() {
    let dir = TempDir::new("col");
    let input = dir.join("d.csv");
    let archive = dir.join("p.tar");
    std::fs::write(&input, "id,city\n1,CDMX\n").unwrap();

    let events = run(base(&input, &archive, "nope"));
    assert!(events.iter().any(|e| matches!(e, ProgressEvent::Error(_))));
}
