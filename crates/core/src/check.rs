//! Self-checks for derived files: "did the operation keep what it promised?"
//!
//! Every write path in this tool makes a claim:
//!
//! | operation | claim |
//! |---|---|
//! | `split` / `merge` | the bytes come back identical (already verified) |
//! | `filter -o` | every matched row is in the output, nothing else is |
//! | `columns` | same rows, exactly the kept columns |
//! | `dedupe` | `kept + removed == scanned`, output has no duplicates |
//! | `groupby` | one output row per group |
//! | `partition` | the partitions together hold every source row |
//! | `sort` | same rows, reordered |
//!
//! `verify` covers the first line byte-for-byte. This module covers the rest by
//! counting: it re-reads the result and checks the counts and the shapes. It is
//! a full extra pass, which is why it is opt-in (`--verify`) rather than always
//! on — but it is cheap next to the operation itself and turns "I think it
//! worked" into "I know it did".

use crate::csv_reader::RecordReader;
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::io::{self, BufReader};
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// Data records (header excluded).
    pub rows: u64,
    /// Records whose field count differs from the header.
    pub ragged: u64,
    /// Columns in the header.
    pub columns: usize,
}

/// Cheap structural count of a CSV: rows, columns, ragged records.
pub fn count(path: &Path, delimiter: Delimiter, tx: &ProgressSender) -> io::Result<Counts> {
    let reader = encoding::open_decoded(path)?;
    let mut records = RecordReader::new(
        BufReader::with_capacity(4 * 1024 * 1024, reader),
        4 * 1024 * 1024,
    );
    let delim = delimiter.byte();
    let mut record = Vec::with_capacity(64 * 1024);

    if !records.next_record(&mut record)? {
        return Ok(Counts::default());
    }
    let header = crate::csv_reader::parse_csv_record(
        String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
        delim,
    );
    let width = header.len();
    let mut counts = Counts {
        columns: width,
        ..Default::default()
    };

    while records.next_record(&mut record)? {
        let fields = crate::csv_reader::parse_csv_record(
            String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
            delim,
        );
        counts.rows += 1;
        if fields.len() != width {
            counts.ragged += 1;
        }
        if counts.rows.is_multiple_of(500_000) {
            tx.send(ProgressEvent::Progress { bytes: counts.rows }).ok();
        }
    }
    Ok(counts)
}

/// A claim an operation makes about its result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Expectation {
    /// Exact number of data rows, when the operation can predict it.
    pub rows: Option<u64>,
    /// Exact number of columns, when the operation can predict it.
    pub columns: Option<usize>,
    /// How many ragged records the *source* had and were copied through.
    ///
    /// A `filter` that passes rows through verbatim must not be blamed for the
    /// source's raggedness — but it must not invent any either.
    pub ragged_expected: u64,
}

impl Expectation {
    pub fn rows(rows: u64) -> Self {
        Self {
            rows: Some(rows),
            ..Default::default()
        }
    }

    pub fn columns(columns: usize) -> Self {
        Self {
            columns: Some(columns),
            ..Default::default()
        }
    }

    pub fn with_columns(mut self, columns: usize) -> Self {
        self.columns = Some(columns);
        self
    }

    pub fn with_ragged(mut self, ragged: u64) -> Self {
        self.ragged_expected = ragged;
        self
    }
}

#[derive(Clone, Debug, Default)]
pub struct CheckReport {
    pub expected: bool,
    pub counts: Counts,
    /// Human-readable reasons the expectation was not met.
    pub problems: Vec<String>,
}

impl CheckReport {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Check a derived file against what the operation claimed.
pub fn check(
    result: &Path,
    delimiter: Delimiter,
    expectation: &Expectation,
    tx: &ProgressSender,
) -> io::Result<CheckReport> {
    let counts = count(result, delimiter, tx)?;
    let mut problems = Vec::new();

    if let Some(rows) = expectation.rows {
        if counts.rows != rows {
            problems.push(format!("expected {rows} data rows, found {}", counts.rows));
        }
    }
    if let Some(columns) = expectation.columns {
        if counts.columns != columns {
            problems.push(format!(
                "expected {columns} columns, found {}",
                counts.columns
            ));
        }
    }
    // Ragged rows are only a problem when the operation did not expect them: a
    // filter that copies rows verbatim inherits the source's raggedness, but it
    // must not create any.
    if counts.ragged > expectation.ragged_expected {
        problems.push(format!(
            "{} row(s) in the result have an unexpected field count (source had {})",
            counts.ragged, expectation.ragged_expected
        ));
    }

    Ok(CheckReport {
        expected: true,
        counts,
        problems,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_check_{tag}_{}_{}",
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

    fn counts(csv: &str, tag: &str) -> Counts {
        let d = Dir::new(tag);
        let p = d.join("c.csv");
        std::fs::write(&p, csv).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        count(&p, Delimiter::Comma, &tx).unwrap()
    }

    #[test]
    fn counts_rows_columns_and_ragged_records() {
        let c = counts("a,b\n1,2\n3,4\n", "plain");
        assert_eq!(c.rows, 2);
        assert_eq!(c.columns, 2);
        assert_eq!(c.ragged, 0);

        let c = counts("a,b\n1,2\n3,4,5\n", "ragged");
        assert_eq!(c.rows, 2);
        assert_eq!(c.ragged, 1);
    }

    #[test]
    fn quoted_newlines_count_as_one_row() {
        let c = counts("a,b\n1,\"x\ny\"\n2,z\n", "quoted");
        assert_eq!(c.rows, 2);
        assert_eq!(c.ragged, 0);
    }

    #[test]
    fn an_empty_file_has_no_columns() {
        let c = counts("", "empty");
        assert_eq!(c.rows, 0);
        assert_eq!(c.columns, 0);
    }

    #[test]
    fn mismatches_are_reported_not_hidden() {
        let d = Dir::new("report");
        let p = d.join("r.csv");
        std::fs::write(&p, "a,b\n1,2\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();

        let report = check(&p, Delimiter::Comma, &Expectation::rows(1), &tx).unwrap();
        assert!(report.ok(), "{:?}", report.problems);

        let report = check(&p, Delimiter::Comma, &Expectation::rows(5), &tx).unwrap();
        assert!(!report.ok());
        assert!(
            report.problems[0].contains("expected 5"),
            "{:?}",
            report.problems
        );

        let report = check(
            &p,
            Delimiter::Comma,
            &Expectation::rows(1).with_columns(3),
            &tx,
        )
        .unwrap();
        assert!(!report.ok());
        assert!(report.problems.iter().any(|p| p.contains("3 columns")));
    }

    #[test]
    fn ragged_output_fails_an_otherwise_correct_count() {
        let d = Dir::new("ragged_out");
        let p = d.join("r.csv");
        std::fs::write(&p, "a,b\n1,2\n3,4,5\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let report = check(&p, Delimiter::Comma, &Expectation::rows(2), &tx).unwrap();
        assert!(!report.ok(), "ragged rows must be flagged");
        assert!(report.problems.iter().any(|p| p.contains("field count")));

        // The same result is fine when the raggedness was already in the source.
        let report = check(
            &p,
            Delimiter::Comma,
            &Expectation::rows(2).with_ragged(1),
            &tx,
        )
        .unwrap();
        assert!(report.ok(), "{:?}", report.problems);
    }
}
