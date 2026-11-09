//! Bounded cache of query results, so a table can scroll without re-running.
//!
//! A viewer asks for rows 0..50, then 50..100, then jumps to 5000..5050. Without a
//! cache each request re-plans and re-scans the file: on a 10 GB CSV the first
//! page costs seconds, and every subsequent page costs the same again. With one,
//! the scan happens once and every scroll is a slice.
//!
//! Memory is the constraint, so the cache is deliberately small and honest:
//!   * each entry holds at most `row_cap` rows, else it is marked oversized and
//!     never cached (we do not silently truncate a query);
//!   * at most `entries` results are kept, oldest first out;
//!   * an entry is invalidated when the file's length or mtime changes.

use crate::types::Delimiter;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Default total rows retained across all entries (~50 MB of cells at 12 columns).
pub const DEFAULT_ROW_BUDGET: usize = 400_000;
/// Row count for a single cached result.
pub const DEFAULT_ROW_CAP: usize = 200_000;

#[derive(Clone, Debug)]
pub struct CachedResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// True when the query produced more rows than the cap, so `rows` is partial.
    pub oversized: bool,
}

impl CachedResult {
    pub fn total(&self) -> usize {
        self.rows.len()
    }

    /// Slice `[start, start + count)` and report whether more rows follow.
    ///
    /// Only meaningful for a complete result; a truncated one has no idea what
    /// lies beyond its prefix. Use [`try_window`](Self::try_window) for those.
    pub fn window(&self, start: usize, count: usize) -> (Vec<Vec<String>>, bool) {
        let from = start.min(self.rows.len());
        let to = (from + count).min(self.rows.len());
        let rows = self.rows[from..to].to_vec();
        let has_more = to < self.rows.len();
        (rows, has_more)
    }

    /// Slice for a possibly-truncated result.
    ///
    /// Returns `None` when the request reaches past what was materialised —
    /// the caller must then re-run the query rather than pretend the result
    /// ends there. Windows fully inside the prefix are served as-is, which is
    /// what makes scrolling near the top free even for huge results.
    pub fn try_window(&self, start: usize, count: usize) -> Option<(Vec<Vec<String>>, bool)> {
        // Only a request that begins beyond the materialised prefix is refused;
        // one that straddles the end is clipped and reported as truncated, which
        // the caller detects with `rows.len() < count`.
        if self.oversized && start >= self.rows.len() {
            return None;
        }
        let from = start.min(self.rows.len());
        let to = start.saturating_add(count).min(self.rows.len());
        let rows = self.rows[from..to].to_vec();
        // Within a truncated prefix we cannot know what follows, so say "maybe".
        let has_more = self.oversized || to < self.rows.len();
        Some((rows, has_more))
    }
}

#[derive(Clone)]
pub struct ResultCache {
    inner: Arc<Mutex<Inner>>,
    row_budget: usize,
    row_cap: usize,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<Key, Entry>,
    /// Monotonic tick used for LRU eviction.
    clock: u64,
}

struct Entry {
    value: CachedResult,
    /// Rows held (0 for oversized entries, which still remember the key).
    weight: usize,
    last_used: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    len: u64,
    mtime_ns: u128,
    sql: String,
    delimiter: char,
}

impl Default for ResultCache {
    fn default() -> Self {
        Self::new(DEFAULT_ROW_BUDGET, DEFAULT_ROW_CAP)
    }
}

impl ResultCache {
    pub fn new(row_budget: usize, row_cap: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            row_budget: row_budget.max(row_cap),
            row_cap: row_cap.max(1),
        }
    }

    /// Run `sql` (via `loader`) only if the result is not already cached.
    pub fn get_or_load<F>(
        &self,
        path: &Path,
        sql: &str,
        delimiter: Delimiter,
        loader: F,
    ) -> io::Result<CachedResult>
    where
        F: FnOnce() -> io::Result<CachedResult>,
    {
        let key = key_for(path, sql, delimiter)?;
        if let Some(hit) = self.touch(&key) {
            return Ok(hit);
        }
        let value = loader()?;
        self.store(key, &value);
        Ok(value)
    }

    fn touch(&self, key: &Key) -> Option<CachedResult> {
        let mut inner = self.inner.lock().unwrap();
        inner.clock += 1;
        let clock = inner.clock;
        let entry = inner.entries.get_mut(key)?;
        entry.last_used = clock;
        Some(entry.value.clone())
    }

    fn store(&self, key: Key, value: &CachedResult) {
        // A truncated result is still useful: requests inside its prefix can be
        // served from it (`try_window` refuses anything beyond). It is counted
        // against the budget by the rows it actually holds.
        let weight = value.rows.len();
        let mut inner = self.inner.lock().unwrap();
        inner.clock += 1;
        let clock = inner.clock;
        inner.entries.insert(
            key,
            Entry {
                value: value.clone(),
                weight,
                last_used: clock,
            },
        );
        // Keep the total under budget by evicting the least recently used first.
        while inner.entries.values().map(|e| e.weight).sum::<usize>() > self.row_budget {
            let victim = inner
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    inner.entries.remove(&k);
                }
                None => break,
            }
        }
    }

    /// True when `path`/`sql` is already cached (and still valid).
    pub fn contains(&self, path: &Path, sql: &str, delimiter: Delimiter) -> bool {
        match key_for(path, sql, delimiter) {
            Ok(key) => self.inner.lock().unwrap().entries.contains_key(&key),
            Err(_) => false,
        }
    }

    /// Store a result under the key derived from `path`/`sql`.
    pub fn put(
        &self,
        path: &Path,
        sql: &str,
        delimiter: Delimiter,
        value: &CachedResult,
    ) -> io::Result<()> {
        let key = key_for(path, sql, delimiter)?;
        self.store(key, value);
        Ok(())
    }

    /// Number of cached results (mostly for tests and diagnostics).
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().entries.clear();
    }

    pub fn row_cap(&self) -> usize {
        self.row_cap
    }
}

fn key_for(path: &Path, sql: &str, delimiter: Delimiter) -> io::Result<Key> {
    let meta = std::fs::metadata(path)?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Ok(Key {
        path: path.to_path_buf(),
        len: meta.len(),
        mtime_ns,
        // Whitespace differences do not change meaning; trim so "SELECT 1" and
        // "SELECT 1 " share an entry.
        sql: sql.trim().to_string(),
        delimiter: delimiter.byte() as char,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn unique_seq() -> u64 {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed) as u64
    }

    fn fixture(rows: usize) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "csvs_cache_{}_{}",
            std::process::id(),
            unique_seq()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c.csv");
        let mut text = String::from("id,val\n");
        for i in 0..rows {
            text.push_str(&format!("{i},v{}\n", i % 10));
        }
        std::fs::write(&p, text).unwrap();
        (dir, p)
    }

    fn plain(rows: usize) -> CachedResult {
        CachedResult {
            columns: vec!["id".into()],
            rows: (0..rows).map(|i| vec![i.to_string()]).collect(),
            oversized: false,
        }
    }

    #[test]
    fn second_call_skips_the_loader() {
        let (_dir, p) = fixture(100);
        let cache = ResultCache::default();
        let calls = AtomicUsize::new(0);

        let load = || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(plain(100))
        };
        let first = cache
            .get_or_load(&p, "SELECT * FROM t", Delimiter::Comma, load)
            .unwrap();
        assert_eq!(first.total(), 100);

        let second = cache
            .get_or_load(&p, "SELECT * FROM t", Delimiter::Comma, || {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(plain(999))
            })
            .unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1, "loader ran twice");
        assert_eq!(second.total(), 100, "cached value should win");

        std::fs::remove_dir_all(&_dir).ok();
    }

    #[test]
    fn windows_slice_and_report_more() {
        let full = plain(10);
        let (rows, more) = full.window(7, 5);
        assert_eq!(rows.len(), 3);
        assert!(!more);
        let (rows, more) = full.window(0, 5);
        assert_eq!(rows.len(), 5);
        assert!(more);
        // Past the end: empty, no more.
        let (rows, more) = full.window(50, 5);
        assert!(rows.is_empty());
        assert!(!more);
    }

    #[test]
    fn truncated_prefix_is_served_inside_but_refused_beyond() {
        // A result that hit the cap can still answer windows inside the prefix,
        // and must refuse anything past it instead of claiming the query ended.
        let prefix = CachedResult {
            columns: vec!["id".into()],
            rows: (0..100).map(|i| vec![i.to_string()]).collect(),
            oversized: true,
        };
        let (rows, has_more) = prefix.try_window(0, 20).unwrap();
        assert_eq!(rows.len(), 20);
        assert!(has_more, "there may be more rows beyond the prefix");

        let (rows, _) = prefix.try_window(90, 20).unwrap();
        assert_eq!(rows.len(), 10, "the prefix tail is still available");

        assert!(prefix.try_window(100, 20).is_none());
        assert!(prefix.try_window(150, 20).is_none());
    }

    #[test]
    fn oversized_results_are_cached_as_prefixes() {
        let (dir, p) = fixture(10);
        let cache = ResultCache::new(1000, 50);
        let big = CachedResult {
            columns: vec!["id".into()],
            rows: vec![vec!["x".into()]; 100],
            oversized: true,
        };
        let mut calls = 0;
        cache
            .get_or_load(&p, "SELECT * FROM t", Delimiter::Comma, || {
                calls += 1;
                Ok(big.clone())
            })
            .unwrap();
        // The second call is answered from the cached prefix, not re-run.
        cache
            .get_or_load(&p, "SELECT * FROM t", Delimiter::Comma, || {
                calls += 1;
                Ok(big.clone())
            })
            .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(cache.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn budget_evicts_least_recently_used() {
        let (dir, p) = fixture(10);
        // Room for one 100-row result only.
        let cache = ResultCache::new(150, 100);
        let a = cache
            .get_or_load(&p, "A", Delimiter::Comma, || Ok(plain(100)))
            .unwrap();
        assert_eq!(a.total(), 100);
        cache
            .get_or_load(&p, "B", Delimiter::Comma, || Ok(plain(100)))
            .unwrap();
        // A should have been evicted to make room for B.
        assert_eq!(cache.len(), 1);
        let mut reloaded = false;
        cache
            .get_or_load(&p, "A", Delimiter::Comma, || {
                reloaded = true;
                Ok(plain(100))
            })
            .unwrap();
        assert!(reloaded, "A should have been evicted");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_change_invalidates_the_entry() {
        let (dir, p) = fixture(10);
        let cache = ResultCache::default();
        cache
            .get_or_load(&p, "Q", Delimiter::Comma, || Ok(plain(10)))
            .unwrap();

        // Rewrite the file with a different size.
        std::thread::sleep(std::time::Duration::from_millis(20));
        let mut text = String::from("id,val\n");
        for i in 0..200 {
            text.push_str(&format!("{i},v\n"));
        }
        std::fs::write(&p, text).unwrap();

        let mut reloaded = false;
        cache
            .get_or_load(&p, "Q", Delimiter::Comma, || {
                reloaded = true;
                Ok(plain(200))
            })
            .unwrap();
        assert!(reloaded, "stale entry should not be reused");
        std::fs::remove_dir_all(&dir).ok();
    }
}
