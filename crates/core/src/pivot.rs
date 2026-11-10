//! Pivot tables: rows × columns × measures, streamed in one pass.
//!
//! A pivot is a `groupby` with a second key spread across the output: group by
//! the row keys, bucket by the column key, accumulate the measures. That makes it
//! cheap for the same reason aggregation is cheap — memory follows the number of
//! *cells* (row keys × column keys), not the number of rows.
//!
//! The danger is asymmetric: a column key is usually low-cardinality (a month, a
//! city), but nothing stops someone from pivoting on an id column, and the matrix
//! then explodes. Two hard ceilings keep that a clear error instead of a swap
//! storm: `max_columns` (distinct column-key values) and `max_cells` (matrix
//! size). Both are checked *before* memory is committed.

use crate::aggregate::{Accumulator, AggOp, AggSpec};
use crate::csv_reader::{
    join_csv_line, parse_csv_record_with, resolve_column, QuoteStyle, RecordReader,
};
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;

/// Ceiling on distinct values of the column key (matrix width).
pub const DEFAULT_MAX_COLUMNS: usize = 512;
/// Ceiling on row keys × column keys × measures (matrix size).
pub const DEFAULT_MAX_CELLS: usize = 2_000_000;

#[derive(Clone, Debug)]
pub struct PivotOptions {
    /// Columns that become the output rows.
    pub rows: Vec<String>,
    /// Column whose values become output columns. `None` degrades to `groupby`.
    pub columns: Option<String>,
    pub values: Vec<AggSpec>,
    /// Sort rows by this output column instead of by the row keys.
    pub order_by: Option<String>,
    pub descending: bool,
    /// Show the aggregate name in the header (`sum_amount`) instead of the bare
    /// column key, which is the only way to tell several measures apart.
    pub qualify_names: bool,
    pub max_columns: usize,
    pub max_cells: usize,
}

impl Default for PivotOptions {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            columns: None,
            values: Vec::new(),
            order_by: None,
            descending: false,
            qualify_names: false,
            max_columns: DEFAULT_MAX_COLUMNS,
            max_cells: DEFAULT_MAX_CELLS,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PivotSummary {
    pub input_rows: u64,
    pub row_keys: u64,
    pub column_keys: u64,
    pub output_rows: u64,
    pub bytes: u64,
    pub row_columns: Vec<String>,
    pub column_values: Vec<String>,
    pub measures: Vec<String>,
}

/// One matrix cell set: everything accumulated for a (row key, column key).
struct RowCells {
    key: Vec<String>,
    /// column key → accumulators (one per measure).
    cells: HashMap<String, Vec<Accumulator>>,
}

/// Run a pivot into `output`.
pub fn pivot(
    path: &Path,
    delimiter: Delimiter,
    output: &Path,
    options: &PivotOptions,
    tx: &ProgressSender,
) -> io::Result<PivotSummary> {
    pivot_quotes(path, delimiter, output, options, QuoteStyle::Rfc4180, tx)
}

/// Same as [`pivot`], with an explicit quote style.
pub struct PivotTable {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub summary: PivotSummary,
}

/// The matrix itself, without a file: the CLI prints it and the API returns it.
pub fn pivot_rows(
    path: &Path,
    delimiter: Delimiter,
    options: &PivotOptions,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<PivotTable> {
    let delim = delimiter.byte();
    let _ = delim;
    if options.rows.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a pivot needs at least one --rows column",
        ));
    }
    if options.values.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a pivot needs at least one --values aggregate",
        ));
    }

    let delim = delimiter.byte();
    let mut records = RecordReader::new_with(
        BufReader::with_capacity(4 * 1024 * 1024, encoding::open_decoded(path)?),
        4 * 1024 * 1024,
        quotes,
    );
    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the file has no header",
        ));
    }
    let header = parse_csv_record_with(
        String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
        delim,
        quotes,
    );

    // Every referenced column is resolved once, with the same rules as the rest of
    // the CLI (exact, unique-after-trim, unique-case-insensitive, else ambiguous).
    let row_idx: Vec<usize> = options
        .rows
        .iter()
        .map(|name| resolve_column(name, &header).map_err(config_error))
        .collect::<io::Result<_>>()?;
    let col_idx: Option<usize> = match &options.columns {
        Some(name) => Some(resolve_column(name, &header).map_err(config_error)?),
        None => None,
    };
    let measure_idx: Vec<usize> = options
        .values
        .iter()
        .map(|spec| resolve_column(&spec.column, &header).map_err(config_error))
        .collect::<io::Result<_>>()?;

    tx.send(ProgressEvent::PhaseStart {
        phase: "Pivoting".into(),
        total_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    })
    .ok();

    let mut rows: HashMap<Vec<String>, RowCells> = HashMap::new();
    let mut column_keys: Vec<String> = Vec::new();
    let mut seen_columns: HashMap<String, usize> = HashMap::new();
    let mut input_rows = 0u64;
    let mut scanned = 0u64;

    while records.next_record(&mut record)? {
        let fields = parse_csv_record_with(
            String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
            delim,
            quotes,
        );
        input_rows += 1;
        scanned += record.len() as u64;
        if input_rows.is_multiple_of(500_000) {
            tx.send(ProgressEvent::Progress { bytes: scanned }).ok();
        }

        let key: Vec<String> = row_idx
            .iter()
            .map(|i| fields.get(*i).cloned().unwrap_or_default())
            .collect();
        // The column key labels a matrix column; an empty value is a real bucket,
        // so it is labelled rather than dropped.
        let column_key = match col_idx {
            Some(i) => fields.get(i).cloned().unwrap_or_default(),
            None => String::new(),
        };

        if let Some(_slot) = seen_columns.get(&column_key) {
            // Already known; no new column.
        } else {
            if column_keys.len() >= options.max_columns {
                return Err(io::Error::other(format!(
                    "more than {} distinct values in '{}'; raise --max-columns or pivot on a lower-cardinality column",
                    options.max_columns,
                    options.columns.as_deref().unwrap_or("")
                )));
            }
            seen_columns.insert(column_key.clone(), column_keys.len());
            column_keys.push(column_key.clone());
        }

        let entry = match rows.get_mut(&key) {
            Some(entry) => entry,
            None => {
                if rows.len() >= options.max_cells.min(u32::MAX as usize) {
                    return Err(io::Error::other(format!(
                        "more than {} row keys; raise --max-cells or pivot on fewer columns",
                        options.max_cells
                    )));
                }
                rows.insert(
                    key.clone(),
                    RowCells {
                        key: key.clone(),
                        cells: HashMap::new(),
                    },
                );
                rows.get_mut(&key).expect("just inserted")
            }
        };

        let cell = entry
            .cells
            .entry(column_key)
            .or_insert_with(|| vec![Accumulator::new(); options.values.len()]);
        for (slot, idx) in measure_idx.iter().enumerate() {
            let value = fields.get(*idx).map(String::as_str).unwrap_or("");
            cell[slot].observe(value);
        }

        // Matrix size guard: distinct row keys × column keys × measures.
        if rows
            .len()
            .saturating_mul(column_keys.len())
            .saturating_mul(options.values.len())
            > options.max_cells
        {
            return Err(io::Error::other(format!(
                "the pivot matrix would exceed {} cells ({} rows × {} columns × {} measures); \
                 raise --max-cells or pivot on lower-cardinality columns",
                options.max_cells,
                rows.len(),
                column_keys.len(),
                options.values.len()
            )));
        }
    }

    column_keys.sort();
    let summary_columns = column_keys.clone();

    // Header: row keys, then one block of measures per column key.
    let mut header_out: Vec<String> = options.rows.clone();
    for column in &column_keys {
        for spec in &options.values {
            header_out.push(if options.qualify_names {
                format!("{column} · {}", spec.output_name())
            } else {
                column.clone()
            });
        }
    }

    let mut rendered: Vec<(Vec<String>, RowCells)> =
        rows.into_values().map(|r| (r.key.clone(), r)).collect();
    let width = summary_columns.len() * options.values.len();
    rendered.sort_by(|a, b| a.0.cmp(&b.0));

    if let Some(order) = &options.order_by {
        // Order by one of the measures, applied to its first column block so the
        // sort key is unambiguous.
        let position = header_out.iter().position(|c| c == order).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot order by '{order}': not an output column"),
            )
        })?;
        let position = position - options.rows.len();
        rendered.sort_by(|a, b| {
            let left = cell_text(
                a.1.cells
                    .get(&summary_columns[position / options.values.len()]),
                position % options.values.len(),
                &options.values,
            );
            let right = cell_text(
                b.1.cells
                    .get(&summary_columns[position / options.values.len()]),
                position % options.values.len(),
                &options.values,
            );
            numeric_or_text(&left, &right)
        });
        if options.descending {
            rendered.reverse();
        }
    } else if options.descending {
        rendered.reverse();
    }

    let mut body: Vec<Vec<String>> = Vec::with_capacity(rendered.len());
    for (_, row) in &rendered {
        let mut line: Vec<String> = row.key.clone();
        for column in &summary_columns {
            match row.cells.get(column) {
                Some(accs) => {
                    for (slot, spec) in options.values.iter().enumerate() {
                        line.push(accs[slot].render(spec.op));
                    }
                }
                None => {
                    // A combination the data never produced. Count is genuinely 0;
                    // a sum of nothing is also 0; a mean of nothing is blank.
                    for spec in &options.values {
                        line.push(if spec.op == AggOp::Count || spec.op == AggOp::Sum {
                            "0".to_string()
                        } else {
                            String::new()
                        });
                    }
                }
            }
        }
        body.push(line);
    }

    tx.send(ProgressEvent::PhaseDone {
        phase: "Pivoting".into(),
    })
    .ok();
    let _ = (width, join_csv_line);

    Ok(PivotTable {
        header: header_out,
        summary: PivotSummary {
            input_rows,
            row_keys: rendered.len() as u64,
            column_keys: summary_columns.len() as u64,
            output_rows: rendered.len() as u64,
            bytes: 0,
            row_columns: options.rows.clone(),
            column_values: summary_columns,
            measures: options.values.iter().map(AggSpec::output_name).collect(),
        },
        rows: body,
    })
}

/// Write the matrix to a CSV file.
pub fn pivot_quotes(
    path: &Path,
    delimiter: Delimiter,
    output: &Path,
    options: &PivotOptions,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<PivotSummary> {
    let table = pivot_rows(path, delimiter, options, quotes, tx)?;
    let delim = delimiter.byte();
    let mut out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(output)?);
    let header: Vec<&str> = table.header.iter().map(String::as_str).collect();
    out.write_all(join_csv_line(&header, delim).as_bytes())?;
    for row in &table.rows {
        let refs: Vec<&str> = row.iter().map(String::as_str).collect();
        out.write_all(join_csv_line(&refs, delim).as_bytes())?;
    }
    out.flush()?;
    let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    let mut summary = table.summary;
    summary.bytes = bytes;
    Ok(summary)
}

fn cell_text(cell: Option<&Vec<Accumulator>>, slot: usize, values: &[AggSpec]) -> String {
    match cell {
        Some(accs) => accs
            .get(slot)
            .map(|a| a.render(values[slot].op))
            .unwrap_or_default(),
        None => String::new(),
    }
}

/// Compare two rendered cells: numerically when both parse, lexically otherwise.
fn numeric_or_text(left: &str, right: &str) -> std::cmp::Ordering {
    match (left.parse::<f64>(), right.parse::<f64>()) {
        (Ok(a), Ok(b)) => a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal),
        _ => left.cmp(right),
    }
}

fn config_error(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_pivot_{tag}_{}_{}",
                std::process::id(),
                unique_seq()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn join(&self, n: &str) -> PathBuf {
            self.0.join(n)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn run(csv: &str, options: PivotOptions) -> (Dir, String, PivotSummary) {
        let dir = Dir::new("run");
        let input = dir.join("in.csv");
        std::fs::write(&input, csv).unwrap();
        let output = dir.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let summary = pivot(&input, Delimiter::Comma, &output, &options, &tx).unwrap();
        let text = std::fs::read_to_string(&output).unwrap();
        (dir, text, summary)
    }

    const SALES: &str = "\
city,month,amount,qty
CDMX,Jan,10,2
CDMX,Feb,20,3
Puebla,Jan,30,1
Puebla,Feb,40,4
Tijuana,Jan,50,5
";

    fn opts(rows: &[&str], columns: Option<&str>, values: &str) -> PivotOptions {
        PivotOptions {
            rows: rows.iter().map(|s| s.to_string()).collect(),
            columns: columns.map(|s| s.to_string()),
            values: AggSpec::parse_list(values).unwrap(),
            ..Default::default()
        }
    }

    #[test]
    fn a_two_dimension_pivot_produces_a_matrix() {
        let (_dir, text, summary) = run(SALES, opts(&["city"], Some("month"), "amount:sum"));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "city,Feb,Jan");
        assert_eq!(lines[1], "CDMX,20,10");
        assert_eq!(lines[2], "Puebla,40,30");
        // A missing combination is a real zero for sum (nothing was summed), not
        // an empty cell — that difference matters when the matrix is exported.
        assert_eq!(
            lines[3], "Tijuana,0,50",
            "no February row for Tijuana: {text}"
        );
        assert_eq!(summary.input_rows, 5);
        assert_eq!(summary.column_keys, 2);
        assert_eq!(summary.output_rows, 3);
    }

    #[test]
    fn missing_combinations_render_zero_for_count_and_sum_but_blank_for_mean() {
        let (_dir, text, _) = run(
            SALES,
            opts(&["city"], Some("month"), "amount:sum,amount:mean"),
        );
        let tijuana = text.lines().nth(3).unwrap();
        // Feb is the first column block: sum missing = 0, mean missing = blank.
        assert_eq!(tijuana, "Tijuana,0,,50,50", "{text}");
    }

    #[test]
    fn several_measures_can_be_qualified_in_the_header() {
        let mut options = opts(&["city"], Some("month"), "amount:sum,qty:max");
        options.qualify_names = true;
        let (_dir, text, _) = run(SALES, options);
        let header = text.lines().next().unwrap();
        assert_eq!(
            header, "city,Feb · sum_amount,Feb · max_qty,Jan · sum_amount,Jan · max_qty",
            "{text}"
        );
    }

    #[test]
    fn multiple_row_keys_nest() {
        let csv = "city,shop,month,amount\nCDMX,A,Jan,10\nCDMX,B,Jan,20\nCDMX,A,Feb,30\n";
        let (_dir, text, summary) = run(csv, opts(&["city", "shop"], Some("month"), "amount:sum"));
        assert_eq!(text.lines().next().unwrap(), "city,shop,Feb,Jan");
        assert_eq!(text.lines().nth(1).unwrap(), "CDMX,A,30,10");
        assert_eq!(summary.row_keys, 2);
    }

    #[test]
    fn without_a_column_key_it_behaves_like_a_group_by() {
        let (_dir, text, _) = run(SALES, opts(&["city"], None, "amount:sum"));
        assert_eq!(text.lines().next().unwrap(), "city,", "{text}");
        assert!(text.contains("CDMX,30"), "{text}");
    }

    #[test]
    fn ordering_accepts_aggregate_output_columns() {
        let mut options = opts(&["city"], Some("month"), "amount:sum");
        options.order_by = Some("Jan".into());
        options.descending = true;
        let (_dir, text, _) = run(SALES, options);
        assert_eq!(text.lines().nth(1).unwrap(), "Tijuana,0,50", "{text}");
    }

    #[test]
    fn a_wide_column_key_is_refused_before_memory_blows_up() {
        let mut csv = String::from("id,k,v\n");
        for i in 0..700 {
            csv.push_str(&format!("{i},x{i},1\n"));
        }
        let mut options = opts(&["id"], Some("k"), "v:sum");
        options.max_columns = 512;
        let dir = Dir::new("wide");
        let input = dir.join("in.csv");
        std::fs::write(&input, &csv).unwrap();
        let output = dir.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = pivot(&input, Delimiter::Comma, &output, &options, &tx).unwrap_err();
        assert!(err.to_string().contains("distinct values"), "{err}");
    }

    #[test]
    fn unknown_columns_are_named_in_the_error() {
        let dir = Dir::new("badcol");
        let input = dir.join("in.csv");
        std::fs::write(&input, SALES).unwrap();
        let output = dir.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let options = opts(&["nope"], Some("month"), "amount:sum");
        let err = pivot(&input, Delimiter::Comma, &output, &options, &tx).unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
    }

    #[test]
    fn the_job_is_one_pass_over_the_input() {
        let (_dir, _text, summary) = run(SALES, opts(&["city"], Some("month"), "amount:sum"));
        // A pivot reads rows once; the summary reports what it saw.
        assert_eq!(summary.input_rows, 5);
    }
}
