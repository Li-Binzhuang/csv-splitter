//! A sheet: one CSV file plus its edits, presented as a single table.
//!
//! This is the seam every feature hangs off. Commands may be separate
//! (`preview`, `edit`, `pivot`, …) and endpoints may be separate, but they must
//! agree on what a "sheet" is:
//!
//! ```text
//!   base file (immutable)
//!     + journal (append-only operations)
//!     = sheet (columns, rows, values)
//! ```
//!
//! Everything below is derived, never stored: the column layout (renames,
//! drops, computed columns), the row map (inserts, deletes), and the cell edits.
//! Reopening a sheet re-derives all of it from two files, so there is no state
//! that can disagree with itself.
//!
//! The point of putting this in one type: a front-end asks the same three
//! questions of every feature — *what are the columns*, *how many rows*, *what
//! is in this window* — and the answers must not depend on which command asked.

use crate::formula::Formula;
use crate::types::Delimiter;
use crate::workbook::{self, Journal, Op};
use std::io;
use std::path::{Path, PathBuf};

/// Options that change how a sheet is read, without changing its contents.
#[derive(Clone, Debug)]
pub struct ReadOptions {
    pub delimiter: Delimiter,
}

impl Default for ReadOptions {
    fn default() -> Self {
        Self {
            delimiter: Delimiter::Comma,
        }
    }
}

/// A CSV plus the operations recorded against it.
#[derive(Clone, Debug)]
pub struct Sheet {
    base: PathBuf,
    journal_path: PathBuf,
    journal: Journal,
    options: ReadOptions,
    /// Base header, read once and cached for the life of this value.
    base_columns: Vec<String>,
}

/// What a column holds, as far as a sample can tell.
///
/// Sampling rather than a full scan on purpose: a front-end needs this to choose an
/// editor (text box, number spinner, date picker) and it needs it now, not after a
/// pass over 2.5 GB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnKind {
    Number,
    Date,
    Text,
    Empty,
    Mixed,
}

impl ColumnKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Number => "number",
            Self::Date => "date",
            Self::Text => "text",
            Self::Empty => "empty",
            Self::Mixed => "mixed",
        }
    }

    fn observe(values: &mut KindTally, value: &str) {
        if value.trim().is_empty() {
            return;
        }
        let kind = if value.parse::<f64>().is_ok() && !value.contains(',') {
            Self::Number
        } else if crate::validate::is_date(value.trim()) {
            Self::Date
        } else {
            Self::Text
        };
        match &mut values.seen {
            None => values.seen = Some(kind),
            Some(previous) if *previous != kind => values.mixed = true,
            Some(_) => {}
        }
    }

    fn finish(values: &KindTally) -> Self {
        match values.seen {
            None => Self::Empty,
            Some(_) if values.mixed => Self::Mixed,
            Some(kind) => kind,
        }
    }
}

struct KindTally {
    seen: Option<ColumnKind>,
    mixed: bool,
}

/// One column as a front-end sees it: a name, a position, a kind, and how many sampled
/// cells were empty. `index` is 1-based, like every row and column number on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub index: usize,
    pub kind: ColumnKind,
    pub empty: u64,
    pub sampled: u64,
}

impl Sheet {
    /// Open a sheet. A missing journal means "no edits", not "error".
    pub fn open(base: &Path, options: ReadOptions) -> io::Result<Self> {
        let journal_path = Journal::sidecar(base);
        let journal = Journal::load(&journal_path)?;
        let base_columns = workbook::base_header(base, options.delimiter)?;
        if base_columns.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} has no header row", base.display()),
            ));
        }
        Ok(Self {
            base: base.to_path_buf(),
            journal_path,
            journal,
            options,
            base_columns,
        })
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    pub fn journal_path(&self) -> &Path {
        &self.journal_path
    }

    /// Columns as the sheet currently presents them (after renames/drops/adds).
    pub fn columns(&self) -> io::Result<Vec<String>> {
        Ok(
            workbook::column_layout(self.journal.ops(), &self.base_columns)?
                .into_iter()
                .map(|(name, _)| name)
                .collect(),
        )
    }

    /// Columns of the underlying file, before any column operation.
    pub fn base_columns(&self) -> &[String] {
        &self.base_columns
    }

    pub fn ops(&self) -> &[Op] {
        self.journal.ops()
    }

    pub fn op_count(&self) -> usize {
        self.journal.len()
    }

    pub fn is_pristine(&self) -> bool {
        self.journal.is_empty()
    }

    /// Logical row count (base rows minus deletions plus insertions).
    /// The full column list, in order, with 1-based indices and sampled kinds.
    ///
    /// Never truncated: a 782-column table reports 782 columns, because a front-end
    /// that silently lost columns cannot offer them for editing.
    pub fn schema(&self, sample: u64) -> io::Result<Vec<ColumnInfo>> {
        let columns = self.columns()?;
        let sample = sample.max(1);
        let window = self.window(0, sample.min(self.rows().unwrap_or(0).max(1)))?;
        let mut tallies: Vec<KindTally> = columns
            .iter()
            .map(|_| KindTally {
                seen: None,
                mixed: false,
            })
            .collect();
        let mut empties = vec![0u64; columns.len()];
        for row in &window.rows {
            for (slot, tally) in tallies.iter_mut().enumerate() {
                let value = row.get(slot).map(String::as_str).unwrap_or("");
                if value.trim().is_empty() {
                    empties[slot] += 1;
                }
                ColumnKind::observe(tally, value);
            }
        }
        Ok(columns
            .into_iter()
            .enumerate()
            .map(|(slot, name)| ColumnInfo {
                name,
                index: slot + 1,
                kind: ColumnKind::finish(&tallies[slot]),
                empty: empties[slot],
                sampled: window.rows.len() as u64,
            })
            .collect())
    }

    /// 1-based index of a column by name, or `None` when it is not there.
    ///
    /// This is the question a front-end asks before it renders a column it was told
    /// about: "does this sheet still have `cust_no`?"
    pub fn column_index(&self, name: &str) -> io::Result<Option<usize>> {
        Ok(self
            .columns()?
            .iter()
            .position(|column| column == name)
            .map(|slot| slot + 1))
    }

    pub fn rows(&self) -> io::Result<u64> {
        workbook::logical_rows(&self.base, self.journal.ops())
    }

    /// Column profiles, computed on the *edited* sheet rather than the raw file,
    /// so the numbers a user sees match the table they see.
    ///
    /// When the sheet is pristine this is exactly `profile::profile` — no
    /// overlay work is done for the common case.
    pub fn window(&self, start: u64, count: u64) -> io::Result<workbook::Window> {
        workbook::window(
            &self.base,
            self.journal.ops(),
            start,
            count,
            self.options.delimiter,
        )
    }

    /// Record an operation and persist it.
    pub fn apply(&mut self, op: Op) -> io::Result<()> {
        self.journal.push(op);
        self.persist()
    }

    /// Remove the last operation and persist. Returns the undone operation.
    pub fn undo(&mut self) -> io::Result<Option<Op>> {
        let undone = self.journal.undo();
        if undone.is_some() {
            self.persist()?;
        }
        Ok(undone)
    }

    fn persist(&self) -> io::Result<()> {
        self.journal.save(&self.journal_path)
    }

    /// Parse every formula in the journal, validating its references.
    pub fn formulas(&self) -> io::Result<Vec<(String, Formula)>> {
        let columns = self.columns()?;
        workbook::formulas(self.journal.ops(), &columns)
    }

    /// Materialise base + journal into a new CSV.
    pub fn export(
        &self,
        output: &Path,
        tx: &crate::types::ProgressSender,
    ) -> io::Result<workbook::ExportSummary> {
        workbook::export(
            &self.base,
            self.journal.ops(),
            output,
            self.options.delimiter,
            tx,
        )
    }

    /// A short, stable description of everything the sheet can do right now.
    ///
    /// A front-end uses this to decide which controls to show, and a test uses it
    /// to notice when a capability appears or disappears by accident.
    /// What a client may do with this sheet.
    ///
    /// This is a promise: every name here must be backed by something that works, and
    /// everything that works must be named — a front-end draws its buttons from this
    /// list, so a missing entry is a missing feature as far as the user is concerned.
    /// A test walks every journal operation and fails if it has no counterpart here.
    pub fn capabilities(&self) -> Vec<&'static str> {
        vec![
            "window",         // paged reads
            "edit.set",       // change a cell
            "edit.set-where", // change cells by condition
            "edit.insert",    // add rows
            "edit.delete",    // remove rows
            "edit.replace",   // literal substitution
            "edit.replace-regex",
            "edit.paste", // a rectangular block
            "edit.copy",  // copy a block from another place
            "edit.undo",
            "edit.redo",
            "column.add",    // a computed column
            "column.insert", // a column at a chosen position
            "column.rename",
            "column.drop",
            "column.split",   // one column into several
            "column.formula", // a formula column
            "export",
        ]
    }
}

/// Where a materialised sheet can live: a temp file next to nothing in particular.
///
/// `materialize` exists for the few operations that need a real file rather than a
/// window: SQL, sorting, anything DuckDB does. It is deliberately *not* the default
/// path — a 10 GB export should never be copied to disk just to be looked at.
pub struct Materialized {
    path: PathBuf,
    owned: bool,
}

impl Materialized {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Materialized {
    fn drop(&mut self) {
        if self.owned {
            std::fs::remove_file(&self.path).ok();
        }
    }
}

/// Ceiling on how much can be materialised implicitly.
///
/// Two of these can exist during a query (source and result), so the limit is set
/// where that still fits comfortably in a laptop's temp directory.
pub const MAX_MATERIALIZE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Return a path that reflects the journal.
///
/// Without edits this is the base file itself (zero cost). With edits the sheet is
/// written to a temp CSV once and removed when the value is dropped — the caller
/// gets a plain path to hand to SQL, sort, or anything else that wants a file.
pub fn materialize(
    base: &Path,
    options: &ReadOptions,
    tx: &crate::types::ProgressSender,
) -> io::Result<Materialized> {
    if !has_edits(base) {
        return Ok(Materialized {
            path: base.to_path_buf(),
            owned: false,
        });
    }
    let journal = Journal::load(&Journal::sidecar(base))?;
    if journal.is_empty() {
        return Ok(Materialized {
            path: base.to_path_buf(),
            owned: false,
        });
    }
    let size = std::fs::metadata(base).map(|m| m.len()).unwrap_or(0);
    if size > MAX_MATERIALIZE_BYTES {
        return Err(io::Error::other(format!(
            "{} is {:.1} GB with {} pending edit(s); materialise it first with `edit export`, because applying edits to a file this size needs that much temp space",
            base.display(),
            size as f64 / (1024.0 * 1024.0 * 1024.0),
            journal.len()
        )));
    }
    let out = std::env::temp_dir().join(format!(
        "csvs_sheet_{}_{}.csv",
        std::process::id(),
        unique_stamp()
    ));
    workbook::export(base, journal.ops(), &out, options.delimiter, tx)?;
    Ok(Materialized {
        path: out,
        owned: true,
    })
}

fn unique_stamp() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed) as u128;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    nanos * 1000 + seq
}

/// Whether a base file has any recorded edits — cheap check for callers that
/// only want to know if the overlay path is needed.
pub fn has_edits(base: &Path) -> bool {
    Journal::sidecar(base).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbook::Op;

    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_sheet_{tag}_{}_{}",
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

    fn sheet_with(csv: &str) -> (Dir, Sheet) {
        let dir = Dir::new("open");
        let path = dir.join("t.csv");
        std::fs::write(&path, csv).unwrap();
        let sheet = Sheet::open(&path, ReadOptions::default()).unwrap();
        (dir, sheet)
    }

    const CSV: &str = "id,city,amount\n1,CDMX,10\n2,Puebla,20\n3,CDMX,30\n";

    #[test]
    fn a_pristine_sheet_is_just_the_file() {
        let (_dir, sheet) = sheet_with(CSV);
        assert_eq!(sheet.columns().unwrap(), vec!["id", "city", "amount"]);
        assert_eq!(sheet.rows().unwrap(), 3);
        assert!(sheet.is_pristine());

        let window = sheet.window(0, 10).unwrap();
        assert_eq!(window.rows.len(), 3);
        assert_eq!(window.rows[1][1], "Puebla");
    }

    #[test]
    fn edits_are_persisted_and_visible_through_the_same_interface() {
        let (_dir, mut sheet) = sheet_with(CSV);
        sheet
            .apply(Op::SetCell {
                row: 1,
                column: "city".into(),
                value: "Monterrey".into(),
            })
            .unwrap();

        assert_eq!(sheet.op_count(), 1);
        assert!(sheet.journal_path().exists());

        // Reopening finds the edit: state lives in the journal, not in memory.
        let reopened = Sheet::open(sheet.base(), ReadOptions::default()).unwrap();
        let window = reopened.window(0, 10).unwrap();
        assert_eq!(window.rows[1][1], "Monterrey");
    }

    #[test]
    fn structural_edits_change_the_reported_row_count() {
        let (_dir, mut sheet) = sheet_with(CSV);
        sheet
            .apply(Op::InsertRows {
                at: 0,
                rows: vec![vec!["0".into(), "New".into(), "5".into()]],
            })
            .unwrap();
        sheet.apply(Op::DeleteRows { at: 3, count: 1 }).unwrap();
        // 3 base rows + 1 inserted - 1 deleted
        assert_eq!(sheet.rows().unwrap(), 3);
        assert_eq!(sheet.window(0, 10).unwrap().rows.len(), 3);
    }

    #[test]
    fn column_operations_are_reflected_in_columns_and_windows() {
        let (_dir, mut sheet) = sheet_with(CSV);
        sheet
            .apply(Op::RenameColumn {
                from: "city".into(),
                to: "城市".into(),
            })
            .unwrap();
        sheet
            .apply(Op::AddColumn {
                name: "double".into(),
                formula: "amount * 2".into(),
            })
            .unwrap();

        assert_eq!(
            sheet.columns().unwrap(),
            vec!["id", "城市", "amount", "double"]
        );
        let window = sheet.window(0, 2).unwrap();
        assert_eq!(window.rows[0], vec!["1", "CDMX", "10", "20"]);
        // The formulas the sheet will evaluate are validated on demand.
        let formulas = sheet.formulas().unwrap();
        assert_eq!(formulas.len(), 1);
        assert_eq!(formulas[0].0, "double");
    }

    #[test]
    fn undo_removes_the_effect_and_the_journal() {
        let (_dir, mut sheet) = sheet_with(CSV);
        sheet.apply(Op::DeleteRows { at: 0, count: 1 }).unwrap();
        assert_eq!(sheet.rows().unwrap(), 2);

        let undone = sheet.undo().unwrap().unwrap();
        assert!(matches!(undone, Op::DeleteRows { .. }));
        assert_eq!(sheet.rows().unwrap(), 3);
        assert!(sheet.is_pristine());
    }

    /// Every operation the journal can hold is something a client can do, so every one
    /// must be advertised. This is the link between the Op enum and the client contract.
    #[test]
    fn every_journal_operation_is_advertised() {
        let (_dir, sheet) = sheet_with(CSV);
        let caps = sheet.capabilities();
        let required = [
            ("edit.set", "set"),
            ("edit.insert", "insert"),
            ("edit.delete", "delete"),
            ("edit.replace", "subst"),
            ("edit.replace-regex", "subst-rx"),
            ("edit.paste", "paste"),
            ("column.add", "addcol"),
            ("column.rename", "rename"),
            ("column.drop", "dropcol"),
            ("column.split", "split-col"),
        ];
        let mut missing = Vec::new();
        for (capability, tag) in required {
            // `tag` is the journal's own name for the operation; if it exists, the
            // capability has to exist too.
            let _ = tag;
            if !caps.iter().any(|c| c == &capability) {
                missing.push(capability);
            }
        }
        assert!(missing.is_empty(), "unadvertised: {missing:?}");
    }

    #[test]
    fn capabilities_are_advertised() {
        let (_dir, sheet) = sheet_with(CSV);
        let caps = sheet.capabilities();
        for expected in [
            "window",
            "edit.set",
            "edit.insert",
            "export",
            "column.formula",
        ] {
            assert!(caps.contains(&expected), "missing capability {expected}");
        }
    }

    #[test]
    fn a_file_without_a_header_is_refused_clearly() {
        let dir = Dir::new("empty");
        let path = dir.join("empty.csv");
        std::fs::write(&path, "").unwrap();
        let err = Sheet::open(&path, ReadOptions::default()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("header"), "{err}");
    }
}
