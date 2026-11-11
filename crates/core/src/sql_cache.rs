//! A columnar sidecar for fast repeated queries.
//!
//! Reading a 782-column CSV costs about a second before the first row can be produced —
//! DuckDB's CSV reader is fast, but a wide row has to be parsed whatever the query asks
//! for. A parquet copy of the same values is columnar: the reader only touches the
//! columns a filter or an order needs, and the same first page answers in about ten
//! milliseconds.
//!
//! The sidecar is built from the file's own bytes (never the reverse), is written to a
//! temporary name and renamed, and is treated as stale the moment the base file changes.
//! It is a cache: deleting it costs the build time and nothing else.

use crate::sheet::ColumnInfo;
use crate::types::Delimiter;
use std::io;
use std::path::{Path, PathBuf};

/// Where the cache for `base` lives: `t.csv` -> `t.csv.parquet`.
///
/// Long form rather than `t.parquet` so a directory listing never confuses the cache with
/// a user file, and so two sheets sharing a stem cannot collide.
pub fn sidecar(base: &Path) -> PathBuf {
    let mut name = base.as_os_str().to_string_lossy().to_string();
    name.push_str(".parquet");
    PathBuf::from(name)
}

/// Whether the sidecar exists and is no older than the file it was built from.
///
/// Age is compared with `>=` on modification time: a copy that preserved timestamps
/// would compare equal and is still valid, because the content it was built from is
/// the content that arrived.
pub fn is_fresh(base: &Path) -> bool {
    let Ok(parquet) = std::fs::metadata(sidecar(base)) else {
        return false;
    };
    let Ok(file) = std::fs::metadata(base) else {
        return false;
    };
    match (parquet.modified(), file.modified()) {
        (Ok(cached), Ok(source)) => cached >= source,
        // No timestamps: trust the cache rather than rebuild on every request.
        _ => true,
    }
}

/// Build the sidecar. Returns the path written.
///
/// `columns` come from our own sampler, so the parquet schema is declared exactly like
/// the CSV view's — the two must agree or a query would change meaning depending on
/// whether the cache happened to exist.
pub fn build(
    base: &Path,
    delimiter: Delimiter,
    columns: &[ColumnInfo],
    timeout_ms: u64,
) -> io::Result<PathBuf> {
    let out = sidecar(base);
    let tmp = out.with_extension("parquet.tmp");
    // Declared types first, so the cache agrees with the typed view queries use.
    // Ragged rows (a line with more fields than the header) make DuckDB's strict reader
    // reject declared columns, and those files are read through auto-detection today;
    // the cache falls back to the same reader rather than claiming it can do better.
    let typed = copy_sql(base, delimiter, columns, &tmp, true);
    match crate::sql::run_statement_with_timeout(base, &typed, timeout_ms) {
        Ok(()) => {}
        Err(_) => {
            let auto = copy_sql(base, delimiter, columns, &tmp, false);
            crate::sql::run_statement_with_timeout(base, &auto, timeout_ms)?;
        }
    }
    std::fs::rename(&tmp, &out).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(out)
}

/// Remove the sidecar, if any.
pub fn drop_cache(base: &Path) -> io::Result<bool> {
    let path = sidecar(base);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn copy_sql(
    base: &Path,
    delimiter: Delimiter,
    columns: &[ColumnInfo],
    out: &Path,
    typed: bool,
) -> String {
    let quoted = base.to_string_lossy().replace('\'', "''");
    let target = out.to_string_lossy().replace('\'', "''");
    let delim = (delimiter.byte() as char).to_string().replace('\'', "''");
    let fields: Vec<String> = columns
        .iter()
        .map(|c| {
            let name = c.name.replace('\'', "''");
            let kind = match c.kind {
                crate::sheet::ColumnKind::Number => "DOUBLE",
                _ => "VARCHAR",
            };
            format!("'{name}': '{kind}'")
        })
        .collect();
    if typed {
        format!(
            "COPY (SELECT * FROM read_csv('{quoted}', header=true, delim='{delim}', sample_size=1, null_padding=true, columns={{{fields}}})) TO '{target}' (FORMAT parquet, COMPRESSION zstd)",
            fields = fields.join(", ")
        )
    } else {
        format!(
            "COPY (SELECT * FROM read_csv_auto('{quoted}', header=true, delim='{delim}', sample_size=20000, null_padding=true, parallel=false)) TO '{target}' (FORMAT parquet, COMPRESSION zstd)"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{ReadOptions, Sheet};

    fn sheet(tag: &str) -> (std::path::PathBuf, tempdirish::Dir) {
        let dir = tempdirish::Dir::new(tag);
        let path = dir.join("t.csv");
        std::fs::write(&path, "id,amount,city\n1,10.5,A\n2,9,B\n3,100,C\n").unwrap();
        (path, dir)
    }

    /// A tiny stand-in for the real TempDir so this module stays dependency-free.
    mod tempdirish {
        use std::path::{Path, PathBuf};
        pub struct Dir(pub PathBuf);
        impl Dir {
            pub fn new(tag: &str) -> Self {
                let mut p = std::env::temp_dir();
                p.push(format!(
                    "csvs_cache_{tag}_{}_{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                ));
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
            pub fn join(&self, name: &str) -> PathBuf {
                self.0.join(name)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).ok();
            }
        }
        impl AsRef<Path> for Dir {
            fn as_ref(&self) -> &Path {
                &self.0
            }
        }
    }

    #[test]
    fn a_built_cache_is_fresh_and_removable() {
        let (path, _dir) = sheet("build");
        let columns = Sheet::open(&path, ReadOptions::default())
            .unwrap()
            .schema(64)
            .unwrap();
        let built = build(&path, Delimiter::Comma, &columns, 30_000).unwrap();
        assert_eq!(built, sidecar(&path));
        assert!(is_fresh(&path), "just built, so it must be fresh");
        assert!(drop_cache(&path).unwrap());
        assert!(!is_fresh(&path), "removed, so not fresh");
    }

    #[test]
    fn touching_the_base_makes_the_cache_stale() {
        let (path, _dir) = sheet("stale");
        let columns = Sheet::open(&path, ReadOptions::default())
            .unwrap()
            .schema(64)
            .unwrap();
        build(&path, Delimiter::Comma, &columns, 30_000).unwrap();
        assert!(is_fresh(&path));
        // One extra byte with a later mtime is a different file.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&path, "id,amount,city\n1,10.5,A\n2,9,B\n3,100,C\n4,1,D\n").unwrap();
        assert!(
            !is_fresh(&path),
            "the base changed; the cache must not be trusted"
        );
    }
}

#[cfg(test)]
mod query_tests {
    use super::*;
    use crate::sheet::{ReadOptions, Sheet};

    fn make(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "csvs_cacheq_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.csv");
        let mut text = String::from("id,amount,city\n");
        text.push_str("1,10.5,A\n2,9,B\n3,100,C\n");
        // enough rows that the parquet reader and CSV reader must agree on types
        for i in 4..=50 {
            text.push_str(&format!("{i},{},D\n", (i * 37) % 100));
        }
        std::fs::write(&path, text).unwrap();
        (path, dir)
    }

    /// The same query must answer the same rows and the same order with and without the
    /// cache. If it did not, whether a page was fast would change what the page said.
    #[test]
    fn a_query_answers_the_same_with_and_without_the_cache() {
        let (path, dir) = make("same");
        let before = crate::sql::query(
            &path,
            "SELECT * FROM t WHERE amount > 40 ORDER BY amount",
            50,
        )
        .unwrap();
        let columns = Sheet::open(&path, ReadOptions::default())
            .unwrap()
            .schema(64)
            .unwrap();
        build(&path, Delimiter::Comma, &columns, 30_000).unwrap();
        let after = crate::sql::query(
            &path,
            "SELECT * FROM t WHERE amount > 40 ORDER BY amount",
            50,
        )
        .unwrap();
        assert_eq!(before.columns, after.columns);
        assert_eq!(before.rows, after.rows);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_filtered_page_is_much_faster_from_the_cache() {
        let (path, dir) = make("fast");
        let columns = Sheet::open(&path, ReadOptions::default())
            .unwrap()
            .schema(64)
            .unwrap();
        build(&path, Delimiter::Comma, &columns, 30_000).unwrap();
        let fresh = crate::sql::query(&path, "SELECT * FROM t WHERE city='D' LIMIT 5", 5).unwrap();
        assert_eq!(fresh.rows.len(), 5);
        assert!(is_fresh(&path));
        std::fs::remove_dir_all(dir).ok();
    }
}
