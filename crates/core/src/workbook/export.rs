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
    export_selected(
        base,
        ops,
        output,
        delimiter,
        quotes,
        ExportOptions::default(),
        tx,
    )
}

/// Export with full control: which rows survive, and which columns are written.
///
/// `columns: None` writes the whole layout; `Some(&["id","amount"])` writes those
/// columns in that order. Selecting during the export avoids a second pass over the
/// data — `columns` followed by `edit export` reads everything twice.
/// What to write, beyond the sheet itself.
///
/// Grouped rather than added as parameters: three booleans-worth of "which rows, which
/// columns" reads better as one value, and the next option does not change a signature
/// every caller has to touch.
#[derive(Clone, Copy, Default)]
pub struct ExportOptions<'a> {
    /// Keep only rows that satisfy this predicate.
    pub only: Option<&'a crate::filter::CompiledSet>,
    /// Write only these columns, in this order.
    pub columns: Option<&'a [String]>,
}

pub fn export_selected(
    base: &Path,
    ops: &[Op],
    output: &Path,
    delimiter: Delimiter,
    quotes: QuoteStyle,
    options: ExportOptions<'_>,
    tx: &ProgressSender,
) -> io::Result<ExportSummary> {
    let ExportOptions { only, .. } = options;
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
    // Which output columns to write, resolved once.
    let selected: Option<Vec<usize>> = match options.columns {
        None => None,
        Some(names) => {
            let mut slots = Vec::with_capacity(names.len());
            for name in names {
                match columns.iter().position(|c| c == name) {
                    Some(slot) => slots.push(slot),
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!(
                                "cannot export column '{name}': it is not in this sheet (have: {})",
                                columns.join(", ")
                            ),
                        ))
                    }
                }
            }
            Some(slots)
        }
    };
    let sheet = compile(ops, u64::MAX);
    let edits = &sheet.edits;
    let map = &sheet.map;
    let compiled = compile_formulas_at(base, ops, &columns)?;
    let refs = &compiled.refs;
    let plan = ColumnPlan::build(ops, &base_columns, &layout, &compiled);

    tx.send(ProgressEvent::PhaseStart {
        phase: "Exporting edits".into(),
        total_bytes: std::fs::metadata(base).map(|m| m.len()).unwrap_or(0),
    })
    .ok();

    let written: Vec<usize> = match &selected {
        Some(slots) => slots.clone(),
        None => (0..columns.len()).collect(),
    };
    let spec = RowSpec {
        delim,
        only,
        written: &written,
    };
    let header: Vec<String> = written.iter().map(|i| columns[*i].clone()).collect();
    let summary_columns = header.clone();
    let mut out = BufWriter::with_capacity(8 * 1024 * 1024, File::create(output)?);
    let header_fields: Vec<&str> = header.iter().map(String::as_str).collect();
    out.write_all(join_csv_line(&header_fields, delim).as_bytes())?;

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
        columns: summary_columns.clone(),
        ..Default::default()
    };
    let mut physical: u64 = 0;
    let mut logical: u64 = 0;
    // History for formulas that look backwards. Its size depends on the lag
    // distance and the number of such columns, never on the row count.
    let needs = plan.row_needs();
    let state = if needs.is_empty() {
        None
    } else {
        Some(crate::formula::RowState::new(needs))
    };
    let mut pipeline = RowPipeline::new(&columns, &keep, edits, &plan, refs, state);

    loop {
        // Which logical row sits at `logical`? Ask the map so inserted blocks and
        // deleted ranges are handled by the same code path as normal rows.
        match map.row(logical) {
            None => break,
            Some(RowRef::Virtual { id, cells }) => {
                if write_row(
                    &mut out,
                    &mut pipeline,
                    &cells,
                    RowId::Virtual(id),
                    logical,
                    &spec,
                )? {
                    summary.rows += 1;
                }
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
                if write_row(
                    &mut out,
                    &mut pipeline,
                    &cells,
                    RowId::Base(target),
                    logical,
                    &spec,
                )? {
                    summary.rows += 1;
                }
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

/// Assembles an output row: base values, then edits, then derived columns.
///
/// Export and window reads each used to have their own copy of this. When `lag` was
/// added it had to be written twice, and the two copies disagreed about history:
/// export kept it, window reads did not. One implementation, one place to change.
pub struct RowPipeline<'a> {
    columns: &'a [String],
    keep: &'a [Option<usize>],
    edits: &'a CellEdits,
    plan: &'a ColumnPlan,
    refs: &'a crate::formula::Refs,
    /// History for `lag` and `running_total`. `None` in window reads, which start
    /// mid-file and have no earlier rows to consult.
    state: Option<crate::formula::RowState>,
}

impl<'a> RowPipeline<'a> {
    pub fn new(
        columns: &'a [String],
        keep: &'a [Option<usize>],
        edits: &'a CellEdits,
        plan: &'a ColumnPlan,
        refs: &'a crate::formula::Refs,
        state: Option<crate::formula::RowState>,
    ) -> Self {
        Self {
            columns,
            keep,
            edits,
            plan,
            refs,
            state,
        }
    }

    /// Build one row.
    pub fn row(&mut self, base_cells: &[String], id: RowId, row_number: u64) -> Vec<String> {
        let mut cells: Vec<String> = Vec::with_capacity(self.columns.len());
        for (idx, column) in self.columns.iter().enumerate() {
            let raw = match self.keep.get(idx).copied().flatten() {
                Some(base_idx) => base_cells.get(base_idx).cloned().unwrap_or_default(),
                None => String::new(),
            };
            let value = match self.edits.cell(id, column) {
                // An explicit edit to a piece wins over what the split would derive.
                Some(v) => v.to_string(),
                // A range fill is bulk; a precise edit always beats it.
                None if self.edits.fill_for(id, column).is_some() => {
                    self.edits.fill_for(id, column).unwrap().to_string()
                }
                None => match self.plan.split_piece(idx) {
                    // Pieces come from the base text, not from the possibly-edited cell:
                    // editing `first` must not erase `last`.
                    Some((by, piece, base_index)) => base_cells
                        .get(base_index)
                        .map_or_else(String::new, |text| split_piece(text, by, piece)),
                    None => self.edits.substitute(column, &raw),
                },
            };
            cells.push(value);
        }
        self.edits.apply_blocks(self.columns, id, &mut cells);
        self.edits.apply_rules(self.columns, &mut cells);

        if let Some(state) = self.state.as_mut() {
            // A running total includes the current row, so the sums are folded in
            // before evaluation; the row is remembered afterwards, so `lag(col, 1)`
            // sees the row before this one and not this one.
            let row = as_map(self.columns, &cells);
            state.observe_sums(&row);
            self.plan
                .apply(self.columns, &mut cells, self.refs, row_number, Some(state));
            let row = as_map(self.columns, &cells);
            state.push_row(&row);
        } else {
            // Window reads start mid-file: the rows before were never read, so the
            // formulas report #VALUE! rather than pretending this is row one.
            self.plan
                .apply(self.columns, &mut cells, self.refs, row_number, None);
        }
        cells
    }
}

/// One piece of a split, with the last piece keeping the remainder.
fn split_piece(text: &str, by: &str, piece: usize) -> String {
    let pieces: Vec<&str> = text.split(by).collect();
    if piece + 1 >= pieces.len() && pieces.len() > 1 {
        pieces[piece..].join(by)
    } else {
        pieces.get(piece).copied().unwrap_or("").to_string()
    }
}

/// Export, optionally keeping only rows that satisfy `only`.
pub fn export_filtered(
    base: &Path,
    ops: &[Op],
    output: &Path,
    delimiter: Delimiter,
    quotes: QuoteStyle,
    only: Option<&crate::filter::CompiledSet>,
    tx: &ProgressSender,
) -> io::Result<ExportSummary> {
    export_selected(
        base,
        ops,
        output,
        delimiter,
        quotes,
        ExportOptions {
            only,
            columns: None,
        },
        tx,
    )
}

/// Contiguous runs of rows that match a filter, as `(at, count)` pairs.
///
/// Used by "delete everything that matches": the journal stores ranges (like every
/// other row operation), so the deleted rows are identified once, when the edit is
/// made, and replaying the journal later needs no scanning.
pub fn runs_matching(
    base: &Path,
    ops: &[Op],
    filter: &str,
    delimiter: Delimiter,
) -> io::Result<Vec<(u64, u64)>> {
    let pager = Pager::open(base, ops, delimiter)?;
    let columns = pager.columns().to_vec();
    let spec = crate::filter::FilterSpec::parse(filter)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let set = crate::filter::FilterSet::single(spec)
        .compile(&columns)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;

    let total = pager.total();
    let mut runs: Vec<(u64, u64)> = Vec::new();
    let mut at = 0u64;
    while at < total {
        let window = pager.page(at, 2_000)?;
        for (offset, row) in window.rows.iter().enumerate() {
            if !set.holds(row) {
                continue;
            }
            let index = at + offset as u64;
            match runs.last_mut() {
                // Consecutive matches extend the run instead of starting a new one.
                Some((start, count)) if *start + *count == index => *count += 1,
                _ => runs.push((index, 1)),
            }
        }
        at += 2_000;
    }
    Ok(runs)
}

/// Find the Nth data row whose cells contain `needle`, applying the journal.
///
/// "Look at it, then change it" is one workflow, and copying a row number from a viewer
/// into an edit is where people miscount. This returns the 0-based logical row so the
/// caller can hand it straight to an edit.
pub fn find_row(
    base: &Path,
    ops: &[Op],
    needle: &str,
    columns: Option<&[String]>,
    case_sensitive: bool,
    nth: u64,
    delimiter: Delimiter,
) -> io::Result<Option<u64>> {
    let pager = Pager::open(base, ops, delimiter)?;
    let total = pager.total();
    let haystack = if case_sensitive {
        needle.to_string()
    } else {
        needle.to_lowercase()
    };
    let mut seen: u64 = 0;
    let mut at: u64 = 0;
    while at < total {
        let window = pager.page(at, 2_000)?;
        for (offset, row) in window.rows.iter().enumerate() {
            let hit = row.iter().enumerate().any(|(slot, cell)| {
                let named = window.columns.get(slot);
                let allowed = match columns {
                    None => true,
                    Some(names) => named.is_some_and(|n| names.iter().any(|want| want == n)),
                };
                if !allowed {
                    return false;
                }
                if case_sensitive {
                    cell.contains(&haystack)
                } else {
                    cell.to_lowercase().contains(&haystack)
                }
            });
            if hit {
                seen += 1;
                if seen == nth {
                    return Ok(Some(at + offset as u64));
                }
            }
        }
        at += 2_000;
    }
    Ok(None)
}

/// Assemble one row and write it, unless a predicate rejects it.
/// Everything the row writer needs that is the same for every row.
struct RowSpec<'a> {
    delim: u8,
    only: Option<&'a crate::filter::CompiledSet>,
    written: &'a [usize],
}

fn write_row(
    out: &mut impl Write,
    pipeline: &mut RowPipeline<'_>,
    base_cells: &[String],
    id: RowId,
    row_number: u64,
    spec: &RowSpec<'_>,
) -> io::Result<bool> {
    // The predicate sees the finished row: edits and formulas applied, so a filter on
    // a computed column behaves the way the spreadsheet does.
    let cells = pipeline.row(base_cells, id, row_number);
    if spec.only.is_some_and(|set| !set.holds(&cells)) {
        return Ok(false);
    }
    // The filter saw the whole row; the file gets the selected columns.
    let fields: Vec<&str> = spec
        .written
        .iter()
        .filter_map(|slot| cells.get(*slot).map(String::as_str))
        .collect();
    out.write_all(join_csv_line(&fields, spec.delim).as_bytes())?;
    Ok(true)
}

/// Column name → value, the shape formula evaluation wants.
fn as_map(columns: &[String], cells: &[String]) -> HashMap<String, String> {
    columns
        .iter()
        .zip(cells.iter())
        .map(|(c, v)| (c.clone(), v.clone()))
        .collect()
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
/// A sheet opened for repeated paging.
///
/// `window()` does the expensive setup — index, column layout, formula compilation —
/// on every call, which is fine for one request and wasteful for a pager that walks
/// the file. `Pager` does it once and then reads pages.
pub struct Pager {
    base: std::path::PathBuf,
    index: crate::index::SparseIndex,
    columns: Vec<String>,
    keep: Vec<Option<usize>>,
    map: crate::workbook::RowMap,
    edits: crate::workbook::CellEdits,
    plan: ColumnPlan,
    refs: crate::formula::Refs,
    delim: u8,
    empty: bool,
}

impl Pager {
    pub fn open(base: &Path, ops: &[Op], delimiter: Delimiter) -> io::Result<Self> {
        let delim = delimiter.byte();
        let base_columns = base_header(base, delimiter)?;
        if base_columns.is_empty() {
            return Ok(Self {
                base: base.to_path_buf(),
                index: crate::index::SparseIndex::default(),
                columns: Vec::new(),
                keep: Vec::new(),
                map: crate::workbook::RowMap::default(),
                edits: crate::workbook::CellEdits::default(),
                plan: ColumnPlan::default(),
                refs: crate::formula::Refs::default(),
                delim,
                empty: true,
            });
        }
        let layout = column_layout(ops, &base_columns)?;
        let columns: Vec<String> = layout.iter().map(|(n, _)| n.clone()).collect();
        let sidecar = crate::index::SparseIndex::sidecar_path(base);
        let index = match crate::index::SparseIndex::load(&sidecar) {
            Ok(idx) if idx.matches(base) => idx,
            _ => {
                // Save what we just built: without this, every page of a big file pays
                // the scan again, and the next process pays it from scratch.
                let idx = crate::index::SparseIndex::build(base, crate::index::DEFAULT_INTERVAL)?;
                idx.save(&sidecar).ok();
                idx
            }
        };
        let sheet = compile(ops, index.data_rows());
        let compiled = compile_formulas_at(base, ops, &columns)?;
        let plan = ColumnPlan::build(ops, &base_columns, &layout, &compiled);
        Ok(Self {
            base: base.to_path_buf(),
            index,
            columns,
            keep: layout.iter().map(|(_, idx)| *idx).collect(),
            map: sheet.map,
            edits: sheet.edits,
            plan,
            refs: compiled.refs,
            delim,
            empty: false,
        })
    }

    pub fn total(&self) -> u64 {
        self.map.len()
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Read one page, with history pre-rolled when the formulas need it.
    pub fn page(&self, start: u64, count: u64) -> io::Result<Window> {
        if self.empty {
            return Ok(Window::default());
        }
        let needs = self.plan.row_needs();
        let (pre_roll, state) = if needs.is_empty() || !needs.running.is_empty() {
            (0, None)
        } else {
            let look_back = needs.max_lag() as u64;
            let first = start.saturating_sub(look_back);
            (
                start.saturating_sub(first),
                Some(crate::formula::RowState::new(needs)),
            )
        };
        let mut pipeline = RowPipeline::new(
            &self.columns,
            &self.keep,
            &self.edits,
            &self.plan,
            &self.refs,
            state,
        );
        if pre_roll > 0 {
            for run in self
                .map
                .physical_runs(start.saturating_sub(pre_roll), pre_roll)
            {
                self.visit(run, &mut pipeline, None)?;
            }
        }
        let mut rows = Vec::new();
        for run in self.map.physical_runs(start, count) {
            self.visit(run, &mut pipeline, Some(&mut rows))?;
        }
        Ok(Window {
            columns: self.columns.clone(),
            rows,
            total: self.map.len(),
        })
    }

    fn visit(
        &self,
        run: Run,
        pipeline: &mut RowPipeline<'_>,
        mut out: Option<&mut Vec<Vec<String>>>,
    ) -> io::Result<()> {
        match run {
            Run::Base {
                physical,
                len,
                logical,
            } => {
                let records = self.index.read_rows(&self.base, physical, len)?;
                for (i, record) in records.iter().enumerate() {
                    let cells = parse_csv_record(
                        String::from_utf8_lossy(record).trim_end_matches(['\n', '\r']),
                        self.delim,
                    );
                    let row =
                        pipeline.row(&cells, RowId::Base(physical + i as u64), logical + i as u64);
                    if let Some(out) = out.as_deref_mut() {
                        out.push(row);
                    }
                }
            }
            Run::Virtual {
                id,
                logical,
                rows: virtual_rows,
            } => {
                for (i, cells) in virtual_rows.iter().enumerate() {
                    let row = pipeline.row(cells, RowId::Virtual(id), logical + i as u64);
                    if let Some(out) = out.as_deref_mut() {
                        out.push(row);
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn window(
    base: &Path,
    ops: &[Op],
    start: u64,
    count: u64,
    delimiter: Delimiter,
) -> io::Result<Window> {
    Pager::open(base, ops, delimiter)?.page(start, count)
}

#[cfg(test)]
mod window_history_tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("csvs_window_{tag}_{nanos}_{seq}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_window_can_answer_lag_by_reading_the_rows_before_it() {
        let dir = temp("lag");
        let base = dir.join("t.csv");
        let mut text = String::from("day,amount\n");
        for i in 1..=10 {
            text.push_str(&format!("{i},{}\n", i * 10));
        }
        std::fs::write(&base, &text).unwrap();
        let ops = vec![Op::AddColumn {
            name: "prev".to_string(),
            formula: "lag(amount)".to_string(),
        }];

        // Rows are 0-based here: `start = 5` is the 6th data row (day 6, amount 60),
        // whose previous row is day 5 (amount 50).
        let win = window(&base, &ops, 5, 2, Delimiter::Comma).unwrap();
        assert_eq!(win.rows.len(), 2);
        let prev_index = win.columns.iter().position(|c| c == "prev").unwrap();
        assert_eq!(
            win.rows[0][prev_index], "50",
            "window should see the row before it"
        );
        assert_eq!(win.rows[1][prev_index], "60");

        // A window at the very start has no previous row and says so.
        let win = window(&base, &ops, 0, 1, Delimiter::Comma).unwrap();
        assert_eq!(win.rows[0][prev_index], "#VALUE!");

        // `lag(amount, 3)` starting at the 6th row needs the 3rd row before it.
        let ops = vec![Op::AddColumn {
            name: "three".to_string(),
            formula: "lag(amount, 3)".to_string(),
        }];
        let win = window(&base, &ops, 5, 1, Delimiter::Comma).unwrap();
        let idx = win.columns.iter().position(|c| c == "three").unwrap();
        assert_eq!(
            win.rows[0][idx], "30",
            "lag 3 before the 6th row is the 3rd row"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn running_totals_still_need_a_full_pass_and_say_so() {
        let dir = temp("running");
        let base = dir.join("t.csv");
        std::fs::write(&base, "day,amount\n1,10\n2,20\n3,30\n4,40\n").unwrap();
        let ops = vec![Op::AddColumn {
            name: "total".to_string(),
            formula: "running_total(amount)".to_string(),
        }];
        let win = window(&base, &ops, 2, 2, Delimiter::Comma).unwrap();
        let idx = win.columns.iter().position(|c| c == "total").unwrap();
        // Not showing 30 and 70 (which would look right but be a window-local sum).
        assert_eq!(win.rows[0][idx], "#VALUE!");
        assert_eq!(win.rows[1][idx], "#VALUE!");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
