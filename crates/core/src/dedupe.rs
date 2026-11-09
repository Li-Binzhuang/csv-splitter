//! Duplicate removal, streaming.
//!
//! "Is this file clean?" is one of the first questions anyone asks of a CSV, and
//! `dedupe` answers it: keep the first occurrence of each key, write everything
//! else, and report how much was removed.
//!
//! Memory is the only real design constraint. Keeping every key as a `String`
//! would cost gigabytes on a large file, so keys are hashed to `u128` and only
//! the hashes are retained:
//!
//!   * 16 bytes per distinct key, so 10 million keys is ~160 MB and 100 million
//!     still fits in a modern machine;
//!   * a 128-bit hash collides for 10 million keys with probability ~1e-24, which
//!     is far below the chance of a disk error — safe to treat as exact;
//!   * the caller can cap the key count; past the cap the run fails loudly
//!     instead of silently dropping unique rows.

use crate::csv_reader::parse_csv_record;
use crate::csv_reader::RecordReader;
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::collections::HashSet;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;

/// Default ceiling on retained keys (~1.6 GB of hashes).
pub const DEFAULT_MAX_KEYS: usize = 100_000_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DedupeSummary {
    pub scanned: u64,
    pub kept: u64,
    pub removed: u64,
    pub bytes: u64,
    /// Kept rows whose field count differed from the header (inherited verbatim
    /// from the source).
    pub kept_ragged: u64,
    /// Columns used as the key; empty means "the whole row".
    pub key_columns: Vec<String>,
}

/// Remove rows whose key was already seen, writing the survivors to `output`.
///
/// `keys` empty means the whole record (after normalising a trailing newline) is
/// the key, so byte-identical duplicates collapse.
pub fn dedupe(
    path: &Path,
    delimiter: Delimiter,
    keys: &[String],
    output: &Path,
    max_keys: usize,
    tx: &ProgressSender,
) -> io::Result<DedupeSummary> {
    let total_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseStart {
        phase: "Removing duplicates".into(),
        total_bytes,
    })
    .ok();

    let reader = encoding::open_decoded(path)?;
    let mut records = RecordReader::new(
        BufReader::with_capacity(8 * 1024 * 1024, reader),
        8 * 1024 * 1024,
    );
    let delim = delimiter.byte();

    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
    }
    let headers = parse_csv_record(&String::from_utf8_lossy(&record), delim);
    let key_indices: Vec<usize> = if keys.is_empty() {
        Vec::new()
    } else {
        let mut found = Vec::with_capacity(keys.len());
        for name in keys {
            let idx = crate::csv_reader::resolve_column(name, &headers)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            found.push(idx);
        }
        found
    };
    let summary_keys: Vec<String> = key_indices.iter().map(|&i| headers[i].clone()).collect();

    let mut out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(output)?);
    let mut header = record.clone();
    if !header.ends_with(b"\n") {
        header.push(b'\n');
    }
    out.write_all(&header)?;

    let width = headers.len();
    let mut seen: HashSet<u128> = HashSet::new();
    let mut summary = DedupeSummary {
        key_columns: summary_keys,
        ..Default::default()
    };
    let max_keys = max_keys.max(1);

    while records.next_record(&mut record)? {
        summary.scanned += 1;
        let hash = key_hash(&record, &key_indices, delim);
        if !seen.insert(hash) {
            summary.removed += 1;
            continue;
        }
        if seen.len() > max_keys {
            return Err(io::Error::other(format!(
                "more than {max_keys} distinct rows; raise --max-keys or dedupe by column"
            )));
        }
        if parse_csv_record(
            String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
            delim,
        )
        .len()
            != width
        {
            summary.kept_ragged += 1;
        }
        let mut line = record.clone();
        if !line.ends_with(b"\n") {
            line.push(b'\n');
        }
        out.write_all(&line)?;
        summary.kept += 1;
        if summary.scanned.is_multiple_of(200_000) {
            tx.send(ProgressEvent::Progress {
                bytes: summary.scanned,
            })
            .ok();
        }
    }
    out.flush()?;
    summary.bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseDone {
        phase: "Removing duplicates".into(),
    })
    .ok();
    Ok(summary)
}

/// 128-bit fingerprint of the chosen key cells (or the whole record).
fn key_hash(record: &[u8], indices: &[usize], delim: u8) -> u128 {
    if indices.is_empty() {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        record.hash(&mut hasher);
        let a = hasher.finish();
        let mut second = std::collections::hash_map::DefaultHasher::new();
        a.hash(&mut second);
        return ((a as u128) << 64) | second.finish() as u128;
    }
    let text = String::from_utf8_lossy(record);
    let trimmed = text.trim_end_matches(['\n', '\r']);
    let fields = parse_csv_record(trimmed, delim);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for &i in indices {
        fields
            .get(i)
            .map(String::as_str)
            .unwrap_or("")
            .hash(&mut hasher);
    }
    let a = hasher.finish();
    let mut second = std::collections::hash_map::DefaultHasher::new();
    a.hash(&mut second);
    ((a as u128) << 64) | second.finish() as u128
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_dedupe_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn join(&self, n: &str) -> std::path::PathBuf {
            self.0.join(n)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn run(csv: &str, keys: &[&str], tag: &str) -> (DedupeSummary, String) {
        let d = Dir::new(tag);
        let input = d.join("in.csv");
        let output = d.join("out.csv");
        std::fs::write(&input, csv).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let summary = dedupe(
            &input,
            Delimiter::Comma,
            &keys.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
            &output,
            DEFAULT_MAX_KEYS,
            &tx,
        )
        .unwrap();
        let text = std::fs::read_to_string(&output).unwrap();
        (summary, text)
    }

    #[test]
    fn whole_row_duplicates_collapse_to_the_first() {
        let (s, text) = run("a,b\n1,x\n2,y\n1,x\n", &[], "row");
        assert_eq!(s.scanned, 3);
        assert_eq!(s.kept, 2);
        assert_eq!(s.removed, 1);
        assert_eq!(text, "a,b\n1,x\n2,y\n", "first occurrence wins");
    }

    #[test]
    fn key_columns_ignore_other_differences() {
        let (s, text) = run("id,v\n1,a\n1,b\n2,c\n", &["id"], "key");
        assert_eq!(s.kept, 2);
        assert_eq!(s.removed, 1);
        assert_eq!(text, "id,v\n1,a\n2,c\n");
        assert_eq!(s.key_columns, vec!["id"]);
    }

    #[test]
    fn quoted_newlines_hash_as_one_record() {
        let (s, text) = run("id,note\n1,\"a\nb\"\n1,\"a\nb\"\n", &[], "quoted");
        assert_eq!(s.scanned, 2);
        assert_eq!(s.kept, 1);
        assert_eq!(text, "id,note\n1,\"a\nb\"\n");
    }

    #[test]
    fn unknown_key_column_is_a_client_error() {
        let d = Dir::new("bad");
        let input = d.join("in.csv");
        std::fs::write(&input, "id\n1\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = dedupe(
            &input,
            Delimiter::Comma,
            &["nope".to_string()],
            &d.join("out.csv"),
            DEFAULT_MAX_KEYS,
            &tx,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn key_cap_fails_loudly_instead_of_dropping_rows() {
        let d = Dir::new("cap");
        let input = d.join("in.csv");
        let mut csv = String::from("v\n");
        for i in 0..100 {
            csv.push_str(&format!("{i}\n"));
        }
        std::fs::write(&input, csv).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = dedupe(&input, Delimiter::Comma, &[], &d.join("out.csv"), 10, &tx).unwrap_err();
        assert!(err.to_string().contains("distinct rows"), "{err}");
    }
}
