//! Column filters over a whole file.
//!
//! The viewer needs "which rows match `city = CDMX`" without loading the file:
//! this scans sequentially (decoding handles GBK/UTF-16), returns **data-row
//! numbers**, and the caller pages through the hits with the sparse index.
//!
//! Matching is deliberately simple — equality, contains, numeric comparison and
//! empty checks. Anything more elaborate belongs in SQL, not in a file tool.

use crate::csv_reader::{parse_csv_record_with, QuoteStyle, RecordReader};
use crate::encoding;
use crate::index::SparseIndex;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::cmp::Ordering;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Contains,
    Gt,
    Gte,
    Lt,
    Lte,
    Empty,
    NotEmpty,
    /// Regular expression match (`col~re:PATTERN`).
    Regex,
    /// Spreadsheet wildcards (`*` any run, `?` one character), case-insensitive.
    /// Only the formula layer builds these, from a criteria literal.
    Wildcard,
    /// Membership: `col in a,b,c` (values are trimmed; no escaping).
    In,
}

impl Op {
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "!=",
            Self::Contains => "~",
            Self::Gt => ">",
            Self::Gte => ">=",
            Self::Lt => "<",
            Self::Lte => "<=",
            Self::Empty => "empty",
            Self::NotEmpty => "notempty",
            Self::Regex => "~re:",
            Self::Wildcard => "~wc:",
            Self::In => "in",
        }
    }
}

/// `city=CDMX`, `amount>1000`, `note~refund`, `email empty`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterSpec {
    pub column: String,
    pub op: Op,
    pub value: String,
}

impl FilterSpec {
    pub fn parse(expr: &str) -> Result<Self, String> {
        let expr = expr.trim();
        if expr.is_empty() {
            return Err("empty filter expression".to_string());
        }
        // `col in a,b,c` has no operator symbol, so match it first.
        if let Some(pos) = expr.to_ascii_lowercase().find(" in ") {
            let column = expr[..pos].trim().to_string();
            if column.is_empty() {
                return Err(format!("missing column name in '{expr}'"));
            }
            let value = expr[pos + 4..].trim().to_string();
            if value.is_empty() {
                return Err(format!("'in' needs at least one value: {expr}"));
            }
            return Ok(Self {
                column,
                op: Op::In,
                value,
            });
        }

        // Longest operators first so `>=` beats `>`.
        const OPS: &[(&str, Op)] = &[
            ("!=", Op::Ne),
            (">=", Op::Gte),
            ("<=", Op::Lte),
            ("=", Op::Eq),
            ("~", Op::Contains),
            (">", Op::Gt),
            ("<", Op::Lt),
        ];
        for (token, op) in OPS {
            if let Some((col, val)) = expr.split_once(token) {
                let column = col.trim().to_string();
                if column.is_empty() {
                    return Err(format!("missing column name in '{expr}'"));
                }
                let value = val.trim().to_string();
                // `col~re:PATTERN` is a regex match; validated here so a typo
                // fails immediately instead of on the first row.
                let op = if *op == Op::Contains {
                    match value.strip_prefix("re:") {
                        Some(pattern) => {
                            regex::Regex::new(pattern)
                                .map_err(|e| format!("bad regular expression: {e}"))?;
                            return Ok(Self {
                                column,
                                op: Op::Regex,
                                value: pattern.to_string(),
                            });
                        }
                        None => Op::Contains,
                    }
                } else {
                    *op
                };
                return Ok(Self { column, op, value });
            }
        }
        // `col empty` / `col notempty`
        let mut parts = expr.splitn(2, char::is_whitespace);
        let column = parts.next().unwrap_or("").trim().to_string();
        let word = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        match word.as_str() {
            "empty" => Ok(Self {
                column,
                op: Op::Empty,
                value: String::new(),
            }),
            "notempty" | "not-empty" => Ok(Self {
                column,
                op: Op::NotEmpty,
                value: String::new(),
            }),
            _ => Err(format!(
                "cannot parse '{expr}'; use col=value, col~text, col>number, col empty"
            )),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    /// 0-based data rows (header excluded) that matched, in file order.
    pub rows: Vec<u64>,
    pub scanned: u64,
    pub matched: u64,
    /// True when `limit` cut the result short.
    pub truncated: bool,
}

impl ScanResult {
    /// Row numbers as a user would count them (1-based, header excluded).
    pub fn row_numbers(&self) -> Vec<u64> {
        self.rows.iter().map(|r| r + 1).collect()
    }
}

/// Scan `path` and collect up to `limit` matching data rows.
pub fn scan(
    path: &Path,
    delimiter: Delimiter,
    spec: &FilterSpec,
    limit: usize,
    tx: &ProgressSender,
) -> io::Result<ScanResult> {
    scan_with_budget(path, delimiter, spec, limit, None, tx)
}

/// Same scan, stopping with `TimedOut` once `budget` elapses.
///
/// A synchronous HTTP request has no cancel button: the client can walk away, but the
/// thread keeps reading. A budget turns "busy forever" into a 504 that says to narrow the
/// filter or page instead.
/// Does one record satisfy a single-column predicate? `None` means the full parser
/// is required (quotes, backslash quoting or non-UTF-8). A short row reads as "".
fn record_cell_matches(
    record: &[u8],
    delim: u8,
    quotes: QuoteStyle,
    index: usize,
    spec: &FilterSpec,
) -> Option<bool> {
    let mut seen = false;
    let mut hit = false;
    let mut invalid = false;
    let complete = crate::csv_reader::visit_fields(record, delim, quotes, |at, bytes| {
        if at != index || invalid {
            return;
        }
        seen = true;
        let Ok(cell) = std::str::from_utf8(bytes) else {
            invalid = true;
            return;
        };
        hit = matches(cell, spec);
    });
    if !complete || invalid {
        return None;
    }
    Some(if seen { hit } else { matches("", spec) })
}

pub fn scan_with_budget(
    path: &Path,
    delimiter: Delimiter,
    spec: &FilterSpec,
    limit: usize,
    budget: Option<std::time::Duration>,
    tx: &ProgressSender,
) -> io::Result<ScanResult> {
    let deadline = budget.and_then(|b| std::time::Instant::now().checked_add(b));
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
    let headers = parse_csv_record_with(
        &String::from_utf8_lossy(&record),
        delim,
        QuoteStyle::Rfc4180,
    );
    let col_idx = crate::csv_reader::resolve_column(&spec.column, &headers)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    tx.send(ProgressEvent::PhaseStart {
        phase: format!("Filtering {}", spec.column),
        total_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    })
    .ok();

    let mut result = ScanResult::default();
    let limit = limit.max(1);
    while records.next_record(&mut record)? {
        result.scanned += 1;
        if let Some(deadline) = deadline {
            if result.scanned % 5_000 == 0 && std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "the scan stopped after {} rows at its time budget",
                        result.scanned
                    ),
                ));
            }
        }
        let hit = record_cell_matches(&record, delim, QuoteStyle::Rfc4180, col_idx, spec)
            .unwrap_or_else(|| {
                let text = String::from_utf8_lossy(&record);
                let fields = parse_csv_record_with(
                    text.trim_end_matches(['\n', '\r']),
                    delim,
                    QuoteStyle::Rfc4180,
                );
                matches(fields.get(col_idx).map(String::as_str).unwrap_or(""), spec)
            });
        if hit {
            result.matched += 1;
            if result.rows.len() < limit {
                result.rows.push(result.scanned - 1);
            } else {
                result.truncated = true;
            }
        }
        if result.scanned % 200_000 == 0 {
            tx.send(ProgressEvent::Progress {
                bytes: result.scanned,
            })
            .ok();
        }
    }

    tx.send(ProgressEvent::PhaseDone {
        phase: format!("Filtering {}", spec.column),
    })
    .ok();
    Ok(result)
}

/// Resolve the predicate's column against the header, once, before splitting the file.
fn column_index(path: &Path, delim: u8, spec: &FilterSpec) -> io::Result<usize> {
    let reader = encoding::open_decoded(path)?;
    let mut records = RecordReader::new(BufReader::with_capacity(1024 * 1024, reader), 1024 * 1024);
    let mut record = Vec::new();
    if !records.next_record(&mut record)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
    }
    let headers = parse_csv_record_with(
        &String::from_utf8_lossy(&record),
        delim,
        QuoteStyle::Rfc4180,
    );
    crate::csv_reader::resolve_column(&spec.column, &headers)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

/// Scan with one thread per checkpoint range when the file is big enough to pay for it.
///
/// The rows come back exactly as the sequential scan returns them; the difference is wall
/// time. Encodings whose byte offsets are not row boundaries (GBK/UTF-16) keep the
/// sequential path, and a timeout is a real answer the HTTP layer turns into 504, so it is
/// not retried.
pub fn scan_fast(
    path: &Path,
    delimiter: Delimiter,
    spec: &FilterSpec,
    limit: usize,
    budget: Option<std::time::Duration>,
    tx: &ProgressSender,
) -> io::Result<ScanResult> {
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let threads = crate::scan_parallel::threads_for(file_len);
    let seekable = crate::scan_parallel::is_seekable(path)?;
    if threads <= 1 || !seekable {
        return scan_with_budget(path, delimiter, spec, limit, budget, tx);
    }
    match filter_parallel(path, delimiter, spec, limit, budget, threads, 0, tx) {
        Ok((result, _)) => Ok(result),
        Err(e) if e.kind() == io::ErrorKind::TimedOut => Err(e),
        Err(_) => scan_with_budget(path, delimiter, spec, limit, budget, tx),
    }
}

/// A resumable page: the rows found so far, plus the cursor for the next call.
///
/// This is what the HTTP layer calls. A page that ran out of budget comes back with
/// `next` set instead of an error or a silently truncated answer; passing that number
/// back as `start_checkpoint` continues exactly where the last wave stopped.
pub fn scan_page(
    path: &Path,
    delimiter: Delimiter,
    spec: &FilterSpec,
    limit: usize,
    budget: Option<std::time::Duration>,
    start_checkpoint: usize,
    tx: &ProgressSender,
) -> io::Result<(ScanResult, Option<usize>)> {
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let threads = crate::scan_parallel::threads_for(file_len);
    if threads <= 1 || !crate::scan_parallel::is_seekable(path)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resumable pages need a UTF-8 file and the parallel scan",
        ));
    }
    filter_parallel(
        path,
        delimiter,
        spec,
        limit,
        budget,
        threads,
        start_checkpoint,
        tx,
    )
}

/// The parallel half of [`scan_fast`], with threads made explicit so a test can force
/// four of them on a file small enough to check by eye.
// The eight arguments are all scalars from one scan request; the HTTP handler builds them
// in one place and a test needs to force `threads`, so a bag struct would only move the
// list somewhere else.
#[allow(clippy::too_many_arguments)]
fn filter_parallel(
    path: &Path,
    delimiter: Delimiter,
    spec: &FilterSpec,
    limit: usize,
    budget: Option<std::time::Duration>,
    threads: usize,
    start_checkpoint: usize,
    tx: &ProgressSender,
) -> io::Result<(ScanResult, Option<usize>)> {
    let delim = delimiter.byte();
    let col_idx = column_index(path, delim, spec)?;
    let predicate = |record: &[u8], delim: u8, quotes: QuoteStyle| {
        record_cell_matches(record, delim, quotes, col_idx, spec).unwrap_or_else(|| {
            let text = String::from_utf8_lossy(record);
            let fields = parse_csv_record_with(text.trim_end_matches(['\n', '\r']), delim, quotes);
            matches(fields.get(col_idx).map(String::as_str).unwrap_or(""), spec)
        })
    };
    let phase = format!("Filtering {}", spec.column);
    let options = crate::scan_parallel::ScanOptions {
        delimiter,
        limit,
        budget,
        threads,
        quotes: QuoteStyle::Rfc4180,
        phase: &phase,
        start_checkpoint,
        checkpoints_per_wave: crate::scan_parallel::CHECKPOINTS_PER_WAVE,
    };
    let batch = crate::scan_parallel::scan_parallel_bytes(path, &options, tx, predicate)?;
    Ok((
        ScanResult {
            rows: batch.rows,
            scanned: batch.scanned,
            matched: batch.matched,
            truncated: batch.truncated,
        },
        batch.next_checkpoint,
    ))
}

/// How several predicates combine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    /// Every predicate must hold.
    All,
    /// Any one predicate is enough.
    Any,
}

/// A set of predicates over one or more columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterSet {
    pub specs: Vec<FilterSpec>,
    pub combine: Combine,
}

impl FilterSet {
    pub fn single(spec: FilterSpec) -> Self {
        Self {
            specs: vec![spec],
            combine: Combine::All,
        }
    }

    /// Column indices for every distinct column the set mentions.
    fn resolve(&self, headers: &[String]) -> io::Result<Vec<(String, usize)>> {
        let mut out = Vec::with_capacity(self.specs.len());
        for spec in &self.specs {
            let idx = crate::csv_reader::resolve_column(&spec.column, headers)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            out.push((spec.column.clone(), idx));
        }
        Ok(out)
    }

    /// Compile every regex once, so a scan does no regex parsing per row.
    pub fn compile(&self, headers: &[String]) -> io::Result<CompiledSet> {
        let resolved = self.resolve(headers)?;
        let mut regexes = Vec::with_capacity(self.specs.len());
        for spec in &self.specs {
            if spec.op == Op::Regex {
                let re = regex::Regex::new(&spec.value).map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("bad regex: {e}"))
                })?;
                regexes.push(Some(re));
            } else {
                regexes.push(None);
            }
        }
        Ok(CompiledSet {
            specs: self.specs.clone(),
            regexes,
            columns: resolved,
            combine: self.combine,
        })
    }
}

/// A [`FilterSet`] with its regexes compiled once.
pub struct CompiledSet {
    specs: Vec<FilterSpec>,
    regexes: Vec<Option<regex::Regex>>,
    columns: Vec<(String, usize)>,
    combine: Combine,
}

impl CompiledSet {
    /// Does this row pass? Public so a viewer can reuse the same semantics the
    /// exporter uses — two implementations would eventually disagree.
    pub fn holds(&self, fields: &[String]) -> bool {
        let mut any = false;
        for (i, spec) in self.specs.iter().enumerate() {
            let idx = self.columns[i].1;
            let cell = fields.get(idx).map(String::as_str).unwrap_or("");
            let hit = match &self.regexes[i] {
                Some(re) => re.is_match(cell),
                None => matches(cell, spec),
            };
            match self.combine {
                Combine::All if !hit => return false,
                Combine::All => {}
                Combine::Any if hit => return true,
                Combine::Any => any |= hit,
            }
        }
        match self.combine {
            Combine::All => true,
            Combine::Any => any,
        }
    }

    /// Borrowed-field predicate for the quote-free fast path. `None` means a cell
    /// this filter reads is not valid UTF-8, so the caller must use the full parser.
    pub fn holds_bytes(&self, cells: &[&[u8]]) -> Option<bool> {
        let mut any = false;
        for (i, spec) in self.specs.iter().enumerate() {
            let index = self.columns[i].1;
            let cell = match cells.get(index) {
                Some(bytes) => std::str::from_utf8(bytes).ok()?,
                None => "",
            };
            let hit = match &self.regexes[i] {
                Some(re) => re.is_match(cell),
                None => matches(cell, spec),
            };
            match self.combine {
                Combine::All if !hit => return Some(false),
                Combine::All => {}
                Combine::Any if hit => return Some(true),
                Combine::Any => any |= hit,
            }
        }
        Some(match self.combine {
            Combine::All => true,
            Combine::Any => any,
        })
    }

    /// Evaluate a whole record from its bytes, with no per-row allocation: fields are
    /// visited once and only the columns this filter reads are inspected. `None` means
    /// the full parser is required (quotes, backslash quoting, non-UTF-8, or >64 specs).
    pub fn holds_record(
        &self,
        record: &[u8],
        delim: u8,
        quotes: QuoteStyle,
    ) -> Option<(bool, usize)> {
        let specs = self.specs.len();
        if specs > 64 {
            return None;
        }
        let mut seen = 0u64;
        let mut hits = 0u64;
        let mut fields = 0usize;
        let mut invalid = false;
        let complete = crate::csv_reader::visit_fields(record, delim, quotes, |index, bytes| {
            fields += 1;
            if invalid {
                return;
            }
            for (i, spec) in self.specs.iter().enumerate() {
                if self.columns[i].1 != index {
                    continue;
                }
                let Ok(cell) = std::str::from_utf8(bytes) else {
                    invalid = true;
                    return;
                };
                let hit = match &self.regexes[i] {
                    Some(re) => re.is_match(cell),
                    None => matches(cell, spec),
                };
                seen |= 1u64 << i;
                if hit {
                    hits |= 1u64 << i;
                }
            }
        });
        if !complete || invalid {
            return None;
        }
        for (i, spec) in self.specs.iter().enumerate() {
            if seen & (1u64 << i) == 0 {
                let hit = match &self.regexes[i] {
                    Some(re) => re.is_match(""),
                    None => matches("", spec),
                };
                if hit {
                    hits |= 1u64 << i;
                }
            }
        }
        let all = if specs == 64 {
            u64::MAX
        } else {
            (1u64 << specs) - 1
        };
        Some((
            match self.combine {
                Combine::All => hits == all,
                Combine::Any => hits != 0,
            },
            fields,
        ))
    }
}

/// Write the rows matching `set` to `output` as CSV, streaming.
///
/// This is the "filter, then keep the result" workflow: memory stays flat
/// because records are copied one at a time, and the header is written once.
pub fn extract(
    path: &Path,
    delimiter: Delimiter,
    set: &FilterSet,
    output: &Path,
    tx: &ProgressSender,
) -> io::Result<ExtractSummary> {
    extract_style(path, delimiter, set, output, QuoteStyle::Rfc4180, tx)
}

/// Same as [`extract`], with an explicit quote style.
pub fn extract_style(
    path: &Path,
    delimiter: Delimiter,
    set: &FilterSet,
    output: &Path,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<ExtractSummary> {
    if set.specs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no filter expressions",
        ));
    }
    // UTF-8 files can be split on line boundaries; workers write ordered part files
    // and the main thread concatenates them. Non-UTF-8 and >64 predicates stay serial.
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let threads = crate::scan_parallel::threads_for(file_len);
    if threads > 1 && set.specs.len() <= 64 && crate::scan_parallel::is_seekable(path)? {
        if let Some(summary) = extract_parallel(path, delimiter, set, output, quotes, tx, threads)?
        {
            return Ok(summary);
        }
    }
    let total_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseStart {
        phase: "Filtering".into(),
        total_bytes,
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
    let compiled = set.compile(&headers)?;

    let mut out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(output)?);
    // The decoded reader strips the BOM, so put it back: Excel reads a BOM-less UTF-8
    // file as the system code page.
    crate::encoding::keep_bom(&mut out, path)?;
    let mut header_line = record.clone();
    if !header_line.ends_with(b"\n") {
        header_line.push(b'\n');
    }
    out.write_all(&header_line)?;

    let mut summary = ExtractSummary::default();
    let mut written: u64 = 0;
    let width = headers.len();
    while records.next_record(&mut record)? {
        summary.scanned += 1;
        written += record.len() as u64;
        // Fast path: quote-free records are matched from borrowed fields, with no
        // per-row String or field vector. Anything else falls back to the parser.
        let (hit, field_count) = match compiled.holds_record(&record, delim, quotes) {
            Some(result) => result,
            None => {
                let text = String::from_utf8_lossy(&record);
                let fields =
                    parse_csv_record_with(text.trim_end_matches(['\n', '\r']), delim, quotes);
                (compiled.holds(&fields), fields.len())
            }
        };
        if hit {
            if field_count != width {
                summary.matched_ragged += 1;
            }
            // Keep the source bytes verbatim: re-serialising would normalise
            // quoting and line endings for no benefit.
            out.write_all(&record)?;
            if !record.ends_with(b"\n") {
                out.write_all(b"\n")?;
            }
            summary.matched += 1;
        }
        if summary.scanned.is_multiple_of(200_000) {
            tx.send(ProgressEvent::Progress { bytes: written }).ok();
        }
    }
    out.flush()?;
    let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseDone {
        phase: "Filtering".into(),
    })
    .ok();
    Ok(ExtractSummary { bytes, ..summary })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractSummary {
    pub scanned: u64,
    pub matched: u64,
    pub bytes: u64,
    /// Matched rows whose field count differed from the header (inherited from
    /// the source, and passed through verbatim).
    pub matched_ragged: u64,
}

/// Below this input size the extra counting scan costs more than the 20 GB part write
/// it avoids; above it, two-pass wins. Measured on 29 GB (parts faster) and 100 GB
/// (two-pass 79.62 s vs 98.56 s).
const TWO_PASS_MIN_BYTES: u64 = 64 * 1024 * 1024 * 1024;

fn extract_parallel(
    path: &Path,
    delimiter: Delimiter,
    set: &FilterSet,
    output: &Path,
    quotes: QuoteStyle,
    tx: &ProgressSender,
    threads: usize,
) -> io::Result<Option<ExtractSummary>> {
    let len = std::fs::metadata(path)?.len();
    if len >= TWO_PASS_MIN_BYTES {
        extract_two_pass(path, delimiter, set, output, quotes, tx, threads)
    } else {
        extract_parts(path, delimiter, set, output, quotes, tx, threads)
    }
}

struct PartSlab<'a> {
    path: &'a Path,
    start: u64,
    end: u64,
    skip_header: bool,
    compiled: &'a CompiledSet,
    width: usize,
    delim: u8,
    quotes: QuoteStyle,
    part: &'a Path,
}

fn write_part_slab(slab: PartSlab<'_>) -> io::Result<ExtractSummary> {
    let PartSlab {
        path,
        start,
        end,
        skip_header,
        compiled,
        width,
        delim,
        quotes,
        part,
    } = slab;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut records = RecordReader::new(BufReader::with_capacity(1024 * 1024, file), 1024 * 1024);
    let mut record = Vec::with_capacity(64 * 1024);
    let mut offset = start;
    if skip_header {
        if !records.next_record(&mut record)? {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
        }
        offset += record.len() as u64;
    }
    let mut summary = ExtractSummary::default();
    let mut out = BufWriter::with_capacity(1024 * 1024, File::create(part)?);
    while records.next_record(&mut record)? {
        if offset >= end {
            break;
        }
        let (hit, field_count) = match compiled.holds_record(&record, delim, quotes) {
            Some(result) => result,
            None => {
                let text = String::from_utf8_lossy(&record);
                let fields =
                    parse_csv_record_with(text.trim_end_matches(['\n', '\r']), delim, quotes);
                (compiled.holds(&fields), fields.len())
            }
        };
        if hit {
            if field_count != width {
                summary.matched_ragged += 1;
            }
            out.write_all(&record)?;
            if !record.ends_with(b"\n") {
                out.write_all(b"\n")?;
            }
            summary.matched += 1;
        }
        summary.scanned += 1;
        offset += record.len() as u64;
    }
    out.flush()?;
    Ok(summary)
}

fn extract_parts(
    path: &Path,
    delimiter: Delimiter,
    set: &FilterSet,
    output: &Path,
    quotes: QuoteStyle,
    tx: &ProgressSender,
    threads: usize,
) -> io::Result<Option<ExtractSummary>> {
    let len = std::fs::metadata(path)?.len();
    let bom = match encoding::detect(path)? {
        encoding::TextEncoding::Utf8 { bom: true } => 3,
        _ => 0,
    };
    let Some(starts) = slab_starts(path, len, threads, bom)? else {
        return Ok(None);
    };

    let reader = encoding::open_decoded(path)?;
    let mut records = RecordReader::new(
        BufReader::with_capacity(4 * 1024 * 1024, reader),
        4 * 1024 * 1024,
    );
    let mut header = Vec::with_capacity(4096);
    if !records.next_record(&mut header)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
    }
    let headers =
        parse_csv_record_with(&String::from_utf8_lossy(&header), delimiter.byte(), quotes);
    let compiled = std::sync::Arc::new(set.compile(&headers)?);
    drop(records);

    tx.send(ProgressEvent::PhaseStart {
        phase: "Filtering".into(),
        total_bytes: len,
    })
    .ok();

    let mut out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(output)?);
    crate::encoding::keep_bom(&mut out, path)?;
    out.write_all(&header)?;
    if !header.ends_with(b"\n") {
        out.write_all(b"\n")?;
    }

    let delim = delimiter.byte();
    let width = headers.len();
    let mut parts = Vec::with_capacity(starts.len());
    let mut handles = Vec::with_capacity(starts.len());
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(len);
        let part = output.with_file_name(format!(
            ".{}.part{index}",
            output
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("filter")
        ));
        parts.push(part.clone());
        let compiled = compiled.clone();
        let path = path.to_path_buf();
        let skip_header = index == 0;
        handles.push(std::thread::spawn(move || {
            let slab = PartSlab {
                path: &path,
                start,
                end,
                skip_header,
                compiled: &compiled,
                width,
                delim,
                quotes,
                part: &part,
            };
            write_part_slab(slab)
        }));
    }

    let mut summary = ExtractSummary::default();
    let mut failure = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok(slab)) => {
                summary.scanned += slab.scanned;
                summary.matched += slab.matched;
                summary.matched_ragged += slab.matched_ragged;
            }
            Ok(Err(error)) => {
                failure = Some(error);
            }
            Err(_) => {
                failure = Some(io::Error::other("a filter worker panicked"));
            }
        }
    }
    if let Some(error) = failure {
        for part in &parts {
            let _ = std::fs::remove_file(part);
        }
        return Err(error);
    }
    // Copy each part into its own byte range in the final file, concurrently. The
    // parts still exist (ordered extraction made them), but the serial 20 GB concat
    // was the 100 GB export bottleneck; parallel writes use the disk instead of one
    // core. Ranges are disjoint and computed from the part sizes.
    out.flush()?;
    drop(out);
    let header_len = std::fs::metadata(output)?.len();
    let mut offsets = Vec::with_capacity(parts.len());
    let mut next_offset = header_len;
    for part in &parts {
        offsets.push(next_offset);
        next_offset += std::fs::metadata(part)?.len();
    }
    let final_path = output.to_path_buf();
    let file = std::fs::OpenOptions::new().write(true).open(&final_path)?;
    file.set_len(next_offset)?;
    drop(file);
    let mut copies = Vec::with_capacity(parts.len());
    for (part, offset) in parts.iter().zip(offsets) {
        let part = part.clone();
        let final_path = final_path.clone();
        copies.push(std::thread::spawn(move || -> io::Result<()> {
            let mut src = File::open(&part)?;
            let mut dst = std::fs::OpenOptions::new().write(true).open(&final_path)?;
            dst.seek(SeekFrom::Start(offset))?;
            let mut buffer = vec![0u8; 4 * 1024 * 1024];
            loop {
                let read = src.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                dst.write_all(&buffer[..read])?;
            }
            let _ = std::fs::remove_file(&part);
            Ok(())
        }));
    }
    for copy in copies {
        match copy.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                failure = Some(error);
            }
            Err(_) => failure = Some(io::Error::other("a filter copy worker panicked")),
        }
    }
    if let Some(error) = failure {
        let _ = std::fs::remove_file(output);
        for part in &parts {
            let _ = std::fs::remove_file(part);
        }
        return Err(error);
    }
    summary.bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseDone {
        phase: "Filtering".into(),
    })
    .ok();
    Ok(Some(summary))
}

fn extract_two_pass(
    path: &Path,
    delimiter: Delimiter,
    set: &FilterSet,
    output: &Path,
    quotes: QuoteStyle,
    tx: &ProgressSender,
    threads: usize,
) -> io::Result<Option<ExtractSummary>> {
    let len = std::fs::metadata(path)?.len();
    let bom = match encoding::detect(path)? {
        encoding::TextEncoding::Utf8 { bom: true } => 3,
        _ => 0,
    };
    let Some(starts) = slab_starts(path, len, threads, bom)? else {
        return Ok(None);
    };

    let reader = encoding::open_decoded(path)?;
    let mut records = RecordReader::new(
        BufReader::with_capacity(4 * 1024 * 1024, reader),
        4 * 1024 * 1024,
    );
    let mut header = Vec::with_capacity(4096);
    if !records.next_record(&mut header)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
    }
    let headers =
        parse_csv_record_with(&String::from_utf8_lossy(&header), delimiter.byte(), quotes);
    let compiled = std::sync::Arc::new(set.compile(&headers)?);
    drop(records);

    tx.send(ProgressEvent::PhaseStart {
        phase: "Filtering".into(),
        total_bytes: len,
    })
    .ok();

    let mut header_out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(output)?);
    crate::encoding::keep_bom(&mut header_out, path)?;
    header_out.write_all(&header)?;
    if !header.ends_with(b"\n") {
        header_out.write_all(b"\n")?;
    }
    header_out.flush()?;
    drop(header_out);

    let delim = delimiter.byte();
    let width = headers.len();
    let base = std::fs::metadata(output)?.len();

    let output_path = output.to_path_buf();
    let scan_pass = |write_ranges: Option<&[(u64, u64)]>| -> io::Result<Vec<SlabStats>> {
        let mut handles = Vec::with_capacity(starts.len());
        for (index, start) in starts.iter().copied().enumerate() {
            let end = starts.get(index + 1).copied().unwrap_or(len);
            let compiled = compiled.clone();
            let path = path.to_path_buf();
            let out_path = output_path.clone();
            let skip_header = index == 0;
            let range = write_ranges.map(|ranges| ranges[index]);
            handles.push(std::thread::spawn(move || {
                let slab = Slab {
                    path: &path,
                    start,
                    end,
                    skip_header,
                    compiled: &compiled,
                    width,
                    delim,
                    quotes,
                };
                scan_slab(slab, range.map(|(at, _)| (out_path.as_path(), at)))
            }));
        }
        let mut stats = Vec::with_capacity(handles.len());
        for handle in handles {
            stats.push(match handle.join() {
                Ok(Ok(stats)) => stats,
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(io::Error::other("a filter worker panicked")),
            });
        }
        Ok(stats)
    };

    // Pass 1: count output bytes per slab without writing them. This is the price of
    // avoiding the part-file double write: one extra parallel scan, no extra bytes.
    let stats = match scan_pass(None) {
        Ok(stats) => stats,
        Err(error) => {
            let _ = std::fs::remove_file(output);
            return Err(error);
        }
    };
    let total: u64 = stats.iter().map(|slab| slab.bytes).sum();
    let mut ranges = Vec::with_capacity(stats.len());
    let mut at = base;
    for slab in &stats {
        let end = at + slab.bytes;
        ranges.push((at, end));
        at = end;
    }
    let file = std::fs::OpenOptions::new().write(true).open(output)?;
    file.set_len(at)?;
    drop(file);

    // Pass 2: parse again and write each slab straight into its final byte range.
    let written = match scan_pass(Some(&ranges)) {
        Ok(stats) => stats,
        Err(error) => {
            let _ = std::fs::remove_file(output);
            return Err(error);
        }
    };
    let written_bytes: u64 = written.iter().map(|slab| slab.bytes).sum();
    if written_bytes != total {
        let _ = std::fs::remove_file(output);
        return Err(io::Error::other(format!(
            "filter output size mismatch: counted {total} bytes, wrote {written_bytes}"
        )));
    }
    let mut summary = ExtractSummary::default();
    for slab in &written {
        summary.scanned += slab.scanned;
        summary.matched += slab.matched;
        summary.matched_ragged += slab.matched_ragged;
    }
    summary.bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseDone {
        phase: "Filtering".into(),
    })
    .ok();
    Ok(Some(summary))
}

/// One aligned start per worker. Returns `None` if splits would overlap (the caller
/// then takes the plain sequential path).
fn slab_starts(path: &Path, len: u64, threads: usize, bom: u64) -> io::Result<Option<Vec<u64>>> {
    let mut starts = Vec::with_capacity(threads);
    starts.push(bom.min(len));
    for index in 1..threads {
        let target = (len.saturating_mul(index as u64) / threads as u64).max(bom);
        let start = next_line_start(path, target, len)?;
        if start < len && start <= *starts.last().unwrap() {
            return Ok(None);
        }
        starts.push(start);
    }
    Ok(Some(starts))
}

fn next_line_start(path: &Path, mut at: u64, len: u64) -> io::Result<u64> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(at))?;
    let mut buf = [0u8; 8192];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            return Ok(len);
        }
        if let Some(pos) = buf[..read].iter().position(|byte| *byte == b'\n') {
            return Ok(at + pos as u64 + 1);
        }
        at += read as u64;
    }
}

#[derive(Default, Clone, Copy)]
struct SlabStats {
    scanned: u64,
    matched: u64,
    matched_ragged: u64,
    bytes: u64,
}

struct Slab<'a> {
    path: &'a Path,
    start: u64,
    end: u64,
    skip_header: bool,
    compiled: &'a CompiledSet,
    width: usize,
    delim: u8,
    quotes: QuoteStyle,
}

fn scan_slab(slab: Slab<'_>, writer: Option<(&Path, u64)>) -> io::Result<SlabStats> {
    const WRITE_BUFFER: usize = 1024 * 1024;
    let Slab {
        path,
        start,
        end,
        skip_header,
        compiled,
        width,
        delim,
        quotes,
    } = slab;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut records = RecordReader::new(BufReader::with_capacity(1024 * 1024, file), 1024 * 1024);
    let mut record = Vec::with_capacity(64 * 1024);
    let mut offset = start;
    if skip_header {
        if !records.next_record(&mut record)? {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
        }
        offset += record.len() as u64;
    }
    let mut out = match writer {
        Some((output, at)) => {
            let mut file = std::fs::OpenOptions::new().write(true).open(output)?;
            file.seek(SeekFrom::Start(at))?;
            Some(BufWriter::with_capacity(WRITE_BUFFER, file))
        }
        None => None,
    };
    let mut stats = SlabStats::default();
    while records.next_record(&mut record)? {
        if offset >= end {
            break;
        }
        let (hit, field_count) = match compiled.holds_record(&record, delim, quotes) {
            Some(result) => result,
            None => {
                let text = String::from_utf8_lossy(&record);
                let fields =
                    parse_csv_record_with(text.trim_end_matches(['\n', '\r']), delim, quotes);
                (compiled.holds(&fields), fields.len())
            }
        };
        if hit {
            if field_count != width {
                stats.matched_ragged += 1;
            }
            if let Some(out) = out.as_mut() {
                out.write_all(&record)?;
                if !record.ends_with(b"\n") {
                    out.write_all(b"\n")?;
                }
            }
            stats.bytes += record.len() as u64 + u64::from(!record.ends_with(b"\n"));
            stats.matched += 1;
        }
        stats.scanned += 1;
        offset += record.len() as u64;
    }
    if let Some(mut out) = out {
        out.flush()?;
    }
    Ok(stats)
}

/// Single-cell predicate.
pub fn matches(cell: &str, spec: &FilterSpec) -> bool {
    match spec.op {
        Op::Empty => cell.trim().is_empty(),
        Op::NotEmpty => !cell.trim().is_empty(),
        Op::Eq => cell == spec.value,
        Op::Ne => cell != spec.value,
        Op::Contains => cell.contains(&spec.value),
        Op::In => {
            let cell_trimmed = cell.trim();
            spec.value
                .split(',')
                .map(|v| v.trim())
                // Matching is case-insensitive, like `=value` is exact and `~`
                // is literal; the list form is the friendly one.
                .any(|v| v.eq_ignore_ascii_case(cell_trimmed))
        }
        Op::Wildcard => wildcard_match(cell, &spec.value),
        Op::Regex => regex::Regex::new(&spec.value)
            .map(|re| re.is_match(cell))
            .unwrap_or(false),
        Op::Gt | Op::Gte | Op::Lt | Op::Lte => {
            // Numeric when both sides parse, string order otherwise (so ISO dates
            // and zero-padded ids behave sensibly).
            let ord = match (cell.trim().parse::<f64>(), spec.value.parse::<f64>()) {
                (Ok(a), Ok(b)) => a.partial_cmp(&b),
                _ => Some(cell.trim().cmp(spec.value.as_str())),
            };
            match ord {
                Some(Ordering::Greater) => matches!(spec.op, Op::Gt | Op::Gte),
                Some(Ordering::Equal) => matches!(spec.op, Op::Gte | Op::Lte),
                Some(Ordering::Less) => matches!(spec.op, Op::Lt | Op::Lte),
                None => false,
            }
        }
    }
}

/// One matched row, decoded and split into cells.
#[derive(Clone, Debug)]
pub struct MatchedRow {
    /// 0-based data row.
    pub row: u64,
    pub cells: Vec<String>,
}

/// Load specific rows (usually filter hits) through the sparse index.
///
/// The index is built on first use and cached in the sidecar file, so repeated
/// filtered views only pay for the rows they display.
pub fn read_rows(path: &Path, delimiter: Delimiter, rows: &[u64]) -> io::Result<Vec<MatchedRow>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let encoding = encoding::detect(path)?;
    let sidecar = SparseIndex::sidecar_path(path);
    let index = match SparseIndex::load(&sidecar) {
        Ok(idx) if idx.matches(path) => idx,
        _ => {
            let idx = SparseIndex::build(path, crate::index::DEFAULT_INTERVAL)?;
            idx.save(&sidecar).ok();
            idx
        }
    };
    let raw = index.read_rows_at(path, rows)?;
    let delim = delimiter.byte();
    Ok(raw
        .into_iter()
        .map(|(row, bytes)| {
            let text = encoding::decode_cell(&bytes, encoding);
            let cells = parse_csv_record_with(
                text.trim_end_matches(['\n', '\r']),
                delim,
                QuoteStyle::Rfc4180,
            );
            MatchedRow { row, cells }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(expr: &str) -> FilterSpec {
        FilterSpec::parse(expr).unwrap()
    }

    #[test]
    fn parses_operators() {
        let s = spec("city=CDMX");
        assert_eq!((s.column.as_str(), s.op), ("city", Op::Eq));
        assert_eq!(s.value, "CDMX");

        assert_eq!(spec("amount>=100").op, Op::Gte);
        assert_eq!(spec("amount>100").value, "100");
        assert_eq!(spec("note~refund").op, Op::Contains);
        assert_eq!(spec("email empty").op, Op::Empty);
        assert_eq!(spec("email notempty").op, Op::NotEmpty);
        assert!(FilterSpec::parse("nonsense").is_err());
        assert!(FilterSpec::parse("=x").is_err());
    }

    #[test]
    fn matches_semantics() {
        assert!(matches("CDMX", &spec("city=CDMX")));
        assert!(!matches("Puebla", &spec("city=CDMX")));
        assert!(matches("refund-2026", &spec("note~refund")));
        assert!(matches("1200", &spec("amt>1000")));
        assert!(matches("1000", &spec("amt>=1000")));
        assert!(matches("1000", &spec("amt<=1000")));
        assert!(!matches("1000", &spec("amt>1000")));
        assert!(matches("  ", &spec("x empty")));
        assert!(!matches("a", &spec("x empty")));
        // Non-numeric falls back to string ordering.
        assert!(matches("2026-02-01", &spec("d>2026-01-01")));
    }

    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("csvx_filter_{}_{}", std::process::id(), name));
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).ok();
            let mut idx = self.0.clone().into_os_string();
            idx.push(".idx");
            std::fs::remove_file(std::path::PathBuf::from(idx)).ok();
        }
    }

    /// The parallel scan exists for wall time only: it must return the sequential scan's
    /// rows, counts and truncation flag exactly. Boundaries fall on index checkpoints, and
    /// one row in four thousand carries an embedded newline, so a plan that assumed one
    /// record per line would drift there.
    #[test]
    fn parallel_scan_agrees_with_sequential() {
        let file = TempFile::new("parallel");
        let mut text = String::from("city,amount,note\n");
        for i in 0..12_000u64 {
            let city = if i % 3 == 0 { "Tijuana" } else { "CDMX" };
            if i % 4000 == 3999 {
                text.push_str(&format!("{city},{i},\"line one\nline two\"\n"));
            } else {
                text.push_str(&format!("{city},{i},note \"quoted, comma\"\n"));
            }
        }
        std::fs::write(file.path(), text).unwrap();

        let predicate = spec("city=Tijuana");
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        let sequential = scan(file.path(), Delimiter::Comma, &predicate, 10_000, &tx).unwrap();
        let parallel = filter_parallel(
            file.path(),
            Delimiter::Comma,
            &predicate,
            10_000,
            None,
            4,
            0,
            &tx,
        )
        .unwrap()
        .0;

        assert_eq!(sequential.rows, parallel.rows);
        assert_eq!(sequential.matched, parallel.matched);
        assert_eq!(sequential.scanned, parallel.scanned);
        assert_eq!(sequential.truncated, parallel.truncated);
        assert_eq!(parallel.rows.len(), 4000);
        let expected: Vec<u64> = (0..12_000u64).filter(|i| i % 3 == 0).collect();
        assert_eq!(parallel.rows, expected);
    }

    /// A limit smaller than the match count must truncate identically, including when the
    /// kills land in different chunks.
    #[test]
    fn parallel_scan_truncates_at_the_same_limit() {
        let file = TempFile::new("truncate");
        let mut text = String::from("city,amount\n");
        for i in 0..6_000u64 {
            text.push_str(&format!("CDMX,{i}\n"));
        }
        std::fs::write(file.path(), text).unwrap();

        let spec = spec("city=CDMX");
        let (tx, rx) = std::sync::mpsc::channel();
        drop(rx);
        let sequential = scan(file.path(), Delimiter::Comma, &spec, 25, &tx).unwrap();
        let parallel = filter_parallel(file.path(), Delimiter::Comma, &spec, 25, None, 4, 0, &tx)
            .unwrap()
            .0;
        assert_eq!(sequential.rows, parallel.rows);
        assert_eq!(parallel.rows.len(), 25);
        assert!(parallel.truncated);
        assert_eq!(parallel.matched, 6_000);
    }
}

#[cfg(test)]
mod set_tests {
    use super::*;
    use std::path::PathBuf;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_fset_{tag}_{}_{}",
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

    fn fixture() -> (Dir, PathBuf) {
        let d = Dir::new("set");
        let p = d.join("a.csv");
        let mut csv = String::from("id,city,amount\n");
        csv.push_str("1,CDMX,10\n");
        csv.push_str("2,Puebla,500\n");
        csv.push_str("3,CDMX,500\n");
        csv.push_str("4,Tijuana,20\n");
        std::fs::write(&p, csv).unwrap();
        (d, p)
    }

    #[test]
    fn and_combines_all_predicates() {
        let (_d, p) = fixture();
        let set = FilterSet {
            specs: vec![
                FilterSpec::parse("city=CDMX").unwrap(),
                FilterSpec::parse("amount>=500").unwrap(),
            ],
            combine: Combine::All,
        };
        let out_dir = Dir::new("and_out");
        let out = out_dir.join("r.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let s = extract(&p, Delimiter::Comma, &set, &out, &tx).unwrap();
        assert_eq!(s.scanned, 4);
        assert_eq!(s.matched, 1);
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(text, "id,city,amount\n3,CDMX,500\n");
    }

    #[test]
    fn or_keeps_either() {
        let (_d, p) = fixture();
        let set = FilterSet {
            specs: vec![
                FilterSpec::parse("city=Tijuana").unwrap(),
                FilterSpec::parse("amount>=500").unwrap(),
            ],
            combine: Combine::Any,
        };
        let out_dir = Dir::new("or_out");
        let out = out_dir.join("r.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let s = extract(&p, Delimiter::Comma, &set, &out, &tx).unwrap();
        assert_eq!(s.matched, 3);
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("2,Puebla,500"), "{text}");
        assert!(text.contains("3,CDMX,500"), "{text}");
        assert!(text.contains("4,Tijuana,20"), "{text}");
        assert!(!text.contains("1,CDMX,10"), "{text}");
    }

    #[test]
    fn record_cell_matches_handles_plain_quoted_short_and_escaped_rows() {
        let spec = FilterSpec::parse("city=CDMX").unwrap();
        assert_eq!(
            record_cell_matches(b"1,CDMX,10\n", b',', QuoteStyle::Rfc4180, 1, &spec),
            Some(true)
        );
        assert_eq!(
            record_cell_matches(b"1,Puebla,10\n", b',', QuoteStyle::Rfc4180, 1, &spec),
            Some(false)
        );
        assert_eq!(
            record_cell_matches(b"1,\"CDMX\",10\n", b',', QuoteStyle::Rfc4180, 1, &spec),
            Some(true)
        );
        assert_eq!(
            record_cell_matches(b"1,\"CD\"\"MX\",10\n", b',', QuoteStyle::Rfc4180, 1, &spec),
            None,
            "escaped quotes need the parser"
        );
        assert_eq!(
            record_cell_matches(b"1,\xff,10\n", b',', QuoteStyle::Rfc4180, 1, &spec),
            None
        );
        assert_eq!(
            record_cell_matches(b"1\n", b',', QuoteStyle::Rfc4180, 1, &spec),
            Some(false),
            "a short row reads as an empty cell"
        );
    }

    #[test]
    fn two_pass_extract_agrees_with_sequential() {
        let d = Dir::new("twopass");
        let p = d.join("p.csv");
        let mut csv = String::from("id,city,amount\n");
        for i in 0..100 {
            let city = if i % 3 == 0 { "CDMX" } else { "Puebla" };
            csv.push_str(&format!("{i},{city},{}\n", 100 + i));
        }
        csv.push_str("100,\"CDMX\",999\n");
        std::fs::write(&p, csv).unwrap();
        let set = FilterSet::single(FilterSpec::parse("city=CDMX").unwrap());
        let seq_out = d.join("seq.csv");
        let two_out = d.join("two.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        extract(&p, Delimiter::Comma, &set, &seq_out, &tx).unwrap();
        extract_two_pass(
            &p,
            Delimiter::Comma,
            &set,
            &two_out,
            QuoteStyle::Rfc4180,
            &tx,
            4,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read(&seq_out).unwrap(),
            std::fs::read(&two_out).unwrap()
        );
        let leftovers: Vec<_> = std::fs::read_dir(&d.0)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".part"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn extract_reads_gzip_and_zstd_input() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let d = Dir::new("compressed");
        let plain = d.join("plain.csv");
        let csv = "id,city,amount\n1,CDMX,10\n2,Puebla,20\n3,CDMX,30\n";
        std::fs::write(&plain, csv).unwrap();
        let set = FilterSet::single(FilterSpec::parse("city=CDMX").unwrap());
        let (tx, _rx) = std::sync::mpsc::channel();
        let expected = d.join("expected.csv");
        extract(&plain, Delimiter::Comma, &set, &expected, &tx).unwrap();
        let expected_bytes = std::fs::read(&expected).unwrap();

        let gz = d.join("plain.csv.gz");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(csv.as_bytes()).unwrap();
        std::fs::write(&gz, encoder.finish().unwrap()).unwrap();
        let gz_out = d.join("gz.csv");
        extract(&gz, Delimiter::Comma, &set, &gz_out, &tx).unwrap();
        assert_eq!(std::fs::read(&gz_out).unwrap(), expected_bytes);

        let zst = d.join("plain.csv.zst");
        let mut encoder = zstd::Encoder::new(Vec::new(), 3).unwrap();
        encoder.write_all(csv.as_bytes()).unwrap();
        std::fs::write(&zst, encoder.finish().unwrap()).unwrap();
        let zst_out = d.join("zst.csv");
        extract(&zst, Delimiter::Comma, &set, &zst_out, &tx).unwrap();
        assert_eq!(std::fs::read(&zst_out).unwrap(), expected_bytes);
    }

    #[test]
    fn parallel_extract_agrees_with_sequential_and_cleans_parts() {
        let d = Dir::new("parallel");
        let p = d.join("p.csv");
        let mut csv = String::from("id,city,amount\n");
        for i in 0..100 {
            let city = match i % 4 {
                0 => "CDMX",
                1 => "Puebla",
                2 => "Tijuana",
                _ => "CDMX",
            };
            csv.push_str(&format!("{i},{city},{}\n", 100 + i));
        }
        // A quoted plain cell must survive both paths byte-for-byte.
        csv.push_str("100,\"CDMX\",999\n");
        std::fs::write(&p, csv).unwrap();
        let set = FilterSet::single(FilterSpec::parse("city=CDMX").unwrap());
        let seq_out = d.join("seq.csv");
        let par_out = d.join("par.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let seq = extract(&p, Delimiter::Comma, &set, &seq_out, &tx).unwrap();
        let par = extract_parallel(
            &p,
            Delimiter::Comma,
            &set,
            &par_out,
            QuoteStyle::Rfc4180,
            &tx,
            4,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read(&seq_out).unwrap(),
            std::fs::read(&par_out).unwrap()
        );
        assert_eq!((seq.scanned, seq.matched), (par.scanned, par.matched));
        assert_eq!(seq.scanned, 101);
        let leftovers: Vec<_> = std::fs::read_dir(&d.0)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".part"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn unknown_column_is_a_client_error() {
        let (_d, p) = fixture();
        let set = FilterSet::single(FilterSpec::parse("nope=1").unwrap());
        let out_dir = Dir::new("bad_col");
        let out = out_dir.join("r.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = extract(&p, Delimiter::Comma, &set, &out, &tx).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

#[cfg(test)]
mod regex_tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_re_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
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

    #[test]
    fn regex_operator_is_parsed_and_validated() {
        let spec = FilterSpec::parse("note~re:^REF").unwrap();
        assert_eq!(spec.op, Op::Regex);
        assert_eq!(spec.value, "^REF");
        // A plain `~` is still a substring search.
        assert_eq!(FilterSpec::parse("note~refund").unwrap().op, Op::Contains);
        // A broken pattern fails at parse time, not on the first row.
        assert!(FilterSpec::parse("note~re:[unclosed").is_err());
    }

    #[test]
    fn regex_filters_and_combines_with_other_predicates() {
        let d = Dir::new("run");
        let input = d.join("in.csv");
        let output = d.join("out.csv");
        let mut csv = String::from("id,city,note\n");
        csv.push_str("1,CDMX,REFUND issued\n");
        csv.push_str("2,CDMX,paid\n");
        csv.push_str("3,Puebla,REFUND-2\n");
        csv.push_str("4,Puebla,ok\n");
        std::fs::write(&input, csv).unwrap();

        let set = FilterSet {
            specs: vec![
                FilterSpec::parse("note~re:^REF").unwrap(),
                FilterSpec::parse("city=Puebla").unwrap(),
            ],
            combine: Combine::All,
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let summary = extract(&input, Delimiter::Comma, &set, &output, &tx).unwrap();
        assert_eq!(summary.matched, 1);
        assert_eq!(
            std::fs::read_to_string(&output).unwrap(),
            "id,city,note\n3,Puebla,REFUND-2\n"
        );
    }

    #[test]
    fn holds_record_matches_plain_quoted_and_escaped_rows() {
        let set = FilterSet {
            specs: vec![
                FilterSpec::parse("city=CDMX").unwrap(),
                FilterSpec::parse("amount>5").unwrap(),
            ],
            combine: Combine::All,
        };
        let compiled = set
            .compile(&["city".to_string(), "amount".to_string()])
            .unwrap();
        assert_eq!(
            compiled.holds_record(b"CDMX,10\n", b',', QuoteStyle::Rfc4180),
            Some((true, 2))
        );
        assert_eq!(
            compiled.holds_record(b"Puebla,10\n", b',', QuoteStyle::Rfc4180),
            Some((false, 2))
        );
        // A quoted plain cell is still readable; a doubled quote needs the parser.
        assert_eq!(
            compiled.holds_record(b"\"CDMX\",10\n", b',', QuoteStyle::Rfc4180),
            Some((true, 2))
        );
        assert_eq!(
            compiled.holds_record(b"\"CD\"\"MX\",10\n", b',', QuoteStyle::Rfc4180),
            None
        );
    }

    #[test]
    fn holds_bytes_agrees_with_holds_and_refuses_non_utf8() {
        let set = FilterSet {
            specs: vec![
                FilterSpec::parse("city=CDMX").unwrap(),
                FilterSpec::parse("amount>5").unwrap(),
            ],
            combine: Combine::All,
        };
        let compiled = set
            .compile(&["city".to_string(), "amount".to_string()])
            .unwrap();
        let cells: [&[u8]; 2] = [b"CDMX", b"10"];
        assert_eq!(compiled.holds_bytes(&cells), Some(true));
        let cells: [&[u8]; 2] = [b"Puebla", b"10"];
        assert_eq!(compiled.holds_bytes(&cells), Some(false));
        let fields = vec!["CDMX".to_string(), "10".to_string()];
        assert!(compiled.holds(&fields), "reference path agrees");
        // A cell the predicate reads must be UTF-8, or the caller falls back.
        let cells: [&[u8]; 2] = [b"CDMX", &[0xff]];
        assert_eq!(compiled.holds_bytes(&cells), None);
    }

    #[test]
    fn regex_matches_an_empty_cell_only_if_the_pattern_allows_it() {
        assert!(matches("", &FilterSpec::parse("x~re:^$").unwrap()));
        assert!(!matches("", &FilterSpec::parse("x~re:^a").unwrap()));
    }
}

#[cfg(test)]
mod in_tests {
    use super::*;

    #[test]
    fn in_lists_are_trimmed_and_case_insensitive() {
        let spec = FilterSpec::parse("city in CDMX, Puebla , Tijuana").unwrap();
        assert_eq!(spec.op, Op::In);
        assert!(matches("CDMX", &spec));
        assert!(matches(" puebla ", &spec));
        assert!(!matches("Monterrey", &spec));
    }

    #[test]
    fn in_needs_values_and_a_column() {
        assert!(FilterSpec::parse(" in a,b").is_err());
        assert!(FilterSpec::parse("city in ").is_err());
    }

    #[test]
    fn in_combines_with_other_predicates() {
        let set = FilterSet {
            specs: vec![
                FilterSpec::parse("city in CDMX,Puebla").unwrap(),
                FilterSpec::parse("amount>100").unwrap(),
            ],
            combine: Combine::All,
        };
        let headers = vec!["city".to_string(), "amount".to_string()];
        let compiled = set.compile(&headers).unwrap();
        let yes = vec!["CDMX".to_string(), "500".to_string()];
        let too_small = vec!["CDMX".to_string(), "50".to_string()];
        let wrong_city = vec!["Tijuana".to_string(), "500".to_string()];
        assert!(compiled.holds(&yes));
        assert!(!compiled.holds(&too_small));
        assert!(!compiled.holds(&wrong_city));
    }
}

/// Byte-level glob matching for spreadsheet criteria: `*` matches any run, `?` one
/// ASCII character, ASCII letters compare case-insensitively. Backtracking without
/// allocation, because a `countif` may call this once per row.
fn wildcard_match(text: &str, pattern: &str) -> bool {
    fn same(a: u8, b: u8) -> bool {
        a.eq_ignore_ascii_case(&b)
    }
    let text = text.as_bytes();
    let pattern = pattern.as_bytes();
    let (mut ti, mut pi) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == b'?' || same(pattern[pi], text[ti])) {
            ti += 1;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(star_at) = star {
            pi = star_at + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    pi == pattern.len()
}

#[cfg(test)]
mod wildcard_tests {
    use super::*;

    fn spec(pattern: &str) -> FilterSpec {
        FilterSpec {
            column: "c".to_string(),
            op: Op::Wildcard,
            value: pattern.to_string(),
        }
    }

    #[test]
    fn spreadsheet_wildcards_match_case_insensitively() {
        assert!(matches("CDMX", &spec("*CDM*")));
        assert!(matches("cdmx", &spec("*CDM*")));
        assert!(matches("CDMX", &spec("CD*")));
        assert!(matches("CDMX", &spec("*MX")));
        assert!(matches("CDMX", &spec("????")));
        assert!(matches("CDMX", &spec("CDM?")));
        assert!(!matches("CDMX", &spec("???")));
        assert!(!matches("CDMX", &spec("CDT*")));
        assert!(matches("", &spec("*")));
    }
}
