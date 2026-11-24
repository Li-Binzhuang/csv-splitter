//! Typed requests/responses for the operations the adapters share.
//!
//! Stage 2 starts with the three front-end reads: schema, view and filter. The
//! adapters still own their command-line/URL spelling; these types own the meaning,
//! so a future CLI/HTTP/MCP migration cannot drift on `limit`, row numbering or
//! delimiter semantics.

use crate::filter::{self, FilterSpec};
use crate::sheet::{ColumnInfo, ReadOptions, Sheet};
use crate::types::{Delimiter, ProgressEvent};
use crate::workbook::Window;
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

/// Scan a sheet for rows matching one predicate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterRequest {
    pub path: PathBuf,
    pub delimiter: Delimiter,
    pub expression: String,
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
    pub fn execute(&self) -> io::Result<FilterResponse> {
        let spec = FilterSpec::parse(&self.expression)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
        drop(rx);
        let result = filter::scan_with_budget(
            &self.path,
            self.delimiter,
            &spec,
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
    fn filter_returns_one_based_rows() {
        let (dir, path) = fixture("filter_returns_one_based_rows");
        let response = FilterRequest {
            path: path.clone(),
            delimiter: Delimiter::Comma,
            expression: "a>1".to_string(),
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
    fn filter_rejects_a_bad_expression_without_scanning() {
        let (dir, path) = fixture("filter_rejects_a_bad_expression_without_scanning");
        let error = FilterRequest {
            path: path.clone(),
            delimiter: Delimiter::Comma,
            expression: "".to_string(),
            limit: 10,
            budget: None,
        }
        .execute()
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        std::fs::remove_dir_all(dir).ok();
    }
}
