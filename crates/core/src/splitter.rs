use crate::compress::{finish_compressor, make_compressor};
use crate::csv_reader::{join_csv_line, parse_csv_record, RecordReader};
use crate::decompress::decompress_to_csv;
use crate::encoding::{decode_record, detect, TextEncoding};
use crate::pack::{pack_tar, pack_zip};
use crate::sort::external_sort;
use crate::split_parallel::CHECKPOINT_INTERVAL;
use crate::tempdir::TempDir;
use crate::types::*;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::thread;

/// Per-chunk write buffer when the caller asks for `buf_mb` total.
/// The old code gave *every* chunk its own 64 MB buffer, so `--splits 10` on a
/// 64 MB request actually allocated ~700 MB. Buffers are now split from one
/// budget and capped, which keeps memory flat as the split count grows.
fn budget_per_chunk(total: usize, chunks: usize) -> usize {
    // Cap at 4 MB: beyond that the syscalls are no longer the bottleneck and the
    // buffers start showing up as resident memory (measured: 205 MB -> 60 MB on
    // a 1 GB / 8-chunk split).
    (total / chunks.max(1)).clamp(64 * 1024, 4 * 1024 * 1024)
}

/// Reader buffer: one big read stream, independent of the chunk count.
fn reader_budget(total: usize) -> usize {
    total.clamp(1024 * 1024, 16 * 1024 * 1024)
}

/// Run the full split pipeline. Blocking — call from background thread.
pub fn run_split(config: SplitConfig, tx: ProgressSender) {
    run_split_threads(config, 1, tx);
}

/// Split with `threads` reader threads. `1` is the sequential path.
pub fn run_split_threads(config: SplitConfig, threads: usize, tx: ProgressSender) {
    if let Err(e) = run_inner(config, threads.max(1), &tx) {
        let _ = tx.send(ProgressEvent::Error(e.to_string()));
    }
}

fn run_inner(config: SplitConfig, threads: usize, tx: &ProgressSender) -> io::Result<()> {
    if config.splits == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "splits must be >= 1",
        ));
    }
    let buf_size = config.buf_mb.clamp(1, 4096) * 1024 * 1024;
    let tmp_dir = TempDir::new("job")?;

    // Step 0: Decompress if needed
    let csv_path = decompress_to_csv(&config.input, tmp_dir.path(), tx)?;
    tx.send(ProgressEvent::DecompressDone {
        csv_path: csv_path.clone(),
    })
    .ok();

    // Step 0.5: Sort if requested
    let work_path = if let Some(ref sort_col) = config.sort_column {
        let sorted = tmp_dir.path().join("_sorted.csv");
        external_sort(
            &csv_path,
            &sorted,
            sort_col,
            config.sort_descending,
            config.delimiter,
            config.buf_mb,
            tx,
        )?;
        sorted
    } else {
        csv_path
    };

    let encoding = detect(&work_path)?;
    if !encoding.is_ascii_compatible() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is {}; convert it to UTF-8 first (GBK and UTF-8 are supported)",
                config.input.display(),
                encoding.label()
            ),
        ));
    }

    // Load column filter
    let desired_cols = match &config.columns_file {
        Some(path) => {
            let content = fs::read_to_string(path)?;
            let set: HashSet<String> = content
                .lines()
                .map(|l| crate::csv_reader::strip_bom(l.trim()).to_string())
                .filter(|l| !l.is_empty())
                .collect();
            tx.send(ProgressEvent::Info(format!(
                "Column filter: {} desired columns",
                set.len()
            )))
            .ok();
            Some(set)
        }
        None => None,
    };

    // Phase 1: Count records (quote-aware: a record may span physical lines)
    let file_size = fs::metadata(&work_path)?.len();
    tx.send(ProgressEvent::PhaseStart {
        phase: "Counting lines".into(),
        total_bytes: file_size,
    })
    .ok();
    let scan = scan_records(&work_path, buf_size, file_size, tx)?;
    let data_records = scan.records.saturating_sub(1);
    let data_bytes = scan.data_bytes;
    tx.send(ProgressEvent::PhaseDone {
        phase: "Counting lines".into(),
    })
    .ok();
    tx.send(ProgressEvent::Info(format!(
        "{data_records} data records, {:.1} MB of CSV",
        data_bytes as f64 / (1024.0 * 1024.0)
    )))
    .ok();

    // A file with only a header (or nothing at all) still deserves a
    // round-trippable result: one chunk holding the header, so `merge` restores
    // the original bytes. Failing here would make an empty export look broken.
    if data_records == 0 {
        let stem = config
            .input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "output".to_string());
        return write_header_only(&work_path, tmp_dir.path(), &stem, &config, tx);
    }

    let plan = SplitPlan::new(config.splits, config.max_bytes, data_records, data_bytes);
    let splits = plan.chunks;
    tx.send(ProgressEvent::Info(format!(
        "{} chunk(s), {}",
        splits,
        plan.describe()
    )))
    .ok();

    // Phase 2: Split + filter + compress
    let stem = config
        .input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".to_string());
    tx.send(ProgressEvent::PhaseStart {
        phase: "Splitting & compressing".into(),
        total_bytes: file_size,
    })
    .ok();

    let compressed = if threads > 1 {
        split_filter_compress_parallel(
            &work_path,
            tmp_dir.path(),
            &stem,
            splits,
            data_records,
            &plan,
            &desired_cols,
            encoding,
            config.compress_format,
            config.compress_level,
            config.delimiter,
            buf_size,
            threads,
            &scan.checkpoints,
            tx,
        )?
    } else {
        split_filter_compress(
            &work_path,
            tmp_dir.path(),
            &stem,
            splits,
            data_records,
            &plan,
            &desired_cols,
            encoding,
            config.compress_format,
            config.compress_level,
            config.delimiter,
            buf_size,
            tx,
        )?
    };
    tx.send(ProgressEvent::PhaseDone {
        phase: "Splitting & compressing".into(),
    })
    .ok();

    // Phase 3: pack the result
    let output_path = config.output.clone().unwrap_or_else(|| {
        let ext = match config.compress_format {
            CompressFormat::Zip => "zip",
            _ => "tar",
        };
        config.input.with_file_name(format!("{stem}_split.{ext}"))
    });

    // A single uncompressed chunk is just the CSV itself — no point wrapping it.
    if splits == 1 && config.compress_format == CompressFormat::None {
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::rename(&compressed[0], &output_path).or_else(|_| {
            fs::copy(&compressed[0], &output_path)?;
            Ok::<(), io::Error>(())
        })?;
        tx.send(ProgressEvent::Finished {
            output: output_path,
        })
        .ok();
        return Ok(());
    }

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let total: u64 = compressed
        .iter()
        .map(|f| fs::metadata(f).map(|m| m.len()).unwrap_or(0))
        .sum();

    if config.compress_format == CompressFormat::Zip {
        tx.send(ProgressEvent::PhaseStart {
            phase: "Packing zip".into(),
            total_bytes: total,
        })
        .ok();
        pack_zip(&compressed, &output_path, buf_size)?;
        tx.send(ProgressEvent::PhaseDone {
            phase: "Packing zip".into(),
        })
        .ok();
    } else {
        tx.send(ProgressEvent::PhaseStart {
            phase: "Packing tar".into(),
            total_bytes: total,
        })
        .ok();
        pack_tar(&compressed, &output_path, buf_size, tx)?;
        tx.send(ProgressEvent::PhaseDone {
            phase: "Packing tar".into(),
        })
        .ok();
    }

    tx.send(ProgressEvent::Finished {
        output: output_path,
    })
    .ok();
    Ok(())
}

/// What a single pass over the file learned about it.
#[derive(Clone, Debug, Default)]
struct Scan {
    /// Logical records, header included.
    records: u64,
    /// Bytes of data records (header excluded).
    data_bytes: u64,
    /// Absolute offset of every CHECKPOINT_INTERVAL-th data record. These are
    /// known-safe seek targets because the scanner was outside quotes there.
    checkpoints: Vec<u64>,
}

fn scan_records(
    path: &Path,
    buf_size: usize,
    file_size: u64,
    tx: &ProgressSender,
) -> io::Result<Scan> {
    // Count through a quote-aware scanner: a record with an embedded newline
    // counts once, and a file without a trailing newline does not lose its last
    // record. The byte total is what `max_bytes` planning needs.
    let buf = reader_budget(buf_size);
    let mut reader = RecordReader::new(BufReader::with_capacity(buf, File::open(path)?), buf);
    let mut scratch = Vec::with_capacity(256 * 1024);
    let mut scan = Scan::default();
    let mut header = true;
    let mut bytes: u64 = 0;
    // Absolute position of the record we are about to read.
    let mut pos: u64 = 0;
    while reader.next_record(&mut scratch)? {
        if header {
            header = false;
            pos += scratch.len() as u64;
            continue;
        }
        if scan.records.is_multiple_of(CHECKPOINT_INTERVAL) {
            scan.checkpoints.push(pos);
        }
        scan.records += 1;
        scan.data_bytes += scratch.len() as u64;
        bytes += scratch.len() as u64;
        pos += scratch.len() as u64;
        if bytes % (1 << 20) < scratch.len() as u64 {
            tx.send(ProgressEvent::Progress { bytes }).ok();
        }
    }
    scan.records += 1; // header
    tx.send(ProgressEvent::Progress { bytes: file_size }).ok();
    Ok(scan)
}

/// Emit a single chunk containing just the header (or nothing, for a 0-byte input).
///
/// `merge` concatenates chunks and drops repeated headers, so a header-only
/// archive round-trips back to the original file — which is what makes an empty
/// export a *result*, not an error.
fn write_header_only(
    input: &Path,
    tmp_dir: &Path,
    stem: &str,
    config: &SplitConfig,
    tx: &ProgressSender,
) -> io::Result<()> {
    let ext = match config.compress_format {
        CompressFormat::Zip | CompressFormat::None => "csv".to_string(),
        other => format!("csv{}", other.extension()),
    };
    let path = tmp_dir.join(format!("{stem}_0001.{ext}"));
    let header = {
        let buf = 64 * 1024;
        let mut reader = RecordReader::new(BufReader::with_capacity(buf, File::open(input)?), buf);
        let mut scratch = Vec::with_capacity(4096);
        if reader.next_record(&mut scratch)? {
            scratch
        } else {
            Vec::new()
        }
    };

    let mut writer: Box<dyn Write + Send> = match config.compress_format {
        CompressFormat::Zip | CompressFormat::None => {
            Box::new(BufWriter::with_capacity(64 * 1024, File::create(&path)?))
        }
        other => crate::compress::make_compressor(&path, other, config.compress_level, 64 * 1024)?,
    };
    if !header.is_empty() {
        writer.write_all(&header)?;
        if !header.ends_with(b"\n") {
            writer.write_all(b"\n")?;
        }
    }
    crate::compress::finish_compressor(writer)?;

    let output = config
        .output
        .clone()
        .unwrap_or_else(|| input.with_file_name(format!("{stem}.tar")));
    crate::pack::pack(
        std::slice::from_ref(&path),
        &output,
        config.compress_format == CompressFormat::Zip,
        4 * 1024 * 1024,
        tx,
    )?;
    tx.send(ProgressEvent::Finished { output }).ok();
    Ok(())
}

/// How the file is carved up: either a fixed chunk count or a target size.
#[derive(Clone, Copy, Debug)]
struct SplitPlan {
    chunks: usize,
    mode: PlanMode,
}

#[derive(Clone, Copy, Debug)]
enum PlanMode {
    /// Exact record counts per chunk (front-loaded remainder).
    Records,
    /// Start a new chunk when this many bytes have been written.
    Bytes(u64),
}

impl SplitPlan {
    fn new(splits: usize, max_bytes: Option<u64>, data_records: u64, data_bytes: u64) -> Self {
        // Never create more chunks than records: more chunks means empty files.
        let ceiling = data_records.max(1) as usize;
        match max_bytes.filter(|b| *b > 0) {
            Some(target) => {
                let chunks = (data_bytes.div_ceil(target).max(1) as usize).min(ceiling);
                SplitPlan {
                    chunks,
                    mode: PlanMode::Bytes(data_bytes.div_ceil(chunks as u64).max(1)),
                }
            }
            None => SplitPlan {
                chunks: (splits as u64).min(data_records.max(1) as u64).max(1) as usize,
                mode: PlanMode::Records,
            },
        }
    }

    fn describe(&self) -> String {
        match self.mode {
            PlanMode::Records => "split by record count".to_string(),
            PlanMode::Bytes(b) => format!(
                "split by size, target {:.1} MB each",
                b as f64 / (1024.0 * 1024.0)
            ),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn split_filter_compress(
    input: &Path,
    tmp_dir: &Path,
    stem: &str,
    splits: usize,
    data_records: u64,
    plan: &SplitPlan,
    desired_cols: &Option<HashSet<String>>,
    encoding: TextEncoding,
    format: CompressFormat,
    level: i32,
    delimiter: Delimiter,
    buf_size: usize,
    tx: &ProgressSender,
) -> io::Result<Vec<PathBuf>> {
    let delim = delimiter.byte();
    let read_buf = reader_budget(buf_size);
    let mut reader = RecordReader::new(
        BufReader::with_capacity(read_buf, File::open(input)?),
        read_buf,
    );

    // Header
    let mut record = Vec::with_capacity(64 * 1024);
    if !reader.next_record(&mut record)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Empty file"));
    }
    let mut header_text = decode_record(&record, encoding);
    if header_text.ends_with('\n') {
        header_text.pop();
    }
    if header_text.ends_with('\r') {
        header_text.pop();
    }
    let header_fields = parse_csv_record(&header_text, delim);

    let (col_indices, new_header) = match desired_cols {
        Some(desired) => {
            let indices: Vec<usize> = header_fields
                .iter()
                .enumerate()
                .filter(|(_, name)| desired.contains(name.as_str()))
                .map(|(i, _)| i)
                .collect();
            tx.send(ProgressEvent::Info(format!(
                "Matched {}/{} columns",
                indices.len(),
                header_fields.len()
            )))
            .ok();
            if indices.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "No matching columns",
                ));
            }
            let names: Vec<&str> = indices.iter().map(|&i| header_fields[i].as_str()).collect();
            let hdr = join_csv_line(&names, delim);
            (Some(indices), hdr.into_bytes())
        }
        None => {
            // Passthrough keeps the original header bytes (BOM, quoting, CRLF).
            let mut raw = record.clone();
            if !raw.ends_with(b"\n") {
                raw.push(b'\n');
            }
            (None, raw)
        }
    };

    // Splits are compressed as raw CSV chunks; a Zip container is built later
    // from those files (the previous code wrote raw bytes into a `.csv.zip`).
    let ext = match format {
        CompressFormat::Zip | CompressFormat::None => "csv".to_string(),
        other => format!("csv{}", other.extension()),
    };

    let per_chunk_buf = budget_per_chunk(buf_size, splits);
    let targets = split_targets(data_records, splits);

    let mut senders = Vec::with_capacity(splits);
    let mut handles = Vec::with_capacity(splits);

    for i in 0..splits {
        let (stx, srx) = std::sync::mpsc::sync_channel::<Vec<u8>>(4096);
        senders.push(stx);

        let out_path = tmp_dir.join(format!("{stem}_{:04}.{ext}", i + 1));
        let hdr = new_header.clone();
        let fmt = format;
        let lvl = level;
        let capacity = per_chunk_buf;

        handles.push(thread::spawn(move || -> io::Result<(PathBuf, u64)> {
            let mut writer: Box<dyn Write + Send> = match fmt {
                CompressFormat::Zip | CompressFormat::None => {
                    Box::new(BufWriter::with_capacity(capacity, File::create(&out_path)?))
                }
                other => make_compressor(&out_path, other, lvl, capacity)?,
            };
            writer.write_all(&hdr)?;
            let mut written = 0u64;
            for data in srx {
                writer.write_all(&data)?;
                written += 1;
            }
            finish_compressor(writer)?;
            Ok((out_path, written))
        }));
    }

    // Read + filter + distribute
    let mut bytes_read = record.len() as u64;
    let mut split_idx: usize = 0;
    let mut used_records: u64 = 0;
    let mut used_bytes: u64 = 0;
    let mut total_written: u64 = 0;

    while reader.next_record(&mut record)? {
        bytes_read += record.len() as u64;

        let data = match &col_indices {
            Some(indices) => {
                // Column filtering has to understand the bytes; the output is
                // normalised to UTF-8 because the header is re-rendered too.
                let text = decode_record(&record, encoding);
                let fields = parse_csv_record(&text, delim);
                let selected: Vec<&str> = indices
                    .iter()
                    .filter_map(|&i| fields.get(i).map(|s| s.as_str()))
                    .collect();
                join_csv_line(&selected, delim).into_bytes()
            }
            None => record.clone(),
        };

        let data_len = data.len() as u64;
        if senders[split_idx].send(data).is_err() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "compressor died"));
        }

        total_written += 1;
        used_records += 1;
        used_bytes += data_len;

        if split_idx + 1 < splits {
            let reached = match plan.mode {
                PlanMode::Records => used_records >= targets[split_idx],
                PlanMode::Bytes(quota) => used_bytes >= quota,
            };
            // Leave at least one record for each remaining chunk.
            let remaining = data_records.saturating_sub(total_written);
            let chunks_left = (splits - split_idx) as u64;
            let must_advance = remaining < chunks_left;
            if reached || must_advance {
                split_idx += 1;
                used_records = 0;
                used_bytes = 0;
            }
        }

        if bytes_read % (1 << 20) < record.len() as u64 {
            tx.send(ProgressEvent::Progress { bytes: bytes_read }).ok();
        }
    }

    drop(senders);

    let mut out_paths = Vec::with_capacity(splits);
    let mut written_total = 0u64;
    for h in handles {
        let (p, written) = h
            .join()
            .map_err(|_| io::Error::other("thread panicked"))??;
        if written == 0 {
            return Err(io::Error::other(format!(
                "chunk {p:?} is empty; refusing to emit a header-only file"
            )));
        }
        written_total += written;
        out_paths.push(p);
    }
    if written_total != data_records {
        return Err(io::Error::other(format!(
            "record mismatch: scanned {data_records}, wrote {written_total}"
        )));
    }

    Ok(out_paths)
}

/// Distribute `total` records over `parts` chunks, front-loading the remainder so
/// the difference between the largest and smallest chunk is exactly one record.
/// Parallel variant of [`split_filter_compress`]: one reader per group of chunks.
#[allow(clippy::too_many_arguments)]
fn split_filter_compress_parallel(
    input: &Path,
    tmp_dir: &Path,
    stem: &str,
    splits: usize,
    data_records: u64,
    _plan: &SplitPlan,
    desired_cols: &Option<HashSet<String>>,
    encoding: TextEncoding,
    format: CompressFormat,
    level: i32,
    delimiter: Delimiter,
    buf_size: usize,
    threads: usize,
    checkpoints: &[u64],
    tx: &ProgressSender,
) -> io::Result<Vec<PathBuf>> {
    let delim = delimiter.byte();
    let (col_indices, header) = read_header_and_indices(input, desired_cols, delim, encoding, tx)?;
    let targets = split_targets(data_records, splits);
    let jobs = crate::split_parallel::plan_readers(&targets, threads, checkpoints);

    let mut handles = Vec::with_capacity(jobs.len());
    for job in jobs {
        let sender = tx.clone();
        let opts = crate::split_parallel::ReaderOptions {
            input: input.to_path_buf(),
            tmp_dir: tmp_dir.to_path_buf(),
            stem: stem.to_string(),
            header: header.clone(),
            col_indices: col_indices.clone(),
            encoding,
            format,
            level,
            delimiter,
            buf_size,
        };
        handles.push(thread::spawn(move || {
            let first = job.chunks.first().map(|c| c.index).unwrap_or(0);
            crate::split_parallel::run_reader(&job, &opts, &sender).map(|paths| (first, paths))
        }));
    }

    let mut ordered: Vec<(usize, Vec<PathBuf>)> = Vec::with_capacity(handles.len());
    for h in handles {
        ordered.push(
            h.join()
                .map_err(|_| io::Error::other("reader panicked"))??,
        );
    }
    ordered.sort_by_key(|(first, _)| *first);

    let mut out = Vec::with_capacity(splits);
    for (_, paths) in ordered {
        out.extend(paths);
    }
    if out.len() != splits {
        return Err(io::Error::other(format!(
            "produced {} chunks, expected {splits}",
            out.len()
        )));
    }
    Ok(out)
}

/// Header bytes (as written to every chunk) plus the column indices to keep.
fn read_header_and_indices(
    input: &Path,
    desired_cols: &Option<HashSet<String>>,
    delim: u8,
    encoding: TextEncoding,
    tx: &ProgressSender,
) -> io::Result<(Option<Vec<usize>>, Vec<u8>)> {
    let buf = 4 * 1024 * 1024;
    let mut reader = RecordReader::new(BufReader::with_capacity(buf, File::open(input)?), buf);
    let mut record = Vec::with_capacity(64 * 1024);
    if !reader.next_record(&mut record)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Empty file"));
    }
    let mut header_text = decode_record(&record, encoding);
    while header_text.ends_with(['\n', '\r']) {
        header_text.pop();
    }
    let header_fields = parse_csv_record(&header_text, delim);

    match desired_cols {
        Some(desired) => {
            let indices: Vec<usize> = header_fields
                .iter()
                .enumerate()
                .filter(|(_, name)| desired.contains(name.as_str()))
                .map(|(i, _)| i)
                .collect();
            tx.send(ProgressEvent::Info(format!(
                "Matched {}/{} columns",
                indices.len(),
                header_fields.len()
            )))
            .ok();
            if indices.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "No matching columns",
                ));
            }
            let names: Vec<&str> = indices.iter().map(|&i| header_fields[i].as_str()).collect();
            Ok((Some(indices), join_csv_line(&names, delim).into_bytes()))
        }
        None => {
            let mut raw = record.clone();
            if !raw.ends_with(b"\n") {
                raw.push(b'\n');
            }
            Ok((None, raw))
        }
    }
}

fn split_targets(total: u64, parts: usize) -> Vec<u64> {
    let parts = parts.max(1) as u64;
    let base = total / parts;
    let rem = total % parts;
    (0..parts).map(|i| base + u64::from(i < rem)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_are_balanced() {
        assert_eq!(split_targets(10, 3), vec![4, 3, 3]);
        assert_eq!(split_targets(9, 3), vec![3, 3, 3]);
        assert_eq!(split_targets(1, 1), vec![1]);
        assert_eq!(split_targets(5, 5), vec![1, 1, 1, 1, 1]);
    }

    #[test]
    fn plan_from_record_count_never_exceeds_records() {
        let p = SplitPlan::new(10, None, 3, 1_000);
        assert_eq!(p.chunks, 3);
        assert!(matches!(p.mode, PlanMode::Records));

        let p = SplitPlan::new(10, None, 100, 1_000);
        assert_eq!(p.chunks, 10);
    }

    #[test]
    fn plan_from_size_derives_chunk_count() {
        let mb = 1024 * 1024;
        // 10 MB of CSV, 3 MB target -> 4 chunks of ~2.5 MB
        let p = SplitPlan::new(10, Some(3 * mb), 10_000, 10 * mb);
        assert_eq!(p.chunks, 4);
        match p.mode {
            PlanMode::Bytes(quota) => assert_eq!(quota, (10 * mb).div_ceil(4)),
            PlanMode::Records => panic!("expected byte quota"),
        }

        // Target larger than the file -> a single chunk.
        let p = SplitPlan::new(10, Some(100 * mb), 10, mb);
        assert_eq!(p.chunks, 1);

        // Never more chunks than records, even if the target is tiny.
        let p = SplitPlan::new(10, Some(1), 3, 10_000);
        assert_eq!(p.chunks, 3);
    }

    #[test]
    fn buffer_budget_does_not_multiply() {
        let total = 64 * 1024 * 1024;
        // Capped at 4 MB per chunk regardless of the shared budget.
        assert_eq!(budget_per_chunk(total, 1), 4 * 1024 * 1024);
        assert_eq!(budget_per_chunk(total, 16), 4 * 1024 * 1024);
        assert_eq!(budget_per_chunk(8 * 1024 * 1024, 8), 1024 * 1024);
        assert_eq!(budget_per_chunk(1024, 16), 64 * 1024);
        // The reader gets its own capped budget, independent of chunk count.
        assert_eq!(reader_budget(64 * 1024 * 1024), 16 * 1024 * 1024);
        assert_eq!(reader_budget(1024), 1024 * 1024);
    }

    #[test]
    fn counts_newlines_inside_quotes_as_one_record() {
        let dir = std::env::temp_dir().join(format!("csvs_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.csv");
        std::fs::write(&p, "h\n1\n\"2\n3\"\n4\n").unwrap();

        let (tx, _rx) = std::sync::mpsc::channel();
        let n = scan_records(&p, 8 * 1024, 0, &tx).unwrap();
        assert_eq!(n.records, 4);
        assert!(n.data_bytes > 0);
        std::fs::remove_dir_all(&dir).ok();
    }
}
