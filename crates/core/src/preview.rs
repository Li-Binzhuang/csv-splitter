use crate::csv_reader::{detect_delimiter, parse_csv_line};
use crate::types::*;
use rand::Rng;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::Path;

/// Load CSV preview: headers + first N rows, auto-detect delimiter.
pub fn load_preview(
    path: &Path,
    max_rows: usize,
    delimiter_override: Option<Delimiter>,
) -> io::Result<CsvPreview> {
    let file_size = fs::metadata(path)?.len();

    // Detect delimiter
    let mut detect_file = File::open(path)?;
    let (detected, _) = detect_delimiter(&mut detect_file)?;
    drop(detect_file);

    let delim = delimiter_override.unwrap_or(detected);
    let delim_byte = delim.byte();

    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(8 * 1024 * 1024, file);

    // Read header
    let mut header_line = String::new();
    reader.read_line(&mut header_line)?;
    let headers = parse_csv_line(&header_line, delim_byte);

    // Read preview rows
    let mut rows = Vec::with_capacity(max_rows);
    let mut line = String::new();
    for _ in 0..max_rows {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        rows.push(parse_csv_line(&line, delim_byte));
    }

    // Estimate total rows from file size and average line length
    let total_rows_estimate = if !rows.is_empty() {
        let bytes_so_far = header_line.len() as u64
            + rows
                .iter()
                .map(|r| r.iter().map(|c| c.len() as u64 + 1).sum::<u64>() + 1)
                .sum::<u64>();
        let avg_line = bytes_so_far as f64 / (rows.len() + 1) as f64;
        if avg_line > 0.0 {
            (file_size as f64 / avg_line).round() as u64
        } else {
            rows.len() as u64
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
    let file_size = fs::metadata(path)?.len();
    tx.send(ProgressEvent::PhaseStart {
        phase: "Computing column stats".into(),
        total_bytes: file_size,
    })
    .ok();

    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(64 * 1024 * 1024, file);
    let delim_byte = delimiter.byte();

    // Read header
    let mut header_line = String::new();
    reader.read_line(&mut header_line)?;
    let headers = parse_csv_line(&header_line, delim_byte);
    let num_cols = headers.len();

    // Reservoir sampling: read all lines but only keep `sample_size` random ones
    let mut rng = rand::thread_rng();
    let mut sampled_rows: Vec<Vec<String>> = Vec::with_capacity(sample_size);
    let mut total_rows: u64 = 0;
    let mut bytes_read = header_line.len() as u64;

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        bytes_read += n as u64;
        total_rows += 1;

        if sampled_rows.len() < sample_size {
            sampled_rows.push(parse_csv_line(&line, delim_byte));
        } else {
            // Reservoir sampling: replace with probability sample_size/total_rows
            let j = rng.gen_range(0..total_rows as usize);
            if j < sample_size {
                sampled_rows[j] = parse_csv_line(&line, delim_byte);
            }
        }

        if bytes_read % (1 << 22) < n as u64 {
            tx.send(ProgressEvent::Progress { bytes: bytes_read }).ok();
        }
    }

    tx.send(ProgressEvent::Progress { bytes: file_size }).ok();

    // Compute stats per column from sampled rows
    let mut stats: Vec<ColumnStats> = Vec::with_capacity(num_cols);
    for (col_idx, col_name) in headers.iter().enumerate() {
        let mut freq: HashMap<String, u64> = HashMap::new();
        let mut empty_count: u64 = 0;

        for row in &sampled_rows {
            let val = row.get(col_idx).map(|s| s.as_str()).unwrap_or("");
            if val.is_empty() {
                empty_count += 1;
            } else {
                // Limit unique tracking to avoid memory blow-up
                if freq.len() < max_unique_per_col || freq.contains_key(val) {
                    *freq.entry(val.to_string()).or_insert(0) += 1;
                }
            }
        }

        let unique_count = freq.len() as u64;
        let mut top_values: Vec<(String, u64)> = freq.into_iter().collect();
        top_values.sort_by(|a, b| b.1.cmp(&a.1));
        top_values.truncate(100); // Keep top 100

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
