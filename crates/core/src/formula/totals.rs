//! Whole-column numeric totals, computed once before the first formula row.
//!
//! `sum(amount)`, `average(amount)` and `count(amount)` are constant for every row of a
//! column, so evaluating them per row would re-derive the same number a hundred million
//! times. A pre-pass computes `(numeric sum, numeric count)` for each referenced column
//! and the formula loop reads it in O(1).
//!
//! The pre-pass reads the **base file**. Pending journal edits are not included; the
//! docs say so, and `edit export` is the command that materialises edits anyway. This
//! is the same boundary `lag`/`running_total` already live with: those also only see the
//! rows the streaming pass has handed them.

use crate::csv_reader::{parse_csv_record_with, resolve_column, QuoteStyle, RecordReader};
use crate::encoding;
use crate::types::Delimiter;
use std::collections::HashMap;
use std::io::{self, BufReader};
use std::path::Path;

/// Numeric sum and numeric count for each column. Non-numeric and empty cells are
/// skipped, which is what a spreadsheet's `SUM`/`COUNT` do over a mixed column.
pub fn column_totals(
    path: &Path,
    delimiter: Delimiter,
    quotes: QuoteStyle,
    columns: &[String],
) -> io::Result<HashMap<String, (f64, u64)>> {
    // With the columnar sidecar present, DuckDB reads one column instead of parsing
    // every field of every row. The cache is a cache: a stale or unreadable one falls
    // back to the CSV scan below rather than failing the export.
    #[cfg(feature = "sql")]
    {
        let cache = crate::sql_cache::sidecar(path);
        if crate::sql_cache::is_fresh(path) {
            if let Ok(entries) = crate::sql::parquet_totals(&cache, columns) {
                let totals = entries
                    .into_iter()
                    .map(|(name, sum, count)| (name, (sum, count)))
                    .collect();
                return Ok(totals);
            }
        }
    }

    let mut totals: HashMap<String, (f64, u64)> =
        columns.iter().map(|c| (c.clone(), (0.0, 0))).collect();
    let reader = encoding::open_decoded(path)?;
    let mut records = RecordReader::new_with(
        BufReader::with_capacity(4 * 1024 * 1024, reader),
        4 * 1024 * 1024,
        quotes,
    );
    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
    }
    let headers =
        parse_csv_record_with(&String::from_utf8_lossy(&record), delimiter.byte(), quotes);
    let mut wanted: Vec<(String, usize)> = Vec::new();
    for name in columns {
        match resolve_column(name, &headers) {
            Ok(index) => wanted.push((name.clone(), index)),
            Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidInput, e)),
        }
    }
    while records.next_record(&mut record)? {
        let text = String::from_utf8_lossy(&record);
        let fields = parse_csv_record_with(
            text.trim_end_matches(['\n', '\r']),
            delimiter.byte(),
            quotes,
        );
        for (name, index) in &wanted {
            let Some(cell) = fields.get(*index) else {
                continue;
            };
            let trimmed = cell.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(value) = trimmed.parse::<f64>() {
                if value.is_finite() {
                    if let Some(slot) = totals.get_mut(name) {
                        slot.0 += value;
                        slot.1 += 1;
                    }
                }
            }
        }
    }
    Ok(totals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totals_skip_empty_and_non_numeric_cells() {
        let path = std::env::temp_dir().join(format!("csvx_totals_{}.csv", std::process::id()));
        std::fs::write(&path, "city,amount\nA,10\nB,\nC,abc\nD,12.5\n").unwrap();
        let totals = column_totals(
            &path,
            Delimiter::Comma,
            QuoteStyle::Rfc4180,
            &["amount".to_string()],
        )
        .unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(totals.get("amount"), Some(&(22.5, 2)));
    }
}
