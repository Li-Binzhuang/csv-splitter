//! One-thread-per-checkpoint scanning, shared by `filter` and `search`.
//!
//! Both commands walk every record and collect row numbers; the only difference is the
//! predicate. The row numbering, the range splitting and the merge live here so the two
//! cannot drift apart — `filter`'s tests compare this path against the sequential scan,
//! and search reuses the same machinery.
//!
//! Scans are resumable: work is done in waves of a few dozen checkpoints, the budget is
//! checked between waves, and a wave that did not run is reported as `next_checkpoint`.
//! That is what lets a caller turn "30 seconds and a 504" into "a page and a cursor".
//!
//! Two deliberate shortcuts:
//!   * only UTF-8 is seeked — GBK/UTF-16 decode on the fly, so a byte offset is not a row
//!     boundary and those files keep the sequential path;
//!   * the sparse index supplies both the boundaries and the row numbers, and is reused
//!     (built once) when missing, because `preview` would build it anyway.

use crate::csv_reader::{parse_csv_record_with, QuoteStyle, RecordReader};
use crate::encoding;
use crate::index::SparseIndex;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::fs::File;
use std::io::{self, BufReader, Seek, SeekFrom};
use std::path::Path;

/// Checkpoints per worker per wave. 64 keeps a wave to a few tens of milliseconds, so a
/// budget lands near the timeout instead of a whole-file slab overshooting it.
pub const CHECKPOINTS_PER_WAVE: usize = 64;

/// Everything except the predicate and the file, kept in one struct so the signature
/// stays under clippy's argument limit and both callers pass the same set.
pub struct ScanOptions<'a> {
    pub delimiter: Delimiter,
    pub limit: usize,
    pub budget: Option<std::time::Duration>,
    pub threads: usize,
    pub quotes: QuoteStyle,
    pub phase: &'a str,
    /// Checkpoint to start from (`0` for a fresh scan). The HTTP cursor.
    pub start_checkpoint: usize,
    pub checkpoints_per_wave: usize,
}

impl Default for ScanOptions<'_> {
    fn default() -> Self {
        Self {
            delimiter: Delimiter::Comma,
            limit: 10_000,
            budget: None,
            threads: 1,
            quotes: QuoteStyle::Rfc4180,
            phase: "",
            start_checkpoint: 0,
            checkpoints_per_wave: CHECKPOINTS_PER_WAVE,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct BatchResult {
    /// 0-based data rows (header excluded) that matched, sorted by row number.
    pub rows: Vec<u64>,
    pub scanned: u64,
    pub matched: u64,
    pub truncated: bool,
    /// Where a resumable scan should continue, when the budget stopped it early.
    pub next_checkpoint: Option<usize>,
}

/// Threads worth starting for a file this size: one below 64 MB, otherwise one per
/// core, capped at 8. A cap keeps memory and IO flat when the caller is a laptop.
pub fn threads_for(file_len: u64) -> usize {
    // An explicit override makes the sequential path reachable from the same binary,
    // which is how the README's before/after numbers were taken, and lets a constrained
    // machine stop the scan from using every core.
    if let Ok(raw) = std::env::var("CSVX_SCAN_THREADS") {
        if let Ok(n) = raw.trim().parse::<usize>() {
            if n >= 1 {
                return n;
            }
        }
    }
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    if file_len < 64 * 1024 * 1024 || cores <= 1 {
        return 1;
    }
    cores.min(8)
}

/// Whether byte offsets in this file are row boundaries.
pub fn is_seekable(path: &Path) -> io::Result<bool> {
    Ok(matches!(
        encoding::detect(path)?,
        encoding::TextEncoding::Utf8 { .. }
    ))
}

/// One contiguous slab of data rows, taken from a checkpoint of the sparse index.
#[derive(Clone, Copy, Debug)]
struct Plan {
    /// Byte offset of the first record, 0 meaning "start at the header".
    offset: u64,
    skip_header: bool,
    /// 0-based data row of the first record this plan reads.
    start_row: u64,
    rows: u64,
}

/// Build the plans for one wave starting at checkpoint `start`.
///
/// Checkpoint `i` is record `i * interval()`; record 0 is the header, so a plan that starts
/// at checkpoint `a` starts at data row `a * interval() - 1`. A wave ending after the last
/// checkpoint runs to EOF, which covers the tail the checkpoints do not reach.
fn wave_plans(
    index: &SparseIndex,
    threads: usize,
    checkpoints_per_wave: usize,
    start: usize,
) -> (Vec<Plan>, Option<usize>) {
    let data_rows = index.data_rows();
    let interval = index.interval();
    let total_cp = index.checkpoints().len();
    if data_rows == 0 || total_cp == 0 || start >= total_cp {
        return (Vec::new(), None);
    }
    let per_wave = checkpoints_per_wave.max(1);
    let wave_end = (start + threads.max(1) * per_wave).min(total_cp);
    let t = (wave_end - start).min(threads.max(1));
    let mut bounds: Vec<usize> = (0..t).map(|j| start + j * (wave_end - start) / t).collect();
    bounds.push(wave_end);
    let mut plans = Vec::new();
    for pair in bounds.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a == b {
            continue;
        }
        let (offset, skip_header, start_row) = if a == 0 {
            (0, true, 0)
        } else {
            (index.checkpoints()[a], false, a as u64 * interval - 1)
        };
        let rows = if b == total_cp {
            data_rows.saturating_sub(start_row)
        } else if a == 0 {
            b as u64 * interval - 1
        } else {
            (b - a) as u64 * interval
        };
        if rows > 0 {
            plans.push(Plan {
                offset,
                skip_header,
                start_row,
                rows,
            });
        }
    }
    let next = if wave_end < total_cp {
        Some(wave_end)
    } else {
        None
    };
    (plans, next)
}

fn scan_plan<P>(
    path: &Path,
    delim: u8,
    quotes: QuoteStyle,
    predicate: &P,
    plan: Plan,
    limit: usize,
) -> io::Result<BatchResult>
where
    P: Fn(&[String]) -> bool + Sync,
{
    let mut file = File::open(path)?;
    if plan.offset > 0 {
        file.seek(SeekFrom::Start(plan.offset))?;
    }
    let mut records = RecordReader::new(
        BufReader::with_capacity(4 * 1024 * 1024, file),
        4 * 1024 * 1024,
    );
    let mut record = Vec::with_capacity(64 * 1024);
    if plan.skip_header {
        records.next_record(&mut record)?;
    }
    let mut result = BatchResult::default();
    for i in 0..plan.rows {
        if !records.next_record(&mut record)? {
            break;
        }
        result.scanned += 1;
        let text = String::from_utf8_lossy(&record);
        let fields = parse_csv_record_with(text.trim_end_matches(['\n', '\r']), delim, quotes);
        if predicate(&fields) {
            result.matched += 1;
            if result.rows.len() < limit {
                result.rows.push(plan.start_row + i);
            } else {
                result.truncated = true;
            }
        }
    }
    Ok(result)
}

/// Scan the file with one thread per checkpoint range, wave by wave.
///
/// `predicate` receives the parsed fields of each record. The caller has already read the
/// header (to resolve columns), so this path never parses it again. When a budget is set
/// and a wave does not get to start before it expires, `next_checkpoint` says where to
/// resume; callers that do not care about resuming can ignore it.
pub fn scan_parallel<P>(
    path: &Path,
    options: &ScanOptions<'_>,
    tx: &ProgressSender,
    predicate: P,
) -> io::Result<BatchResult>
where
    P: Fn(&[String]) -> bool + Sync,
{
    let delim = options.delimiter.byte();
    let index = match SparseIndex::load(&SparseIndex::sidecar_path(path)) {
        Ok(idx) if idx.matches(path) => idx,
        _ => SparseIndex::build(path, crate::index::DEFAULT_INTERVAL)?,
    };
    let limit = options.limit.max(1);
    let deadline = options
        .budget
        .and_then(|b| std::time::Instant::now().checked_add(b));
    tx.send(ProgressEvent::PhaseStart {
        phase: options.phase.to_string(),
        total_bytes: index.file_len(),
    })
    .ok();
    let mut merged = BatchResult::default();
    let mut start = options.start_checkpoint;
    loop {
        let (plans, next) =
            wave_plans(&index, options.threads, options.checkpoints_per_wave, start);
        if plans.is_empty() {
            merged.next_checkpoint = next;
            break;
        }
        let results: Vec<io::Result<BatchResult>> = std::thread::scope(|scope| {
            let predicate = &predicate;
            let handles: Vec<_> = plans
                .iter()
                .map(|plan| {
                    scope.spawn(move || {
                        scan_plan(path, delim, options.quotes, predicate, *plan, limit)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(io::Error::other("a scan worker panicked")))
                })
                .collect()
        });
        for result in results {
            let result = result?;
            merged.scanned += result.scanned;
            merged.matched += result.matched;
            merged.truncated |= result.truncated;
            // Cap the kept rows at every wave, not only at the end: a 250 M-row scan
            // that matches a quarter of the file would otherwise accumulate tens of
            // millions of row numbers before the final truncate (hundreds of MB), even
            // though only `limit` are ever reported.
            let room = limit.saturating_sub(merged.rows.len());
            if result.rows.len() > room {
                merged.truncated = true;
            }
            merged.rows.extend(result.rows.into_iter().take(room));
        }
        match next {
            Some(offset) => {
                if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                    // Report the next wave as the cursor; this one is complete, so no row
                    // is returned twice.
                    merged.next_checkpoint = Some(offset);
                    break;
                }
                start = offset;
            }
            None => break,
        }
    }
    merged.rows.sort_unstable();
    if merged.rows.len() > limit {
        merged.rows.truncate(limit);
        merged.truncated = true;
    }
    if merged.matched > limit as u64 {
        merged.truncated = true;
    }
    tx.send(ProgressEvent::PhaseDone {
        phase: options.phase.to_string(),
    })
    .ok();
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page that runs out of budget returns a cursor instead of throwing work away;
    /// resuming at that cursor must yield exactly the rows a full scan yields.
    #[test]
    fn a_resumed_scan_returns_the_same_rows_as_one_pass() {
        let path =
            std::env::temp_dir().join(format!("csvx_scan_resume_{}.csv", std::process::id()));
        let mut text = String::from("city,amount\n");
        for i in 0..3_000u64 {
            text.push_str(&format!("city{i}, {i}\n"));
        }
        std::fs::write(&path, text).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        let mut options = ScanOptions {
            delimiter: Delimiter::Comma,
            limit: 10_000,
            budget: Some(std::time::Duration::ZERO),
            threads: 2,
            quotes: QuoteStyle::Rfc4180,
            phase: "test",
            start_checkpoint: 0,
            checkpoints_per_wave: 1,
        };
        let predicate = |_fields: &[String]| true;
        let mut rows = Vec::new();
        let mut pages = 0;
        loop {
            let batch = scan_parallel(&path, &options, &tx, predicate).unwrap();
            rows.extend(batch.rows);
            pages += 1;
            match batch.next_checkpoint {
                Some(next) => options.start_checkpoint = next,
                None => break,
            }
            assert!(pages < 50, "the cursor did not advance");
        }
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(SparseIndex::sidecar_path(&path)).ok();
        let expected: Vec<u64> = (0..3_000).collect();
        assert_eq!(rows, expected);
        assert!(
            pages > 1,
            "a zero budget should have produced several pages"
        );
    }
}
