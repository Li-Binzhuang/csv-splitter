//! Editing a huge CSV without rewriting it.
//!
//! Excel and WPS load the file and keep it in memory, which is exactly what this
//! tool cannot do for a 10 GB export. So editing works the other way round:
//!
//! ```text
//!   immutable base file  +  append-only journal  =  a "sheet"
//! ```
//!
//! Every edit is an operation appended to a sidecar journal (`data.csv.journal`).
//! The base file is never touched. Reading a window means: map the logical row to
//! its physical row, then apply the edits that touch it — cost proportional to
//! the number of edits, never to the number of rows. Materialising an export is a
//! single streaming pass.
//!
//! Why a journal rather than an overlay file:
//!
//! * **Undo is free** — pop the last operation.
//! * **It is inspectable** — the journal is a CSV of operations, readable in any
//!   editor, diffable, and safe to commit next to the data.
//! * **It stays small** — a thousand cell edits are kilobytes, however big the
//!   file is.
//! * **The original stays intact** — if an edit turns out wrong, the source is
//!   still there, and `export` simply stops applying it.

use crate::csv_reader::{
    join_csv_line, parse_csv_record, parse_csv_record_with, QuoteStyle, RecordReader,
};
use crate::encoding;
use crate::formula::Formula;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::collections::HashMap;
use std::fs::File;
mod export;
mod journal;
mod rows;

pub use journal::{Journal, RowRef};
pub use rows::{
    column_layout, compile, compile_formulas, compile_formulas_at, formulas, parse_a1,
    parse_a1_range, regex_replace, resolve_columns, CellEdits, ColumnPlan, CompiledFormulas, RowId,
    RowMap,
};

pub use export::{export, export_quotes, logical_rows, window, ExportSummary, Pager, Window};

use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Ceiling on cells in one pasted block.
///
/// A paste is stored as literal values, so its journal cost is the block itself.
/// Beyond this size the right tool is `edit set --where` (one rule) or an export,
/// not a million-value journal entry.
pub const MAX_BLOCK_CELLS: usize = 100_000;

/// Where an inserted column's values come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnSource {
    /// The same text in every row.
    Literal(String),
    /// An expression, evaluated per row like any other computed column.
    Formula(String),
}

/// One recorded edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Replace one cell. Rows are logical (0-based, header excluded).
    SetCell {
        row: u64,
        column: String,
        value: String,
    },
    /// Insert rows at a logical position, with explicit cell values.
    InsertRows {
        at: u64,
        rows: Vec<Vec<String>>,
    },
    /// Delete `count` logical rows starting at `at`.
    DeleteRows {
        at: u64,
        count: u64,
    },
    /// Set a value in every row that matches a filter.
    ///
    /// Recorded as a **rule**, not as one operation per matching row: a bulk edit
    /// over a million rows would otherwise turn a 60-byte journal into megabytes.
    /// The rule is evaluated while the sheet is read, so it applies to the data as
    /// it stands at that moment — which is also how a spreadsheet's "fill down"
    /// behaves when rows are inserted afterwards.
    SetWhere {
        filter: String,
        column: String,
        value: String,
    },
    /// Replace a substring in one column (`None` = every column).
    Substitute {
        column: Option<String>,
        from: String,
        to: String,
    },
    /// Split one column into several, by a literal separator.
    SplitColumn {
        column: String,
        by: String,
        into: Vec<String>,
    },
    /// Regex substitution, using the same pattern subset as conditional formatting.
    SubstituteRegex {
        column: Option<String>,
        pattern: String,
        to: String,
    },
    /// Append a computed column (formula evaluated at export time).
    AddColumn {
        name: String,
        formula: String,
    },
    /// Paste a rectangular block of literal values at a position.
    ///
    /// Copy/paste stores the *values*, not a reference to the source range: if it
    /// stored a reference, editing the source later would silently change what had
    /// already been pasted. The block is bounded (`MAX_BLOCK_CELLS`) because a
    /// million-cell paste belongs in an export, not in a journal.
    PasteBlock {
        row: u64,
        column: String,
        rows: Vec<Vec<String>>,
    },
    /// Insert a column, either a constant or a formula, at a chosen position.
    ///
    /// Inserting rather than appending is what makes this usable for real tidy-up
    /// work: "add a note column right after city" is the common request, and a
    /// column that lands at the far right is not the same thing.
    AddColumnAt {
        /// Insert after this column; `None` means append.
        after: Option<String>,
        name: String,
        source: ColumnSource,
    },
    DropColumn {
        name: String,
    },
    RenameColumn {
        from: String,
        to: String,
    },
}

impl Op {
    /// The verb used in the journal file (also what `edit --json` reports).
    pub fn tag(&self) -> &'static str {
        match self {
            Self::SetCell { .. } => "set",
            Self::InsertRows { .. } => "insert",
            Self::DeleteRows { .. } => "delete",
            Self::SetWhere { .. } => "setwhere",
            Self::Substitute { .. } => "substitute",
            Self::SubstituteRegex { .. } => "subst-rx",
            Self::SplitColumn { .. } => "split-col",
            Self::AddColumn { .. } => "addcolumn",
            Self::AddColumnAt { .. } => "addcolumnat",
            Self::PasteBlock { .. } => "pasteblock",
            Self::DropColumn { .. } => "dropcolumn",
            Self::RenameColumn { .. } => "renamecolumn",
        }
    }

    /// One-line human description, used by `edit status` and by the API.
    pub fn describe(&self) -> String {
        match self {
            Self::SetCell { row, column, value } => {
                format!("set row {row} '{column}' = '{}'", truncate(value, 40))
            }
            Self::InsertRows { at, rows } => format!("insert {} row(s) at {at}", rows.len()),
            Self::DeleteRows { at, count } => format!("delete {count} row(s) at {at}"),
            Self::SetWhere {
                filter,
                column,
                value,
            } => format!("set '{column}' = '{}' where {filter}", truncate(value, 30)),
            Self::Substitute { column, from, to } => format!(
                "substitute '{}' → '{}' in {}",
                truncate(from, 20),
                truncate(to, 20),
                column.as_deref().unwrap_or("every column")
            ),
            Self::SplitColumn { column, by, into } => format!(
                "split '{column}' by '{}' into {}",
                truncate(by, 12),
                into.join(", ")
            ),
            Self::SubstituteRegex {
                column,
                pattern,
                to,
            } => format!(
                "replace /{}/ → '{}' in {}",
                truncate(pattern, 24),
                truncate(to, 20),
                column.as_deref().unwrap_or("every column")
            ),
            Self::AddColumn { name, formula } => format!("add column '{name}' = {formula}"),
            Self::PasteBlock { row, column, rows } => format!(
                "paste {} × {} block at row {row} '{column}'",
                rows.len(),
                rows.first().map(Vec::len).unwrap_or(0)
            ),
            Self::AddColumnAt {
                after,
                name,
                source,
            } => match source {
                ColumnSource::Literal(value) => format!(
                    "insert column '{name}' = '{}' {}",
                    truncate(value, 30),
                    after
                        .as_ref()
                        .map(|a| format!("after '{a}'"))
                        .unwrap_or_else(|| "at the end".to_string())
                ),
                ColumnSource::Formula(formula) => format!(
                    "insert column '{name}' = {formula} {}",
                    after
                        .as_ref()
                        .map(|a| format!("after '{a}'"))
                        .unwrap_or_else(|| "at the end".to_string())
                ),
            },
            Self::DropColumn { name } => format!("drop column '{name}'"),
            Self::RenameColumn { from, to } => format!("rename '{from}' → '{to}'"),
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    let chars: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        format!("{chars}…")
    } else {
        chars
    }
}

/// Read the base file's header.
pub fn base_header(base: &Path, delimiter: Delimiter) -> io::Result<Vec<String>> {
    let reader = encoding::open_decoded(base)?;
    let mut records = RecordReader::new(BufReader::with_capacity(256 * 1024, reader), 256 * 1024);
    let mut record = Vec::with_capacity(4096);
    if !records.next_record(&mut record)? {
        return Ok(Vec::new());
    }
    Ok(parse_csv_record(
        String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
        delimiter.byte(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_wb_{tag}_{}_{}",
                std::process::id(),
                unique_seq()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn join(&self, n: &str) -> std::path::PathBuf {
            self.0.join(n)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn base_with(dir: &Dir, csv: &str) -> std::path::PathBuf {
        let p = dir.join("base.csv");
        std::fs::write(&p, csv).unwrap();
        p
    }

    fn export_with(dir: &Dir, base: &std::path::Path, ops: Vec<Op>) -> String {
        let out = dir.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        export(base, &ops, &out, Delimiter::Comma, &tx).unwrap();
        std::fs::read_to_string(&out).unwrap()
    }

    const BASE: &str = "id,city,amount\n1,CDMX,10\n2,Puebla,20\n3,CDMX,30\n4,Tijuana,40\n";

    #[test]
    fn a_set_cell_changes_exactly_one_row() {
        let dir = Dir::new("set");
        let base = base_with(&dir, BASE);
        let ops = vec![Op::SetCell {
            row: 1,
            column: "city".into(),
            value: "Monterrey".into(),
        }];
        let text = export_with(&dir, &base, ops);
        assert_eq!(
            text,
            "id,city,amount\n1,CDMX,10\n2,Monterrey,20\n3,CDMX,30\n4,Tijuana,40\n"
        );
    }

    #[test]
    fn inserted_rows_shift_later_edits() {
        let dir = Dir::new("insert");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::InsertRows {
                at: 1,
                rows: vec![vec!["9".into(), "New".into(), "50".into()]],
            },
            // Row 2 is *after* the insert, so it is the original row 1.
            Op::SetCell {
                row: 2,
                column: "city".into(),
                value: "Edited".into(),
            },
        ];
        let text = export_with(&dir, &base, ops);
        assert_eq!(
            text,
            "id,city,amount\n1,CDMX,10\n9,New,50\n2,Edited,20\n3,CDMX,30\n4,Tijuana,40\n"
        );
    }

    #[test]
    fn deleted_rows_disappear_and_later_edits_shift() {
        let dir = Dir::new("delete");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::DeleteRows { at: 1, count: 2 },
            Op::SetCell {
                row: 1,
                column: "city".into(),
                value: "Last".into(),
            },
        ];
        let text = export_with(&dir, &base, ops);
        assert_eq!(text, "id,city,amount\n1,CDMX,10\n4,Last,40\n");
    }

    #[test]
    fn substitute_hits_one_column_or_every_column() {
        let dir = Dir::new("sub");
        let base = base_with(&dir, BASE);
        let ops = vec![Op::Substitute {
            column: Some("city".into()),
            from: "CDMX".into(),
            to: "Ciudad de México".into(),
        }];
        let text = export_with(&dir, &base, ops);
        assert!(text.contains("Ciudad de México"));
        assert!(!text.contains(",CDMX,"), "{text}");

        let ops = vec![Op::Substitute {
            column: None,
            from: "1".into(),
            to: "one".into(),
        }];
        let text = export_with(&dir, &base, ops);
        assert!(text.contains("one,CDMX,one0"), "{text}");
    }

    #[test]
    fn column_operations_rewrite_the_header() {
        let dir = Dir::new("cols");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::DropColumn { name: "id".into() },
            Op::RenameColumn {
                from: "city".into(),
                to: "城市".into(),
            },
            Op::AddColumn {
                name: "double".into(),
                formula: "amount * 2".into(),
            },
        ];
        let text = export_with(&dir, &base, ops);
        assert_eq!(
            text.lines().next().unwrap(),
            "城市,amount,double",
            "header should follow the operations"
        );
        // The computed column is evaluated per row, and the renamed column keeps
        // its data (it used to come out blank when matched by its old name).
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "CDMX,10,20", "{text}");
        assert_eq!(lines[4], "Tijuana,40,80", "{text}");
    }

    #[test]
    fn journal_round_trips_through_its_csv_sidecar() {
        let dir = Dir::new("journal");
        let base = base_with(&dir, BASE);
        let path = Journal::sidecar(&base);

        let mut journal = Journal::new();
        journal.push(Op::SetCell {
            row: 3,
            column: "city".into(),
            value: "with, comma and \"quote\"".into(),
        });
        journal.push(Op::InsertRows {
            at: 0,
            rows: vec![vec!["0".into(), "zero".into(), "-".into()]],
        });
        journal.push(Op::Substitute {
            column: None,
            from: "a".into(),
            to: "A".into(),
        });
        journal.save(&path).unwrap();

        let loaded = Journal::load(&path).unwrap();
        assert_eq!(loaded, journal, "journal must survive a save/load cycle");

        // And the reloaded journal produces the same export.
        let a = export_with(&dir, &base, journal.ops().to_vec());
        let b = export_with(&dir, &base, loaded.ops().to_vec());
        assert_eq!(a, b);
    }

    #[test]
    fn undo_pops_the_last_operation() {
        let dir = Dir::new("undo");
        let base = base_with(&dir, BASE);
        let mut journal = Journal::new();
        journal.push(Op::SetCell {
            row: 0,
            column: "city".into(),
            value: "X".into(),
        });
        let before = export_with(&dir, &base, journal.ops().to_vec());
        assert!(before.contains(",X,"), "{before}");

        assert_eq!(
            journal.undo(),
            Some(Op::SetCell {
                row: 0,
                column: "city".into(),
                value: "X".into()
            })
        );
        let after = export_with(&dir, &base, journal.ops().to_vec());
        assert_eq!(after, BASE, "undo should restore the original");
        assert!(journal.is_empty());
    }

    #[test]
    fn row_map_resolves_windows_without_touching_the_file() {
        // 10 base rows, delete rows 2..5, insert 2 rows at 0.
        let ops = vec![
            Op::DeleteRows { at: 2, count: 3 },
            Op::InsertRows {
                at: 0,
                rows: vec![vec!["a".into()], vec!["b".into()]],
            },
        ];
        let map = RowMap::build(&ops, 10);
        assert_eq!(map.len(), 10 - 3 + 2);

        assert!(matches!(map.row(0), Some(RowRef::Virtual { .. })));
        assert_eq!(map.row(2), Some(RowRef::Base(0)));
        // Base rows 2,3,4 were deleted, so logical 2 is base 0 and logical 3 base 1.
        assert_eq!(map.row(3), Some(RowRef::Base(1)));
        // Then it jumps over the deleted range to base 5.
        assert_eq!(map.row(4), Some(RowRef::Base(5)));

        let runs = map.physical_runs(0, 5);
        assert_eq!(runs.len(), 3, "virtual block, then two base runs: {runs:?}");
        assert_eq!(runs[1].logical_start(), 2);
    }

    #[test]
    fn a_huge_file_with_a_few_edits_keeps_a_tiny_journal() {
        // 100k base rows recorded as a length only: the map must not iterate them.
        let ops = vec![
            Op::DeleteRows { at: 0, count: 5 },
            Op::InsertRows {
                at: 7,
                rows: vec![vec!["new".into()]],
            },
            Op::SetCell {
                row: 1000,
                column: "x".into(),
                value: "y".into(),
            },
        ];
        let map = RowMap::build(&ops, 100_000);
        assert_eq!(map.len(), 100_000 - 5 + 1);
        assert_eq!(map.row(0), Some(RowRef::Base(5)));
        // A row far past every edit still resolves in O(log segments).
        // 5 deleted at the top, 1 inserted after them: 99000 + 5 - 1.
        assert_eq!(map.row(99_000), Some(RowRef::Base(99_004)));
    }

    #[test]
    fn window_applies_edits_without_rewriting_the_file() {
        let dir = Dir::new("window");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::SetCell {
                row: 1,
                column: "city".into(),
                value: "Monterrey".into(),
            },
            Op::InsertRows {
                at: 2,
                rows: vec![vec!["9".into(), "New".into(), "50".into()]],
            },
        ];

        // The source file must be untouched.
        assert_eq!(std::fs::read_to_string(&base).unwrap(), BASE);

        let w = window(&base, &ops, 0, 10, Delimiter::Comma).unwrap();
        assert_eq!(w.total, 5, "4 base rows + 1 inserted");
        assert_eq!(w.columns, vec!["id", "city", "amount"]);
        assert_eq!(w.rows[1][1], "Monterrey", "edit visible");
        assert_eq!(w.rows[2][1], "New", "inserted row visible");

        // A window that lands only on the tail must not include earlier rows.
        let w = window(&base, &ops, 2, 2, Delimiter::Comma).unwrap();
        assert_eq!(w.rows.len(), 2);
        assert_eq!(w.rows[0][1], "New");
        assert_eq!(w.rows[1][1], "CDMX");
    }

    /// The behaviour that separates a spreadsheet from a text editor: an edit
    /// stays attached to its row when rows above it are inserted.
    #[test]
    fn an_edit_survives_a_later_insert_above_it() {
        let dir = Dir::new("stable");
        let base = base_with(&dir, BASE);
        // Edit the last row, then insert a row at the very top.
        let ops = vec![
            Op::SetCell {
                row: 3,
                column: "city".into(),
                value: "Edited".into(),
            },
            Op::InsertRows {
                at: 0,
                rows: vec![vec!["0".into(), "Top".into(), "0".into()]],
            },
        ];
        let text = export_with(&dir, &base, ops);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 6, "header + 5 rows");
        assert_eq!(lines[1], "0,Top,0", "inserted row");
        // The edited row moved down by one and kept its edit.
        assert_eq!(lines[5], "4,Edited,40", "{text}");
    }

    #[test]
    fn an_edit_on_an_inserted_row_is_independent() {
        let dir = Dir::new("virtual_edit");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::InsertRows {
                at: 0,
                rows: vec![vec!["9".into(), "New".into(), "50".into()]],
            },
            // This row is the inserted one (logical 0 at the time of the edit).
            Op::SetCell {
                row: 0,
                column: "city".into(),
                value: "Newer".into(),
            },
            Op::InsertRows {
                at: 0,
                rows: vec![vec!["8".into(), "First".into(), "40".into()]],
            },
        ];
        let text = export_with(&dir, &base, ops);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "8,First,40");
        assert_eq!(lines[2], "9,Newer,50", "edit follows the inserted row");
    }

    #[test]
    fn a_formula_that_reads_a_dropped_column_is_refused() {
        let dir = Dir::new("badformula");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::DropColumn {
                name: "amount".into(),
            },
            Op::AddColumn {
                name: "double".into(),
                formula: "amount * 2".into(),
            },
        ];
        let out = dir.join("o.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = export(&base, &ops, &out, Delimiter::Comma, &tx).unwrap_err();
        // Failing before writing beats writing #VALUE! into a million rows.
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("amount"), "{err}");
    }

    #[test]
    fn formulas_see_edited_values() {
        let dir = Dir::new("formulaedit");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::SetCell {
                row: 0,
                column: "amount".into(),
                value: "100".into(),
            },
            Op::AddColumn {
                name: "double".into(),
                formula: "amount * 2".into(),
            },
        ];
        let text = export_with(&dir, &base, ops);
        assert_eq!(text.lines().nth(1).unwrap(), "1,CDMX,100,200", "{text}");
        assert_eq!(text.lines().nth(2).unwrap(), "2,Puebla,20,40", "{text}");
    }

    #[test]
    fn a_formula_can_reference_another_sheet() {
        let dir = Dir::new("sheetref");
        let base = dir.join("orders.csv");
        std::fs::write(&base, "order,cust_no,amount\nA,1,100\nB,2,50\nC,9,10\n").unwrap();
        let customers = dir.join("customers.csv");
        std::fs::write(&customers, "cust_no,name\n1,Ana\n2,Beto\n").unwrap();

        let ops = vec![Op::AddColumn {
            name: "customer".into(),
            formula: r#"ref("customers.csv", "cust_no", "name")"#.into(),
        }];
        let text = export_with(&dir, &base, ops);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "order,cust_no,amount,customer");
        assert_eq!(lines[1], "A,1,100,Ana");
        assert_eq!(lines[2], "B,2,50,Beto");
        // A key the other sheet does not have is #N/A, like VLOOKUP.
        assert_eq!(lines[3], "C,9,10,#N/A");
    }

    #[test]
    fn a_missing_referenced_sheet_fails_before_writing() {
        let dir = Dir::new("noref");
        let base = dir.join("orders.csv");
        std::fs::write(&base, "order,cust_no\nA,1\n").unwrap();
        let ops = vec![Op::AddColumn {
            name: "customer".into(),
            formula: r#"ref("nope.csv", "cust_no", "name")"#.into(),
        }];
        let out = dir.join("o.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = export(&base, &ops, &out, Delimiter::Comma, &tx).unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[test]
    fn a_bulk_edit_touches_every_matching_row_and_stays_small() {
        let dir = Dir::new("bulk");
        let base = base_with(&dir, BASE);
        let ops = vec![Op::SetWhere {
            filter: "city=CDMX".into(),
            column: "amount".into(),
            value: "0".into(),
        }];
        let text = export_with(&dir, &base, ops);
        // CDMX rows (1 and 3) get amount 0; the others keep theirs.
        assert_eq!(
            text,
            "id,city,amount\n1,CDMX,0\n2,Puebla,20\n3,CDMX,0\n4,Tijuana,40\n"
        );
    }

    #[test]
    fn a_bulk_edit_is_one_operation_however_many_rows_it_touches() {
        // The journal must not grow with the match count: this is a rule, not a
        // list of per-row operations.
        let dir = Dir::new("bulkjournal");
        let base = base_with(&dir, BASE);
        let path = Journal::sidecar(&base);
        let mut journal = Journal::new();
        journal.push(Op::SetWhere {
            filter: "city=CDMX".into(),
            column: "amount".into(),
            value: "0".into(),
        });
        journal.save(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 1, "{raw}");
        assert!(raw.len() < 64, "journal must stay tiny: {raw}");

        let loaded = Journal::load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        let text = export_with(&dir, &base, loaded.ops().to_vec());
        assert!(text.contains("1,CDMX,0"), "{text}");
    }

    #[test]
    fn a_bulk_edit_sees_edited_values() {
        let dir = Dir::new("bulkorder");
        let base = base_with(&dir, BASE);
        let ops = vec![
            // First make row 1 CDMX, then the bulk rule should catch it.
            Op::SetCell {
                row: 1,
                column: "city".into(),
                value: "CDMX".into(),
            },
            Op::SetWhere {
                filter: "city=CDMX".into(),
                column: "amount".into(),
                value: "0".into(),
            },
        ];
        let text = export_with(&dir, &base, ops);
        assert_eq!(
            text,
            "id,city,amount\n1,CDMX,0\n2,CDMX,0\n3,CDMX,0\n4,Tijuana,40\n"
        );
    }

    #[test]
    fn a_column_can_be_inserted_where_it_belongs() {
        let dir = Dir::new("insertcol");
        let base = base_with(&dir, BASE);
        let ops = vec![Op::AddColumnAt {
            after: Some("city".into()),
            name: "note".into(),
            source: ColumnSource::Literal("manual".into()),
        }];
        let text = export_with(&dir, &base, ops);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "id,city,note,amount", "{text}");
        assert_eq!(lines[1], "1,CDMX,manual,10", "{text}");
    }

    #[test]
    fn an_inserted_column_can_be_a_formula() {
        let dir = Dir::new("insertformula");
        let base = base_with(&dir, BASE);
        let ops = vec![Op::AddColumnAt {
            after: Some("id".into()),
            name: "double".into(),
            source: ColumnSource::Formula("amount * 2".into()),
        }];
        let text = export_with(&dir, &base, ops);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "id,double,city,amount", "{text}");
        assert_eq!(lines[1], "1,20,CDMX,10", "{text}");
    }

    #[test]
    fn inserting_after_a_missing_column_is_refused() {
        let dir = Dir::new("badinsert");
        let base = base_with(&dir, BASE);
        let ops = vec![Op::AddColumnAt {
            after: Some("nope".into()),
            name: "x".into(),
            source: ColumnSource::Literal("y".into()),
        }];
        let out = dir.join("o.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = export(&base, &ops, &out, Delimiter::Comma, &tx).unwrap_err();
        assert!(err.to_string().contains("no such column"), "{err}");
    }

    #[test]
    fn an_inserted_column_round_trips_through_the_journal() {
        let dir = Dir::new("insertjournal");
        let base = base_with(&dir, BASE);
        let path = Journal::sidecar(&base);
        let mut journal = Journal::new();
        journal.push(Op::AddColumnAt {
            after: Some("city".into()),
            name: "note".into(),
            source: ColumnSource::Literal("manual".into()),
        });
        journal.save(&path).unwrap();
        let loaded = Journal::load(&path).unwrap();
        assert_eq!(loaded, journal);
    }

    #[test]
    fn a_pasted_block_lands_on_the_rows_it_names() {
        let dir = Dir::new("paste");
        let base = base_with(&dir, BASE);
        let ops = vec![Op::PasteBlock {
            row: 2,
            column: "city".into(),
            rows: vec![vec!["A".into(), "1".into()], vec!["B".into(), "2".into()]],
        }];
        let text = export_with(&dir, &base, ops);
        let lines: Vec<&str> = text.lines().collect();
        // Rows 0 and 1 untouched, rows 2 and 3 overwritten.
        assert_eq!(lines[1], "1,CDMX,10", "{text}");
        assert_eq!(lines[3], "3,A,1", "{text}");
        assert_eq!(lines[4], "4,B,2", "{text}");
    }

    #[test]
    fn a_paste_follows_its_rows_when_something_is_inserted_above() {
        let dir = Dir::new("pastefollow");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::PasteBlock {
                row: 2,
                column: "city".into(),
                rows: vec![vec!["A".into(), "1".into()]],
            },
            Op::InsertRows {
                at: 0,
                rows: vec![vec!["0".into(), "Top".into(), "0".into()]],
            },
        ];
        let text = export_with(&dir, &base, ops);
        let lines: Vec<&str> = text.lines().collect();
        // The paste was on base row 2; after the insert it is at logical row 3.
        assert_eq!(lines[4], "3,A,1", "{text}");
    }

    #[test]
    fn paste_block_round_trips_through_the_journal() {
        let dir = Dir::new("pastejournal");
        let base = base_with(&dir, BASE);
        let path = Journal::sidecar(&base);
        let mut journal = Journal::new();
        journal.push(Op::PasteBlock {
            row: 0,
            column: "city".into(),
            rows: vec![vec!["X".into(), "Y".into()]],
        });
        journal.save(&path).unwrap();
        assert_eq!(Journal::load(&path).unwrap(), journal);
    }

    #[test]
    fn unknown_column_operations_are_refused() {
        let dir = Dir::new("badcols");
        let base = base_with(&dir, BASE);
        let ops = vec![Op::DropColumn {
            name: "nope".into(),
        }];
        let out = dir.join("o.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = export(&base, &ops, &out, Delimiter::Comma, &tx).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

// ---------------------------------------------------------------- window read

#[cfg(test)]
mod redo_tests {
    use super::*;
    use std::path::PathBuf;

    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("csvs_redo_{tag}_{nanos}_{seq}"));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn set(row: u64, value: &str) -> Op {
        Op::SetCell {
            row,
            column: "city".to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn undo_moves_the_operation_to_the_redo_stack() {
        let mut journal = Journal::new();
        journal.push(set(0, "a"));
        journal.push(set(1, "b"));

        let undone = journal.undo().unwrap();
        assert_eq!(journal.undo_len(), 1);
        assert_eq!(journal.redo_len(), 1);
        assert!(matches!(undone, Op::SetCell { ref value, .. } if value == "b"));

        let redone = journal.redo().unwrap();
        assert_eq!(journal.undo_len(), 2);
        assert_eq!(journal.redo_len(), 0);
        assert!(matches!(redone, Op::SetCell { ref value, .. } if value == "b"));
    }

    #[test]
    fn undo_and_redo_survive_a_save_and_reload() {
        let dir = TmpDir::new("redo");
        let base: PathBuf = dir.join("t.csv");
        std::fs::write(&base, "id,city\n1,x\n").unwrap();
        let sidecar = Journal::sidecar(&base);

        let mut journal = Journal::new();
        journal.push(set(0, "a"));
        journal.push(set(1, "b"));
        journal.undo().unwrap();
        journal.save(&sidecar).unwrap();

        let mut reloaded = Journal::load(&sidecar).unwrap();
        assert_eq!(reloaded.undo_len(), 1);
        assert_eq!(
            reloaded.redo_len(),
            1,
            "the redo stack is in its own sidecar"
        );
        reloaded.redo().unwrap();
        assert_eq!(reloaded.undo_len(), 2);
        reloaded.save(&sidecar).unwrap();
        // Saving with an empty redo stack removes the file rather than leaving it.
        assert!(!Journal::redo_sidecar(&base).exists());
    }

    #[test]
    fn a_new_edit_discards_the_redo_branch() {
        let mut journal = Journal::new();
        journal.push(set(0, "a"));
        journal.undo().unwrap();
        journal.push(set(0, "c"));
        assert_eq!(journal.redo_len(), 0, "editing after undo is a new branch");
        assert_eq!(journal.undo_len(), 1);
    }

    #[test]
    fn redo_on_an_empty_stack_is_harmless() {
        let mut journal = Journal::new();
        assert!(journal.redo().is_none());
        assert!(journal.undo().is_none());
    }
}

#[cfg(test)]
mod regex_tests {
    use super::*;
    use crate::workbook::journal::{parse_op, render_op};

    #[test]
    fn a_pattern_replaces_every_match() {
        assert_eq!(regex_replace("[0-9]", "a1b22c", "x"), "axbxxc");
        assert_eq!(regex_replace("ab", "abab", "-"), "--");
    }

    #[test]
    fn anchors_bind_to_the_whole_value() {
        assert_eq!(regex_replace("^1[0-9]{3}$", "1234", "ok"), "ok");
        // The same pattern does not match a value with more digits around it.
        assert_eq!(regex_replace("^1[0-9]{3}$", "x1234y", "ok"), "x1234y");
    }

    #[test]
    fn a_pattern_that_does_not_compile_leaves_the_value_alone() {
        assert_eq!(regex_replace("[", "abc", "x"), "abc");
    }

    #[test]
    fn non_ascii_values_are_handled_by_character() {
        // 谢谢你 is two matches, not one.
        assert_eq!(regex_replace("谢", "谢谢你", "bye"), "byebye你");
        assert_eq!(regex_replace("你", "谢谢你", "!"), "谢谢!");
    }

    #[test]
    fn the_op_round_trips_through_the_journal_format() {
        let op = Op::SubstituteRegex {
            column: Some("note".to_string()),
            pattern: r"\s+".to_string(),
            to: " ".to_string(),
        };
        let rendered = render_op(&op);
        let back = parse_op(rendered.as_bytes()).unwrap();
        match back {
            Op::SubstituteRegex {
                column,
                pattern,
                to,
            } => {
                assert_eq!(column.as_deref(), Some("note"));
                assert_eq!(pattern, r"\s+");
                assert_eq!(to, " ");
            }
            other => panic!("parsed as {other:?}"),
        }
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("csvs_hist_{tag}_{nanos}_{seq}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn the_plan_reports_the_history_it_needs() {
        let columns: Vec<String> = vec!["day".into(), "amount".into(), "prev".into()];
        let ops = vec![Op::AddColumn {
            name: "prev".to_string(),
            formula: "lag(amount)".to_string(),
        }];
        let compiled = compile_formulas(&ops, &columns).unwrap();
        let base: Vec<String> = vec!["day".into(), "amount".into()];
        let layout: Vec<(String, Option<usize>)> = column_layout(&ops, &base).unwrap();
        let plan = ColumnPlan::build(&ops, &base, &layout, &compiled);
        let needs = plan.row_needs();
        assert_eq!(needs.lags, vec![("amount".to_string(), 1)], "{needs:?}");
        assert!(needs.running.is_empty());
    }

    #[test]
    fn lag_and_running_total_see_previous_rows() {
        let dir = temp("lag");
        let base = dir.join("t.csv");
        std::fs::write(&base, "day,amount\n1,10\n2,20\n3,30\n").unwrap();
        let ops = vec![
            Op::AddColumn {
                name: "prev".to_string(),
                formula: "lag(amount)".to_string(),
            },
            Op::AddColumn {
                name: "running".to_string(),
                formula: "running_total(amount)".to_string(),
            },
        ];
        let out = dir.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        export(&base, &ops, &out, Delimiter::Comma, &tx).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "day,amount,prev,running");
        // The first row has no previous value; the total includes the current row.
        assert_eq!(lines[1], "1,10,#VALUE!,10", "{text}");
        assert_eq!(lines[2], "2,20,10,30", "{text}");
        assert_eq!(lines[3], "3,30,20,60", "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
