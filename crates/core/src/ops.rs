//! Typed requests/responses for the operations the adapters share.
//!
//! Stage 2 starts with the three front-end reads: schema, view and filter. The
//! adapters still own their command-line/URL spelling; these types own the meaning,
//! so a future CLI/HTTP/MCP migration cannot drift on `limit`, row numbering or
//! delimiter semantics.

use crate::filter::{self, FilterSpec};
use crate::sheet::{ColumnInfo, ReadOptions, Sheet};
use crate::types::{Delimiter, ProgressEvent};
use crate::viewer::WindowViewer;
use crate::workbook::{Journal, Op, RowId, Window};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Read a sheet's columns and logical row count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaRequest {
    pub path: PathBuf,
    pub delimiter: Delimiter,
    pub sample: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaResponse {
    pub rows: u64,
    pub columns: Vec<ColumnInfo>,
}

impl SchemaRequest {
    /// Open the sheet once; adapters that also need `--has`/`--diff` reuse it.
    pub fn open(&self) -> io::Result<Sheet> {
        Sheet::open(
            &self.path,
            ReadOptions {
                delimiter: self.delimiter,
            },
        )
    }

    /// Sample columns without building the row index (the pristine-file path).
    pub fn sample(&self) -> io::Result<Vec<ColumnInfo>> {
        crate::sheet::schema_sample(&self.path, self.delimiter, self.sample.max(1))
    }

    /// Build the response from an already-open sheet, avoiding a second index pass.
    pub fn response(&self, sheet: &Sheet) -> io::Result<SchemaResponse> {
        let rows = sheet.rows()?;
        let columns = sheet.schema(self.sample.max(1))?;
        Ok(SchemaResponse { rows, columns })
    }

    pub fn execute(&self) -> io::Result<SchemaResponse> {
        let sheet = self.open()?;
        self.response(&sheet)
    }
}

/// Read one page of the edited sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewRequest {
    pub path: PathBuf,
    pub delimiter: Delimiter,
    pub start: u64,
    pub count: u64,
}

#[derive(Clone, Debug)]
pub struct ViewResponse {
    pub window: Window,
}

impl ViewRequest {
    pub fn execute(&self) -> io::Result<ViewResponse> {
        // No journal means the base bytes already are the sheet. The sparse viewer
        // reads only the requested window, matching the HTTP no-edit path instead of
        // paying for a Sheet index on every page.
        if !crate::sheet::has_edits(&self.path) {
            let mut viewer = WindowViewer::open(&self.path, self.delimiter)?;
            let columns = viewer.headers()?;
            let total = viewer.total_rows();
            if self.count == 0 || self.start >= total {
                return Ok(ViewResponse {
                    window: Window {
                        columns,
                        rows: Vec::new(),
                        keys: Vec::new(),
                        total,
                    },
                });
            }
            viewer.jump_to(self.start)?;
            let rows: Vec<Vec<String>> = viewer
                .rows()
                .iter()
                .take(self.count as usize)
                .cloned()
                .collect();
            let keys = (0..rows.len())
                .map(|i| RowId::Base(self.start + i as u64))
                .collect();
            return Ok(ViewResponse {
                window: Window {
                    columns,
                    rows,
                    keys,
                    total,
                },
            });
        }
        let sheet = Sheet::open(
            &self.path,
            ReadOptions {
                delimiter: self.delimiter,
            },
        )?;
        let window = sheet.window(self.start, self.count)?;
        Ok(ViewResponse { window })
    }
}

/// Apply a validated transaction batch. Parsing and preview stay in the adapters'
/// hands because their error spellings differ; the write itself is this one type.
#[derive(Clone, Debug)]
pub struct EditApplyRequest {
    pub path: PathBuf,
    pub delimiter: Delimiter,
    pub ops: Vec<Op>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditApplyResponse {
    /// Journal operations after the append, as the response reports them.
    pub applied: usize,
    pub operations: usize,
}

impl EditApplyRequest {
    pub fn execute(&self) -> io::Result<EditApplyResponse> {
        let _ = self.delimiter;
        let sidecar = Journal::sidecar(&self.path);
        let applied = crate::workbook::append_atomically(&sidecar, &self.ops)?;
        Ok(EditApplyResponse {
            applied,
            operations: self.ops.len(),
        })
    }
}

/// Scan a sheet for rows matching one predicate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterRequest {
    pub path: PathBuf,
    pub delimiter: Delimiter,
    pub spec: FilterSpec,
    pub limit: usize,
    pub budget: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterResponse {
    /// 1-based data rows (header excluded), matching `csvx filter --json`.
    pub rows: Vec<u64>,
    pub scanned: u64,
    pub matched: u64,
    pub truncated: bool,
}

impl FilterRequest {
    pub fn parse(
        path: PathBuf,
        delimiter: Delimiter,
        expression: &str,
        limit: usize,
    ) -> io::Result<Self> {
        let spec = FilterSpec::parse(expression)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        Ok(Self {
            path,
            delimiter,
            spec,
            limit,
            budget: None,
        })
    }

    pub fn execute(&self) -> io::Result<FilterResponse> {
        let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
        drop(rx);
        let result = filter::scan_with_budget(
            &self.path,
            self.delimiter,
            &self.spec,
            self.limit.max(1),
            self.budget,
            &tx,
        )?;
        Ok(FilterResponse {
            rows: result.row_numbers(),
            scanned: result.scanned,
            matched: result.matched,
            truncated: result.truncated,
        })
    }

    /// Resumable page: returns the next checkpoint when the scan stopped at its
    /// page budget, or `None` when the result is complete.
    pub fn execute_page(&self, cursor: usize) -> io::Result<(FilterResponse, Option<usize>)> {
        let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
        drop(rx);
        let page = filter::scan_page(
            &self.path,
            self.delimiter,
            &self.spec,
            self.limit.max(1),
            self.budget,
            cursor,
            &tx,
        )
        .or_else(|error| {
            if error.kind() == io::ErrorKind::InvalidInput {
                filter::scan_fast(
                    &self.path,
                    self.delimiter,
                    &self.spec,
                    self.limit.max(1),
                    self.budget,
                    &tx,
                )
                .map(|result| (result, None))
            } else {
                Err(error)
            }
        })?;
        let (result, next) = page;
        Ok((
            FilterResponse {
                rows: result.row_numbers(),
                scanned: result.scanned,
                matched: result.matched,
                truncated: result.truncated,
            },
            next,
        ))
    }

    /// The CLI's optimized path: same predicate, no page budget.
    pub fn execute_fast(&self) -> io::Result<FilterResponse> {
        let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
        drop(rx);
        let result = filter::scan_fast(
            &self.path,
            self.delimiter,
            &self.spec,
            self.limit.max(1),
            None,
            &tx,
        )?;
        Ok(FilterResponse {
            rows: result.row_numbers(),
            scanned: result.scanned,
            matched: result.matched,
            truncated: result.truncated,
        })
    }
}

/// A stable, testable path for a sheet: callers can use it to decide whether the
/// response is about the file they asked for.
pub fn canonical_sheet(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> (PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("csvx_ops_{}_{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.csv");
        std::fs::write(&path, "a,b\n1,x\n2,y\n3,z\n").unwrap();
        (dir, path)
    }

    #[test]
    fn schema_request_returns_rows_and_columns() {
        let (dir, path) = fixture("schema_request_returns_rows_and_columns");
        let response = SchemaRequest {
            path: path.clone(),
            delimiter: Delimiter::Comma,
            sample: 10,
        }
        .execute()
        .unwrap();
        assert_eq!(response.rows, 3);
        assert_eq!(response.columns.len(), 2);
        assert_eq!(response.columns[0].name, "a");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn view_request_returns_a_window() {
        let (dir, path) = fixture("view_request_returns_a_window");
        let response = ViewRequest {
            path: path.clone(),
            delimiter: Delimiter::Comma,
            start: 1,
            count: 1,
        }
        .execute()
        .unwrap();
        assert_eq!(response.window.total, 3);
        assert_eq!(response.window.rows.len(), 1);
        assert_eq!(response.window.rows[0][0], "2");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn edit_apply_writes_one_journal_batch() {
        let (dir, path) = fixture("edit_apply_writes_one_journal_batch");
        let columns = vec!["a".to_string(), "b".to_string()];
        let ops = crate::workbook::parse_ops("set:1:a:9", 3, &columns).unwrap();
        let response = EditApplyRequest {
            path: path.clone(),
            delimiter: Delimiter::Comma,
            ops,
        }
        .execute()
        .unwrap();
        assert_eq!(response.operations, 1);
        assert_eq!(response.applied, 1);
        assert!(crate::workbook::Journal::sidecar(&path).exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn typed_view_matches_the_sheet_window_without_edits() {
        let (dir, path) = fixture("typed_view_matches_the_sheet_window_without_edits");
        let sheet = Sheet::open(
            &path,
            ReadOptions {
                delimiter: Delimiter::Comma,
            },
        )
        .unwrap();
        let expected = sheet.window(1, 2).unwrap();
        let typed = ViewRequest {
            path: path.clone(),
            delimiter: Delimiter::Comma,
            start: 1,
            count: 2,
        }
        .execute()
        .unwrap();
        assert_eq!(typed.window.columns, expected.columns);
        assert_eq!(typed.window.rows, expected.rows);
        assert_eq!(typed.window.keys, expected.keys);
        assert_eq!(typed.window.total, expected.total);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn typed_view_returns_an_empty_window_past_the_end() {
        let (dir, path) = fixture("typed_view_returns_an_empty_window_past_the_end");
        let response = ViewRequest {
            path: path.clone(),
            delimiter: Delimiter::Comma,
            start: 99,
            count: 10,
        }
        .execute()
        .unwrap();
        assert_eq!(response.window.total, 3);
        assert!(response.window.rows.is_empty());
        assert!(response.window.keys.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn filter_returns_one_based_rows() {
        let (dir, path) = fixture("filter_returns_one_based_rows");
        let response = FilterRequest {
            path: path.clone(),
            delimiter: Delimiter::Comma,
            spec: FilterSpec::parse("a>1").unwrap(),
            limit: 10,
            budget: None,
        }
        .execute()
        .unwrap();
        assert_eq!(response.rows, vec![2, 3]);
        assert_eq!(response.scanned, 3);
        assert_eq!(response.matched, 2);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn filter_fast_path_matches_the_budgeted_path() {
        let (dir, path) = fixture("filter_fast_path_matches_the_budgeted_path");
        let request = FilterRequest::parse(path.clone(), Delimiter::Comma, "a>1", 10).unwrap();
        let slow = request.execute().unwrap();
        let fast = request.execute_fast().unwrap();
        assert_eq!(fast.rows, slow.rows);
        assert_eq!(fast.scanned, slow.scanned);
        assert_eq!(fast.matched, slow.matched);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn filter_page_returns_rows_and_optional_cursor() {
        let (dir, path) = fixture("filter_page_returns_rows_and_optional_cursor");
        let request = FilterRequest::parse(path.clone(), Delimiter::Comma, "a>1", 10).unwrap();
        let (response, next) = request.execute_page(0).unwrap();
        assert_eq!(response.rows, vec![2, 3]);
        assert!(next.is_none(), "small fixture must finish in one page");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn preview_reports_rows_ranges_and_journal_hash() {
        use crate::workbook::Op;
        let (dir, path) = fixture("preview_reports_rows_ranges_and_journal_hash");
        let ops = vec![
            Op::SetCell {
                row: 0,
                column: "a".to_string(),
                value: "9".to_string(),
            },
            Op::DeleteRows { at: 1, count: 1 },
        ];
        let preview = preview_edit(&path, &ops, 3, &["a".to_string(), "b".to_string()]).unwrap();
        assert_eq!(preview.operations, 2);
        assert_eq!(preview.rows_before, 3);
        assert_eq!(preview.rows_after, 2);
        assert_eq!(preview.row_delta, -1);
        assert_eq!(preview.affected_ranges, vec![(1, 1), (2, 2)]);
        assert_eq!(preview.columns, 2);
        assert!(!preview.journal_hash.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn preview_marks_rule_ops_instead_of_inventing_rows() {
        use crate::workbook::Op;
        let (dir, path) = fixture("preview_marks_rule_ops_instead_of_inventing_rows");
        let ops = vec![Op::SetWhere {
            filter: "a>1".to_string(),
            column: "b".to_string(),
            value: "z".to_string(),
        }];
        let preview = preview_edit(&path, &ops, 3, &["a".to_string(), "b".to_string()]).unwrap();
        assert_eq!(preview.rule_operations, 1);
        assert!(preview.affected_ranges.is_empty());
        assert_eq!(preview.rows_after, 3);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn filter_rejects_a_bad_expression_without_scanning() {
        let (dir, path) = fixture("filter_rejects_a_bad_expression_without_scanning");
        let error = FilterRequest::parse(path.clone(), Delimiter::Comma, "", 10).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        std::fs::remove_dir_all(dir).ok();
    }
}

/// A transaction preview: what the ops would do, without writing anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditPreview {
    pub operations: usize,
    pub rows_before: u64,
    pub rows_after: u64,
    pub row_delta: i64,
    /// 1-based inclusive ranges with an exact row target. Rule/structural ops are
    /// counted separately because they do not name concrete rows.
    pub affected_ranges: Vec<(u64, u64)>,
    pub rule_operations: usize,
    pub structural_operations: usize,
    pub columns: usize,
    pub base_bytes: u64,
    pub journal_bytes: u64,
    /// FNV-1a of the current journal bytes; a change detector, not a SHA.
    pub journal_hash: String,
}

impl EditPreview {
    pub fn affected_rows(&self) -> u64 {
        self.affected_ranges
            .iter()
            .map(|(start, end)| end.saturating_sub(*start) + 1)
            .sum()
    }

    pub fn affected_label(&self) -> String {
        if self.affected_ranges.is_empty() {
            return "-".to_string();
        }
        let shown: Vec<String> = self
            .affected_ranges
            .iter()
            .take(8)
            .map(|(start, end)| {
                if start == end {
                    start.to_string()
                } else {
                    format!("{start}-{end}")
                }
            })
            .collect();
        let extra = self.affected_ranges.len().saturating_sub(shown.len());
        if extra > 0 {
            format!("{} (+{extra} more)", shown.join(", "))
        } else {
            shown.join(", ")
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Describe a validated transaction against the current sheet shape.
///
/// The caller supplies the already-parsed ops and the current logical row count;
/// this function never opens the journal for writing and never touches the base file.
pub fn preview_edit(
    base: &Path,
    ops: &[crate::workbook::Op],
    total_rows: u64,
    columns: &[String],
) -> io::Result<EditPreview> {
    use crate::workbook::Op;

    let mut rows_after = total_rows as i64;
    let mut affected_ranges: Vec<(u64, u64)> = Vec::new();
    let mut rule_operations = 0usize;
    let mut structural_operations = 0usize;

    for op in ops {
        match op {
            Op::SetCell { row, .. } => affected_ranges.push((row + 1, row + 1)),
            Op::InsertRows { at, rows } => {
                if !rows.is_empty() {
                    affected_ranges.push((at + 1, at + rows.len() as u64));
                    rows_after += rows.len() as i64;
                }
            }
            Op::DeleteRows { at, count } => {
                if *count > 0 {
                    affected_ranges.push((at + 1, at + count));
                    rows_after -= *count as i64;
                }
            }
            Op::DeleteRanges { runs } => {
                for (at, count) in runs {
                    if *count > 0 {
                        affected_ranges.push((at + 1, at + count));
                        rows_after -= *count as i64;
                    }
                }
            }
            Op::FillRange { from, to, .. } | Op::FillColumns { from, to, .. } => {
                if from <= to {
                    affected_ranges.push((from + 1, to + 1));
                }
            }
            Op::PasteBlock { row, rows, .. } => {
                if !rows.is_empty() {
                    affected_ranges.push((row + 1, row + rows.len() as u64));
                }
            }
            Op::SetWhere { .. } | Op::Substitute { .. } | Op::SubstituteRegex { .. } => {
                rule_operations += 1;
            }
            Op::SplitColumn { .. }
            | Op::AddColumn { .. }
            | Op::AddColumnAt { .. }
            | Op::DropColumn { .. }
            | Op::RenameColumn { .. }
            | Op::SetColumnFormula { .. } => structural_operations += 1,
        }
    }

    let rows_after = rows_after.max(0) as u64;
    let sidecar = crate::workbook::Journal::sidecar(base);
    let journal = std::fs::read(&sidecar).unwrap_or_default();
    let base_bytes = std::fs::metadata(base).map(|m| m.len()).unwrap_or(0);
    Ok(EditPreview {
        operations: ops.len(),
        rows_before: total_rows,
        rows_after,
        row_delta: rows_after as i64 - total_rows as i64,
        affected_ranges,
        rule_operations,
        structural_operations,
        columns: columns.len(),
        base_bytes,
        journal_bytes: journal.len() as u64,
        journal_hash: format!("{:016x}", fnv1a(&journal)),
    })
}
