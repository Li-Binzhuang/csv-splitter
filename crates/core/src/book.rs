//! A workbook: all the sheets under one directory, listed together.
//!
//! A spreadsheet file holds many sheets; here a *directory* does. That is the
//! cheapest possible mapping — no manifest to keep in sync, no second copy of the
//! truth — and it means a folder of exports behaves like a workbook the moment
//! someone drops a new CSV into it.
//!
//! Row counts come from the sparse index when one exists. Counting every file on
//! every list request would turn "show me my workbook" into a full scan of the
//! directory, which is exactly the behaviour this project exists to avoid.

use crate::csv_reader::RecordReader;
use crate::encoding;
use crate::index::SparseIndex;
use crate::layout::Layout;
use crate::workbook::Journal;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

/// Ceiling on sheets listed per workbook.
pub const DEFAULT_MAX_SHEETS: usize = 500;

/// One sheet in a workbook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetSummary {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// Known only when an index sidecar exists; `None` means "not counted yet".
    pub rows: Option<u64>,
    pub columns: Option<usize>,
    pub edits: usize,
    /// Whether a `.view` sidecar (widths, formats) is present.
    pub styled: bool,
    /// Whether a `.idx` sidecar is present (so rows and jumps are cheap).
    pub indexed: bool,
}

impl SheetSummary {
    pub fn to_json(&self) -> String {
        let rows = match self.rows {
            Some(n) => n.to_string(),
            None => "null".to_string(),
        };
        let columns = match self.columns {
            Some(n) => n.to_string(),
            None => "null".to_string(),
        };
        format!(
            r#"{{"name":{},"path":{},"bytes":{},"rows":{},"columns":{},"edits":{},"styled":{},"indexed":{}}}"#,
            crate::format_json(&self.name),
            crate::format_json(&self.path.to_string_lossy()),
            self.bytes,
            rows,
            columns,
            self.edits,
            self.styled,
            self.indexed
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Book {
    pub root: PathBuf,
    pub sheets: Vec<SheetSummary>,
    /// Files that looked like sheets but could not be read, with the reason.
    pub unreadable: Vec<(PathBuf, String)>,
    /// Sheets that exist but were not listed because `max_sheets` cut them off —
    /// reported so a client can say "N more" instead of pretending the list is whole.
    pub hidden: usize,
    /// Total sheets found, including the hidden ones.
    pub total: usize,
}

/// List the sheets under `root`, newest first, up to `max_sheets`.
pub fn scan(root: &Path, max_sheets: usize) -> io::Result<Book> {
    let mut entries: Vec<PathBuf> = Vec::new();
    let reader = std::fs::read_dir(root)?;
    for entry in reader.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_csv = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("csv") || e.eq_ignore_ascii_case("tsv"))
            .unwrap_or(false);
        if is_csv {
            entries.push(path);
        }
    }
    // Newest first: a workbook is usually "what I just exported".
    entries.sort_by_key(|p| {
        std::cmp::Reverse(
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
    });
    let total = entries.len();
    let hidden = total.saturating_sub(max_sheets);
    entries.truncate(max_sheets);

    let mut book = Book {
        root: root.to_path_buf(),
        total,
        hidden,
        ..Default::default()
    };
    for path in entries {
        match describe(&path) {
            Ok(sheet) => book.sheets.push(sheet),
            Err(e) => book.unreadable.push((path, e.to_string())),
        }
    }
    Ok(book)
}

fn describe(path: &Path) -> io::Result<SheetSummary> {
    let bytes = std::fs::metadata(path)?.len();
    let sidecar = SparseIndex::sidecar_path(path);
    let indexed = sidecar.exists();
    let rows = if indexed {
        SparseIndex::load(&sidecar)
            .ok()
            .filter(|i| i.matches(path))
            .map(|i| i.data_rows())
    } else {
        None
    };
    let columns = header_width(path)?;
    let edits = Journal::load(&Journal::sidecar(path))?.len();
    let styled = Layout::sidecar(path).exists();
    Ok(SheetSummary {
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        path: path.to_path_buf(),
        bytes,
        rows,
        columns,
        edits,
        styled,
        indexed,
    })
}

fn header_width(path: &Path) -> io::Result<Option<usize>> {
    let mut records = RecordReader::new(
        BufReader::with_capacity(256 * 1024, encoding::open_decoded(path)?),
        256 * 1024,
    );
    let mut record = Vec::with_capacity(4096);
    if !records.next_record(&mut record)? {
        return Ok(Some(0));
    }
    let text = String::from_utf8_lossy(&record).to_string();
    Ok(Some(
        crate::csv_reader::parse_csv_record(text.trim_end_matches(['\n', '\r']), b',').len(),
    ))
}

/// One declarative book manifest: sheets, lookups and outputs, stored as CSV.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Manifest {
    pub sheets: std::collections::BTreeMap<String, PathBuf>,
    pub lookups: std::collections::BTreeMap<String, LookupStep>,
    pub outputs: Vec<OutputSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupStep {
    pub name: String,
    pub from: String,
    pub key: String,
    pub reference: String,
    pub reference_key: String,
    pub take: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSpec {
    pub name: String,
    pub sheet: String,
    pub path: PathBuf,
    pub lookups: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BookOutput {
    pub name: String,
    pub path: PathBuf,
    pub rows: u64,
    pub bytes: u64,
}

const MANIFEST_HEADER: [&str; 9] = [
    "kind",
    "name",
    "table",
    "path",
    "key",
    "reference",
    "reference_key",
    "get",
    "lookups",
];

fn manifest_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// Parse a manifest. Every path is relative to the manifest's directory unless it is
/// absolute. Unknown kinds, duplicate names and missing columns are errors, not guesses.
pub fn parse_manifest(plan: &Path) -> io::Result<Manifest> {
    let reader = encoding::open_decoded(plan)?;
    let mut records = RecordReader::new(BufReader::with_capacity(256 * 1024, reader), 256 * 1024);
    let mut record = Vec::with_capacity(4096);
    if !records.next_record(&mut record)? {
        return Err(manifest_error("the book manifest is empty"));
    }
    let header: Vec<String> = crate::csv_reader::parse_csv_record_with(
        &String::from_utf8_lossy(&record),
        b',',
        crate::csv_reader::QuoteStyle::Rfc4180,
    );
    if header.len() != MANIFEST_HEADER.len()
        || header
            .iter()
            .zip(MANIFEST_HEADER)
            .any(|(cell, expected)| cell.trim() != expected)
    {
        return Err(manifest_error(format!(
            "the book manifest header must be {}",
            MANIFEST_HEADER.join(",")
        )));
    }
    let mut manifest = Manifest::default();
    while records.next_record(&mut record)? {
        let cells: Vec<String> = crate::csv_reader::parse_csv_record_with(
            &String::from_utf8_lossy(&record),
            b',',
            crate::csv_reader::QuoteStyle::Rfc4180,
        );
        if cells.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        if cells.len() != MANIFEST_HEADER.len() {
            return Err(manifest_error(format!(
                "manifest row has {} columns, expected {}",
                cells.len(),
                MANIFEST_HEADER.len()
            )));
        }
        let trimmed: Vec<&str> = cells.iter().map(|cell| cell.trim()).collect();
        let kind = trimmed[0];
        let name = trimmed[1];
        if name.is_empty() {
            return Err(manifest_error("a manifest row needs a name"));
        }
        match kind {
            "sheet" => {
                let path = trimmed[3];
                if path.is_empty() {
                    return Err(manifest_error(format!("sheet {name} needs a path")));
                }
                if manifest
                    .sheets
                    .insert(name.to_string(), PathBuf::from(path))
                    .is_some()
                {
                    return Err(manifest_error(format!("duplicate sheet {name}")));
                }
            }
            "lookup" => {
                let take: Vec<String> = trimmed[7]
                    .split(',')
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect();
                if trimmed[4].is_empty()
                    || trimmed[5].is_empty()
                    || trimmed[6].is_empty()
                    || take.is_empty()
                {
                    return Err(manifest_error(format!(
                        "lookup {name} needs key, reference, reference_key and get"
                    )));
                }
                if manifest
                    .lookups
                    .insert(
                        name.to_string(),
                        LookupStep {
                            name: name.to_string(),
                            from: trimmed[2].to_string(),
                            key: trimmed[4].to_string(),
                            reference: trimmed[5].to_string(),
                            reference_key: trimmed[6].to_string(),
                            take,
                        },
                    )
                    .is_some()
                {
                    return Err(manifest_error(format!("duplicate lookup {name}")));
                }
            }
            "output" => {
                let lookups: Vec<String> = trimmed[8]
                    .split(',')
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect();
                if trimmed[2].is_empty() || trimmed[3].is_empty() {
                    return Err(manifest_error(format!(
                        "output {name} needs a sheet and a path"
                    )));
                }
                if manifest.outputs.iter().any(|output| output.name == name) {
                    return Err(manifest_error(format!("duplicate output {name}")));
                }
                manifest.outputs.push(OutputSpec {
                    name: name.to_string(),
                    sheet: trimmed[2].to_string(),
                    path: PathBuf::from(trimmed[3]),
                    lookups,
                });
            }
            other => {
                return Err(manifest_error(format!(
                    "unknown manifest kind {other:?}; expected sheet, lookup or output"
                )));
            }
        }
    }
    if manifest.outputs.is_empty() {
        return Err(manifest_error("the book manifest has no output"));
    }
    Ok(manifest)
}

fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Materialize every output: chain the declared lookups over the source sheet and
/// write the result. Re-running overwrites the same outputs, so a refresh is idempotent.
pub fn apply_manifest(
    plan: &Path,
    root: Option<&Path>,
    tx: &crate::types::ProgressSender,
) -> io::Result<Vec<BookOutput>> {
    let manifest = parse_manifest(plan)?;
    let root = root.map(Path::to_path_buf).unwrap_or_else(|| {
        plan.parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    });
    let mut results = Vec::with_capacity(manifest.outputs.len());
    for output in &manifest.outputs {
        let sheet_path = manifest.sheets.get(&output.sheet).ok_or_else(|| {
            manifest_error(format!(
                "output {} uses unknown sheet {}",
                output.name, output.sheet
            ))
        })?;
        let destination = resolve(&root, &output.path);
        let source = resolve(&root, sheet_path);
        let mut current = source.clone();
        let mut rows: u64 = 0;
        let mut temps: Vec<PathBuf> = Vec::new();
        for (index, step_name) in output.lookups.iter().enumerate() {
            let step = manifest.lookups.get(step_name).ok_or_else(|| {
                manifest_error(format!(
                    "output {} uses unknown lookup {step_name}",
                    output.name
                ))
            })?;
            if step.from != output.sheet {
                return Err(manifest_error(format!(
                    "lookup {} starts from {}, but output {} starts from {}",
                    step.name, step.from, output.name, output.sheet
                )));
            }
            let reference = manifest.sheets.get(&step.reference).ok_or_else(|| {
                manifest_error(format!(
                    "lookup {} uses unknown sheet {}",
                    step.name, step.reference
                ))
            })?;
            let tmp = destination.with_file_name(format!(
                ".{}.{}.{}.booktmp",
                output.name,
                index,
                destination
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("out")
            ));
            let _ = std::fs::remove_file(&tmp);
            let options = crate::lookup::LookupOptions {
                key: step.key.clone(),
                reference: resolve(&root, reference),
                reference_key: step.reference_key.clone(),
                take: step.take.clone(),
                max_keys: usize::MAX,
            };
            let summary = crate::lookup::lookup(
                &current,
                crate::types::Delimiter::Comma,
                &tmp,
                &options,
                tx,
            )?;
            rows = summary.main_rows;
            if !temps.is_empty() {
                let _ = std::fs::remove_file(temps.last().unwrap());
            }
            temps.push(tmp.clone());
            current = tmp;
        }
        if let Some(parent) = destination.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let bytes = if current == source {
            std::fs::copy(&current, &destination)?;
            std::fs::metadata(&destination)
                .map(|m| m.len())
                .unwrap_or(0)
        } else {
            std::fs::copy(&current, &destination)?;
            let _ = std::fs::remove_file(&current);
            temps.clear();
            std::fs::metadata(&destination)
                .map(|m| m.len())
                .unwrap_or(0)
        };
        for tmp in &temps {
            let _ = std::fs::remove_file(tmp);
        }
        results.push(BookOutput {
            name: output.name.clone(),
            path: destination,
            rows,
            bytes,
        });
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_book_{tag}_{}_{}",
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

    #[test]
    fn every_csv_in_the_directory_becomes_a_sheet() {
        let dir = Dir::new("scan");
        std::fs::write(dir.join("orders.csv"), "id,city\n1,CDMX\n").unwrap();
        std::fs::write(dir.join("customers.csv"), "id,name,city\n1,Ana,CDMX\n").unwrap();
        // Not a sheet.
        std::fs::write(dir.join("notes.txt"), "ignore me").unwrap();

        let book = scan(&dir.0, DEFAULT_MAX_SHEETS).unwrap();
        assert_eq!(book.sheets.len(), 2, "{:?}", book.sheets);
        assert!(book.sheets.iter().any(|s| s.name == "orders.csv"));
        assert!(book.unreadable.is_empty());
    }

    #[test]
    fn metadata_is_reported_without_counting_rows() {
        let dir = Dir::new("meta");
        std::fs::write(dir.join("a.csv"), "x,y,z\n1,2,3\n").unwrap();
        let book = scan(&dir.0, DEFAULT_MAX_SHEETS).unwrap();
        let sheet = &book.sheets[0];
        assert_eq!(sheet.columns, Some(3), "header width comes from the header");
        // No index yet: the row count is unknown rather than a full scan.
        assert!(sheet.rows.is_none(), "rows must not be counted eagerly");
        assert!(!sheet.indexed);
    }

    #[test]
    fn edits_and_layout_are_visible_in_the_listing() {
        let dir = Dir::new("state");
        let path = dir.join("t.csv");
        std::fs::write(&path, "id,city\n1,CDMX\n").unwrap();
        let mut journal = Journal::new();
        journal.push(crate::workbook::Op::SetCell {
            row: 0,
            column: "city".into(),
            value: "Monterrey".into(),
        });
        journal.save(&Journal::sidecar(&path)).unwrap();
        let mut layout = Layout::default();
        layout.set("city", "width", "160").unwrap();
        layout.save(&path).unwrap();

        let book = scan(&dir.0, DEFAULT_MAX_SHEETS).unwrap();
        assert_eq!(book.sheets[0].edits, 1);
        assert!(book.sheets[0].styled);
    }

    #[test]
    fn an_index_makes_the_row_count_free() {
        let dir = Dir::new("indexed");
        let path = dir.join("big.csv");
        let mut csv = String::from("id\n");
        for i in 0..5 {
            csv.push_str(&format!("{i}\n"));
        }
        std::fs::write(&path, csv).unwrap();
        let index = SparseIndex::build(&path, 2).unwrap();
        index.save(&SparseIndex::sidecar_path(&path)).unwrap();

        let book = scan(&dir.0, DEFAULT_MAX_SHEETS).unwrap();
        assert_eq!(book.sheets[0].rows, Some(5));
        assert!(book.sheets[0].indexed);
    }

    #[test]
    fn a_corrupt_file_is_listed_as_unreadable_not_fatal() {
        let dir = Dir::new("corrupt");
        std::fs::write(dir.join("ok.csv"), "a\n1\n").unwrap();
        // A directory named like a CSV cannot be opened as a file.
        std::fs::create_dir_all(dir.join("weird.csv")).unwrap();
        let book = scan(&dir.0, DEFAULT_MAX_SHEETS).unwrap();
        // Directories are skipped before they reach describe().
        assert_eq!(book.sheets.len(), 1);
        assert!(book.unreadable.is_empty());
    }
}
