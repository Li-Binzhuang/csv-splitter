//! Row assembly, export and windowed reads.
//!
//! Split out of `workbook` when that file passed 2500 lines. This is the part that
//! touches every row, so it is also the part most likely to change.

use super::*;
use crate::workbook::journal::RowRef;
use crate::workbook::rows::Run;

/// Materialise `base + journal` into `output`.
///
/// One streaming pass over the base file: no per-row seeking, no memory that
/// grows with the file. Cells are rewritten only where an edit touches them, and
/// the write-side encoding is UTF-8 (documented; the base stays untouched).
pub fn export(
    base: &Path,
    ops: &[Op],
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
) -> io::Result<ExportSummary> {
    export_quotes(
        base,
        ops,
        output,
        delimiter,
        crate::csv_reader::QuoteStyle::Rfc4180,
        tx,
    )
}

/// Same as [`export`], with an explicit quote style for the base file.
pub fn export_quotes(
    base: &Path,
    ops: &[Op],
    output: &Path,
    delimiter: Delimiter,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<ExportSummary> {
    let delim = delimiter.byte();
    let base_columns = base_header(base, delimiter)?;
    if base_columns.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the base file has no header",
        ));
    }
    let layout = column_layout(ops, &base_columns)?;
    let columns: Vec<String> = layout.iter().map(|(n, _)| n.clone()).collect();
    let sheet = compile(ops, u64::MAX);
    let edits = &sheet.edits;
    let map = &sheet.map;
    let compiled = compile_formulas_at(base, ops, &columns)?;
    let refs = &compiled.refs;
    let plan = ColumnPlan::build(ops, &columns, &compiled);

    tx.send(ProgressEvent::PhaseStart {
        phase: "Exporting edits".into(),
        total_bytes: std::fs::metadata(base).map(|m| m.len()).unwrap_or(0),
    })
    .ok();

    let mut out = BufWriter::with_capacity(8 * 1024 * 1024, File::create(output)?);
    let header: Vec<&str> = columns.iter().map(String::as_str).collect();
    out.write_all(join_csv_line(&header, delim).as_bytes())?;

    let keep: Vec<Option<usize>> = layout.iter().map(|(_, idx)| *idx).collect();

    let reader_bytes = encoding::open_decoded(base)?;
    let mut records = RecordReader::new_with(
        BufReader::with_capacity(8 * 1024 * 1024, reader_bytes),
        8 * 1024 * 1024,
        quotes,
    );
    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Ok(ExportSummary::default());
    }

    let mut summary = ExportSummary {
        columns: columns.clone(),
        ..Default::default()
    };
    let mut physical: u64 = 0;
    let mut logical: u64 = 0;
    // History for formulas that look backwards. Its size depends on the lag
    // distance and the number of such columns, never on the row count.
    let needs = plan.row_needs();
    let mut state = if needs.is_empty() {
        None
    } else {
        Some(crate::formula::RowState::new(needs))
    };

    loop {
        // Which logical row sits at `logical`? Ask the map so inserted blocks and
        // deleted ranges are handled by the same code path as normal rows.
        match map.row(logical) {
            None => break,
            Some(RowRef::Virtual { id, cells }) => {
                emit(
                    &mut out,
                    &columns,
                    &keep,
                    &cells,
                    edits,
                    RowId::Virtual(id),
                    logical,
                    &plan,
                    refs,
                    state.as_mut(),
                    delim,
                )?;
                summary.rows += 1;
                logical += 1;
            }
            Some(RowRef::Base(target)) => {
                // Skip base rows that were deleted, then read the target one.
                while physical <= target {
                    if !records.next_record(&mut record)? {
                        physical = u64::MAX;
                        break;
                    }
                    physical += 1;
                }
                if physical == u64::MAX {
                    break;
                }
                let cells = parse_csv_record_with(
                    String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
                    delim,
                    quotes,
                );
                emit(
                    &mut out,
                    &columns,
                    &keep,
                    &cells,
                    edits,
                    RowId::Base(target),
                    logical,
                    &plan,
                    refs,
                    state.as_mut(),
                    delim,
                )?;
                summary.rows += 1;
                logical += 1;
            }
        }
    }

    // Anything still in the file but not addressed by the map is unreachable;
    // report it rather than silently dropping rows.
    out.flush()?;
    summary.bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseDone {
        phase: "Exporting edits".into(),
    })
    .ok();
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn emit(
    out: &mut impl Write,
    columns: &[String],
    keep: &[Option<usize>],
    base_cells: &[String],
    edits: &CellEdits,
    id: RowId,
    row_number: u64,
    plan: &ColumnPlan,
    refs: &crate::formula::Refs,
    state: Option<&mut crate::formula::RowState>,
    delim: u8,
) -> io::Result<()> {
    let mut cells: Vec<String> = Vec::with_capacity(columns.len());
    for (idx, column) in columns.iter().enumerate() {
        let raw = match keep[idx] {
            Some(base_idx) => base_cells.get(base_idx).cloned().unwrap_or_default(),
            None => String::new(),
        };
        let value = match edits.cell(id, column) {
            Some(v) => v.to_string(),
            None => edits.substitute(column, &raw),
        };
        cells.push(value);
    }
    edits.apply_blocks(columns, id, &mut cells);
    edits.apply_rules(columns, &mut cells);

    // History: totals are folded in before evaluation (a running total includes the
    // current row), the row itself is remembered after it, so `lag(col, 1)` sees the
    // row before this one and not this one.
    if let Some(state) = state {
        let row: HashMap<String, String> = columns
            .iter()
            .zip(cells.iter())
            .map(|(c, v)| (c.clone(), v.clone()))
            .collect();
        state.observe_sums(&row);
        // Derived columns are evaluated against the finished row, so a formula may
        // read a value that was just edited.
        plan.apply(columns, &mut cells, refs, row_number, Some(state));
        let row: HashMap<String, String> = columns
            .iter()
            .zip(cells.iter())
            .map(|(c, v)| (c.clone(), v.clone()))
            .collect();
        state.push_row(&row);
    } else {
        plan.apply(columns, &mut cells, refs, row_number, None);
    }
    let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
    out.write_all(join_csv_line(&refs, delim).as_bytes())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportSummary {
    pub rows: u64,
    pub bytes: u64,
    pub columns: Vec<String>,
}

// ---- windowed reads ----

/// One window of the edited sheet, ready to render.
#[derive(Clone, Debug, Default)]
pub struct Window {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Logical row count after the journal is applied.
    pub total: u64,
}

/// Logical row count after the journal is applied.
///
/// Needs the base row count, which comes from the sparse index (built once,
/// reused from the sidecar). The `u64::MAX` sentinel inside [`RowMap`] must never
/// reach a caller: it is an implementation detail of "the file is open-ended".
pub fn logical_rows(base: &Path, ops: &[Op]) -> io::Result<u64> {
    let sidecar = crate::index::SparseIndex::sidecar_path(base);
    let index = match crate::index::SparseIndex::load(&sidecar) {
        Ok(idx) if idx.matches(base) => idx,
        _ => crate::index::SparseIndex::build(base, crate::index::DEFAULT_INTERVAL)?,
    };
    Ok(RowMap::build(ops, index.data_rows()).len())
}

/// Read `count` logical rows starting at `start`, applying edits.
///
/// The base file is read only where the window lands: with a row index that is
/// one or two seeks, without one it is a single sequential pass. Memory is
/// bounded by the window, not the file — the point of the whole design.
pub fn window(
    base: &Path,
    ops: &[Op],
    start: u64,
    count: u64,
    delimiter: Delimiter,
) -> io::Result<Window> {
    let delim = delimiter.byte();
    let base_columns = base_header(base, delimiter)?;
    if base_columns.is_empty() {
        return Ok(Window::default());
    }
    let layout = column_layout(ops, &base_columns)?;
    let columns: Vec<String> = layout.iter().map(|(n, _)| n.clone()).collect();

    let sidecar = crate::index::SparseIndex::sidecar_path(base);
    let index = match crate::index::SparseIndex::load(&sidecar) {
        Ok(idx) if idx.matches(base) => idx,
        _ => crate::index::SparseIndex::build(base, crate::index::DEFAULT_INTERVAL)?,
    };
    let sheet = compile(ops, index.data_rows());
    let map = &sheet.map;
    let edits = &sheet.edits;
    let compiled = compile_formulas_at(base, ops, &columns)?;
    let refs = &compiled.refs;
    let plan = ColumnPlan::build(ops, &columns, &compiled);

    let row_shape = RowShape {
        columns: &columns,
        layout: &layout,
        edits,
        plan: &plan,
        refs,
    };
    let mut rows = Vec::new();
    for run in map.physical_runs(start, count) {
        match run {
            Run::Base {
                physical,
                len,
                logical,
            } => {
                let records = index.read_rows(base, physical, len)?;
                for (i, record) in records.iter().enumerate() {
                    let cells = parse_csv_record(
                        String::from_utf8_lossy(record).trim_end_matches(['\n', '\r']),
                        delim,
                    );
                    rows.push(shape(
                        &row_shape,
                        &cells,
                        RowId::Base(physical + i as u64),
                        logical + i as u64,
                    ));
                }
            }
            Run::Virtual {
                id,
                logical,
                rows: virtual_rows,
            } => {
                for (i, cells) in virtual_rows.iter().enumerate() {
                    rows.push(shape(
                        &row_shape,
                        cells,
                        RowId::Virtual(id),
                        logical + i as u64,
                    ));
                }
            }
        }
    }

    Ok(Window {
        columns,
        rows,
        total: map.len(),
    })
}

/// Project a base row (or an inserted one) onto the journal's column layout.
/// What every row needs to be assembled: the same for all rows except the row
/// identity itself.
struct RowShape<'a> {
    columns: &'a [String],
    layout: &'a [(String, Option<usize>)],
    edits: &'a CellEdits,
    plan: &'a ColumnPlan,
    refs: &'a crate::formula::Refs,
}

fn shape(shape: &RowShape<'_>, base_cells: &[String], id: RowId, row_number: u64) -> Vec<String> {
    let RowShape {
        columns,
        layout,
        edits,
        plan,
        refs,
    } = shape;
    let mut out = Vec::with_capacity(columns.len());
    for (idx, column) in columns.iter().enumerate() {
        let raw = layout
            .get(idx)
            .and_then(|(_, base_idx)| *base_idx)
            .and_then(|base_idx| base_cells.get(base_idx))
            .cloned()
            .unwrap_or_default();
        let value = match edits.cell(id, column) {
            Some(v) => v.to_string(),
            None => edits.substitute(column, &raw),
        };
        out.push(value);
    }
    edits.apply_blocks(columns, id, &mut out);
    edits.apply_rules(columns, &mut out);
    // A window starts mid-file: the rows before it were never read, so history
    // formulas cannot be answered here. They report #VALUE! rather than pretending
    // the window's first row is the file's first row.
    plan.apply(columns, &mut out, refs, row_number, None);
    out
}
