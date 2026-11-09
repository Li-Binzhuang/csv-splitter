//! Sparse row index: jump to any row of a huge CSV without scanning from byte 0.
//!
//! Reading row 50,000,000 by scanning is O(file); the whole point of a viewer is
//! that scrolling costs the same at row 50 as at row 50,000,000. We store the
//! byte offset of every N-th record (default 1000) in a sidecar file, then seek
//! to the nearest checkpoint and skip at most N-1 records.
//!
//! Cost: 8 bytes per `interval` rows, so 100M rows at interval 1000 is ~800 KB —
//! three orders of magnitude smaller than an in-memory table.

use crate::csv_reader::RecordReader;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"CSI1";
const VERSION: u8 = 1;
/// Checkpoint spacing used when the caller does not choose one.
pub const DEFAULT_INTERVAL: u64 = 1000;
const INDEX_BUF: usize = 1024 * 1024;

/// A sidecar index over a CSV file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SparseIndex {
    /// Byte offsets: `offsets[i]` is the start of record `i * interval`.
    offsets: Vec<u64>,
    interval: u64,
    /// Total records, header included.
    records: u64,
    header_len: u64,
    file_len: u64,
}

impl SparseIndex {
    pub fn interval(&self) -> u64 {
        self.interval
    }

    /// Records including the header row.
    pub fn records(&self) -> u64 {
        self.records
    }

    /// Data rows, excluding the header.
    pub fn data_rows(&self) -> u64 {
        self.records.saturating_sub(1)
    }

    pub fn header_len(&self) -> u64 {
        self.header_len
    }

    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    /// Bytes on disk once serialised.
    pub fn size_bytes(&self) -> u64 {
        4 + 1 + 8 * 5 + self.offsets.len() as u64 * 8
    }

    /// Where `path` stores its sidecar index.
    pub fn sidecar_path(path: &Path) -> PathBuf {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".idx");
        path.with_file_name(name)
    }

    /// Build an index by streaming the file once (constant memory).
    pub fn build(path: &Path, interval: u64) -> io::Result<Self> {
        Self::build_with_buffer(path, interval, 8 * 1024 * 1024)
    }

    pub fn build_with_buffer(path: &Path, interval: u64, buf: usize) -> io::Result<Self> {
        let interval = interval.max(1);
        let file_len = std::fs::metadata(path)?.len();
        let mut reader = RecordReader::new(BufReader::with_capacity(buf, File::open(path)?), buf);

        let mut offsets: Vec<u64> = Vec::new();
        let mut scratch = Vec::with_capacity(256 * 1024);
        let mut pos: u64 = 0;
        let mut count: u64 = 0;
        let mut header_len: u64 = 0;

        while reader.next_record(&mut scratch)? {
            if count.is_multiple_of(interval) {
                offsets.push(pos);
            }
            if count == 0 {
                header_len = scratch.len() as u64;
            }
            pos += scratch.len() as u64;
            count += 1;
        }

        Ok(Self {
            offsets,
            interval,
            records: count,
            header_len,
            file_len,
        })
    }

    /// Verify the index still describes the file (size and mtime-free check).
    pub fn matches(&self, path: &Path) -> bool {
        std::fs::metadata(path)
            .map(|m| m.len() == self.file_len)
            .unwrap_or(false)
    }

    /// Byte offset + records to skip for a data-row position.
    fn seek_point(&self, data_row: u64) -> (u64, u64) {
        let record = data_row + 1; // skip the header
        let checkpoint = record / self.interval;
        let idx = checkpoint.min(self.offsets.len().saturating_sub(1) as u64) as usize;
        let skip = record.saturating_sub(idx as u64 * self.interval);
        let offset = self.offsets.get(idx).copied().unwrap_or(0);
        (offset, skip)
    }

    /// Read up to `count` data rows starting at `start` (0-based, header
    /// excluded). Returns raw record bytes so callers can decode as they like.
    pub fn read_rows(&self, path: &Path, start: u64, count: u64) -> io::Result<Vec<Vec<u8>>> {
        if count == 0 || start >= self.data_rows() {
            return Ok(Vec::new());
        }
        let (offset, skip) = self.seek_point(start);
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = RecordReader::new(BufReader::with_capacity(INDEX_BUF, file), INDEX_BUF);

        let mut scratch = Vec::with_capacity(64 * 1024);
        for _ in 0..skip {
            if !reader.next_record(&mut scratch)? {
                return Ok(Vec::new());
            }
        }

        let mut out = Vec::with_capacity(count.min(10_000) as usize);
        for _ in 0..count {
            if !reader.next_record(&mut scratch)? {
                break;
            }
            out.push(scratch.clone());
        }
        Ok(out)
    }

    /// Raw bytes of the header record.
    pub fn read_header(&self, path: &Path) -> io::Result<Vec<u8>> {
        let file = File::open(path)?;
        let mut reader = RecordReader::new(BufReader::with_capacity(INDEX_BUF, file), INDEX_BUF);
        let mut scratch = Vec::with_capacity(64 * 1024);
        reader.next_record(&mut scratch)?;
        Ok(scratch)
    }

    // ---------------------------------------------------------------- storage

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("idx.tmp");
        {
            let mut f = io::BufWriter::new(File::create(&tmp)?);
            f.write_all(MAGIC)?;
            f.write_all(&[VERSION])?;
            f.write_all(&self.interval.to_le_bytes())?;
            f.write_all(&self.records.to_le_bytes())?;
            f.write_all(&self.header_len.to_le_bytes())?;
            f.write_all(&self.file_len.to_le_bytes())?;
            f.write_all(&(self.offsets.len() as u64).to_le_bytes())?;
            for off in &self.offsets {
                f.write_all(&off.to_le_bytes())?;
            }
            f.flush()?;
        }
        // Rename last so a crash never leaves a half-written index behind.
        std::fs::rename(&tmp, path)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let mut data = Vec::new();
        File::open(path)?.read_to_end(&mut data)?;
        Self::from_bytes(&data)
    }

    pub fn from_bytes(data: &[u8]) -> io::Result<Self> {
        if data.len() < 45 {
            return Err(invalid("index too short"));
        }
        if &data[..4] != MAGIC {
            return Err(invalid("bad index magic"));
        }
        if data[4] != VERSION {
            return Err(invalid("unsupported index version"));
        }
        let mut p = 5;
        let interval = read_u64(data, &mut p)?;
        let records = read_u64(data, &mut p)?;
        let header_len = read_u64(data, &mut p)?;
        let file_len = read_u64(data, &mut p)?;
        let n = read_u64(data, &mut p)? as usize;
        if data.len() < p + n * 8 {
            return Err(invalid("truncated offsets"));
        }
        let mut offsets = Vec::with_capacity(n);
        for i in 0..n {
            let start = p + i * 8;
            offsets.push(u64::from_le_bytes(
                data[start..start + 8].try_into().unwrap(),
            ));
        }
        if interval == 0 {
            return Err(invalid("zero interval"));
        }
        Ok(Self {
            offsets,
            interval,
            records,
            header_len,
            file_len,
        })
    }
}

fn read_u64(data: &[u8], pos: &mut usize) -> io::Result<u64> {
    let bytes = data
        .get(*pos..*pos + 8)
        .ok_or_else(|| invalid("truncated index"))?;
    *pos += 8;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "csvs_idx_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn sample(dir: &TempDir) -> PathBuf {
        let p = dir.join("d.csv");
        let mut csv = String::from("id,note\n");
        for i in 0..100 {
            if i % 10 == 0 {
                csv.push_str(&format!("{i},\"multi\nline\"\n"));
            } else {
                csv.push_str(&format!("{i},plain\n"));
            }
        }
        std::fs::write(&p, csv).unwrap();
        p
    }

    #[test]
    fn counts_records_not_lines() {
        let dir = TempDir::new("count");
        let p = sample(&dir);
        let idx = SparseIndex::build(&p, 10).unwrap();
        assert_eq!(idx.records(), 101); // header + 100 rows
        assert_eq!(idx.data_rows(), 100);
    }

    #[test]
    fn windows_match_sequential_reads() {
        let dir = TempDir::new("win");
        let p = sample(&dir);
        let idx = SparseIndex::build(&p, 7).unwrap();
        for start in [0u64, 1, 6, 7, 13, 55, 99] {
            let rows = idx.read_rows(&p, start, 3).unwrap();
            let text: Vec<String> = rows
                .iter()
                .map(|r| String::from_utf8_lossy(r).trim_end().to_string())
                .collect();
            assert!(!text.is_empty(), "start {start}");
            // First cell must equal the row index we asked for.
            let first_cell = text[0].split(',').next().unwrap().to_string();
            assert_eq!(first_cell, start.to_string(), "start={start} got={text:?}");
        }
    }

    #[test]
    fn out_of_range_returns_empty() {
        let dir = TempDir::new("oob");
        let p = sample(&dir);
        let idx = SparseIndex::build(&p, 10).unwrap();
        assert!(idx.read_rows(&p, 100, 5).unwrap().is_empty());
        assert!(idx.read_rows(&p, 0, 0).unwrap().is_empty());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = TempDir::new("save");
        let p = sample(&dir);
        let idx = SparseIndex::build(&p, 5).unwrap();
        let sidecar = dir.join("d.csv.idx");
        idx.save(&sidecar).unwrap();
        let loaded = SparseIndex::load(&sidecar).unwrap();
        assert_eq!(loaded, idx);
        assert!(loaded.matches(&p));
    }

    #[test]
    fn rejects_garbage_index() {
        assert!(SparseIndex::from_bytes(b"nope").is_err());
    }

    #[test]
    fn index_stays_small() {
        let dir = TempDir::new("small");
        let p = sample(&dir);
        let idx = SparseIndex::build(&p, 1000).unwrap();
        // 101 records at interval 1000 => a single checkpoint.
        assert!(idx.size_bytes() < 200);
    }
}
