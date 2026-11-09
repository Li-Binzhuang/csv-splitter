use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Scratch directory that cleans itself up, including on the error paths.
///
/// `Drop` is what makes this safe: the previous implementation only removed the
/// directory after a successful run, so every failed job (bad column, broken
/// pipe, cancelled process) left its chunks behind in `/tmp`.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> io::Result<Self> {
        // pid + nanos + a per-process sequence: two jobs started in the same
        // millisecond used to share a directory and delete each other's chunks.
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "csv_split_{tag}_{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            seq
        ));
        fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}
