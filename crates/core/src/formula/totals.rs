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
use crate::index::SparseIndex;
use crate::types::Delimiter;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Seek, SeekFrom};
use std::path::Path;

/// Splitting the pre-pass costs a sparse index and thread spawn; below this the pass is
/// only a few milliseconds, so one thread wins.
const PARALLEL_MIN_BYTES: u64 = 8 * 1024 * 1024;

/// Below this the parquet sidecar is *slower* than the sliced parallel CSV scan: DuckDB's
/// start-up costs more than reading 33 MB of columns. Large files still prefer parquet.
#[cfg(feature = "sql")]
const PARQUET_MIN_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct ColumnTotals {
    /// Numeric sum and numeric count for each column.
    pub plain: HashMap<String, (f64, u64)>,
    /// `(column, criteria)` -> (numeric sum, count of matching cells).
    pub filtered: HashMap<(String, String), (f64, u64)>,
    /// Numeric minimum and maximum for each column (Excel's MIN/MAX ignore text).
    pub bounds: HashMap<String, (f64, f64)>,
    /// Sum of squares per column, so stdev/var need no second pass.
    pub squares: HashMap<String, f64>,
}

impl ColumnTotals {
    fn add(&mut self, other: ColumnTotals) {
        for (column, (sum, count)) in other.plain {
            let slot = self.plain.entry(column).or_insert((0.0, 0));
            slot.0 += sum;
            slot.1 += count;
        }
        for (column, square) in other.squares {
            *self.squares.entry(column).or_insert(0.0) += square;
        }
        for (column, (min, max)) in other.bounds {
            match self.bounds.entry(column) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert((min, max));
                }
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    let bounds = slot.get_mut();
                    bounds.0 = bounds.0.min(min);
                    bounds.1 = bounds.1.max(max);
                }
            }
        }
        for (pair, (sum, count)) in other.filtered {
            let slot = self.filtered.entry(pair).or_insert((0.0, 0));
            slot.0 += sum;
            slot.1 += count;
        }
    }
}

/// Threads for the pre-pass: an explicit `CSVX_SCAN_THREADS` wins (tests and benchmarks
/// rely on that), otherwise one per core but only above [`PARALLEL_MIN_BYTES`].
fn aggregate_threads(file_len: u64) -> usize {
    if let Ok(raw) = std::env::var("CSVX_SCAN_THREADS") {
        if let Ok(n) = raw.trim().parse::<usize>() {
            if n >= 1 {
                return n;
            }
        }
    }
    if file_len < PARALLEL_MIN_BYTES {
        return 1;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8)
}

/// One slab of data rows, split at a sparse-index checkpoint.
struct Slab {
    offset: u64,
    skip_header: bool,
    rows: u64,
}

/// Split the checkpoints into `threads` slabs; the last runs to EOF so the tail after the
/// final checkpoint is still counted.
fn slabs(index: &SparseIndex, threads: usize) -> Vec<Slab> {
    let data_rows = index.data_rows();
    let interval = index.interval();
    let total_cp = index.checkpoints().len();
    if data_rows == 0 || total_cp == 0 {
        return Vec::new();
    }
    let t = threads.min(total_cp).max(1);
    let mut bounds: Vec<usize> = (1..t).map(|j| j * total_cp / t).collect();
    bounds.retain(|c| (1..total_cp).contains(c));
    bounds.dedup();
    let mut all = vec![0usize];
    all.extend(bounds);
    all.push(total_cp);
    let mut out = Vec::new();
    for pair in all.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a == b {
            continue;
        }
        let (offset, skip_header) = if a == 0 {
            (0, true)
        } else {
            (index.checkpoints()[a], false)
        };
        let start_row = if a == 0 { 0 } else { a as u64 * interval - 1 };
        let rows = if b == total_cp {
            data_rows.saturating_sub(start_row)
        } else if a == 0 {
            b as u64 * interval - 1
        } else {
            (b - a) as u64 * interval
        };
        if rows > 0 {
            out.push(Slab {
                offset,
                skip_header,
                rows,
            });
        }
    }
    out
}

/// Scan one slab, accumulating the same numbers the whole-file loop would.
fn scan_slab(
    path: &Path,
    slab: &Slab,
    delimiter: Delimiter,
    quotes: QuoteStyle,
    plain: &[(String, usize)],
    filtered: &[(String, String, usize, FilterSpec)],
) -> io::Result<ColumnTotals> {
    let mut file = File::open(path)?;
    if slab.offset > 0 {
        file.seek(SeekFrom::Start(slab.offset))?;
    }
    let mut records = RecordReader::new_with(
        BufReader::with_capacity(4 * 1024 * 1024, file),
        4 * 1024 * 1024,
        quotes,
    );
    let mut record = Vec::with_capacity(64 * 1024);
    if slab.skip_header && !records.next_record(&mut record)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
    }
    let mut out = ColumnTotals::default();
    for (name, _) in plain {
        out.plain.insert(name.clone(), (0.0, 0));
    }
    for (column, criteria, _, _) in filtered {
        out.filtered
            .insert((column.clone(), criteria.clone()), (0.0, 0));
    }
    for _ in 0..slab.rows {
        if !records.next_record(&mut record)? {
            break;
        }
        // Fast path: slice the record instead of building a String per cell. Falls back
        // to the full parser for escaped quotes, backslash quoting, or bad UTF-8.
        if let Some(cells) = slice_cells(&record, delimiter.byte(), quotes) {
            accumulate_cells(&cells, plain, filtered, &mut out);
        } else {
            let text = String::from_utf8_lossy(&record);
            let fields = parse_csv_record_with(
                text.trim_end_matches(['\n', '\r']),
                delimiter.byte(),
                quotes,
            );
            let refs: Vec<&str> = fields.iter().map(|f| f.as_str()).collect();
            accumulate_cells(&refs, plain, filtered, &mut out);
        }
    }
    Ok(out)
}

/// The record's fields as borrowed strings, or `None` when the full parser is needed.
fn slice_cells(record: &[u8], delim: u8, quotes: QuoteStyle) -> Option<Vec<&str>> {
    let slices = crate::csv_reader::field_slices(record, delim, quotes)?;
    let mut out = Vec::with_capacity(slices.len());
    for slice in slices {
        out.push(std::str::from_utf8(slice).ok()?);
    }
    Some(out)
}

/// Fold one record's cells into the running totals.
fn accumulate_cells(
    cells: &[&str],
    plain: &[(String, usize)],
    filtered: &[(String, String, usize, FilterSpec)],
    out: &mut ColumnTotals,
) {
    for (name, index) in plain {
        let Some(cell) = cells.get(*index) else {
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
    for (column, criteria, index, spec) in filtered {
        let Some(cell) = cells.get(*index) else {
            continue;
        };
        // An ordering criterion is numeric: COUNTIF(">100") must not count the cell
        // "not-a-number" just because 'n' sorts after '1'.
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
///
/// The answer depends only on the base file's bytes, so it is cached next to the sheet
/// (`t.csv.agg`) and keyed on the file's length and mtime. A stale cache is ignored;
/// a missing or unwritable one costs the scan and nothing else.
pub fn column_totals(
    path: &Path,
    delimiter: Delimiter,
    quotes: QuoteStyle,
    needs: &RowNeeds,
) -> io::Result<ColumnTotals> {
    if let Some(totals) = cache::load(path, needs) {
        return Ok(totals);
    }
    let totals = compute_totals(path, delimiter, quotes, needs)?;
    cache::store(path, &totals);
    Ok(totals)
}

mod cache {
    use super::*;
    use std::time::UNIX_EPOCH;

    pub fn sidecar(base: &Path) -> std::path::PathBuf {
        let mut name = base.to_string_lossy().to_string();
        name.push_str(".agg");
        std::path::PathBuf::from(name)
    }

    /// Percent-encode the four characters that would break the line format.
    fn esc(text: &str) -> String {
        text.replace('%', "%25")
            .replace(' ', "%20")
            .replace('\t', "%09")
            .replace('\n', "%0A")
            .replace('\r', "%0D")
    }

    fn unesc(text: &str) -> String {
        text.replace("%0D", "\r")
            .replace("%0A", "\n")
            .replace("%09", "\t")
            .replace("%20", " ")
            .replace("%25", "%")
    }

    fn stamp(path: &Path) -> Option<(u64, u128)> {
        let meta = std::fs::metadata(path).ok()?;
        let since = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        Some((meta.len(), since.as_nanos()))
    }

    pub fn load(base: &Path, needs: &RowNeeds) -> Option<ColumnTotals> {
        let (len, mtime) = stamp(base)?;
        let text = std::fs::read_to_string(sidecar(base)).ok()?;
        let mut lines = text.lines();
        if lines.next()? != "csvx-agg 3" {
            return None;
        }
        let cached_len: u64 = lines.next()?.strip_prefix("len ")?.parse().ok()?;
        let cached_mtime: u128 = lines.next()?.strip_prefix("mtime ")?.parse().ok()?;
        if cached_len != len || cached_mtime != mtime {
            return None;
        }
        let mut totals = ColumnTotals::default();
        for line in lines {
            let mut parts = line.split(' ');
            match parts.next()? {
                "plain" => {
                    let column = unesc(parts.next()?);
                    let sum: f64 = parts.next()?.parse().ok()?;
                    let count: u64 = parts.next()?.parse().ok()?;
                    totals.plain.insert(column, (sum, count));
                }
                "squares" => {
                    let column = unesc(parts.next()?);
                    let square: f64 = parts.next()?.parse().ok()?;
                    totals.squares.insert(column, square);
                }
                "bounds" => {
                    let column = unesc(parts.next()?);
                    let min: f64 = parts.next()?.parse().ok()?;
                    let max: f64 = parts.next()?.parse().ok()?;
                    totals.bounds.insert(column, (min, max));
                }
                "filtered" => {
                    let column = unesc(parts.next()?);
                    let criteria = unesc(parts.next()?);
                    let sum: f64 = parts.next()?.parse().ok()?;
                    let count: u64 = parts.next()?.parse().ok()?;
                    totals.filtered.insert((column, criteria), (sum, count));
                }
                _ => {}
            }
        }
        if !needs.aggregates.iter().all(|c| {
            totals.plain.contains_key(c)
                && totals.bounds.contains_key(c)
                && totals.squares.contains_key(c)
        }) {
            return None;
        }
        if !needs
            .filtered
            .iter()
            .all(|(c, crit)| totals.filtered.contains_key(&(c.clone(), crit.clone())))
        {
            return None;
        }
        Some(totals)
    }

    pub fn store(base: &Path, totals: &ColumnTotals) {
        let Some((len, mtime)) = stamp(base) else {
            return;
        };
        let mut text = String::from("csvx-agg 3\n");
        text.push_str(&format!("len {len}\n"));
        text.push_str(&format!("mtime {mtime}\n"));
        for (name, (sum, count)) in &totals.plain {
            text.push_str(&format!("plain {} {sum} {count}\n", esc(name)));
        }
        for (name, square) in &totals.squares {
            text.push_str(&format!("squares {} {square}\n", esc(name)));
        }
        for (name, (min, max)) in &totals.bounds {
            text.push_str(&format!("bounds {} {min} {max}\n", esc(name)));
        }
        for ((column, criteria), (sum, count)) in &totals.filtered {
            text.push_str(&format!(
                "filtered {} {} {sum} {count}\n",
                esc(column),
                esc(criteria)
            ));
        }
        let path = sidecar(base);
        let tmp = path.with_extension("agg.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn compute_totals(
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
    if needs.filtered.is_empty()
        && std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) >= PARQUET_MIN_BYTES
    {
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

    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let threads = aggregate_threads(file_len);
    if threads > 1 && crate::scan_parallel::is_seekable(path)? {
        let index = match SparseIndex::load(&SparseIndex::sidecar_path(path)) {
            Ok(idx) if idx.matches(path) => idx,
            _ => SparseIndex::build(path, crate::index::DEFAULT_INTERVAL)?,
        };
        let plans = slabs(&index, threads);
        let results: Vec<io::Result<ColumnTotals>> = std::thread::scope(|scope| {
            let handles: Vec<_> = plans
                .iter()
                .map(|slab| {
                    scope.spawn(|| scan_slab(path, slab, delimiter, quotes, &plain, &filtered))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(io::Error::other("a totals worker panicked")))
                })
                .collect()
        });
        for result in results {
            out.add(result?);
        }
        return Ok(out);
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
                    *out.squares.entry(name.clone()).or_insert(0.0) += value * value;
                    match out.bounds.entry(name.clone()) {
                        std::collections::hash_map::Entry::Vacant(slot) => {
                            slot.insert((value, value));
                        }
                        std::collections::hash_map::Entry::Occupied(mut slot) => {
                            let bounds = slot.get_mut();
                            bounds.0 = bounds.0.min(value);
                            bounds.1 = bounds.1.max(value);
                        }
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

    /// The sidecar is keyed on the file's length and mtime: the second call answers
    /// from disk, and a changed file must not be served from the old copy.
    #[test]
    fn a_second_prepass_reads_the_cache_and_a_change_invalidates_it() {
        let path = write("cache", "city,amount\nA,10\nB,20\n");
        let needs = needs(&["amount"], &[]);
        let first = column_totals(&path, Delimiter::Comma, QuoteStyle::Rfc4180, &needs).unwrap();
        let cache = cache::sidecar(&path);
        assert!(cache.exists(), "the first pass must write the sidecar");
        let second = column_totals(&path, Delimiter::Comma, QuoteStyle::Rfc4180, &needs).unwrap();
        assert_eq!(first.plain.get("amount"), second.plain.get("amount"));

        std::fs::write(&path, "city,amount\nA,10\nB,20\nC,30\n").unwrap();
        let third = column_totals(&path, Delimiter::Comma, QuoteStyle::Rfc4180, &needs).unwrap();
        assert_eq!(third.plain.get("amount"), Some(&(60.0, 3)));
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(cache).ok();
    }

    /// The slice fast path must agree with the full parser on quoted cells.
    #[test]
    fn totals_read_quoted_and_multiline_cells() {
        let path = write(
            "quoted",
            "city,amount,note\nA,10,\"x,y\"\nB,20,\"line1\nline2\"\nC,30,\"a\"\"b\"\n",
        );
        let totals = column_totals(
            &path,
            Delimiter::Comma,
            QuoteStyle::Rfc4180,
            &needs(&["amount"], &[]),
        )
        .unwrap();
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(cache::sidecar(&path)).ok();
        assert_eq!(totals.plain.get("amount"), Some(&(60.0, 3)));
        assert_eq!(totals.bounds.get("amount"), Some(&(10.0, 30.0)));
        assert_eq!(totals.squares.get("amount"), Some(&1400.0));
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

    /// The slabs must add up to exactly what the single-threaded pass computes: same
    /// rows, same quoted cells, same criteria hits.
    #[test]
    fn slab_totals_match_the_single_threaded_pass() {
        let path = write("slabs", "");
        let mut text = String::from("city,amount\n");
        for i in 0..3_000u64 {
            text.push_str(&format!("C{},{}\n", i % 7, i % 1000));
        }
        std::fs::write(&path, text).unwrap();
        let needs = needs(&["amount"], &[("amount", ">100")]);
        let seq = column_totals(&path, Delimiter::Comma, QuoteStyle::Rfc4180, &needs).unwrap();

        let index = SparseIndex::build(&path, crate::index::DEFAULT_INTERVAL).unwrap();
        let plans = slabs(&index, 4);
        assert!(plans.len() > 2, "the fixture must split into several slabs");
        let plain = vec![("amount".to_string(), 1usize)];
        let spec = compile_criteria("amount", ">100").unwrap();
        let filtered = vec![("amount".to_string(), ">100".to_string(), 1usize, spec)];
        let mut merged = ColumnTotals::default();
        for slab in &plans {
            merged.add(
                scan_slab(
                    &path,
                    slab,
                    Delimiter::Comma,
                    QuoteStyle::Rfc4180,
                    &plain,
                    &filtered,
                )
                .unwrap(),
            );
        }
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(SparseIndex::sidecar_path(&path)).ok();
        assert_eq!(merged.plain, seq.plain);
        assert_eq!(merged.filtered, seq.filtered);
    }
}
