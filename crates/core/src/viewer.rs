//! Windowed CSV viewer: the piece the GUI table talks to.
//!
//! Rendering 100M rows is not the problem — keeping them is. A viewer only needs
//! the rows currently on screen, so this type holds exactly one window and
//! refetches it through the sparse index when the table asks for a range outside
//! the cached one.
//!
//! The logic lives in core (not egui) so it can be tested without a window.

use crate::csv_reader::parse_csv_record;
use crate::encoding::{decode_cell, detect, TextEncoding};
use crate::index::{SparseIndex, DEFAULT_INTERVAL};
use crate::types::Delimiter;
use std::io;
use std::path::{Path, PathBuf};

/// Default number of rows kept in memory at once.
pub const DEFAULT_WINDOW: u64 = 2_000;

pub struct WindowViewer {
    path: PathBuf,
    delimiter: Delimiter,
    encoding: TextEncoding,
    index: SparseIndex,
    /// First data row held in `rows` (0-based).
    start: u64,
    rows: Vec<Vec<String>>,
}

impl WindowViewer {
    /// Open a file, using its sidecar index when it is still valid and building
    /// one otherwise (constant memory, roughly 0.5 s per 100 MB).
    pub fn open(path: &Path, delimiter: Delimiter) -> io::Result<Self> {
        let encoding = detect(path)?;
        if !encoding.is_ascii_compatible() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is {}; random access needs an ASCII-compatible encoding (utf-8 / gbk)",
                    path.display(),
                    encoding.label()
                ),
            ));
        }
        let sidecar = SparseIndex::sidecar_path(path);
        let index = match SparseIndex::load(&sidecar) {
            Ok(idx) if idx.matches(path) => idx,
            _ => {
                let idx = SparseIndex::build(path, DEFAULT_INTERVAL)?;
                // Best effort: a read-only directory still works, just slower next time.
                idx.save(&sidecar).ok();
                idx
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            delimiter,
            encoding,
            index,
            start: 0,
            rows: Vec::new(),
        })
    }

    /// Open with an already-built index (e.g. one built on a worker thread).
    pub fn with_index(path: &Path, delimiter: Delimiter, index: SparseIndex) -> Self {
        Self {
            path: path.to_path_buf(),
            delimiter,
            encoding: TextEncoding::Utf8 { bom: false },
            index,
            start: 0,
            rows: Vec::new(),
        }
    }

    pub fn total_rows(&self) -> u64 {
        self.index.data_rows()
    }

    /// First data row of the cached window.
    pub fn window_start(&self) -> u64 {
        self.start
    }

    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    /// Ensure `[start, start + count)` is cached, refetching if it is not.
    ///
    /// The window is aligned to `count` so small scrolls do not trigger a read
    /// on every frame.
    pub fn ensure_range(&mut self, start: u64, count: u64) -> io::Result<()> {
        if count == 0 || start >= self.total_rows() {
            return Ok(());
        }
        let count = count.min(DEFAULT_WINDOW);
        let end = start.saturating_add(count).min(self.total_rows());
        let covered = !self.rows.is_empty()
            && start >= self.start
            && end <= self.start + self.rows.len() as u64;
        if covered {
            return Ok(());
        }
        self.load_at(start, count)
    }

    /// Jump so that `row` becomes the first visible row.
    pub fn jump_to(&mut self, row: u64) -> io::Result<()> {
        let row = row.min(self.total_rows().saturating_sub(1));
        self.load_at(row, DEFAULT_WINDOW)
    }

    fn load_at(&mut self, start: u64, count: u64) -> io::Result<()> {
        let raw = self.index.read_rows(&self.path, start, count)?;
        let delim = self.delimiter.byte();
        self.rows = raw
            .iter()
            .map(|r| {
                let text = decode_cell(r, self.encoding);
                parse_csv_record(text.trim_end_matches(['\n', '\r']), delim)
            })
            .collect();
        self.start = start;
        Ok(())
    }

    /// Header record, parsed.
    pub fn headers(&self) -> io::Result<Vec<String>> {
        let raw = self.index.read_header(&self.path)?;
        let text = decode_cell(&raw, self.encoding);
        Ok(parse_csv_record(text.trim_end(), self.delimiter.byte()))
    }

    /// Encoding used to decode cells (reported by the API).
    pub fn encoding_label(&self) -> &'static str {
        self.encoding.label()
    }

    /// Bytes of the backing file (for the status line).
    pub fn file_len(&self) -> u64 {
        self.index.file_len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "csvs_view_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
                    + unique_seq() as u128
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

    fn sample(dir: &TempDir, rows: usize) -> PathBuf {
        let p = dir.join("d.csv");
        let mut csv = String::from("id,note\n");
        for i in 0..rows {
            if i % 50 == 0 {
                csv.push_str(&format!("{i},\"a\nb\"\n"));
            } else {
                csv.push_str(&format!("{i},ok\n"));
            }
        }
        std::fs::write(&p, csv).unwrap();
        p
    }

    #[test]
    fn opens_and_reports_total_rows() {
        let dir = TempDir::new("open");
        let p = sample(&dir, 500);
        let viewer = WindowViewer::open(&p, Delimiter::Comma).unwrap();
        assert_eq!(viewer.total_rows(), 500);
        assert_eq!(viewer.headers().unwrap(), vec!["id", "note"]);
    }

    #[test]
    fn ensure_range_loads_only_what_is_asked() {
        let dir = TempDir::new("range");
        let p = sample(&dir, 5000);
        let mut viewer = WindowViewer::open(&p, Delimiter::Comma).unwrap();

        viewer.ensure_range(0, 50).unwrap();
        assert_eq!(viewer.rows().len(), 50);
        assert_eq!(viewer.rows()[0][0], "0");

        // Already covered: no reload, window start unchanged.
        viewer.ensure_range(10, 20).unwrap();
        assert_eq!(viewer.window_start(), 0);

        viewer.ensure_range(4000, 50).unwrap();
        assert_eq!(viewer.window_start(), 4000);
        assert_eq!(viewer.rows()[0][0], "4000");
    }

    #[test]
    fn jump_clamps_at_end() {
        let dir = TempDir::new("jump");
        let p = sample(&dir, 300);
        let mut viewer = WindowViewer::open(&p, Delimiter::Comma).unwrap();
        viewer.jump_to(10_000).unwrap();
        // Clamped to the last row, so the window starts there and holds one row.
        assert_eq!(viewer.rows().len(), 1);
        assert_eq!(viewer.rows()[0][0], "299");
    }

    #[test]
    fn window_larger_than_file_is_fine() {
        let dir = TempDir::new("short");
        let p = sample(&dir, 3);
        let mut viewer = WindowViewer::open(&p, Delimiter::Comma).unwrap();
        viewer.ensure_range(0, 2000).unwrap();
        assert_eq!(viewer.rows().len(), 3);
    }

    #[test]
    fn multiline_records_stay_whole() {
        let dir = TempDir::new("multi");
        let p = sample(&dir, 100);
        let mut viewer = WindowViewer::open(&p, Delimiter::Comma).unwrap();
        viewer.ensure_range(0, 1).unwrap();
        assert_eq!(viewer.rows()[0], vec!["0", "a\nb"]);
        viewer.ensure_range(50, 1).unwrap();
        assert_eq!(viewer.rows()[0], vec!["50", "a\nb"]);
    }
}
