use crate::compress::{finish_compressor, make_compressor};
use crate::csv_reader::{join_csv_line, parse_csv_line};
use crate::decompress::decompress_to_csv;
use crate::sort::external_sort;
use crate::types::*;
use memchr::memchr_iter;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

/// Run the full split pipeline. Blocking — call from background thread.
pub fn run_split(config: SplitConfig, tx: ProgressSender) {
    if let Err(e) = run_inner(config, &tx) {
        let _ = tx.send(ProgressEvent::Error(e.to_string()));
    }
}

fn run_inner(config: SplitConfig, tx: &ProgressSender) -> io::Result<()> {
    let buf_size = config.buf_mb * 1024 * 1024;
    let tmp_dir = std::env::temp_dir().join(format!("csv_split_{}", std::process::id()));
    fs::create_dir_all(&tmp_dir)?;

    // Step 0: Decompress if needed
    let csv_path = decompress_to_csv(&config.input, &tmp_dir, tx)?;
    tx.send(ProgressEvent::DecompressDone { csv_path: csv_path.clone() }).ok();

    // Step 0.5: Sort if requested
    let work_path = if let Some(ref sort_col) = config.sort_column {
        let sorted = tmp_dir.join("_sorted.csv");
        external_sort(&csv_path, &sorted, sort_col, config.sort_descending, config.delimiter, config.buf_mb, tx)?;
        sorted
    } else {
        csv_path
    };

    // Load column filter
    let desired_cols = match &config.columns_file {
        Some(path) => {
            let content = fs::read_to_string(path)?;
            let set: HashSet<String> = content.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            tx.send(ProgressEvent::Info(format!("Column filter: {} desired columns", set.len()))).ok();
            Some(set)
        }
        None => None,
    };

    // Phase 1: Count lines
    let file_size = fs::metadata(&work_path)?.len();
    tx.send(ProgressEvent::PhaseStart { phase: "Counting lines".into(), total_bytes: file_size }).ok();
    let total_lines = count_lines(&work_path, buf_size, file_size, tx)?;
    let data_lines = total_lines.saturating_sub(1);
    tx.send(ProgressEvent::PhaseDone { phase: "Counting lines".into() }).ok();
    tx.send(ProgressEvent::Info(format!("{} data lines", data_lines))).ok();

    if data_lines == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "No data lines"));
    }

    let splits = config.splits as u64;
    let lines_per_split = data_lines / splits;
    let remainder = data_lines % splits;

    // Phase 2: Split + filter + compress
    let stem = config.input.file_stem().unwrap().to_string_lossy().to_string();
    tx.send(ProgressEvent::PhaseStart { phase: "Splitting & compressing".into(), total_bytes: file_size }).ok();

    let compressed = split_filter_compress(
        &work_path, &tmp_dir, &stem,
        config.splits, lines_per_split, remainder,
        &desired_cols, config.compress_format, config.compress_level,
        config.delimiter, buf_size, tx,
    )?;
    tx.send(ProgressEvent::PhaseDone { phase: "Splitting & compressing".into() }).ok();

    // Phase 3: Pack tar (unless zip format, which is self-contained)
    let output_path = config.output.unwrap_or_else(|| {
        config.input.with_file_name(format!("{}_split.tar", stem))
    });

    let tar_total: u64 = compressed.iter()
        .map(|f| fs::metadata(f).map(|m| m.len()).unwrap_or(0))
        .sum();
    tx.send(ProgressEvent::PhaseStart { phase: "Packing tar".into(), total_bytes: tar_total }).ok();
    pack_tar(&compressed, &output_path, buf_size, tx)?;
    tx.send(ProgressEvent::PhaseDone { phase: "Packing tar".into() }).ok();

    // Cleanup
    fs::remove_dir_all(&tmp_dir).ok();
    tx.send(ProgressEvent::Finished { output: output_path }).ok();
    Ok(())
}

fn count_lines(path: &Path, buf_size: usize, file_size: u64, tx: &ProgressSender) -> io::Result<u64> {
    let mut file = BufReader::with_capacity(buf_size, File::open(path)?);
    let mut count: u64 = 0;
    let mut bytes: u64 = 0;
    let mut buf = vec![0u8; buf_size];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 { break; }
        count += memchr_iter(b'\n', &buf[..n]).count() as u64;
        bytes += n as u64;
        if bytes % (1 << 20) < n as u64 {
            tx.send(ProgressEvent::Progress { bytes }).ok();
        }
    }
    tx.send(ProgressEvent::Progress { bytes: file_size }).ok();
    Ok(count)
}

fn split_filter_compress(
    input: &Path,
    tmp_dir: &Path,
    stem: &str,
    splits: usize,
    lines_per_split: u64,
    remainder: u64,
    desired_cols: &Option<HashSet<String>>,
    format: CompressFormat,
    level: i32,
    delimiter: Delimiter,
    buf_size: usize,
    tx: &ProgressSender,
) -> io::Result<Vec<PathBuf>> {
    let delim = delimiter.byte();
    let mut reader = BufReader::with_capacity(buf_size, File::open(input)?);

    let mut header_line = String::new();
    reader.read_line(&mut header_line)?;
    let header_fields = parse_csv_line(&header_line, delim);

    let (col_indices, new_header) = match desired_cols {
        Some(desired) => {
            let indices: Vec<usize> = header_fields.iter().enumerate()
                .filter(|(_, name)| desired.contains(name.as_str()))
                .map(|(i, _)| i)
                .collect();
            tx.send(ProgressEvent::Info(format!("Matched {}/{} columns", indices.len(), header_fields.len()))).ok();
            if indices.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "No matching columns"));
            }
            let names: Vec<&str> = indices.iter().map(|&i| header_fields[i].as_str()).collect();
            let hdr = join_csv_line(&names, delim);
            (Some(indices), hdr.into_bytes())
        }
        None => (None, header_line.as_bytes().to_vec()),
    };

    let ext = format!("csv{}", format.extension());

    // Spawn compressor threads
    let mut senders: Vec<mpsc::SyncSender<Vec<u8>>> = Vec::with_capacity(splits);
    let mut handles: Vec<thread::JoinHandle<io::Result<PathBuf>>> = Vec::with_capacity(splits);

    for i in 0..splits {
        let (stx, srx) = mpsc::sync_channel::<Vec<u8>>(4096);
        senders.push(stx);

        let out_path = tmp_dir.join(format!("{}_{:04}.{}", stem, i + 1, ext));
        let hdr = new_header.clone();
        let fmt = format;
        let lvl = level;

        handles.push(thread::spawn(move || -> io::Result<PathBuf> {
            let mut writer = make_compressor(&out_path, fmt, lvl)?;
            writer.write_all(&hdr)?;
            for data in srx {
                writer.write_all(&data)?;
            }
            finish_compressor(writer)?;
            Ok(out_path)
        }));
    }

    // Read + filter + distribute
    let mut bytes_read = header_line.len() as u64;
    let mut split_idx: usize = 0;
    let mut lines_in_current: u64 = 0;
    let mut current_target = lines_per_split + if 0 < remainder { 1 } else { 0 };

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 { break; }
        bytes_read += n as u64;

        let data = match &col_indices {
            Some(indices) => {
                let fields = parse_csv_line(&line, delim);
                let selected: Vec<&str> = indices.iter()
                    .filter_map(|&i| fields.get(i).map(|s| s.as_str()))
                    .collect();
                join_csv_line(&selected, delim).into_bytes()
            }
            None => line.as_bytes().to_vec(),
        };

        if senders[split_idx].send(data).is_err() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "compressor died"));
        }

        lines_in_current += 1;
        if lines_in_current >= current_target && split_idx + 1 < splits {
            split_idx += 1;
            lines_in_current = 0;
            let extra = if (split_idx as u64) < remainder { 1 } else { 0 };
            current_target = lines_per_split + extra;
        }

        if bytes_read % (1 << 20) < n as u64 {
            tx.send(ProgressEvent::Progress { bytes: bytes_read }).ok();
        }
    }

    drop(senders);

    let mut out_paths = Vec::with_capacity(splits);
    for h in handles {
        let p = h.join().map_err(|_| io::Error::new(io::ErrorKind::Other, "thread panicked"))??;
        out_paths.push(p);
    }

    Ok(out_paths)
}

fn pack_tar(files: &[PathBuf], output: &Path, buf_size: usize, tx: &ProgressSender) -> io::Result<()> {
    let tar_file = BufWriter::with_capacity(buf_size, File::create(output)?);
    let mut archive = tar::Builder::new(tar_file);
    let mut bytes_done: u64 = 0;

    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let fsize = fs::metadata(path)?.len();

        let mut header = tar::Header::new_gnu();
        header.set_size(fsize);
        header.set_mode(0o644);
        header.set_cksum();

        let r = BufReader::with_capacity(buf_size, File::open(path)?);
        archive.append_data(&mut header, &name, r)?;

        bytes_done += fsize;
        tx.send(ProgressEvent::Progress { bytes: bytes_done }).ok();
    }

    archive.finish()?;
    Ok(())
}
