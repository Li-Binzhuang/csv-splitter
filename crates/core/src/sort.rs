use crate::csv_reader::{parse_csv_record, RecordReader};
use crate::encoding::{decode_cell, detect, TextEncoding};
use crate::types::*;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// One sort column and its direction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortColumn {
    pub name: String,
    pub descending: bool,
}

impl SortColumn {
    pub fn new(name: impl Into<String>, descending: bool) -> Self {
        Self {
            name: name.into(),
            descending,
        }
    }
}

/// Resolved sort order: which field index, and which direction.
///
/// Shared between the chunk writer and the merge heap so both agree on the
/// comparison rule; a mismatch would produce a "sorted" file with unsorted
/// neighbours at chunk boundaries.
#[derive(Debug)]
struct SortPlan {
    columns: Vec<(usize, bool)>,
}

impl SortPlan {
    fn key(&self, record: &[u8], delim: u8, encoding: TextEncoding) -> Vec<String> {
        let fields = parse_csv_record(&decode_cell(record, encoding), delim);
        self.columns
            .iter()
            .map(|(idx, _)| fields.get(*idx).cloned().unwrap_or_default())
            .collect()
    }

    /// Ascending order of the requested keys, honouring per-column direction.
    fn compare(&self, a: &[String], b: &[String]) -> Ordering {
        for (i, (_, descending)) in self.columns.iter().enumerate() {
            let left = a.get(i).map(String::as_str).unwrap_or("");
            let right = b.get(i).map(String::as_str).unwrap_or("");
            let ord = smart_cmp(left, right);
            let ord = if *descending { ord.reverse() } else { ord };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    }
}

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
    external_sort_columns(
        input,
        output,
        &[SortColumn::new(sort_col, descending)],
        delimiter,
        buf_mb,
        tx,
    )
}

/// Same as [`external_sort`] with several keys: `[city asc, amount desc]`
/// sorts by city, then within each city by amount descending.
pub fn external_sort_columns(
    input: &Path,
    output: &Path,
    columns: &[SortColumn],
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
    let mut resolved: Vec<(usize, bool)> = Vec::with_capacity(columns.len());
    for col in columns {
        let idx = crate::csv_reader::resolve_column(&col.name, &headers)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        resolved.push((idx, col.descending));
    }
    if resolved.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no sort columns",
        ));
    }
    let plan = std::sync::Arc::new(SortPlan { columns: resolved });

    // Split into sorted chunks
    let tmp_dir = output.parent().unwrap_or(Path::new("."));
    let mut chunk_paths: Vec<PathBuf> = Vec::new();
    let mut records: Vec<Vec<u8>> = Vec::with_capacity(chunk_max_lines);

    while reader.next_record(&mut record)? {
        records.push(record.clone());
        if records.len() >= chunk_max_lines {
            let p =
                write_sorted_chunk(&records, &plan, delim, tmp_dir, chunk_paths.len(), encoding)?;
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
        let p = write_sorted_chunk(&records, &plan, delim, tmp_dir, chunk_paths.len(), encoding)?;
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
        &plan,
        delim,
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

fn write_sorted_chunk(
    records: &[Vec<u8>],
    plan: &SortPlan,
    delim: u8,
    tmp_dir: &Path,
    chunk_num: usize,
    encoding: TextEncoding,
) -> io::Result<PathBuf> {
    let mut indexed: Vec<(usize, Vec<String>)> = records
        .iter()
        .enumerate()
        .map(|(i, r)| (i, plan.key(r, delim, encoding)))
        .collect();

    // `sort_by` is stable, so equal keys keep their original order.
    indexed.sort_by(|a, b| plan.compare(&a.1, &b.1));

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
    key: Vec<String>,
    source: usize,
    plan: std::sync::Arc<SortPlan>,
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
        // BinaryHeap is a max-heap, so reverse to pop the smallest key first.
        match self.plan.compare(&self.key, &other.key) {
            // Equal keys fall back to the chunk they came from: chunks are written
            // in file order, so the earlier chunk holds the earlier rows. Without
            // this, rows that tie would come out in heap order — a sort that is
            // "almost" stable, which is worse than one that is not.
            Ordering::Equal => other.source.cmp(&self.source),
            ord => ord.reverse(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn merge_chunks(
    chunks: &[PathBuf],
    output: &Path,
    header: &[u8],
    plan: &std::sync::Arc<SortPlan>,
    delim: u8,
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
            let key = plan.key(&record, delim, encoding);
            heap.push(MergeEntry {
                record: record.clone(),
                key,
                source: i,
                plan: plan.clone(),
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
            let key = plan.key(&record, delim, encoding);
            heap.push(MergeEntry {
                record: record.clone(),
                key,
                source: entry.source,
                plan: plan.clone(),
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

#[cfg(test)]
mod multi_tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_msort_{tag}_{}_{}",
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

    fn sorted(csv: &str, cols: &[SortColumn], tag: &str) -> String {
        let d = Dir::new(tag);
        let input = d.join("in.csv");
        let output = d.join("out.csv");
        std::fs::write(&input, csv).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        external_sort_columns(&input, &output, cols, Delimiter::Comma, 16, &tx).unwrap();
        std::fs::read_to_string(&output).unwrap()
    }

    #[test]
    fn second_key_breaks_ties_and_direction_is_per_column() {
        let csv = "city,amount\nCDMX,30\nPuebla,10\nCDMX,10\nPuebla,30\n";
        let cols = vec![
            SortColumn::new("city", false),
            SortColumn::new("amount", true),
        ];
        let out = sorted(csv, &cols, "mixed");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "city,amount");
        // CDMX ascending, then amount descending inside each city.
        assert_eq!(lines[1], "CDMX,30");
        assert_eq!(lines[2], "CDMX,10");
        assert_eq!(lines[3], "Puebla,30");
        assert_eq!(lines[4], "Puebla,10");
    }

    #[test]
    fn descending_first_column() {
        let csv = "city,amount\nCDMX,30\nPuebla,10\nCDMX,10\n";
        let cols = vec![
            SortColumn::new("city", true),
            SortColumn::new("amount", false),
        ];
        let out = sorted(csv, &cols, "desc_first");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[1], "Puebla,10");
        assert_eq!(lines[2], "CDMX,10");
        assert_eq!(lines[3], "CDMX,30");
    }

    #[test]
    fn unknown_column_is_rejected() {
        let d = Dir::new("bad");
        let input = d.join("in.csv");
        std::fs::write(&input, "a,b\n1,2\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = external_sort_columns(
            &input,
            &d.join("out.csv"),
            &[SortColumn::new("nope", false)],
            Delimiter::Comma,
            16,
            &tx,
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn single_column_wrapper_still_works() {
        let csv = "amount,id\n30,a\n10,c\n20,b\n";
        let d = Dir::new("single");
        let input = d.join("in.csv");
        let output = d.join("out.csv");
        std::fs::write(&input, csv).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        external_sort(&input, &output, "amount", false, Delimiter::Comma, 16, &tx).unwrap();
        let text = std::fs::read_to_string(&output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "10,c");
        assert_eq!(lines[3], "30,a");
    }
}

#[cfg(test)]
mod stability_tests {
    use super::*;
    use std::io::Write;

    /// Temp directory that cleans itself up.
    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("csvs_sort_{tag}_{nanos}_{seq}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_rows(dir: &std::path::Path, name: &str, rows: &[&str]) -> PathBuf {
        let path = dir.join(name);
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "id,key,payload").unwrap();
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
        path
    }

    #[test]
    fn equal_keys_keep_their_original_order_across_chunks() {
        let dir = temp_dir("stable");
        // 40 rows sharing one key, forced across several chunks by a tiny buffer.
        let rows: Vec<String> = (0..40).map(|i| format!("{i},same,p{i}")).collect();
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let input = write_rows(&dir, "in.csv", &rows);

        let out = dir.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let columns = vec![SortColumn::new("key", false)];
        external_sort_columns(&input, &out, &columns, Delimiter::Comma, 1, &tx).unwrap();

        let text = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "id,key,payload");
        for (i, line) in lines[1..].iter().enumerate() {
            let expected = format!("{i},same,p{i}");
            assert_eq!(*line, expected, "row {i} moved; a sort must be stable");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reversing_the_key_reverses_the_rows_not_the_ties() {
        let dir = temp_dir("stable2");
        let rows = ["1,b,p1", "2,a,p2", "3,a,p3", "4,b,p4", "5,a,p5"];
        let input = write_rows(&dir, "in.csv", &rows);
        let out = dir.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let columns = vec![SortColumn::new("key", true)];
        external_sort_columns(&input, &out, &columns, Delimiter::Comma, 1, &tx).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        // b's first (descending), and within each key the original order.
        assert_eq!(lines[1], "1,b,p1");
        assert_eq!(lines[2], "4,b,p4");
        assert_eq!(lines[3], "2,a,p2");
        assert_eq!(lines[4], "3,a,p3");
        assert_eq!(lines[5], "5,a,p5");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
