use crate::csv_reader::{parse_csv_record, RecordReader};
use crate::encoding::{decode_cell, detect, TextEncoding};
use crate::types::*;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// External merge sort for large CSV files.
///
/// Splits into sorted chunks, then merges. Records are read with
/// [`RecordReader`], so a quoted field containing a newline stays one record —
/// sorting must never reorder half a row.
pub fn external_sort(
    input: &Path,
    output: &Path,
    sort_col: &str,
    descending: bool,
    delimiter: Delimiter,
    buf_mb: usize,
    tx: &ProgressSender,
) -> io::Result<()> {
    let buf_size = buf_mb.clamp(1, 4096) * 1024 * 1024;
    let delim = delimiter.byte();
    let chunk_max_lines = 500_000; // ~500K records per chunk, tune for memory

    tx.send(ProgressEvent::PhaseStart {
        phase: "Sorting: creating chunks".into(),
        total_bytes: 0,
    })
    .ok();

    let encoding = detect(input)?;
    if !encoding.is_ascii_compatible() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} is {}; sorting needs utf-8 or gbk",
                input.display(),
                encoding.label()
            ),
        ));
    }
    let mut reader = RecordReader::new(
        BufReader::with_capacity(buf_size, File::open(input)?),
        buf_size,
    );

    // Read header
    let mut record = Vec::with_capacity(64 * 1024);
    if !reader.next_record(&mut record)? {
        return Ok(());
    }
    let header_line = record.clone();
    let headers = parse_csv_record(&decode_cell(&record, encoding), delim);
    let col_idx = headers.iter().position(|h| h == sort_col).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Sort column '{sort_col}' not found"),
        )
    })?;

    // Split into sorted chunks
    let tmp_dir = output.parent().unwrap_or(Path::new("."));
    let mut chunk_paths: Vec<PathBuf> = Vec::new();
    let mut records: Vec<Vec<u8>> = Vec::with_capacity(chunk_max_lines);

    while reader.next_record(&mut record)? {
        records.push(record.clone());
        if records.len() >= chunk_max_lines {
            let p = write_sorted_chunk(
                &records,
                col_idx,
                delim,
                descending,
                tmp_dir,
                chunk_paths.len(),
                encoding,
            )?;
            chunk_paths.push(p);
            records.clear();
            tx.send(ProgressEvent::Info(format!(
                "Chunk {} written",
                chunk_paths.len()
            )))
            .ok();
        }
    }
    if !records.is_empty() {
        let p = write_sorted_chunk(
            &records,
            col_idx,
            delim,
            descending,
            tmp_dir,
            chunk_paths.len(),
            encoding,
        )?;
        chunk_paths.push(p);
    }

    tx.send(ProgressEvent::PhaseDone {
        phase: "Sorting: creating chunks".into(),
    })
    .ok();
    tx.send(ProgressEvent::Info(format!(
        "{} sorted chunks created",
        chunk_paths.len()
    )))
    .ok();

    // Merge sorted chunks
    tx.send(ProgressEvent::PhaseStart {
        phase: "Sorting: merging".into(),
        total_bytes: 0,
    })
    .ok();
    merge_chunks(
        &chunk_paths,
        output,
        &header_line,
        col_idx,
        delim,
        descending,
        buf_size,
        encoding,
    )?;
    tx.send(ProgressEvent::PhaseDone {
        phase: "Sorting: merging".into(),
    })
    .ok();

    // Cleanup chunks
    for p in &chunk_paths {
        std::fs::remove_file(p).ok();
    }

    Ok(())
}

fn sort_key(record: &[u8], col_idx: usize, delim: u8, encoding: TextEncoding) -> String {
    let fields = parse_csv_record(&decode_cell(record, encoding), delim);
    fields.get(col_idx).cloned().unwrap_or_default()
}

fn write_sorted_chunk(
    records: &[Vec<u8>],
    col_idx: usize,
    delim: u8,
    desc: bool,
    tmp_dir: &Path,
    chunk_num: usize,
    encoding: TextEncoding,
) -> io::Result<PathBuf> {
    let mut indexed: Vec<(usize, String)> = records
        .iter()
        .enumerate()
        .map(|(i, r)| (i, sort_key(r, col_idx, delim, encoding)))
        .collect();

    // `sort_by` is stable, so equal keys keep their original order.
    indexed.sort_by(|a, b| {
        let ord = smart_cmp(&a.1, &b.1);
        if desc {
            ord.reverse()
        } else {
            ord
        }
    });

    let path = tmp_dir.join(format!("_sort_chunk_{chunk_num:04}.tmp"));
    let mut writer = BufWriter::with_capacity(1024 * 1024, File::create(&path)?);
    for (idx, _) in &indexed {
        write_record(&mut writer, &records[*idx])?;
    }
    writer.flush()?;
    Ok(path)
}

/// Write one record and guarantee it ends with a newline, so concatenating two
/// chunks never glues the last row of one to the first row of the next.
fn write_record(w: &mut impl Write, record: &[u8]) -> io::Result<()> {
    w.write_all(record)?;
    if !record.ends_with(b"\n") {
        w.write_all(b"\n")?;
    }
    Ok(())
}

/// Smart comparison: try numeric first, fall back to string.
fn smart_cmp(a: &str, b: &str) -> Ordering {
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(fa), Ok(fb)) => fa.partial_cmp(&fb).unwrap_or(Ordering::Equal),
        _ => a.cmp(b),
    }
}

struct MergeEntry {
    record: Vec<u8>,
    key: String,
    source: usize,
    descending: bool,
}

impl PartialEq for MergeEntry {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl Eq for MergeEntry {}

impl PartialOrd for MergeEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MergeEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap, so reverse for ascending order.
        let ord = smart_cmp(&self.key, &other.key);
        if self.descending {
            ord
        } else {
            ord.reverse()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn merge_chunks(
    chunks: &[PathBuf],
    output: &Path,
    header: &[u8],
    col_idx: usize,
    delim: u8,
    descending: bool,
    buf_size: usize,
    encoding: TextEncoding,
) -> io::Result<()> {
    let per_reader = (buf_size / chunks.len().max(1)).max(64 * 1024);
    let mut readers: Vec<RecordReader<BufReader<File>>> = chunks
        .iter()
        .map(|p| {
            Ok(RecordReader::new(
                BufReader::with_capacity(per_reader, File::open(p)?),
                per_reader,
            ))
        })
        .collect::<io::Result<Vec<_>>>()?;

    let mut heap = BinaryHeap::new();
    let mut record = Vec::with_capacity(64 * 1024);

    // Seed the heap with the first record from each chunk.
    for (i, reader) in readers.iter_mut().enumerate() {
        if reader.next_record(&mut record)? {
            let key = sort_key(&record, col_idx, delim, encoding);
            heap.push(MergeEntry {
                record: record.clone(),
                key,
                source: i,
                descending,
            });
        }
    }

    let mut writer = BufWriter::with_capacity(
        buf_size.clamp(256 * 1024, 8 * 1024 * 1024),
        File::create(output)?,
    );
    writer.write_all(header)?;
    if !header.ends_with(b"\n") {
        writer.write_all(b"\n")?;
    }

    while let Some(entry) = heap.pop() {
        write_record(&mut writer, &entry.record)?;

        if readers[entry.source].next_record(&mut record)? {
            let key = sort_key(&record, col_idx, delim, encoding);
            heap.push(MergeEntry {
                record: record.clone(),
                key,
                source: entry.source,
                descending,
            });
        }
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_sort(input: &str, column: &str, desc: bool) -> String {
        let dir = std::env::temp_dir().join(format!(
            "csvs_sort_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                + unique_seq() as u128
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.csv");
        let dst = dir.join("out.csv");
        std::fs::write(&src, input).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        external_sort(&src, &dst, column, desc, Delimiter::Comma, 1, &tx).unwrap();
        let out = std::fs::read_to_string(&dst).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        out
    }

    /// Per-process unique suffix for temp directories.
    ///
    /// `SystemTime::now()` only has microsecond resolution on macOS, so a dozen
    /// tests starting at once can read the same value and share a directory — one
    /// test's cleanup then deletes the other's files mid-run.
    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn sorts_numeric_column_ascending_and_descending() {
        let src = "id,score\n1,10\n2,9\n3,100\n";
        assert_eq!(
            run_sort(src, "score", false),
            "id,score\n2,9\n1,10\n3,100\n"
        );
        assert_eq!(run_sort(src, "score", true), "id,score\n3,100\n1,10\n2,9\n");
    }

    #[test]
    fn keeps_multiline_records_intact() {
        let src = "id,note\n2,\"b\nline\"\n1,short\n";
        let out = run_sort(src, "id", false);
        assert_eq!(out, "id,note\n1,short\n2,\"b\nline\"\n");
    }

    #[test]
    fn missing_column_is_an_error() {
        let dir = std::env::temp_dir().join(format!("csvs_sort_err_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.csv");
        let dst = dir.join("out.csv");
        std::fs::write(&src, "a,b\n1,2\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = external_sort(&src, &dst, "nope", false, Delimiter::Comma, 1, &tx).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        std::fs::remove_dir_all(&dir).ok();
    }
}
