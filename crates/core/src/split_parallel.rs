//! Parallel splitting: several readers, each owning a contiguous run of chunks.
//!
//! A single reader tops out around the disk's sequential throughput (measured
//! ~490 MB/s on an M1 Pro), while an 8-core machine has spare cores and often
//! spare bandwidth (RAID, or a warm page cache). Reading in parallel is the
//! obvious win — the non-obvious part is *where to start*.
//!
//! Naively seeking to `file_len * i / n` lands in the middle of a record, and
//! possibly inside a quoted field containing a newline. Finding a safe boundary
//! requires quote state, which cannot be recovered without scanning from byte 0.
//!
//! So the counting pass records **checkpoints**: the byte offset of every N-th
//! record, known to be a record boundary because the scanner was in the
//! outside-quotes state there. Readers start from a checkpoint (skipping at most
//! N-1 records) and each handles whole chunks, so no record is split, duplicated
//! or reordered across threads.

use crate::compress::{finish_compressor, make_compressor};
use crate::csv_reader::{join_csv_line, parse_csv_record, RecordReader};
use crate::types::*;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::mpsc;

/// Records between checkpoints. 1000 keeps the offset list tiny (8 bytes per
/// 1000 records) while bounding the per-reader skip to under a millisecond.
pub const CHECKPOINT_INTERVAL: u64 = 1000;

/// One reader's assignment: the chunks it produces and where its data starts.
pub struct ReaderJob {
    /// Absolute byte offset of the first record this reader must emit.
    pub start_offset: u64,
    /// Records to skip from `start_offset` to reach the first record to emit.
    pub skip: u64,
    /// Chunk indices this reader writes, in order.
    pub chunks: Vec<ChunkSlice>,
}

#[derive(Clone, Debug)]
pub struct ChunkSlice {
    pub index: usize,
    /// Records this chunk receives.
    pub records: u64,
}

/// Group contiguous chunks into `readers` jobs.
///
/// Chunk boundaries are record boundaries, so grouping whole chunks means
/// readers never share a record.
pub fn plan_readers(plan: &[u64], readers: usize, checkpoints: &[u64]) -> Vec<ReaderJob> {
    let readers = readers.clamp(1, plan.len().max(1));
    let mut jobs = Vec::with_capacity(readers);
    let mut first_record: u64 = 0;

    for r in 0..readers {
        // Split the chunk list as evenly as possible.
        let from = r * plan.len() / readers;
        let to = (r + 1) * plan.len() / readers;
        let chunks: Vec<ChunkSlice> = (from..to)
            .map(|i| ChunkSlice {
                index: i,
                records: plan[i],
            })
            .collect();
        if chunks.is_empty() {
            continue;
        }
        let (start_offset, skip) = seek_point(first_record, checkpoints);
        jobs.push(ReaderJob {
            start_offset,
            skip,
            chunks,
        });
        first_record += (from..to).map(|i| plan[i]).sum::<u64>();
    }
    jobs
}

/// Offset + skip for a data record index, using the checkpoint table.
fn seek_point(record: u64, checkpoints: &[u64]) -> (u64, u64) {
    if checkpoints.is_empty() {
        return (0, record);
    }
    let cp = (record / CHECKPOINT_INTERVAL).min(checkpoints.len() as u64 - 1) as usize;
    let skip = record - cp as u64 * CHECKPOINT_INTERVAL;
    (checkpoints[cp], skip)
}

/// Append a checkpoint entry for every interval-th data record.
pub struct CheckpointSink {
    pub offsets: Vec<u64>,
    pub seen: u64,
}

impl CheckpointSink {
    pub fn new() -> Self {
        Self {
            offsets: Vec::new(),
            seen: 0,
        }
    }

    pub fn observe(&mut self, record_index: u64, offset: u64) {
        self.seen += 1;
        if record_index.is_multiple_of(CHECKPOINT_INTERVAL) {
            self.offsets.push(offset);
        }
    }
}

impl Default for CheckpointSink {
    fn default() -> Self {
        Self::new()
    }
}

/// Settings shared by every reader thread.
///
/// Owned rather than borrowed so each thread can hold its own copy; the strings
/// and header are small and cloning them is cheaper than juggling scopes.
#[derive(Clone)]
pub struct ReaderOptions {
    pub input: std::path::PathBuf,
    pub tmp_dir: std::path::PathBuf,
    pub stem: String,
    pub header: Vec<u8>,
    pub col_indices: Option<Vec<usize>>,
    pub encoding: crate::encoding::TextEncoding,
    pub format: CompressFormat,
    pub level: i32,
    pub delimiter: Delimiter,
    pub buf_size: usize,
}

/// Run one reader job: seek, skip, then write its chunks.
pub fn run_reader(
    job: &ReaderJob,
    opts: &ReaderOptions,
    tx: &mpsc::Sender<ProgressEvent>,
) -> io::Result<Vec<PathBuf>> {
    use std::io::{Seek, SeekFrom};

    let mut file = File::open(&opts.input)?;
    file.seek(SeekFrom::Start(job.start_offset))?;
    let read_buf = opts.buf_size.clamp(1024 * 1024, 16 * 1024 * 1024);
    let mut reader = RecordReader::new(BufReader::with_capacity(read_buf, file), read_buf);

    let mut record = Vec::with_capacity(64 * 1024);
    for _ in 0..job.skip {
        if !reader.next_record(&mut record)? {
            return Ok(Vec::new());
        }
    }

    let ext = match opts.format {
        CompressFormat::Zip | CompressFormat::None => "csv".to_string(),
        other => format!("csv{}", other.extension()),
    };
    let per_file_buf = (opts.buf_size / job.chunks.len().max(1)).clamp(64 * 1024, 4 * 1024 * 1024);
    let delim = opts.delimiter.byte();

    let mut out = Vec::with_capacity(job.chunks.len());
    for slice in &job.chunks {
        let path = opts
            .tmp_dir
            .join(format!("{}_{:04}.{ext}", opts.stem, slice.index + 1));
        let mut writer: Box<dyn Write + Send> = match opts.format {
            CompressFormat::Zip | CompressFormat::None => {
                Box::new(BufWriter::with_capacity(per_file_buf, File::create(&path)?))
            }
            other => make_compressor(&path, other, opts.level, per_file_buf)?,
        };
        writer.write_all(&opts.header)?;

        for _ in 0..slice.records {
            if !reader.next_record(&mut record)? {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("file ended inside chunk {}", slice.index + 1),
                ));
            }
            match &opts.col_indices {
                Some(indices) => {
                    let text = crate::encoding::decode_record(&record, opts.encoding);
                    let fields = parse_csv_record(&text, delim);
                    let selected: Vec<&str> = indices
                        .iter()
                        .filter_map(|&i| fields.get(i).map(|s| s.as_str()))
                        .collect();
                    writer.write_all(&join_csv_line(&selected, delim).into_bytes())?;
                }
                None => writer.write_all(&record)?,
            }
        }
        finish_compressor(writer)?;
        out.push(path);
        let _ = tx.send(ProgressEvent::Info(format!(
            "chunk {} done",
            slice.index + 1
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_groups_whole_chunks_without_gaps() {
        let plan = vec![4u64, 3, 3, 3, 3];
        let jobs = plan_readers(&plan, 3, &[0, 1000, 2000, 3000, 4000, 5000]);
        let total: u64 = jobs
            .iter()
            .flat_map(|j| j.chunks.iter().map(|c| c.records))
            .sum();
        assert_eq!(total, plan.iter().sum::<u64>(), "records must add up");

        let indices: Vec<usize> = jobs
            .iter()
            .flat_map(|j| j.chunks.iter().map(|c| c.index))
            .collect();
        assert_eq!(indices, vec![0, 1, 2, 3, 4], "chunks in order, no gaps");
    }

    #[test]
    fn seek_points_follow_the_checkpoints() {
        let cps = vec![0u64, 1000, 2000];
        assert_eq!(seek_point(0, &cps), (0, 0));
        assert_eq!(seek_point(999, &cps), (0, 999));
        assert_eq!(seek_point(1000, &cps), (1000, 0));
        assert_eq!(seek_point(1500, &cps), (1000, 500));
        // Beyond the last checkpoint we fall back to the last one.
        assert_eq!(seek_point(9_999, &cps), (2000, 7999));
    }

    #[test]
    fn checkpoints_record_every_interval() {
        let mut sink = CheckpointSink::new();
        for i in 0..=CHECKPOINT_INTERVAL * 2 {
            sink.observe(i, i * 10);
        }
        assert_eq!(sink.offsets, vec![0, 1000 * 10, 2000 * 10]);
    }
}
