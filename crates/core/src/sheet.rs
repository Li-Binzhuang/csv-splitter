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

use crate::csv_reader::RecordReader;
use crate::encoding;
use crate::formula::Formula;
use crate::types::Delimiter;
use crate::workbook::{self, Journal, Op, RowId};
use std::io::{self, BufReader};
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
    pub fn capabilities(&self) -> Vec<&'static str> {
        vec![
            "window",      // paged reads
            "edit.set",    // change a cell
            "edit.insert", // add rows
            "edit.delete", // remove rows
            "edit.replace",
            "edit.undo",
            "column.rename",
            "column.drop",
            "column.formula",
            "export",
        ]
    }
}

/// Count the records in a file, header excluded (used when a caller only needs
/// the row count and has no index).
pub fn count_data_rows(base: &Path) -> io::Result<u64> {
    let mut records = RecordReader::new(
        BufReader::with_capacity(256 * 1024, encoding::open_decoded(base)?),
        256 * 1024,
    );
    let mut record = Vec::with_capacity(4096);
    let mut rows = 0u64;
    let mut first = true;
    while records.next_record(&mut record)? {
        if first {
            first = false;
            continue;
        }
        rows += 1;
    }
    Ok(rows)
}

/// Whether a base file has any recorded edits — cheap check for callers that
/// only want to know if the overlay path is needed.
pub fn has_edits(base: &Path) -> bool {
    Journal::sidecar(base).exists()
}

/// Identity of the row a caller is looking at.
///
/// Exposed so edit handlers can report *which* row an operation landed on
/// (useful when rows shift under an insert).
pub fn row_id(sheet: &Sheet, logical: u64) -> Option<RowId> {
    let sidecar = crate::index::SparseIndex::sidecar_path(sheet.base());
    let index = match crate::index::SparseIndex::load(&sidecar) {
        Ok(idx) if idx.matches(sheet.base()) => idx,
        _ => return None,
    };
    let map = workbook::RowMap::build(sheet.ops(), index.data_rows());
    map.id_at(logical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbook::Op;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_sheet_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
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
