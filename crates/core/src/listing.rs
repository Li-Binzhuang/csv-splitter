//! Directory listing: the "which files can I open?" half of a viewer.
//!
//! `serve --root` already confines every request to one tree; a front-end needs
//! to show what is inside it before it can ask for anything. This lists files by
//! extension, relative to the root, with size and modification time — enough to
//! render a picker without a second request per row.

use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct FileEntry {
    /// Path relative to the root, using `/` separators on every platform.
    pub relative: String,
    pub bytes: u64,
    /// Modification time as seconds since the Unix epoch (0 when unknown).
    pub modified_secs: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub root: PathBuf,
    pub files: Vec<FileEntry>,
    /// True when `max` cut the list short.
    pub truncated: bool,
}

/// List files under `root` whose extension is in `exts` (comma separated).
///
/// Only regular files are returned, at most `max` of them. Directories are not
/// descended into symlinks: a listing that can loop forever is worse than one
/// that misses a corner of the filesystem.
pub fn list(root: &Path, exts: &str, max: usize) -> io::Result<Listing> {
    let root = root.canonicalize()?;
    if !root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", root.display()),
        ));
    }
    let wanted: Vec<String> = exts
        .split(',')
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();

    let mut files = Vec::new();
    let mut truncated = false;
    let mut stack = vec![root.clone()];

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            // Unreadable directory: skip it rather than fail the whole listing.
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                // `symlink_metadata` is what `metadata` follows; guard loops by
                // refusing to descend into anything that is not a real dir entry.
                if !path.is_symlink() {
                    stack.push(path);
                }
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if !wanted.is_empty() && !matches_extension(&name, &wanted) {
                continue;
            }
            if files.len() >= max {
                truncated = true;
                break;
            }
            let relative = relative_path(&root, &path);
            let modified_secs = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            files.push(FileEntry {
                relative,
                bytes: meta.len(),
                modified_secs,
            });
        }
        if truncated {
            break;
        }
    }

    files.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(Listing {
        root,
        files,
        truncated,
    })
}

/// `report.csv.gz` counts as a `.gz` file; we match the last extension only.
fn matches_extension(name: &str, wanted: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    match lower.rsplit_once('.') {
        Some((_, ext)) => wanted.iter().any(|w| w == ext),
        None => false,
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "csvs_list_{tag}_{}_{}",
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

    #[test]
    fn lists_matching_files_with_relative_paths() {
        let dir = TempDir::new("basic");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.csv"), "id\n1\n").unwrap();
        std::fs::write(dir.join("sub/b.tsv"), "id\n1\n").unwrap();
        std::fs::write(dir.join("note.md"), "x").unwrap();

        let listing = list(&dir.0, "csv,tsv", 100).unwrap();
        let names: Vec<&str> = listing.files.iter().map(|f| f.relative.as_str()).collect();
        assert_eq!(names, vec!["a.csv", "sub/b.tsv"]);

        let listing = list(&dir.0, "csv", 100).unwrap();
        assert_eq!(listing.files.len(), 1);
        assert_eq!(listing.files[0].bytes, 5, "'id\n1\n' is 5 bytes");
    }

    #[test]
    fn truncates_at_the_limit() {
        let dir = TempDir::new("limit");
        for i in 0..10 {
            std::fs::write(dir.join(&format!("f{i}.csv")), "x").unwrap();
        }
        let listing = list(&dir.0, "csv", 3).unwrap();
        assert_eq!(listing.files.len(), 3);
        assert!(listing.truncated);
    }

    #[test]
    fn rejects_a_file_as_root() {
        let dir = TempDir::new("notdir");
        let f = dir.join("a.csv");
        std::fs::write(&f, "x").unwrap();
        assert!(list(&f, "csv", 10).is_err());
    }

    #[test]
    fn extension_match_is_case_insensitive_and_compound_aware() {
        assert!(matches_extension("A.CSV", &["csv".to_string()]));
        assert!(matches_extension("b.csv.gz", &["gz".to_string()]));
        assert!(!matches_extension("c.csv.gz", &["csv".to_string()]));
    }
}
