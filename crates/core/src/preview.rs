use crate::csv_reader::{
    detect_delimiter, parse_csv_line, parse_csv_record_with, QuoteStyle, RecordReader,
};
use crate::encoding;
use crate::types::*;
use rand::Rng;
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufReader};
use std::path::Path;

/// Load CSV preview: headers + first N rows, auto-detect delimiter.
pub fn load_preview(
    path: &Path,
    max_rows: usize,
    delimiter_override: Option<Delimiter>,
) -> io::Result<CsvPreview> {
    load_preview_with(path, max_rows, delimiter_override, QuoteStyle::Rfc4180)
}

/// Same as [`load_preview`], with an explicit quote style.
pub fn load_preview_with(
    path: &Path,
    max_rows: usize,
    delimiter_override: Option<Delimiter>,
    quotes: QuoteStyle,
) -> io::Result<CsvPreview> {
    let file_size = fs::metadata(path)?.len();

    // Detect delimiter (over the decoded stream, so GBK does not confuse it)
    let mut detect_file = encoding::open_decoded(path)?;
    let (detected, _) = detect_delimiter(&mut detect_file)?;

    let delim = delimiter_override.unwrap_or(detected);
    let delim_byte = delim.byte();

    // Record-aware reading: a quoted field may contain newlines, and a naive
    // `read_line` loop turns one row into two (and shifts every later column).
    let file = encoding::open_decoded(path)?;
    let mut reader = RecordReader::new(
        BufReader::with_capacity(8 * 1024 * 1024, file),
        8 * 1024 * 1024,
    );

    let mut record = Vec::with_capacity(64 * 1024);
    let headers = if reader.next_record(&mut record)? {
        parse_csv_record_with(&String::from_utf8_lossy(&record), delim_byte, quotes)
    } else {
        Vec::new()
    };
    let header_len = record.len() as u64;

    let mut rows = Vec::with_capacity(max_rows);
    let mut bytes = header_len;
    let mut sampled_records: u64 = 0;
    // Read a wider window than we display: an estimate based on the first three
    // short rows is wildly wrong on files that mix short and long records.
    let estimate_window = max_rows.max(2_000);
    while sampled_records < estimate_window as u64 {
        if !reader.next_record(&mut record)? {
            break;
        }
        bytes += record.len() as u64;
        sampled_records += 1;
        if rows.len() < max_rows {
            rows.push(parse_csv_record_with(
                &String::from_utf8_lossy(&record),
                delim_byte,
                quotes,
            ));
        }
    }

    let total_rows_estimate = if sampled_records > 0 && bytes > 0 {
        let avg_record = bytes as f64 / (sampled_records + 1) as f64;
        if avg_record > 0.0 {
            (file_size as f64 / avg_record).round() as u64
        } else {
            sampled_records
        }
    } else {
        0
    };

    Ok(CsvPreview {
        headers,
        rows,
        total_rows_estimate,
        file_size,
        detected_delimiter: delim,
    })
}

/// Compute column statistics by reservoir sampling.
/// Samples up to `sample_size` rows to avoid memory explosion on 100GB files.
/// For each column, tracks top values by frequency.
pub fn compute_column_stats(
    path: &Path,
    delimiter: Delimiter,
    sample_size: usize,
    max_unique_per_col: usize,
    tx: &ProgressSender,
) -> io::Result<Vec<ColumnStats>> {
    compute_column_stats_with(
        path,
        delimiter,
        sample_size,
        max_unique_per_col,
        QuoteStyle::Rfc4180,
        tx,
    )
}

/// Same as [`compute_column_stats`], with an explicit quote style.
pub fn compute_column_stats_with(
    path: &Path,
    delimiter: Delimiter,
    sample_size: usize,
    max_unique_per_col: usize,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<Vec<ColumnStats>> {
    let file_size = fs::metadata(path)?.len();
    tx.send(ProgressEvent::PhaseStart {
        phase: "Computing column stats".into(),
        total_bytes: file_size,
    })
    .ok();

    let file = encoding::open_decoded(path)?;
    let mut reader = RecordReader::new(
        BufReader::with_capacity(8 * 1024 * 1024, file),
        8 * 1024 * 1024,
    );
    let delim_byte = delimiter.byte();

    let mut record = Vec::with_capacity(64 * 1024);
    if !reader.next_record(&mut record)? {
        tx.send(ProgressEvent::PhaseDone {
            phase: "Computing column stats".into(),
        })
        .ok();
        return Ok(Vec::new());
    }
    let headers = parse_csv_record_with(&String::from_utf8_lossy(&record), delim_byte, quotes);
    let num_cols = headers.len();

    // Reservoir sampling: stream every record, keep `sample_size` at random.
    let mut rng = rand::thread_rng();
    let mut sampled_rows: Vec<Vec<String>> = Vec::with_capacity(sample_size);
    let mut total_rows: u64 = 0;
    let mut bytes_read = record.len() as u64;

    while reader.next_record(&mut record)? {
        bytes_read += record.len() as u64;
        total_rows += 1;

        let parsed =
            || parse_csv_record_with(&String::from_utf8_lossy(&record), delim_byte, quotes);
        if sampled_rows.len() < sample_size {
            sampled_rows.push(parsed());
        } else {
            let j = rng.gen_range(0..total_rows as usize);
            if j < sample_size {
                sampled_rows[j] = parsed();
            }
        }

        if bytes_read % (1 << 22) < record.len() as u64 {
            tx.send(ProgressEvent::Progress { bytes: bytes_read }).ok();
        }
    }

    tx.send(ProgressEvent::Progress { bytes: file_size }).ok();

    let mut stats: Vec<ColumnStats> = Vec::with_capacity(num_cols);
    for (col_idx, col_name) in headers.iter().enumerate() {
        let mut freq: HashMap<String, u64> = HashMap::new();
        let mut empty_count: u64 = 0;

        for row in &sampled_rows {
            let val = row.get(col_idx).map(|s| s.as_str()).unwrap_or("");
            if val.is_empty() {
                empty_count += 1;
            } else if freq.len() < max_unique_per_col || freq.contains_key(val) {
                *freq.entry(val.to_string()).or_insert(0) += 1;
            }
        }

        let unique_count = freq.len() as u64;
        let mut top_values: Vec<(String, u64)> = freq.into_iter().collect();
        top_values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        top_values.truncate(100);

        stats.push(ColumnStats {
            name: col_name.clone(),
            total_sampled: sampled_rows.len() as u64,
            top_values,
            unique_count,
            empty_count,
        });
    }

    tx.send(ProgressEvent::PhaseDone {
        phase: "Computing column stats".into(),
    })
    .ok();
    Ok(stats)
}

/// Kept for API compatibility: parse a plain line without record awareness.
pub fn parse_plain_line(line: &str, delimiter: Delimiter) -> Vec<String> {
    parse_csv_line(line, delimiter.byte())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_does_not_shift_columns_on_embedded_newlines() {
        let dir = std::env::temp_dir().join(format!("csvs_prev_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.csv");
        std::fs::write(&p, "id,note\n1,\"a\nb\"\n2,ok\n").unwrap();

        let preview = load_preview(&p, 10, None).unwrap();
        assert_eq!(preview.headers, vec!["id", "note"]);
        assert_eq!(preview.rows.len(), 2);
        assert_eq!(preview.rows[0], vec!["1", "a\nb"]);
        assert_eq!(preview.rows[1], vec!["2", "ok"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stats_count_empty_cells_without_column_shift() {
        let dir = std::env::temp_dir().join(format!("csvs_stats_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.csv");
        std::fs::write(&p, "a,b\n1,\"x\ny\"\n2,\n").unwrap();

        let (tx, _rx) = std::sync::mpsc::channel();
        let stats = compute_column_stats(&p, Delimiter::Comma, 100, 1000, &tx).unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[1].empty_count, 1);
        assert_eq!(stats[0].unique_count, 2);
        std::fs::remove_dir_all(&dir).ok();
    }
}
