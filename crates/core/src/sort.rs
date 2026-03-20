use crate::csv_reader::parse_csv_line;
use crate::types::*;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// External merge sort for large CSV files.
/// Splits into sorted chunks, then merges.
pub fn external_sort(
    input: &Path,
    output: &Path,
    sort_col: &str,
    descending: bool,
    delimiter: Delimiter,
    buf_mb: usize,
    tx: &ProgressSender,
) -> io::Result<()> {
    let buf_size = buf_mb * 1024 * 1024;
    let delim = delimiter.byte();
    let chunk_max_lines = 500_000; // ~500K lines per chunk, tune for memory

    tx.send(ProgressEvent::PhaseStart { phase: "Sorting: creating chunks".into(), total_bytes: 0 }).ok();

    let mut reader = BufReader::with_capacity(buf_size, File::open(input)?);

    // Read header
    let mut header_line = String::new();
    reader.read_line(&mut header_line)?;
    let headers = parse_csv_line(&header_line, delim);
    let col_idx = headers.iter().position(|h| h == sort_col)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
            format!("Sort column '{}' not found", sort_col)))?;

    // Split into sorted chunks
    let tmp_dir = output.parent().unwrap_or(Path::new("."));
    let mut chunk_paths: Vec<PathBuf> = Vec::new();
    let mut lines: Vec<String> = Vec::with_capacity(chunk_max_lines);

    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            if !lines.is_empty() {
                let p = write_sorted_chunk(&lines, col_idx, delim, descending, tmp_dir, chunk_paths.len())?;
                chunk_paths.push(p);
            }
            break;
        }
        lines.push(line.clone());
        if lines.len() >= chunk_max_lines {
            let p = write_sorted_chunk(&lines, col_idx, delim, descending, tmp_dir, chunk_paths.len())?;
            chunk_paths.push(p);
            lines.clear();
            tx.send(ProgressEvent::Info(format!("Chunk {} written", chunk_paths.len()))).ok();
        }
    }

    tx.send(ProgressEvent::PhaseDone { phase: "Sorting: creating chunks".into() }).ok();
    tx.send(ProgressEvent::Info(format!("{} sorted chunks created", chunk_paths.len()))).ok();

    // Merge sorted chunks
    tx.send(ProgressEvent::PhaseStart { phase: "Sorting: merging".into(), total_bytes: 0 }).ok();
    merge_chunks(&chunk_paths, output, &header_line, col_idx, delim, descending, buf_size)?;
    tx.send(ProgressEvent::PhaseDone { phase: "Sorting: merging".into() }).ok();

    // Cleanup chunks
    for p in &chunk_paths {
        std::fs::remove_file(p).ok();
    }

    Ok(())
}

fn write_sorted_chunk(
    lines: &[String], col_idx: usize, delim: u8, desc: bool,
    tmp_dir: &Path, chunk_num: usize,
) -> io::Result<PathBuf> {
    let mut indexed: Vec<(usize, String)> = lines.iter().enumerate()
        .map(|(i, l)| {
            let fields = parse_csv_line(l, delim);
            let key = fields.get(col_idx).cloned().unwrap_or_default();
            (i, key)
        })
        .collect();

    indexed.sort_by(|a, b| {
        let ord = smart_cmp(&a.1, &b.1);
        if desc { ord.reverse() } else { ord }
    });

    let path = tmp_dir.join(format!("_sort_chunk_{:04}.tmp", chunk_num));
    let mut writer = BufWriter::new(File::create(&path)?);
    for (idx, _) in &indexed {
        writer.write_all(lines[*idx].as_bytes())?;
    }
    writer.flush()?;
    Ok(path)
}

/// Smart comparison: try numeric first, fall back to string.
fn smart_cmp(a: &str, b: &str) -> Ordering {
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(fa), Ok(fb)) => fa.partial_cmp(&fb).unwrap_or(Ordering::Equal),
        _ => a.cmp(b),
    }
}

struct MergeEntry {
    line: String,
    key: String,
    source: usize,
    descending: bool,
}

impl PartialEq for MergeEntry {
    fn eq(&self, other: &Self) -> bool { self.key == other.key }
}
impl Eq for MergeEntry {}

impl PartialOrd for MergeEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

impl Ord for MergeEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is max-heap, so reverse for ascending
        let ord = smart_cmp(&self.key, &other.key);
        if self.descending { ord } else { ord.reverse() }
    }
}

fn merge_chunks(
    chunks: &[PathBuf], output: &Path, header: &str,
    col_idx: usize, delim: u8, descending: bool, buf_size: usize,
) -> io::Result<()> {
    let mut readers: Vec<BufReader<File>> = chunks.iter()
        .map(|p| Ok(BufReader::with_capacity(buf_size / chunks.len().max(1), File::open(p)?)))
        .collect::<io::Result<Vec<_>>>()?;

    let mut heap = BinaryHeap::new();

    // Seed heap with first line from each chunk
    for (i, reader) in readers.iter_mut().enumerate() {
        let mut line = String::new();
        if reader.read_line(&mut line)? > 0 {
            let fields = parse_csv_line(&line, delim);
            let key = fields.get(col_idx).cloned().unwrap_or_default();
            heap.push(MergeEntry { line, key, source: i, descending });
        }
    }

    let mut writer = BufWriter::with_capacity(buf_size, File::create(output)?);
    writer.write_all(header.as_bytes())?;

    while let Some(entry) = heap.pop() {
        writer.write_all(entry.line.as_bytes())?;

        // Read next line from same source
        let mut line = String::new();
        if readers[entry.source].read_line(&mut line)? > 0 {
            let fields = parse_csv_line(&line, delim);
            let key = fields.get(col_idx).cloned().unwrap_or_default();
            heap.push(MergeEntry { line, key, source: entry.source, descending });
        }
    }

    writer.flush()?;
    Ok(())
}
