//! Whole-column totals, computed once before the first formula row.
//!
//! `sum(amount)`, `average(amount)` and `count(amount)` are constant for every row of a
//! column, so evaluating them per row would re-derive the same number a hundred million
//! times. A pre-pass computes `(numeric sum, numeric count)` for each referenced column
//! and the formula loop reads it in O(1). `countif`/`sumif` ride along in the same pass,
//! with the criteria compiled once through the same parser `filter --where` uses.
//!
//! The pre-pass reads the **base file**. Pending journal edits are not included; the
//! docs say so, and `edit export` is the command that materialises edits anyway. This
//! is the same boundary `lag`/`running_total` already live with.

use super::RowNeeds;
use crate::csv_reader::{parse_csv_record_with, resolve_column, QuoteStyle, RecordReader};
use crate::encoding;
use crate::filter::{self, FilterSpec};
use crate::types::Delimiter;
use std::collections::HashMap;
use std::io::{self, BufReader};
use std::path::Path;

#[derive(Debug, Default)]
pub struct ColumnTotals {
    /// Numeric sum and numeric count for each column.
    pub plain: HashMap<String, (f64, u64)>,
    /// `(column, criteria)` -> (numeric sum, count of matching cells).
    pub filtered: HashMap<(String, String), (f64, u64)>,
}

/// Compile a spreadsheet criteria, where `">1000"` means `amount>1000` and a bare
/// `CDMX` means `amount=CDMX`. `<>` is Excel's not-equal; the filter language spells
/// it `!=`.
fn compile_criteria(column: &str, criteria: &str) -> Result<FilterSpec, String> {
    let trimmed = criteria.trim();
    let expression = if let Some(rest) = trimmed.strip_prefix("<>") {
        format!("{column}!={rest}")
    } else if matches!(
        trimmed.chars().next(),
        Some('=') | Some('<') | Some('>') | Some('~')
    ) {
        format!("{column}{trimmed}")
    } else {
        format!("{column}={trimmed}")
    };
    FilterSpec::parse(&expression)
}

/// Numeric sum and count for every column/criteria the formulas asked for.
pub fn column_totals(
    path: &Path,
    delimiter: Delimiter,
    quotes: QuoteStyle,
    needs: &RowNeeds,
) -> io::Result<ColumnTotals> {
    let mut out = ColumnTotals::default();
    if needs.aggregates.is_empty() && needs.filtered.is_empty() {
        return Ok(out);
    }

    // With the columnar sidecar present, DuckDB reads one column instead of parsing
    // every field of every row. Criteria are evaluated here rather than translated to
    // SQL, so a countif/sumif request keeps the CSV path; a stale cache falls back too.
    #[cfg(feature = "sql")]
    if needs.filtered.is_empty() {
        let cache = crate::sql_cache::sidecar(path);
        if crate::sql_cache::is_fresh(path) {
            if let Ok(entries) = crate::sql::parquet_totals(&cache, &needs.aggregates) {
                out.plain = entries
                    .into_iter()
                    .map(|(name, sum, count)| (name, (sum, count)))
                    .collect();
                return Ok(out);
            }
        }
    }

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

    let mut plain: Vec<(String, usize)> = Vec::new();
    for name in &needs.aggregates {
        match resolve_column(name, &headers) {
            Ok(index) => plain.push((name.clone(), index)),
            Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidInput, e)),
        }
    }
    let mut filtered: Vec<(String, String, usize, FilterSpec)> = Vec::new();
    for (column, criteria) in &needs.filtered {
        let index = resolve_column(column, &headers)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let spec = compile_criteria(column, criteria)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        filtered.push((column.clone(), criteria.clone(), index, spec));
        out.filtered
            .insert((column.clone(), criteria.clone()), (0.0, 0));
    }
    for (name, _) in &plain {
        out.plain.insert(name.clone(), (0.0, 0));
    }

    while records.next_record(&mut record)? {
        let text = String::from_utf8_lossy(&record);
        let fields = parse_csv_record_with(
            text.trim_end_matches(['\n', '\r']),
            delimiter.byte(),
            quotes,
        );
        for (name, index) in &plain {
            let Some(cell) = fields.get(*index) else {
                continue;
            };
            let trimmed = cell.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(value) = trimmed.parse::<f64>() {
                if value.is_finite() {
                    if let Some(slot) = out.plain.get_mut(name) {
                        slot.0 += value;
                        slot.1 += 1;
                    }
                }
            }
        }
        for (column, criteria, index, spec) in &filtered {
            let Some(cell) = fields.get(*index) else {
                continue;
            };
            // An ordering criterion is numeric: a spreadsheet's COUNTIF(">100") does not
            // count the cell "not-a-number" just because 'n' sorts after '1'.
            if matches!(
                spec.op,
                filter::Op::Gt | filter::Op::Gte | filter::Op::Lt | filter::Op::Lte
            ) && cell.trim().parse::<f64>().is_err()
            {
                continue;
            }
            if !filter::matches(cell, spec) {
                continue;
            }
            if let Some(slot) = out.filtered.get_mut(&(column.clone(), criteria.clone())) {
                slot.1 += 1;
                if let Ok(value) = cell.trim().parse::<f64>() {
                    if value.is_finite() {
                        slot.0 += value;
                    }
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(name: &str, body: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("csvx_totals_{}_{}", name, std::process::id()));
        std::fs::write(&path, body).unwrap();
        path
    }

    fn needs(plain: &[&str], filtered: &[(&str, &str)]) -> RowNeeds {
        RowNeeds {
            lags: Vec::new(),
            running: Vec::new(),
            aggregates: plain.iter().map(|c| c.to_string()).collect(),
            filtered: filtered
                .iter()
                .map(|(c, crit)| (c.to_string(), crit.to_string()))
                .collect(),
        }
    }

    #[test]
    fn totals_skip_empty_and_non_numeric_cells() {
        let path = write("plain", "city,amount\nA,10\nB,\nC,abc\nD,12.5\n");
        let totals = column_totals(
            &path,
            Delimiter::Comma,
            QuoteStyle::Rfc4180,
            &needs(&["amount"], &[]),
        )
        .unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(totals.plain.get("amount"), Some(&(22.5, 2)));
    }

    /// Excel criteria are written without the column (`">1000"`), and `<>` is the
    /// not-equal spelling; both go through the same parser `filter --where` uses.
    #[test]
    fn countif_and_sumif_share_the_prepass() {
        let path = write(
            "filtered",
            "city,amount\nA,10\nB,2000\nC,1500\nD,not-a-number\n",
        );
        let totals = column_totals(
            &path,
            Delimiter::Comma,
            QuoteStyle::Rfc4180,
            &needs(&[], &[("amount", ">100")]),
        )
        .unwrap();
        std::fs::remove_file(&path).ok();
        let pair = ("amount".to_string(), ">100".to_string());
        assert_eq!(totals.filtered.get(&pair), Some(&(3500.0, 2)));

        let path = write("noteq", "city,amount\nA,10\nB,20\n");
        let totals = column_totals(
            &path,
            Delimiter::Comma,
            QuoteStyle::Rfc4180,
            &needs(&[], &[("amount", "<>10")]),
        )
        .unwrap();
        std::fs::remove_file(&path).ok();
        let pair = ("amount".to_string(), "<>10".to_string());
        assert_eq!(totals.filtered.get(&pair), Some(&(20.0, 1)));
    }
}
