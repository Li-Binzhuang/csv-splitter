//! Search across all columns: the one thing every CSV viewer needs and this
//! tool could not do.
//!
//! Filtering answers "which rows have city = CDMX"; searching answers "which
//! rows mention *refund* anywhere". Both return row numbers so the caller can
//! jump there through the sparse index instead of loading the file.
//!
//! Matching is deliberately literal: a case-insensitive substring, optionally
//! narrowed to chosen columns or to whole-cell equality. No regex — a `~`
//! filter already covers the useful half, and a regex engine here would cost a
//! dependency for something the SQL endpoint does better.

use crate::csv_reader::{parse_csv_record_with, QuoteStyle, RecordReader};
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::io::{self, BufReader};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchSpec {
    pub query: String,
    /// Restrict to these column names; empty means every column.
    pub columns: Vec<String>,
    pub case_sensitive: bool,
    /// Match the whole cell instead of a substring.
    pub whole_cell: bool,
    /// Treat `query` as a pattern from the conditional-formatting subset instead of
    /// literal text, so `--find "C1[0-9]"` works the way people expect.
    pub regex: bool,
}

impl SearchSpec {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            columns: Vec::new(),
            case_sensitive: false,
            whole_cell: false,
            regex: false,
        }
    }

    fn matches(&self, cell: &str) -> bool {
        if self.regex {
            let pattern = if self.case_sensitive {
                self.query.clone()
            } else {
                self.query.to_lowercase()
            };
            let cell = if self.case_sensitive {
                cell.to_string()
            } else {
                cell.to_lowercase()
            };
            return crate::format::pattern_matches(&pattern, &cell);
        }
        if self.case_sensitive {
            return if self.whole_cell {
                cell == self.query
            } else {
                cell.contains(&self.query)
            };
        }
        // Case-insensitive is the hot path, and `to_lowercase()` allocates per cell —
        // on an 11 GB sheet that is one allocation per field of every row. ASCII data
        // (almost all of it) is compared byte-wise instead; only a non-ASCII query or
        // cell falls back to the allocating comparison, which keeps Unicode semantics.
        if self.whole_cell {
            if cell.eq_ignore_ascii_case(&self.query) {
                return true;
            }
            return !(cell.is_ascii() && self.query.is_ascii())
                && cell.to_lowercase() == self.query.to_lowercase();
        }
        if cell.is_ascii() && self.query.is_ascii() {
            return ascii_contains_ignore_case(cell.as_bytes(), self.query.as_bytes());
        }
        cell.to_lowercase().contains(&self.query.to_lowercase())
    }
}

/// Case-insensitive substring search over ASCII bytes, without allocating.
fn ascii_contains_ignore_case(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

#[derive(Clone, Debug, Default)]
pub struct SearchResult {
    /// 0-based data rows that matched, in file order.
    pub rows: Vec<u64>,
    pub scanned: u64,
    pub matched: u64,
    /// True when `limit` cut the result short.
    pub truncated: bool,
    /// Column names actually searched (the projection of `spec.columns`).
    pub columns: Vec<String>,
}

impl SearchResult {
    pub fn row_numbers(&self) -> Vec<u64> {
        self.rows.iter().map(|r| r + 1).collect()
    }
}

/// Scan the file and collect up to `limit` matching data rows.
pub fn search(
    path: &Path,
    delimiter: Delimiter,
    spec: &SearchSpec,
    limit: usize,
    tx: &ProgressSender,
) -> io::Result<SearchResult> {
    search_quotes(path, delimiter, spec, limit, QuoteStyle::Rfc4180, tx)
}

/// Same as [`search`], with an explicit quote style.
pub fn search_quotes(
    path: &Path,
    delimiter: Delimiter,
    spec: &SearchSpec,
    limit: usize,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<SearchResult> {
    search_quotes_with_budget(path, delimiter, spec, limit, None, quotes, tx)
}

/// Same search, stopping with `TimedOut` once `budget` elapses.
pub fn search_quotes_with_budget(
    path: &Path,
    delimiter: Delimiter,
    spec: &SearchSpec,
    limit: usize,
    budget: Option<std::time::Duration>,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<SearchResult> {
    let deadline = budget.and_then(|b| std::time::Instant::now().checked_add(b));
    if spec.query.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty search query",
        ));
    }
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseStart {
        phase: format!("searching {}", spec.query),
        total_bytes: size,
    })
    .ok();

    let reader = encoding::open_decoded(path)?;
    let mut records = RecordReader::new(
        BufReader::with_capacity(4 * 1024 * 1024, reader),
        4 * 1024 * 1024,
    );
    let delim = delimiter.byte();

    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
    }
    let headers = parse_csv_record_with(&String::from_utf8_lossy(&record), delim, quotes);
    let indices: Vec<usize> = if spec.columns.is_empty() {
        (0..headers.len()).collect()
    } else {
        let mut found = Vec::new();
        for name in &spec.columns {
            match crate::csv_reader::resolve_column(name, &headers) {
                Ok(i) => found.push(i),
                Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidInput, e)),
            }
        }
        found
    };
    let columns: Vec<String> = indices.iter().map(|&i| headers[i].clone()).collect();

    let mut result = SearchResult {
        columns,
        ..Default::default()
    };
    let limit = limit.max(1);
    while records.next_record(&mut record)? {
        result.scanned += 1;
        if let Some(deadline) = deadline {
            if result.scanned.is_multiple_of(5_000) && std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "the search stopped after {} rows at its time budget",
                        result.scanned
                    ),
                ));
            }
        }
        let text = String::from_utf8_lossy(&record);
        let fields = parse_csv_record_with(text.trim_end_matches(['\n', '\r']), delim, quotes);
        let hit = indices
            .iter()
            .filter_map(|&i| fields.get(i))
            .any(|cell| spec.matches(cell));
        if hit {
            result.matched += 1;
            if result.rows.len() < limit {
                result.rows.push(result.scanned - 1);
            } else {
                result.truncated = true;
            }
        }
        if result.scanned.is_multiple_of(100_000) {
            tx.send(ProgressEvent::Progress {
                bytes: result.scanned,
            })
            .ok();
        }
    }

    tx.send(ProgressEvent::PhaseDone {
        phase: format!("searching {}", spec.query),
    })
    .ok();
    Ok(result)
}

/// Scan with one thread per checkpoint range when the file is big enough to pay for it.
///
/// Same rows as `search_quotes_with_budget`; falls back to it for encodings whose byte
/// offsets are not row boundaries, or when the split would cost more than it saves.
pub fn search_fast(
    path: &Path,
    delimiter: Delimiter,
    spec: &SearchSpec,
    limit: usize,
    budget: Option<std::time::Duration>,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<SearchResult> {
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let threads = crate::scan_parallel::threads_for(file_len);
    let seekable = crate::scan_parallel::is_seekable(path)?;
    if threads <= 1 || !seekable || spec.query.is_empty() {
        return search_quotes_with_budget(path, delimiter, spec, limit, budget, quotes, tx);
    }
    let phase = format!("searching {}", spec.query);
    let options = crate::scan_parallel::ScanOptions {
        delimiter,
        limit,
        budget,
        threads,
        quotes,
        phase: &phase,
        start_checkpoint: 0,
        checkpoints_per_wave: crate::scan_parallel::CHECKPOINTS_PER_WAVE,
    };
    match search_parallel(path, spec, &options, tx) {
        Ok((result, _)) => Ok(result),
        Err(e) if e.kind() == io::ErrorKind::TimedOut => Err(e),
        Err(_) => search_quotes_with_budget(path, delimiter, spec, limit, budget, quotes, tx),
    }
}

/// A resumable page: rows found so far plus the cursor for the next call.
///
/// Same contract as [`crate::filter::scan_page`]: only the UTF-8 parallel path can
/// resume, because a cursor is a byte offset in the sparse index.
// See filter::scan_page: the scalars are one request; grouping them here would just
// relocate the list for the two call sites that use it.
#[allow(clippy::too_many_arguments)]
pub fn search_page(
    path: &Path,
    delimiter: Delimiter,
    spec: &SearchSpec,
    limit: usize,
    budget: Option<std::time::Duration>,
    quotes: QuoteStyle,
    start_checkpoint: usize,
    tx: &ProgressSender,
) -> io::Result<(SearchResult, Option<usize>)> {
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let threads = crate::scan_parallel::threads_for(file_len);
    if threads <= 1 || !crate::scan_parallel::is_seekable(path)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resumable pages need a UTF-8 file and the parallel scan",
        ));
    }
    let phase = format!("searching {}", spec.query);
    let options = crate::scan_parallel::ScanOptions {
        delimiter,
        limit,
        budget,
        threads,
        quotes,
        phase: &phase,
        start_checkpoint,
        checkpoints_per_wave: crate::scan_parallel::CHECKPOINTS_PER_WAVE,
    };
    search_parallel(path, spec, &options, tx)
}

/// The parallel half of [`search_fast`], with threads made explicit so a test can force
/// four of them on a file small enough to check by eye.
fn search_parallel(
    path: &Path,
    spec: &SearchSpec,
    options: &crate::scan_parallel::ScanOptions<'_>,
    tx: &ProgressSender,
) -> io::Result<(SearchResult, Option<usize>)> {
    let delim = options.delimiter.byte();
    let reader = encoding::open_decoded(path)?;
    let mut records = RecordReader::new(BufReader::with_capacity(1024 * 1024, reader), 1024 * 1024);
    let mut record = Vec::new();
    if !records.next_record(&mut record)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
    }
    let headers = parse_csv_record_with(&String::from_utf8_lossy(&record), delim, options.quotes);
    let indices: Vec<usize> = if spec.columns.is_empty() {
        (0..headers.len()).collect()
    } else {
        let mut found = Vec::new();
        for name in &spec.columns {
            match crate::csv_reader::resolve_column(name, &headers) {
                Ok(i) => found.push(i),
                Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidInput, e)),
            }
        }
        found
    };
    let columns: Vec<String> = indices.iter().map(|&i| headers[i].clone()).collect();
    let predicate = |fields: &[String]| {
        indices
            .iter()
            .filter_map(|&i| fields.get(i))
            .any(|cell| spec.matches(cell))
    };
    let batch = crate::scan_parallel::scan_parallel(path, options, tx, predicate)?;
    Ok((
        SearchResult {
            rows: batch.rows,
            scanned: batch.scanned,
            matched: batch.matched,
            truncated: batch.truncated,
            columns,
        },
        batch.next_checkpoint,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(std::path::PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "csvs_search_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn join(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn fixture() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new("basic");
        let p = dir.join("s.csv");
        let mut csv = String::from("id,city,note\n");
        csv.push_str("1,CDMX,refund issued\n");
        csv.push_str("2,Puebla,ok\n");
        csv.push_str("3,CDMX,\"line one\nrefund again\"\n");
        std::fs::write(&p, csv).unwrap();
        (dir, p)
    }

    fn run(spec: SearchSpec, limit: usize) -> SearchResult {
        let (_dir, p) = fixture();
        let (tx, _rx) = std::sync::mpsc::channel();
        search(&p, Delimiter::Comma, &spec, limit, &tx).unwrap()
    }

    #[test]
    fn finds_a_substring_across_all_columns() {
        let r = run(SearchSpec::new("refund"), 100);
        assert_eq!(r.matched, 2);
        // Row 3 has the match inside a quoted newline field: it must count once,
        // and the row number must not drift.
        assert_eq!(r.row_numbers(), vec![1, 3]);
        assert_eq!(r.columns, vec!["id", "city", "note"]);
    }

    #[test]
    fn is_case_insensitive_by_default_and_exact_when_asked() {
        let r = run(SearchSpec::new("REFUND"), 100);
        assert_eq!(r.matched, 2);
        let mut exact = SearchSpec::new("REFUND");
        exact.case_sensitive = true;
        let r = run(exact, 100);
        assert_eq!(r.matched, 0);
    }

    #[test]
    fn narrows_to_chosen_columns() {
        let mut spec = SearchSpec::new("CDMX");
        spec.columns = vec!["city".into()];
        let r = run(spec, 100);
        assert_eq!(r.row_numbers(), vec![1, 3]);
        assert_eq!(r.columns, vec!["city"]);

        let mut unknown = SearchSpec::new("x");
        unknown.columns = vec!["nope".into()];
        let (_dir, p) = fixture();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = search(&p, Delimiter::Comma, &unknown, 10, &tx).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn whole_cell_matching_does_not_match_prefixes() {
        let mut spec = SearchSpec::new("CDMX");
        spec.whole_cell = true;
        assert_eq!(run(spec, 100).matched, 2);

        let mut spec = SearchSpec::new("CDM");
        spec.whole_cell = true;
        assert_eq!(run(spec, 100).matched, 0);
    }

    #[test]
    fn respects_the_limit_and_reports_truncation() {
        let r = run(SearchSpec::new("CDMX"), 1);
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.matched, 2);
        assert!(r.truncated);
    }

    #[test]
    fn empty_query_is_rejected() {
        let (_dir, p) = fixture();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = search(&p, Delimiter::Comma, &SearchSpec::new(""), 10, &tx).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// Same contract as filter's parallel test: the rows, counts and the searched column
    /// set must match the sequential scan, with a quoted newline crossing a checkpoint.
    #[test]
    fn parallel_search_agrees_with_sequential() {
        let dir = TempDir::new("parallel");
        let path = dir.join("t.csv");
        let mut text = String::from("city,note\n");
        for i in 0..12_000u64 {
            if i % 4000 == 3999 {
                text.push_str("CDMX,\"refund line one\nrefund line two\"\n");
            } else if i % 7 == 0 {
                text.push_str(&format!("CDMX,refund {i}\n"));
            } else {
                text.push_str("Puebla,note\n");
            }
        }
        std::fs::write(&path, text).unwrap();
        let spec = SearchSpec::new("refund");
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        let sequential = search_quotes(
            &path,
            Delimiter::Comma,
            &spec,
            100_000,
            QuoteStyle::Rfc4180,
            &tx,
        )
        .unwrap();
        let phase = String::from("searching refund");
        let options = crate::scan_parallel::ScanOptions {
            delimiter: Delimiter::Comma,
            limit: 100_000,
            budget: None,
            threads: 4,
            quotes: QuoteStyle::Rfc4180,
            phase: &phase,
            start_checkpoint: 0,
            checkpoints_per_wave: crate::scan_parallel::CHECKPOINTS_PER_WAVE,
        };
        let parallel = search_parallel(&path, &spec, &options, &tx).unwrap().0;
        assert_eq!(sequential.rows, parallel.rows);
        assert_eq!(sequential.matched, parallel.matched);
        assert_eq!(sequential.scanned, parallel.scanned);
        assert_eq!(sequential.columns, parallel.columns);
    }

    /// ASCII goes through the byte-wise fast path; non-ASCII must keep the allocating
    /// lowercase comparison, or "café" stops matching "CAFÉ".
    #[test]
    fn non_ascii_is_still_case_insensitive() {
        let spec = SearchSpec::new("café");
        assert!(spec.matches("CAFÉ"));
        assert!(!spec.matches("tea"));
        let spec = SearchSpec::new("CAFÉ");
        assert!(spec.matches("café"));
    }
}
