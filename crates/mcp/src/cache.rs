use std::path::Path;

pub(crate) struct CachedMaterialized {
    pub(crate) key: String,
    pub(crate) path: std::path::PathBuf,
    pub(crate) size: u64,
}

const MATERIALIZE_CACHE_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MATERIALIZE_CACHE_MAX_ENTRIES: usize = 16;

pub(crate) fn materialize_cache_limits() -> (u64, usize) {
    let bytes = std::env::var("CSVX_MCP_CACHE_BYTES")
        .ok()
        .and_then(|value| match value.parse::<u64>() {
            Ok(parsed) if parsed > 0 => Some(parsed),
            _ => {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    eprintln!(
                        "warning: CSVX_MCP_CACHE_BYTES={value:?} is invalid; using default {}",
                        MATERIALIZE_CACHE_MAX_BYTES
                    );
                });
                None
            }
        });
    let entries = std::env::var("CSVX_MCP_CACHE_ENTRIES")
        .ok()
        .and_then(|value| match value.parse::<usize>() {
            Ok(parsed) if parsed > 0 => Some(parsed),
            _ => {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    eprintln!(
                        "warning: CSVX_MCP_CACHE_ENTRIES={value:?} is invalid; using default {}",
                        MATERIALIZE_CACHE_MAX_ENTRIES
                    );
                });
                None
            }
        });
    (
        bytes.unwrap_or(MATERIALIZE_CACHE_MAX_BYTES),
        entries.unwrap_or(MATERIALIZE_CACHE_MAX_ENTRIES),
    )
}

pub(crate) fn cache_lookup(
    entries: &mut Vec<CachedMaterialized>,
    key: &str,
) -> Option<std::path::PathBuf> {
    let position = entries.iter().position(|entry| entry.key == key)?;
    let entry = entries.remove(position);
    if !entry.path.exists() {
        return None;
    }
    let path = entry.path.clone();
    entries.push(entry);
    Some(path)
}

pub(crate) fn cache_insert(
    entries: &mut Vec<CachedMaterialized>,
    entry: CachedMaterialized,
    max_bytes: u64,
    max_entries: usize,
) {
    let mut total: u64 = entries.iter().map(|entry| entry.size).sum();
    while !entries.is_empty()
        && (entries.len() >= max_entries || total.saturating_add(entry.size) > max_bytes)
    {
        let evicted = entries.remove(0);
        total = total.saturating_sub(evicted.size);
        let _ = std::fs::remove_file(evicted.path);
    }
    entries.push(entry);
}

pub(crate) static MATERIALIZE_CACHE: std::sync::OnceLock<
    std::sync::Mutex<Vec<CachedMaterialized>>,
> = std::sync::OnceLock::new();

fn mcp_process_alive(pid: u32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output();
    match output {
        Ok(output) => output.status.success() && !output.stdout.is_empty(),
        Err(_) => true,
    }
}

pub(crate) fn cleanup_stale_mcp_materialized() -> usize {
    let mut removed = 0usize;
    let dir = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return removed;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("csvx-materialized-") else {
            continue;
        };
        let Some(pid) = rest
            .split('-')
            .next()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != std::process::id()
            && !mcp_process_alive(pid)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

pub(crate) static MATERIALIZE_HITS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub(crate) static MATERIALIZE_MISSES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub(crate) fn mcp_materialize(input: &Path) -> Result<std::path::PathBuf, String> {
    if !csvx_core::encoding::is_compressed(input) {
        return Ok(input.to_path_buf());
    }
    let metadata = std::fs::metadata(input).map_err(|error| error.to_string())?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        format!(
            "{}:{}",
            metadata.ino(),
            metadata.ctime_nsec() + metadata.ctime() * 1_000_000_000
        )
    };
    #[cfg(not(unix))]
    let identity = "0".to_string();
    let key = format!(
        "{}:{}:{}:{}",
        input
            .canonicalize()
            .unwrap_or_else(|_| input.to_path_buf())
            .display(),
        metadata.len(),
        modified,
        identity
    );
    let cache = MATERIALIZE_CACHE.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    if let Ok(mut entries) = cache.lock() {
        if let Some(path) = cache_lookup(&mut entries, &key) {
            MATERIALIZE_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(path);
        }
    }
    MATERIALIZE_MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    );
    let path = std::env::temp_dir().join(format!("csvx-materialized-{stamp}.csv"));
    let mut reader = csvx_core::encoding::open_decoded(input).map_err(|error| error.to_string())?;
    let mut writer = std::fs::File::create(&path).map_err(|error| error.to_string())?;
    std::io::copy(&mut reader, &mut writer).map_err(|error| error.to_string())?;
    let size = std::fs::metadata(&path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let (max_bytes, max_entries) = materialize_cache_limits();
    if let Ok(mut entries) = cache.lock() {
        cache_insert(
            &mut entries,
            CachedMaterialized {
                key,
                path: path.clone(),
                size,
            },
            max_bytes,
            max_entries,
        );
    }
    Ok(path)
}

#[cfg(test)]
pub(crate) fn cleanup_mcp_materialized() {
    if let Some(cache) = MATERIALIZE_CACHE.get() {
        if let Ok(mut entries) = cache.lock() {
            for entry in entries.drain(..) {
                let _ = std::fs::remove_file(entry.path);
            }
        }
    }
}
