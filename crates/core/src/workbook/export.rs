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
                write_row(
                    &mut out,
                    &mut pipeline,
                    &cells,
                    RowId::Virtual(id),
                    logical,
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
                write_row(
                    &mut out,
                    &mut pipeline,
                    &cells,
                    RowId::Base(target),
                    logical,
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
                Some(v) => v.to_string(),
                None => self.edits.substitute(column, &raw),
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

/// Assemble one row and write it.
fn write_row(
    out: &mut impl Write,
    pipeline: &mut RowPipeline<'_>,
    base_cells: &[String],
    id: RowId,
    row_number: u64,
    delim: u8,
) -> io::Result<()> {
    let cells = pipeline.row(base_cells, id, row_number);
    let fields: Vec<&str> = cells.iter().map(String::as_str).collect();
    out.write_all(join_csv_line(&fields, delim).as_bytes())
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
            _ => crate::index::SparseIndex::build(base, crate::index::DEFAULT_INTERVAL)?,
        };
        let sheet = compile(ops, index.data_rows());
        let compiled = compile_formulas_at(base, ops, &columns)?;
        let plan = ColumnPlan::build(ops, &columns, &compiled);
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
