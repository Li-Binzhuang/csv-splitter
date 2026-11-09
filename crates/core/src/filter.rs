//! Column filters over a whole file.
//!
//! The viewer needs "which rows match `city = CDMX`" without loading the file:
//! this scans sequentially (decoding handles GBK/UTF-16), returns **data-row
//! numbers**, and the caller pages through the hits with the sparse index.
//!
//! Matching is deliberately simple — equality, contains, numeric comparison and
//! empty checks. Anything more elaborate belongs in SQL, not in a file tool.

use crate::csv_reader::{parse_csv_record, RecordReader};
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::cmp::Ordering;
use std::io::{self, BufReader};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Contains,
    Gt,
    Gte,
    Lt,
    Lte,
    Empty,
    NotEmpty,
}

impl Op {
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "!=",
            Self::Contains => "~",
            Self::Gt => ">",
            Self::Gte => ">=",
            Self::Lt => "<",
            Self::Lte => "<=",
            Self::Empty => "empty",
            Self::NotEmpty => "notempty",
        }
    }
}

/// `city=CDMX`, `amount>1000`, `note~refund`, `email empty`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterSpec {
    pub column: String,
    pub op: Op,
    pub value: String,
}

impl FilterSpec {
    pub fn parse(expr: &str) -> Result<Self, String> {
        let expr = expr.trim();
        if expr.is_empty() {
            return Err("empty filter expression".to_string());
        }
        // Longest operators first so `>=` beats `>`.
        const OPS: &[(&str, Op)] = &[
            ("!=", Op::Ne),
            (">=", Op::Gte),
            ("<=", Op::Lte),
            ("=", Op::Eq),
            ("~", Op::Contains),
            (">", Op::Gt),
            ("<", Op::Lt),
        ];
        for (token, op) in OPS {
            if let Some((col, val)) = expr.split_once(token) {
                let column = col.trim().to_string();
                if column.is_empty() {
                    return Err(format!("missing column name in '{expr}'"));
                }
                return Ok(Self {
                    column,
                    op: *op,
                    value: val.trim().to_string(),
                });
            }
        }
        // `col empty` / `col notempty`
        let mut parts = expr.splitn(2, char::is_whitespace);
        let column = parts.next().unwrap_or("").trim().to_string();
        let word = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        match word.as_str() {
            "empty" => Ok(Self {
                column,
                op: Op::Empty,
                value: String::new(),
            }),
            "notempty" | "not-empty" => Ok(Self {
                column,
                op: Op::NotEmpty,
                value: String::new(),
            }),
            _ => Err(format!(
                "cannot parse '{expr}'; use col=value, col~text, col>number, col empty"
            )),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    /// 0-based data rows (header excluded) that matched, in file order.
    pub rows: Vec<u64>,
    pub scanned: u64,
    pub matched: u64,
    /// True when `limit` cut the result short.
    pub truncated: bool,
}

impl ScanResult {
    /// Row numbers as a user would count them (1-based, header excluded).
    pub fn row_numbers(&self) -> Vec<u64> {
        self.rows.iter().map(|r| r + 1).collect()
    }
}

/// Scan `path` and collect up to `limit` matching data rows.
pub fn scan(
    path: &Path,
    delimiter: Delimiter,
    spec: &FilterSpec,
    limit: usize,
    tx: &ProgressSender,
) -> io::Result<ScanResult> {
    let reader = encoding::open_decoded(path)?;
    let mut records = RecordReader::new(
        BufReader::with_capacity(4 * 1024 * 1024, reader),
        4 * 1024 * 1024,
    );
    let delim = delimiter.byte();

    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
    }
    let headers = parse_csv_record(&String::from_utf8_lossy(&record), delim);
    let col_idx = headers
        .iter()
        .position(|h| h == &spec.column)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("column '{}' not found", spec.column),
            )
        })?;
    tx.send(ProgressEvent::PhaseStart {
        phase: format!("Filtering {}", spec.column),
        total_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    })
    .ok();

    let mut result = ScanResult::default();
    let limit = limit.max(1);
    while records.next_record(&mut record)? {
        result.scanned += 1;
        let text = String::from_utf8_lossy(&record);
        let fields = parse_csv_record(text.trim_end_matches(['\n', '\r']), delim);
        let cell = fields.get(col_idx).map(String::as_str).unwrap_or("");
        if matches(cell, spec) {
            result.matched += 1;
            if result.rows.len() < limit {
                result.rows.push(result.scanned - 1);
            } else {
                result.truncated = true;
            }
        }
        if result.scanned % 200_000 == 0 {
            tx.send(ProgressEvent::Progress {
                bytes: result.scanned,
            })
            .ok();
        }
    }

    tx.send(ProgressEvent::PhaseDone {
        phase: format!("Filtering {}", spec.column),
    })
    .ok();
    Ok(result)
}

/// Single-cell predicate.
pub fn matches(cell: &str, spec: &FilterSpec) -> bool {
    match spec.op {
        Op::Empty => cell.trim().is_empty(),
        Op::NotEmpty => !cell.trim().is_empty(),
        Op::Eq => cell == spec.value,
        Op::Ne => cell != spec.value,
        Op::Contains => cell.contains(&spec.value),
        Op::Gt | Op::Gte | Op::Lt | Op::Lte => {
            // Numeric when both sides parse, string order otherwise (so ISO dates
            // and zero-padded ids behave sensibly).
            let ord = match (cell.trim().parse::<f64>(), spec.value.parse::<f64>()) {
                (Ok(a), Ok(b)) => a.partial_cmp(&b),
                _ => Some(cell.trim().cmp(spec.value.as_str())),
            };
            match ord {
                Some(Ordering::Greater) => matches!(spec.op, Op::Gt | Op::Gte),
                Some(Ordering::Equal) => matches!(spec.op, Op::Gte | Op::Lte),
                Some(Ordering::Less) => matches!(spec.op, Op::Lt | Op::Lte),
                None => false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(expr: &str) -> FilterSpec {
        FilterSpec::parse(expr).unwrap()
    }

    #[test]
    fn parses_operators() {
        let s = spec("city=CDMX");
        assert_eq!((s.column.as_str(), s.op), ("city", Op::Eq));
        assert_eq!(s.value, "CDMX");

        assert_eq!(spec("amount>=100").op, Op::Gte);
        assert_eq!(spec("amount>100").value, "100");
        assert_eq!(spec("note~refund").op, Op::Contains);
        assert_eq!(spec("email empty").op, Op::Empty);
        assert_eq!(spec("email notempty").op, Op::NotEmpty);
        assert!(FilterSpec::parse("nonsense").is_err());
        assert!(FilterSpec::parse("=x").is_err());
    }

    #[test]
    fn matches_semantics() {
        assert!(matches("CDMX", &spec("city=CDMX")));
        assert!(!matches("Puebla", &spec("city=CDMX")));
        assert!(matches("refund-2026", &spec("note~refund")));
        assert!(matches("1200", &spec("amt>1000")));
        assert!(matches("1000", &spec("amt>=1000")));
        assert!(matches("1000", &spec("amt<=1000")));
        assert!(!matches("1000", &spec("amt>1000")));
        assert!(matches("  ", &spec("x empty")));
        assert!(!matches("a", &spec("x empty")));
        // Non-numeric falls back to string ordering.
        assert!(matches("2026-02-01", &spec("d>2026-01-01")));
    }
}
