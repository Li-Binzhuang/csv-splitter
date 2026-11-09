//! Column profiles: what a column actually contains.
//!
//! `stats` answers "which values are most common" from a sample. This answers
//! "what is in this column at all" — and it does it in one streaming pass, so a
//! 10 GB file costs one read rather than a load:
//!
//!   * empty vs non-empty counts (exact, every row)
//!   * numeric summary: count, min, max, sum, mean (exact) and a median
//!     estimated from a bounded reservoir
//!   * distinct count, exact up to a cap and then reported as "at least N"
//!   * shortest/longest value, which catches stray padding and encoding damage
//!
//! Nothing here buffers a row: the same scanner that handles quoted newlines and
//! GBK feeds the accumulators.

use crate::csv_reader::{parse_csv_record, RecordReader};
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::collections::HashSet;
use std::io::{self, BufReader};
use std::path::Path;

/// Rows kept per column for the median estimate.
const MEDIAN_RESERVOIR: usize = 20_000;
/// Distinct values tracked per column before the count becomes an estimate.
pub const DEFAULT_DISTINCT_CAP: usize = 100_000;

#[derive(Clone, Debug, Default)]
pub struct NumericSummary {
    pub count: u64,
    pub min: f64,
    pub max: f64,
    pub sum: f64,
    pub mean: f64,
    /// Estimated from the reservoir; `None` when nothing was numeric.
    pub median: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct ColumnProfile {
    pub name: String,
    /// Rows seen (data rows, header excluded).
    pub rows: u64,
    pub empty: u64,
    pub non_empty: u64,
    /// Exact distinct count when below the cap.
    pub distinct: u64,
    /// True when the distinct count hit the cap and is a lower bound.
    pub distinct_truncated: bool,
    /// Most common values (up to 10), by frequency.
    pub top: Vec<(String, u64)>,
    /// Numeric summary, present only when every non-empty value parses as a
    /// number (a column of ids with one "N/A" is not numeric).
    pub numeric: Option<NumericSummary>,
    /// Present only when the column is not numeric: lexical extremes.
    pub shortest: Option<String>,
    pub longest: Option<String>,
    /// True when some value was rejected as non-numeric while parsing.
    pub mixed_types: bool,
    /// Rows whose field count differed from the header. Reported on the first
    /// column only (it is a property of the record, not of a field).
    pub ragged: u64,
}

struct Acc {
    rows: u64,
    empty: u64,
    distinct: HashSet<String>,
    truncated: bool,
    freq: Vec<(String, u64)>,
    reservoir: Vec<f64>,
    rng_state: u64,
    numeric_count: u64,
    numeric_sum: f64,
    numeric_min: f64,
    numeric_max: f64,
    non_numeric: u64,
    shortest: Option<String>,
    longest: Option<String>,
    non_empty: u64,
}

impl Acc {
    fn new() -> Self {
        Self {
            rows: 0,
            empty: 0,
            distinct: HashSet::new(),
            truncated: false,
            freq: Vec::new(),
            reservoir: Vec::with_capacity(64.min(MEDIAN_RESERVOIR)),
            rng_state: 0x2545_F491_4F6C_DD1D,
            numeric_count: 0,
            numeric_sum: 0.0,
            numeric_min: f64::INFINITY,
            numeric_max: f64::NEG_INFINITY,
            non_numeric: 0,
            shortest: None,
            longest: None,
            non_empty: 0,
        }
    }

    /// xorshift: a tiny deterministic RNG keeps reservoir sampling dependency
    /// free and reproducible across runs.
    fn next_rand(&mut self) -> u64 {
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng_state = x;
        x
    }

    fn observe(&mut self, value: &str, distinct_cap: usize) {
        self.rows += 1;
        if value.is_empty() {
            self.empty += 1;
            return;
        }
        self.non_empty += 1;

        // Distinctness: exact up to the cap, then a documented lower bound.
        if !self.truncated {
            if self.distinct.contains(value) {
                // Counted below via the frequency map.
            } else if self.distinct.len() < distinct_cap {
                self.distinct.insert(value.to_string());
            } else {
                self.truncated = true;
            }
        }

        // Frequency: keep a small top list rather than every value.
        if let Some(slot) = self.freq.iter_mut().find(|(v, _)| v == value) {
            slot.1 += 1;
        } else if self.freq.len() < 64 {
            self.freq.push((value.to_string(), 1));
        }

        // Length extremes.
        match &self.shortest {
            Some(s) if s.len() <= value.len() => {}
            _ => self.shortest = Some(value.to_string()),
        }
        match &self.longest {
            Some(s) if s.len() >= value.len() => {}
            _ => self.longest = Some(value.to_string()),
        }

        // Numeric accumulation.
        match value.trim().parse::<f64>() {
            Ok(n) if n.is_finite() => {
                self.numeric_count += 1;
                self.numeric_sum += n;
                if n < self.numeric_min {
                    self.numeric_min = n;
                }
                if n > self.numeric_max {
                    self.numeric_max = n;
                }
                if self.reservoir.len() < MEDIAN_RESERVOIR {
                    self.reservoir.push(n);
                } else {
                    let j = (self.next_rand() % self.numeric_count) as usize;
                    if j < MEDIAN_RESERVOIR {
                        self.reservoir[j] = n;
                    }
                }
            }
            _ => self.non_numeric += 1,
        }
    }

    fn finish(&mut self, name: &str) -> ColumnProfile {
        let numeric = if self.non_numeric == 0 && self.numeric_count > 0 {
            let mut sample = self.reservoir.clone();
            sample.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = if sample.is_empty() {
                None
            } else {
                Some(sample[sample.len() / 2])
            };
            Some(NumericSummary {
                count: self.numeric_count,
                min: self.numeric_min,
                max: self.numeric_max,
                sum: self.numeric_sum,
                mean: self.numeric_sum / self.numeric_count as f64,
                median,
            })
        } else {
            None
        };

        let mut top = self.freq.clone();
        top.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        top.truncate(10);

        let is_numeric = numeric.is_some();
        ColumnProfile {
            name: name.to_string(),
            rows: self.rows,
            ragged: 0,
            empty: self.empty,
            non_empty: self.non_empty,
            distinct: self.distinct.len() as u64,
            distinct_truncated: self.truncated,
            top,
            numeric,
            shortest: if is_numeric {
                None
            } else {
                self.shortest.take()
            },
            longest: if is_numeric {
                None
            } else {
                self.longest.take()
            },
            mixed_types: self.non_numeric > 0 && self.numeric_count > 0,
        }
    }
}

/// Profile every column in one streaming pass.
pub fn profile(
    path: &Path,
    delimiter: Delimiter,
    distinct_cap: usize,
    tx: &ProgressSender,
) -> io::Result<Vec<ColumnProfile>> {
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseStart {
        phase: "Profiling columns".into(),
        total_bytes: file_size,
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
        // Not an error: a 0-byte file simply has no columns, and saying so is
        // more useful than "empty file" on a stderr line.
        tx.send(ProgressEvent::PhaseDone {
            phase: "Profiling columns".into(),
        })
        .ok();
        return Ok(Vec::new());
    }
    let headers = parse_csv_record(&String::from_utf8_lossy(&record), delim);
    let cap = distinct_cap.max(1);
    let mut cols: Vec<Acc> = (0..headers.len()).map(|_| Acc::new()).collect();

    let mut bytes_read = record.len() as u64;
    let mut ragged: u64 = 0;
    let width = headers.len();
    while records.next_record(&mut record)? {
        bytes_read += record.len() as u64;
        let text = String::from_utf8_lossy(&record);
        let fields = parse_csv_record(text.trim_end_matches(['\n', '\r']), delim);
        if fields.len() != width {
            ragged += 1;
        }
        for (i, acc) in cols.iter_mut().enumerate() {
            let value = fields.get(i).map(String::as_str).unwrap_or("");
            acc.observe(value, cap);
        }
        if bytes_read % (1 << 22) < record.len() as u64 {
            tx.send(ProgressEvent::Progress { bytes: bytes_read }).ok();
        }
    }

    let mut profiles: Vec<ColumnProfile> = headers
        .iter()
        .zip(cols.iter_mut())
        .map(|(name, acc)| acc.finish(name))
        .collect();
    // Report the ragged count once, on the first column: it describes the
    // records, and repeating it per column would just be noise.
    if let Some(first) = profiles.first_mut() {
        first.ragged = ragged;
    }

    tx.send(ProgressEvent::PhaseDone {
        phase: "Profiling columns".into(),
    })
    .ok();
    Ok(profiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_prof_{tag}_{}_{}",
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

    fn fixture(csv: &str, tag: &str) -> (Dir, std::path::PathBuf) {
        let d = Dir::new(tag);
        let p = d.join("p.csv");
        std::fs::write(&p, csv).unwrap();
        (d, p)
    }

    fn run(csv: &str, tag: &str) -> Vec<ColumnProfile> {
        let (_d, p) = fixture(csv, tag);
        let (tx, _rx) = std::sync::mpsc::channel();
        profile(&p, Delimiter::Comma, DEFAULT_DISTINCT_CAP, &tx).unwrap()
    }

    #[test]
    fn summarises_numbers_exactly() {
        let p = run("amount\n10\n20\n30\n40\n", "num");
        let n = p[0].numeric.clone().expect("numeric column");
        assert_eq!(n.count, 4);
        assert_eq!(n.min, 10.0);
        assert_eq!(n.max, 40.0);
        assert_eq!(n.sum, 100.0);
        assert_eq!(n.mean, 25.0);
        assert_eq!(n.median, Some(30.0), "reservoir is exact below the cap");
    }

    #[test]
    fn a_single_non_number_makes_the_column_textual() {
        let p = run("x\n1\n2\nN/A\n", "mixed");
        assert!(p[0].numeric.is_none());
        assert!(p[0].mixed_types);
        assert_eq!(p[0].distinct, 3);
        assert_eq!(p[0].shortest.as_deref(), Some("1"));
        assert_eq!(p[0].longest.as_deref(), Some("N/A"));
    }

    #[test]
    fn counts_empties_and_distinct_separately() {
        let p = run("city\nCDMX\n\nCDMX\nPuebla\n", "empty");
        assert_eq!(p[0].rows, 4);
        assert_eq!(p[0].empty, 1);
        assert_eq!(p[0].non_empty, 3);
        assert_eq!(p[0].distinct, 2);
        assert_eq!(p[0].top[0], ("CDMX".to_string(), 2));
    }

    #[test]
    fn distinct_cap_is_reported_not_hidden() {
        let mut csv = String::from("v\n");
        for i in 0..50 {
            csv.push_str(&format!("{i}\n"));
        }
        let (_d, p) = fixture(&csv, "cap");
        let (tx, _rx) = std::sync::mpsc::channel();
        let prof = profile(&p, Delimiter::Comma, 10, &tx).unwrap();
        assert!(prof[0].distinct_truncated);
        assert!(prof[0].distinct <= 10);
    }

    #[test]
    fn ragged_rows_are_counted_once_on_the_first_column() {
        // The middle row has an extra field: a real-world export defect.
        let p = run("a,b\n1,x\n2,y,extra\n3,z\n", "ragged");
        assert_eq!(p[0].ragged, 1);
        assert_eq!(p[1].ragged, 0, "reported once, not per column");
    }

    #[test]
    fn quoted_newlines_do_not_shift_columns() {
        let p = run("id,note\n1,\"a\nb\"\n2,plain\n", "quoted");
        assert_eq!(p[0].rows, 2);
        assert_eq!(p[1].rows, 2);
        assert_eq!(p[1].top[0].1, 1, "one record per row, not per line");
    }
}
