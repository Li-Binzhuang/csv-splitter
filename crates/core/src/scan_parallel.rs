//! One-thread-per-checkpoint scanning, shared by `filter` and `search`.
//!
//! Both commands walk every record and collect row numbers; the only difference is the
//! predicate. The row numbering, the range splitting and the merge live here so the two
//! cannot drift apart — `filter`'s tests compare this path against the sequential scan,
//! and search reuses the same machinery.
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

/// Everything except the predicate and the file, kept in one struct so the signature
/// stays under clippy's argument limit and both callers pass the same set.
pub struct ScanOptions<'a> {
    pub delimiter: Delimiter,
    pub limit: usize,
    pub budget: Option<std::time::Duration>,
    pub threads: usize,
    pub quotes: QuoteStyle,
    pub phase: &'a str,
}

#[derive(Clone, Debug, Default)]
pub struct BatchResult {
    /// 0-based data rows (header excluded) that matched, sorted by row number.
    pub rows: Vec<u64>,
    pub scanned: u64,
    pub matched: u64,
    pub truncated: bool,
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

/// Split the index checkpoints into one plan per thread.
///
/// Checkpoint `i` is record `i * interval()`; record 0 is the header, so a plan that starts
/// at checkpoint `a` starts at data row `a * interval() - 1`. The last plan runs to EOF,
/// which covers the tail the checkpoints do not reach.
fn chunk_plans(index: &SparseIndex, threads: usize) -> Vec<Plan> {
    let data_rows = index.data_rows();
    let interval = index.interval();
    let total_cp = index.checkpoints().len();
    if data_rows == 0 || total_cp == 0 {
        return Vec::new();
    }
    let data_checkpoints = total_cp - 1;
    let t = threads.min(data_checkpoints).max(1);
    let mut splits: Vec<usize> = (1..t).map(|j| 1 + j * data_checkpoints / t).collect();
    splits.retain(|c| (1..total_cp).contains(c));
    splits.dedup();
    let mut bounds = vec![0usize];
    bounds.extend(splits);
    bounds.push(usize::MAX);
    let mut plans = Vec::new();
    for pair in bounds.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (offset, skip_header, start_row) = if a == 0 {
            (0, true, 0)
        } else {
            (index.checkpoints()[a], false, a as u64 * interval - 1)
        };
        let rows = if b == usize::MAX {
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
    plans
}

fn scan_plan<P>(
    path: &Path,
    delim: u8,
    quotes: QuoteStyle,
    predicate: &P,
    plan: Plan,
    limit: usize,
    deadline: Option<std::time::Instant>,
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
        if let Some(deadline) = deadline {
            if result.scanned.is_multiple_of(5_000) && std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the scan stopped at its time budget",
                ));
            }
        }
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

/// Scan the file with one thread per checkpoint range.
///
/// `predicate` receives the parsed fields of each record. The caller has already read the
/// header (to resolve columns), so this path never parses it again.
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
    let plans = chunk_plans(&index, options.threads);
    if plans.is_empty() {
        return Ok(BatchResult::default());
    }
    let deadline = options
        .budget
        .and_then(|b| std::time::Instant::now().checked_add(b));
    let limit = options.limit.max(1);
    tx.send(ProgressEvent::PhaseStart {
        phase: options.phase.to_string(),
        total_bytes: index.file_len(),
    })
    .ok();
    let results: Vec<io::Result<BatchResult>> = std::thread::scope(|scope| {
        let predicate = &predicate;
        let handles: Vec<_> = plans
            .iter()
            .map(|plan| {
                scope.spawn(move || {
                    scan_plan(
                        path,
                        delim,
                        options.quotes,
                        predicate,
                        *plan,
                        limit,
                        deadline,
                    )
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
    let mut merged = BatchResult::default();
    for result in results {
        let result = result?;
        merged.scanned += result.scanned;
        merged.matched += result.matched;
        merged.truncated |= result.truncated;
        merged.rows.extend(result.rows);
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
