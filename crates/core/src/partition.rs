//! Split a CSV into one file per distinct value of a column.
//!
//! "Give me one file per city" is a much more natural request than "give me 7
//! equally sized chunks", but it needs grouping rather than counting. Instead of
//! keeping one writer per value (which explodes on high-cardinality columns and
//! hits the open-file limit), the data is first sorted by that column with the
//! existing external sort — equal keys become contiguous, so only one writer is
//! open at a time.

use crate::compress::{finish_compressor, make_compressor};
use crate::csv_reader::{parse_csv_record, RecordReader};
use crate::decompress::decompress_to_csv;
use crate::pack::pack;
use crate::sort::external_sort;
use crate::tempdir::TempDir;
use crate::types::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Default guard: more partitions than this is almost certainly a mistake
/// (e.g. partitioning by `id`), and would create thousands of files.
pub const DEFAULT_MAX_PARTITIONS: usize = 512;

#[derive(Clone, Debug)]
pub struct PartitionConfig {
    pub input: PathBuf,
    pub output: Option<PathBuf>,
    /// Column whose distinct values become file names.
    pub column: String,
    pub compress_format: CompressFormat,
    pub compress_level: i32,
    pub buf_mb: usize,
    pub delimiter: Delimiter,
    pub max_partitions: usize,
}

impl Default for PartitionConfig {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            output: None,
            column: String::new(),
            compress_format: CompressFormat::Zstd,
            compress_level: 3,
            buf_mb: 64,
            delimiter: Delimiter::Comma,
            max_partitions: DEFAULT_MAX_PARTITIONS,
        }
    }
}

pub fn run_partition(config: PartitionConfig, tx: ProgressSender) {
    if let Err(e) = run_inner(config, &tx) {
        let _ = tx.send(ProgressEvent::Error(e.to_string()));
    }
}

fn run_inner(config: PartitionConfig, tx: &ProgressSender) -> io::Result<()> {
    if config.column.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "partition column must not be empty",
        ));
    }
    let buf_size = config.buf_mb.clamp(1, 4096) * 1024 * 1024;
    let tmp = TempDir::new("partition")?;
    let csv = decompress_to_csv(&config.input, tmp.path(), tx)?;

    // Sort by the partition column so equal keys are contiguous.
    let sorted = tmp.path().join("_sorted.csv");
    tx.send(ProgressEvent::PhaseStart {
        phase: "Partitioning (sorting)".into(),
        total_bytes: std::fs::metadata(&sorted).map(|m| m.len()).unwrap_or(0),
    })
    .ok();
    external_sort(
        &csv,
        &sorted,
        &config.column,
        false,
        config.delimiter,
        config.buf_mb,
        tx,
    )?;
    tx.send(ProgressEvent::PhaseDone {
        phase: "Partitioning (sorting)".into(),
    })
    .ok();

    let files = write_partitions(&sorted, tmp.path(), &config, buf_size, tx)?;
    if files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no partitions produced",
        ));
    }

    let stem = config
        .input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".to_string());
    let is_zip = config.compress_format == CompressFormat::Zip;
    let output = config.output.clone().unwrap_or_else(|| {
        config.input.with_file_name(format!(
            "{stem}_by_{}.{}",
            sanitize(&config.column),
            if is_zip { "zip" } else { "tar" }
        ))
    });
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    tx.send(ProgressEvent::PhaseStart {
        phase: "Packing partitions".into(),
        total_bytes: files
            .iter()
            .map(|f| std::fs::metadata(f).map(|m| m.len()).unwrap_or(0))
            .sum(),
    })
    .ok();
    pack(&files, &output, is_zip, buf_size, tx)?;
    tx.send(ProgressEvent::PhaseDone {
        phase: "Packing partitions".into(),
    })
    .ok();

    tx.send(ProgressEvent::Finished { output }).ok();
    Ok(())
}

fn write_partitions(
    sorted: &Path,
    tmp_dir: &Path,
    config: &PartitionConfig,
    buf_size: usize,
    tx: &ProgressSender,
) -> io::Result<Vec<PathBuf>> {
    let delim = config.delimiter.byte();
    let file_size = std::fs::metadata(sorted)?.len();
    let mut reader = RecordReader::new(
        BufReader::with_capacity(buf_size, File::open(sorted)?),
        buf_size,
    );

    let mut record = Vec::with_capacity(64 * 1024);
    if !reader.next_record(&mut record)? {
        return Ok(Vec::new());
    }
    let header = record.clone();
    let header_fields = parse_csv_record(&String::from_utf8_lossy(&header), delim);
    let col_idx = header_fields
        .iter()
        .position(|h| h == &config.column)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Partition column '{}' not found", config.column),
            )
        })?;

    let ext = match config.compress_format {
        CompressFormat::Zip | CompressFormat::None => "csv".to_string(),
        other => format!("csv{}", other.extension()),
    };
    let per_file_buf = (buf_size / 8).clamp(64 * 1024, 4 * 1024 * 1024);

    let mut files: Vec<PathBuf> = Vec::new();
    let mut used_names: HashMap<String, usize> = HashMap::new();
    let mut current_key: Option<String> = None;
    let mut writer: Option<Box<dyn Write + Send>> = None;
    let mut bytes_read = header.len() as u64;

    while reader.next_record(&mut record)? {
        bytes_read += record.len() as u64;
        let fields = parse_csv_record(&String::from_utf8_lossy(&record), delim);
        let key = fields.get(col_idx).cloned().unwrap_or_default();

        if current_key.as_deref() != Some(key.as_str()) {
            finish(&mut writer);
            if files.len() >= config.max_partitions {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "'{}' has more than {} distinct values; partitioning would create \
                         too many files (use -n or --by-size instead)",
                        config.column, config.max_partitions
                    ),
                ));
            }
            let name = unique_name(&key, &mut used_names);
            let path = tmp_dir.join(format!("{name}.{ext}"));
            let mut w: Box<dyn Write + Send> = match config.compress_format {
                CompressFormat::Zip | CompressFormat::None => {
                    Box::new(BufWriter::with_capacity(per_file_buf, File::create(&path)?))
                }
                other => make_compressor(&path, other, config.compress_level, per_file_buf)?,
            };
            w.write_all(&header)?;
            writer = Some(w);
            files.push(path);
            current_key = Some(key);
        }

        if let Some(w) = writer.as_mut() {
            w.write_all(&record)?;
        }

        if bytes_read % (1 << 20) < record.len() as u64 {
            tx.send(ProgressEvent::Progress { bytes: bytes_read }).ok();
        }
    }
    finish(&mut writer);
    tx.send(ProgressEvent::Progress { bytes: file_size }).ok();

    Ok(files)
}

fn finish(writer: &mut Option<Box<dyn Write + Send>>) {
    if let Some(w) = writer.take() {
        finish_compressor(w).ok();
    }
}

/// Turn a cell value into something safe for a file name.
///
/// Keeps letters/digits (including CJK), `-` and `_`; everything else becomes
/// `_`. Empty values become `_empty`, and duplicates (case-insensitive, as on
/// macOS) get a numeric suffix.
pub fn sanitize(value: &str) -> String {
    let trimmed = value.trim();
    let mut out = String::new();
    for ch in trimmed.chars().take(48) {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' || !ch.is_ascii() {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "_empty".to_string()
    } else {
        out
    }
}

fn unique_name(value: &str, used: &mut HashMap<String, usize>) -> String {
    let base = sanitize(value);
    let key = base.to_lowercase();
    let count = used.entry(key).or_insert(0);
    *count += 1;
    if *count == 1 {
        base
    } else {
        format!("{base}_{}", *count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_unsafe_characters() {
        assert_eq!(sanitize("New York"), "New_York");
        assert_eq!(sanitize("../etc/passwd"), "___etc_passwd");
        assert_eq!(sanitize(""), "_empty");
        assert_eq!(sanitize("北京"), "北京");
    }

    #[test]
    fn deduplicates_case_insensitively() {
        let mut used = HashMap::new();
        assert_eq!(unique_name("a", &mut used), "a");
        assert_eq!(unique_name("A", &mut used), "A_2");
        assert_eq!(unique_name("a", &mut used), "a_3");
    }
}
