//! MCP stdio server: the thin agent-facing adapter over csvx.
//!
//! Tool execution deliberately shells out to the `csvx` binary, so this crate
//! owns the protocol and safety envelope but not the CSV engine.

use std::io;
use std::path::{Path, PathBuf};

mod rmcp_server;
pub use rmcp_server::run_stdio as run_rmcp_stdio;

/// The official rmcp transport; the old hand-written protocol loop is gone.
mod jobs;
mod tools;
pub use jobs::McpJobRegistry;
#[cfg(test)]
pub(crate) use jobs::{
    drop_mcp_job_files, prune_mcp_jobs, shutdown_mcp_jobs, McpJob, MCP_JOB_HISTORY,
};
pub use tools::mcp_tools;

pub fn run_mcp_stdio(root: &Path) -> io::Result<()> {
    run_rmcp_stdio(root)
}

/// The one-paragraph contract an agent should receive before it starts calling tools.
pub const AGENT_INSTRUCTIONS: &str = "csvx is a local, single-user CSV backend. Start with csvx_schema and csvx_view; inspect before editing. Use csvx_edit_preview, then csvx_edit_apply only after the preview is acceptable. All paths must stay inside the MCP root. Long work belongs in csvx_job_submit. Reports use csvx_report (CSV spec or specText); paging uses csvx_view; gates use csvx_gate; xlsx goes through csvx_convert. There is no GUI and no multi-user auth; keep destructive actions explicit and auditable. csvx://agent returns the machine-readable manifest.";

/// Minimal JSON value for the MCP envelope. Arguments are small and the protocol
/// needs one object per line, so this avoids a new dependency in the hot CLI.
#[derive(Clone, Debug)]
pub enum McpJson {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    #[allow(dead_code)]
    Array(Vec<McpJson>),
    Object(Vec<(String, McpJson)>),
}

impl McpJson {
    pub fn get(&self, key: &str) -> Option<&McpJson> {
        match self {
            Self::Object(pairs) => pairs
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }
    fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value) if *value >= 0.0 => Some(*value as u64),
            _ => None,
        }
    }
}

pub struct McpParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> McpParser<'a> {
    pub fn parse(text: &'a str) -> Result<McpJson, String> {
        let mut parser = Self {
            bytes: text.as_bytes(),
            pos: 0,
        };
        let value = parser.value()?;
        parser.whitespace();
        if parser.pos != parser.bytes.len() {
            return Err(format!("trailing input at byte {}", parser.pos));
        }
        Ok(value)
    }
    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.pos)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.pos += 1;
        }
    }
    fn value(&mut self) -> Result<McpJson, String> {
        self.whitespace();
        match self.bytes.get(self.pos).copied() {
            Some(b'n') => {
                self.literal(b"null")?;
                Ok(McpJson::Null)
            }
            Some(b't') => {
                self.literal(b"true")?;
                Ok(McpJson::Bool(true))
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(McpJson::Bool(false))
            }
            Some(b'"') => Ok(McpJson::String(self.string()?)),
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                self.whitespace();
                if self.bytes.get(self.pos) == Some(&b']') {
                    self.pos += 1;
                    return Ok(McpJson::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    self.whitespace();
                    match self.bytes.get(self.pos) {
                        Some(b',') => self.pos += 1,
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        _ => return Err(format!("expected , or ] at byte {}", self.pos)),
                    }
                }
                Ok(McpJson::Array(items))
            }
            Some(b'{') => {
                self.pos += 1;
                let mut pairs = Vec::new();
                self.whitespace();
                if self.bytes.get(self.pos) == Some(&b'}') {
                    self.pos += 1;
                    return Ok(McpJson::Object(pairs));
                }
                loop {
                    self.whitespace();
                    let key = self.string()?;
                    self.whitespace();
                    if self.bytes.get(self.pos) != Some(&b':') {
                        return Err(format!("expected : at byte {}", self.pos));
                    }
                    self.pos += 1;
                    let value = self.value()?;
                    pairs.push((key, value));
                    self.whitespace();
                    match self.bytes.get(self.pos) {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            break;
                        }
                        _ => return Err(format!("expected , or }} at byte {}", self.pos)),
                    }
                }
                Ok(McpJson::Object(pairs))
            }
            Some(byte) if byte == b'-' || byte.is_ascii_digit() => {
                let start = self.pos;
                if self.bytes[self.pos] == b'-' {
                    self.pos += 1;
                }
                while self.bytes.get(self.pos).is_some_and(|byte| {
                    byte.is_ascii_digit()
                        || *byte == b'.'
                        || *byte == b'e'
                        || *byte == b'E'
                        || *byte == b'-'
                        || *byte == b'+'
                }) {
                    self.pos += 1;
                }
                let raw =
                    std::str::from_utf8(&self.bytes[start..self.pos]).map_err(|e| e.to_string())?;
                raw.parse::<f64>()
                    .map(McpJson::Number)
                    .map_err(|_| format!("bad number {raw:?}"))
            }
            _ => Err(format!("unexpected value at byte {}", self.pos)),
        }
    }
    fn literal(&mut self, word: &[u8]) -> Result<(), String> {
        if self.bytes.get(self.pos..self.pos + word.len()) == Some(word) {
            self.pos += word.len();
            Ok(())
        } else {
            Err(format!(
                "expected {} at byte {}",
                String::from_utf8_lossy(word),
                self.pos
            ))
        }
    }
    fn string(&mut self) -> Result<String, String> {
        if self.bytes.get(self.pos) != Some(&b'"') {
            return Err(format!("expected string at byte {}", self.pos));
        }
        self.pos += 1;
        let mut out: Vec<u8> = Vec::new();
        while let Some(byte) = self.bytes.get(self.pos).copied() {
            self.pos += 1;
            match byte {
                b'"' => {
                    return String::from_utf8(out)
                        .map_err(|_| "invalid UTF-8 in string".to_string())
                }
                b'\\' => {
                    let escape = self
                        .bytes
                        .get(self.pos)
                        .copied()
                        .ok_or("unterminated escape")?;
                    self.pos += 1;
                    let ch = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{0008}',
                        b'f' => '\u{000c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hex = self
                                .bytes
                                .get(self.pos..self.pos + 4)
                                .ok_or("short unicode escape")?;
                            self.pos += 4;
                            let code =
                                u16::from_str_radix(std::str::from_utf8(hex).unwrap_or(""), 16)
                                    .map_err(|_| "bad unicode escape")?;
                            char::from_u32(code as u32).unwrap_or('\u{fffd}')
                        }
                        _ => return Err(format!("bad escape \\{escape}")),
                    };
                    out.extend_from_slice(ch.to_string().as_bytes());
                }
                _ => out.push(byte),
            }
        }
        Err("unterminated string".into())
    }
}

pub(crate) fn mcp_quote(text: &str) -> String {
    csvx_core::json::quote(text)
}

fn mcp_path(root: &Path, args: &McpJson) -> Result<std::path::PathBuf, String> {
    let raw = args
        .get("path")
        .and_then(McpJson::as_str)
        .ok_or("path is required")?;
    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        root.join(raw)
    };
    // Reject parent traversal lexically: a missing ../escape.csv must still be reported
    // as outside the root, not as an unrelated file-not-found.
    if candidate
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "path {} is outside the MCP root",
            candidate.display()
        ));
    }
    let canonical = std::fs::canonicalize(&candidate)
        .map_err(|e| format!("cannot resolve {}: {e}", candidate.display()))?;
    let root = std::fs::canonicalize(root).map_err(|e| format!("cannot resolve root: {e}"))?;
    if !canonical.starts_with(&root) {
        return Err(format!(
            "path {} is outside the MCP root",
            candidate.display()
        ));
    }
    Ok(canonical)
}

fn mcp_named_file(root: &Path, args: &McpJson, key: &str) -> Result<Option<PathBuf>, String> {
    let Some(raw) = args.get(key).and_then(McpJson::as_str) else {
        return Ok(None);
    };
    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        root.join(raw)
    };
    if candidate
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("{key} {raw} is outside the MCP root"));
    }
    let canonical = std::fs::canonicalize(&candidate)
        .map_err(|e| format!("cannot resolve {key} {raw}: {e}"))?;
    let root = std::fs::canonicalize(root).map_err(|e| format!("cannot resolve root: {e}"))?;
    if !canonical.starts_with(&root) {
        return Err(format!("{key} {raw} is outside the MCP root"));
    }
    Ok(Some(canonical))
}

fn mcp_output_path(root: &Path, args: &McpJson) -> Result<PathBuf, String> {
    let raw = args
        .get("out")
        .and_then(McpJson::as_str)
        .ok_or("out is required")?;
    let candidate = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        root.join(raw)
    };
    let parent = candidate.parent().ok_or("out has no parent directory")?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|e| format!("cannot resolve output directory: {e}"))?;
    let root = std::fs::canonicalize(root).map_err(|e| format!("cannot resolve root: {e}"))?;
    if !parent.starts_with(&root) {
        return Err("output is outside the MCP root".to_string());
    }
    Ok(candidate)
}

fn mcp_concat_command(root: &Path, args: &McpJson) -> Result<std::process::Command, String> {
    let bin = std::env::current_exe().map_err(|e| e.to_string())?;
    let paths = args
        .get("paths")
        .and_then(McpJson::as_str)
        .filter(|v| !v.trim().is_empty());
    let dir = args
        .get("dir")
        .and_then(McpJson::as_str)
        .filter(|v| !v.trim().is_empty());
    if paths.is_some() && dir.is_some() {
        return Err("use paths or dir, not both".to_string());
    }
    let Some(out) = mcp_output_path(root, args).ok() else {
        return Err("out must stay inside the MCP root".to_string());
    };
    let mut command = std::process::Command::new(bin);
    command.arg("concat");
    if let Some(list) = paths {
        let mut count = 0usize;
        for raw in list.split(',').map(str::trim).filter(|v| !v.is_empty()) {
            let candidate = if Path::new(raw).is_absolute() {
                PathBuf::from(raw)
            } else {
                root.join(raw)
            };
            let canonical = std::fs::canonicalize(&candidate)
                .map_err(|_| format!("{raw} is not a readable file inside the MCP root"))?;
            let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
            if !canonical.starts_with(&root) || !canonical.is_file() {
                return Err(format!("{raw} is outside the MCP root or not a file"));
            }
            command.arg(canonical);
            count += 1;
        }
        if count < 2 {
            return Err("concat needs at least two files".to_string());
        }
    } else if let Some(directory) = dir {
        let candidate = if Path::new(directory).is_absolute() {
            PathBuf::from(directory)
        } else {
            root.join(directory)
        };
        let canonical = std::fs::canonicalize(&candidate)
            .map_err(|_| format!("{directory} is not a readable directory inside the MCP root"))?;
        let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
        if !canonical.starts_with(&root) || !canonical.is_dir() {
            return Err(format!(
                "{directory} is outside the MCP root or not a directory"
            ));
        }
        command.arg(canonical);
    } else {
        return Err("paths or dir is required".to_string());
    }
    command.args(["-o", out.to_str().unwrap_or("")]);
    if args
        .get("byName")
        .and_then(|value| match value {
            McpJson::Bool(flag) => Some(*flag),
            _ => None,
        })
        .unwrap_or(false)
    {
        command.arg("--by-name");
    }
    if args
        .get("ignoreHeader")
        .and_then(|value| match value {
            McpJson::Bool(flag) => Some(*flag),
            _ => None,
        })
        .unwrap_or(false)
    {
        command.arg("--ignore-header");
    }
    command.arg("--json");
    Ok(command)
}

struct CachedMaterialized {
    key: String,
    path: std::path::PathBuf,
    size: u64,
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

fn cache_lookup(entries: &mut Vec<CachedMaterialized>, key: &str) -> Option<std::path::PathBuf> {
    let position = entries.iter().position(|entry| entry.key == key)?;
    let entry = entries.remove(position);
    if !entry.path.exists() {
        return None;
    }
    let path = entry.path.clone();
    entries.push(entry);
    Some(path)
}

fn cache_insert(
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

fn mcp_materialize(input: &Path) -> Result<std::path::PathBuf, String> {
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

pub(crate) fn mcp_command(
    root: &Path,
    name: &str,
    args: &McpJson,
) -> Result<std::process::Command, String> {
    let bin = std::env::current_exe().map_err(|e| e.to_string())?;
    // Tool identity is checked before the path: an unknown tool must not be reported as
    // a missing path (the caller did pass a path, it just named the wrong tool).
    let known = matches!(
        name,
        "csvx_schema"
            | "csvx_view"
            | "csvx_filter"
            | "csvx_search"
            | "csvx_quality"
            | "csvx_pipeline_status"
            | "csvx_edit_set"
            | "csvx_edit_delete"
            | "csvx_edit_undo"
            | "csvx_edit_redo"
            | "csvx_edit_preview"
            | "csvx_edit_apply"
            | "csvx_report"
            | "csvx_gate"
            | "csvx_concat"
            | "csvx_convert"
            | "csvx_pipeline_run"
    );
    if !known {
        return Err(format!("unknown tool {name}"));
    }
    if name == "csvx_concat" {
        return mcp_concat_command(root, args);
    }
    let path = mcp_path(root, args)?;
    let wants_materialize = matches!(name, "csvx_filter" | "csvx_search")
        && args
            .get("materialize")
            .and_then(|value| match value {
                McpJson::Bool(flag) => Some(*flag),
                _ => None,
            })
            .unwrap_or(false);
    let path = if wants_materialize {
        mcp_materialize(&path).map_err(|error| error.to_string())?
    } else {
        path
    };
    let mut command = std::process::Command::new(bin);
    match name {
        "csvx_schema" => {
            command.args(["schema", path.to_str().unwrap_or(""), "--json"]);
        }
        "csvx_view" => {
            command.args(["view", path.to_str().unwrap_or(""), "--json"]);
            let start = args.get("start").and_then(McpJson::as_u64).unwrap_or(0);
            let count = args
                .get("count")
                .and_then(McpJson::as_u64)
                .unwrap_or(100)
                .min(1000);
            command.args(["--start", &start.to_string(), "--count", &count.to_string()]);
            if let Some(where_clause) = args.get("where").and_then(McpJson::as_str) {
                command.args(["--where", where_clause]);
            }
        }
        "csvx_filter" => {
            let where_clause = args
                .get("where")
                .and_then(McpJson::as_str)
                .ok_or("where is required")?;
            let limit = args
                .get("limit")
                .and_then(McpJson::as_u64)
                .unwrap_or(100)
                .min(1000);
            command.args([
                "filter",
                path.to_str().unwrap_or(""),
                "--where",
                where_clause,
                "--json",
                "--limit",
                &limit.to_string(),
            ]);
            if args
                .get("show")
                .and_then(|value| match value {
                    McpJson::Bool(flag) => Some(*flag),
                    _ => None,
                })
                .unwrap_or(true)
            {
                command.args(["--show", &limit.to_string()]);
            }
        }
        "csvx_search" => {
            let query = args
                .get("query")
                .and_then(McpJson::as_str)
                .ok_or("query is required")?;
            let limit = args
                .get("limit")
                .and_then(McpJson::as_u64)
                .unwrap_or(100)
                .min(1000);
            command.args([
                "search",
                path.to_str().unwrap_or(""),
                query,
                "--json",
                "--limit",
                &limit.to_string(),
            ]);
            if let Some(columns) = args.get("columns").and_then(McpJson::as_str) {
                command.args(["--columns", columns]);
            }
        }
        "csvx_quality" => {
            command.args([
                "analyze",
                path.to_str().unwrap_or(""),
                "--quality",
                "--json",
            ]);
            if let Some(key) = args.get("key").and_then(McpJson::as_str) {
                command.args(["--key", key]);
            }
            if let Some(group) = args.get("group").and_then(McpJson::as_str) {
                command.args(["--group", group]);
            }
        }
        "csvx_pipeline_status" => {
            command.args(["pipeline", "status", path.to_str().unwrap_or(""), "--json"]);
            if args
                .get("verify")
                .and_then(|value| match value {
                    McpJson::Bool(flag) => Some(*flag),
                    _ => None,
                })
                .unwrap_or(false)
            {
                command.arg("--verify");
            }
            if args
                .get("contentCheck")
                .and_then(|value| match value {
                    McpJson::Bool(flag) => Some(*flag),
                    _ => None,
                })
                .unwrap_or(false)
            {
                command.arg("--content-check");
            }
        }
        "csvx_report" => {
            let format = args.get("format").and_then(McpJson::as_str).unwrap_or("md");
            let format = if format.eq_ignore_ascii_case("html") {
                "html"
            } else {
                "md"
            };
            command.args([
                "report",
                path.to_str().unwrap_or(""),
                "--root",
                root.to_str().unwrap_or(""),
                "--format",
                format,
            ]);
            if let Some(text) = args.get("specText").and_then(McpJson::as_str) {
                command.args(["--spec-text", text]);
            } else if let Some(spec) = mcp_named_file(root, args, "spec")? {
                command.args(["--spec", spec.to_str().unwrap_or("")]);
            }
            if let Some(title) = args.get("title").and_then(McpJson::as_str) {
                command.args(["--title", title]);
            }
        }
        "csvx_pipeline_run" => {
            command.args([
                "pipeline",
                "run",
                path.to_str().unwrap_or(""),
                "--root",
                root.to_str().unwrap_or(""),
                "--json",
            ]);
            if args
                .get("force")
                .and_then(|value| match value {
                    McpJson::Bool(flag) => Some(*flag),
                    _ => None,
                })
                .unwrap_or(false)
            {
                command.arg("--force");
            }
            if args
                .get("dryRun")
                .and_then(|value| match value {
                    McpJson::Bool(flag) => Some(*flag),
                    _ => None,
                })
                .unwrap_or(false)
            {
                command.arg("--dry-run");
            }
            if args
                .get("dag")
                .and_then(|value| match value {
                    McpJson::Bool(flag) => Some(*flag),
                    _ => None,
                })
                .unwrap_or(false)
            {
                command.arg("--dag");
            }
            if let Some(jobs) = args.get("jobs").and_then(McpJson::as_u64) {
                command.args(["--jobs", &jobs.clamp(1, 64).to_string()]);
            }
            if args
                .get("clean")
                .and_then(|value| match value {
                    McpJson::Bool(flag) => Some(*flag),
                    _ => None,
                })
                .unwrap_or(false)
            {
                command.arg("--clean");
            }
            if args
                .get("contentCheck")
                .and_then(|value| match value {
                    McpJson::Bool(flag) => Some(*flag),
                    _ => None,
                })
                .unwrap_or(false)
            {
                command.arg("--content-check");
            }
        }
        "csvx_edit_set" => {
            let row = args
                .get("row")
                .and_then(McpJson::as_u64)
                .ok_or("row is required")?;
            let col = args
                .get("col")
                .and_then(McpJson::as_str)
                .ok_or("col is required")?;
            let value = args
                .get("value")
                .and_then(McpJson::as_str)
                .ok_or("value is required")?;
            command.args([
                "edit",
                "set",
                path.to_str().unwrap_or(""),
                "--row",
                &row.to_string(),
                "--col",
                col,
                "--value",
                value,
                "--json",
            ]);
            if let Some(expect) = args.get("expect").and_then(McpJson::as_str) {
                command.args(["--expect", expect]);
            }
        }
        "csvx_edit_delete" => {
            command.args(["edit", "delete", path.to_str().unwrap_or(""), "--json"]);
            if let Some(row) = args.get("row").and_then(McpJson::as_u64) {
                command.args(["--row", &row.to_string()]);
            } else if let Some(where_clause) = args.get("where").and_then(McpJson::as_str) {
                command.args(["--where", where_clause]);
            } else {
                return Err("row or where is required".to_string());
            }
        }
        "csvx_edit_preview" => {
            let ops = args
                .get("ops")
                .and_then(McpJson::as_str)
                .ok_or("ops is required")?;
            command.args([
                "edit",
                "transaction",
                path.to_str().unwrap_or(""),
                "--ops",
                ops,
                "--dry-run",
                "--json",
            ]);
        }
        "csvx_edit_apply" => {
            let ops = args
                .get("ops")
                .and_then(McpJson::as_str)
                .ok_or("ops is required")?;
            command.args([
                "edit",
                "transaction",
                path.to_str().unwrap_or(""),
                "--ops",
                ops,
                "--json",
            ]);
        }
        "csvx_edit_undo" => {
            command.args(["edit", "undo", path.to_str().unwrap_or(""), "--json"]);
        }
        "csvx_edit_redo" => {
            command.args(["edit", "redo", path.to_str().unwrap_or(""), "--json"]);
        }
        "csvx_gate" => {
            command.args(["gate", path.to_str().unwrap_or(""), "--json"]);
            if let Some(minimum) = args.get("minRows").and_then(McpJson::as_u64) {
                command.args(["--min-rows", &minimum.to_string()]);
            }
            if let Some(maximum) = args.get("maxEmpty").and_then(McpJson::as_u64) {
                command.args(["--max-empty", &maximum.to_string()]);
            }
            if let Some(name) = args.get("unique").and_then(McpJson::as_str) {
                command.args(["--unique", name]);
            }
            if args
                .get("uniqueGlobal")
                .and_then(|value| match value {
                    McpJson::Bool(flag) => Some(*flag),
                    _ => None,
                })
                .unwrap_or(false)
            {
                command.arg("--unique-global");
            }
        }
        "csvx_convert" => {
            let format = args
                .get("format")
                .and_then(McpJson::as_str)
                .ok_or("format is required")?;
            let out = mcp_output_path(root, args)?;
            command.args([
                "convert",
                path.to_str().unwrap_or(""),
                "--format",
                format,
                "--out",
                out.to_str().unwrap_or(""),
            ]);
            if let Some(sheet) = args.get("sheet").and_then(McpJson::as_str) {
                command.args(["--sheet", sheet]);
            }
        }
        _ => return Err(format!("unknown tool {name}")),
    }
    Ok(command)
}

pub fn mcp_run_tool(root: &Path, name: &str, args: &McpJson) -> Result<String, String> {
    let mut command = mcp_command(root, name, args)?;
    command.stdin(std::process::Stdio::null());
    let output = command
        .output()
        .map_err(|e| format!("cannot run csvx: {e}"))?;
    if !output.status.success() {
        if name == "csvx_gate" && !output.stdout.is_empty() {
            return Err(format!(
                "gate failed: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            ));
        }
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn agent_manifest_json() -> String {
    format!(
        r#"{{"protocol":"csvx-agent/1","transport":["mcp-stdio","http"],"tools":{},"workflow":["schema","view","edit_preview","edit_apply","report","gate"],"safety":{{"preview_before_apply":true,"root_bound":true,"single_user":true,"gui":false}},"endpoints":{{"manifest":"/agent/v1/manifest","sessions":"/agent/v1/sessions","actions":"/agent/v1/sessions/{{id}}/actions","audit":"/agent/v1/sessions/{{id}}/audit","replay":"/agent/v1/sessions/{{id}}/replay","curl":"/agent/v1/sessions/{{id}}/curl","runbook":"/agent/v1/sessions/{{id}}/runbook.md","approvals":"/agent/v1/sessions/{{id}}/approvals"}},"instructions":{}}}"#,
        mcp_tools(),
        mcp_quote(AGENT_INSTRUCTIONS)
    )
}

/// Shared job and cache state used by the official rmcp adapter.
#[cfg(test)]
fn cleanup_mcp_materialized() {
    if let Some(cache) = MATERIALIZE_CACHE.get() {
        if let Ok(mut entries) = cache.lock() {
            for entry in entries.drain(..) {
                let _ = std::fs::remove_file(entry.path);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod mcp_job_shutdown_tests {
    use super::*;

    #[test]
    fn shutdown_kills_a_running_job_and_removes_its_files() {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let prefix = format!("csvx-mcp-shutdown-{}", std::process::id());
        let stdout = std::env::temp_dir().join(format!("{prefix}.out"));
        let stderr = std::env::temp_dir().join(format!("{prefix}.err"));
        std::fs::write(&stdout, b"partial").unwrap();
        std::fs::write(&stderr, b"partial").unwrap();
        let mut jobs = std::collections::HashMap::new();
        jobs.insert(
            "job-running".to_string(),
            McpJob {
                child,
                stdout: stdout.clone(),
                stderr: stderr.clone(),
                result: None,
                started: std::time::Instant::now(),
            },
        );
        shutdown_mcp_jobs(&mut jobs);
        let job = jobs.get_mut("job-running").expect("job retained");
        assert!(
            job.child.try_wait().unwrap().is_some(),
            "running child should be reaped"
        );
        assert!(!stdout.exists());
        assert!(!stderr.exists());
    }
}

#[cfg(all(test, unix))]
mod mcp_job_retention_tests {
    use super::*;

    #[test]
    fn prune_keeps_the_cap_and_deletes_the_oldest_files() {
        let mut jobs = std::collections::HashMap::new();
        let prefix = format!("csvx-mcp-prune-{}", std::process::id());
        for index in 0..=MCP_JOB_HISTORY {
            let mut child = std::process::Command::new("true")
                .spawn()
                .expect("spawn true");
            child.wait().expect("wait true");
            let stdout = std::env::temp_dir().join(format!("{prefix}-{index}.out"));
            let stderr = std::env::temp_dir().join(format!("{prefix}-{index}.err"));
            std::fs::write(&stdout, b"ok").unwrap();
            std::fs::write(&stderr, b"").unwrap();
            jobs.insert(
                format!("job-{index}"),
                McpJob {
                    child,
                    stdout,
                    stderr,
                    result: Some((true, 0, "ok".to_string())),
                    started: std::time::Instant::now(),
                },
            );
        }
        assert_eq!(jobs.len(), MCP_JOB_HISTORY + 1);
        // Called just before an insert, pruning leaves one free slot.
        prune_mcp_jobs(&mut jobs);
        assert_eq!(jobs.len(), MCP_JOB_HISTORY - 1);
        let remaining = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
            .count();
        assert_eq!(remaining, (MCP_JOB_HISTORY - 1) * 2);
        let mut child = std::process::Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        let stdout = std::env::temp_dir().join(format!("{prefix}-new.out"));
        let stderr = std::env::temp_dir().join(format!("{prefix}-new.err"));
        std::fs::write(&stdout, b"ok").unwrap();
        std::fs::write(&stderr, b"").unwrap();
        jobs.insert(
            "job-new".to_string(),
            McpJob {
                child,
                stdout,
                stderr,
                result: Some((true, 0, "ok".to_string())),
                started: std::time::Instant::now(),
            },
        );
        assert_eq!(jobs.len(), MCP_JOB_HISTORY);
        for job in jobs.values() {
            drop_mcp_job_files(job);
        }
    }
}

#[cfg(test)]
mod moved_cache_tests {
    use super::*;

    #[test]
    fn startup_cleanup_removes_dead_materialized_files() {
        let dir = std::env::temp_dir();
        let dead = dir.join("csvx-materialized-4294967295-1.csv");
        let live = dir.join(format!("csvx-materialized-{}-test.csv", std::process::id()));
        std::fs::write(&dead, "x").unwrap();
        std::fs::write(&live, "x").unwrap();
        let removed = cleanup_stale_mcp_materialized();
        assert_eq!(removed, 1);
        assert!(!dead.exists());
        assert!(live.exists());
        std::fs::remove_file(live).ok();
    }

    #[test]
    fn cache_evicts_lru_when_limit_is_exceeded() {
        let dir = std::env::temp_dir().join(format!("csvs_cache_evict_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.csv");
        let second = dir.join("second.csv");
        std::fs::write(&first, "a\n1\n").unwrap();
        std::fs::write(&second, "a\n2\n").unwrap();
        let mut entries = Vec::new();
        cache_insert(
            &mut entries,
            CachedMaterialized {
                key: "first".to_string(),
                path: first.clone(),
                size: 10,
            },
            100,
            1,
        );
        cache_insert(
            &mut entries,
            CachedMaterialized {
                key: "second".to_string(),
                path: second.clone(),
                size: 10,
            },
            100,
            1,
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "second");
        assert!(!first.exists());
        assert!(second.exists());
        cleanup_mcp_materialized();
        std::fs::remove_file(&second).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cache_lookup_moves_a_hit_to_the_back() {
        let dir = std::env::temp_dir().join(format!("csvs_cache_lru_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.csv");
        let second = dir.join("second.csv");
        let third = dir.join("third.csv");
        for path in [&first, &second, &third] {
            std::fs::write(path, "a\n1\n").unwrap();
        }
        let mut entries = Vec::new();
        for (key, path) in [("first", &first), ("second", &second)] {
            cache_insert(
                &mut entries,
                CachedMaterialized {
                    key: key.to_string(),
                    path: path.clone(),
                    size: 10,
                },
                100,
                2,
            );
        }
        assert_eq!(cache_lookup(&mut entries, "first"), Some(first.clone()));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        cache_insert(
            &mut entries,
            CachedMaterialized {
                key: "third".to_string(),
                path: third.clone(),
                size: 10,
            },
            100,
            2,
        );
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "third"]
        );
        assert!(!second.exists());
        assert!(first.exists());
        assert!(third.exists());
        cleanup_mcp_materialized();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn materialize_cache_reuses_one_temp_file() {
        let dir = std::env::temp_dir().join(format!("csvs_materialize_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("orders.csv");
        std::fs::write(&source, "id,city\n1,CDMX\n").unwrap();
        let gzip = dir.join("orders.csv.gz");
        let status = std::process::Command::new("gzip")
            .arg("-c")
            .arg(&source)
            .stdout(std::fs::File::create(&gzip).unwrap())
            .status();
        if !status.map(|status| status.success()).unwrap_or(false) {
            std::fs::remove_dir_all(&dir).ok();
            return;
        }
        let first = mcp_materialize(&gzip).unwrap();
        let second = mcp_materialize(&gzip).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "id,city\n1,CDMX\n"
        );
        cleanup_mcp_materialized();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn mcp_tools_is_valid_json_and_has_all_tools() {
        let value = McpParser::parse(mcp_tools()).expect("tools/list must be valid JSON");
        let McpJson::Array(tools) = value else {
            panic!("tools/list must be an array");
        };
        assert_eq!(tools.len(), 23);
        for tool in &tools {
            let McpJson::Object(pairs) = tool else {
                panic!("tool must be an object");
            };
            assert!(pairs.iter().any(|(key, _)| key == "name"));
            assert!(pairs.iter().any(|(key, _)| key == "inputSchema"));
        }
        assert!(mcp_tools().contains(
        r#""name":"csvx_cache_clear","description":"Delete cached materialized temp files, clear the cache and sweep dead-process temp files; returns removed/stale counts and bytes.","annotations":{"destructiveHint":true,"idempotentHint":true}"#
    ));
        assert!(mcp_tools().contains(
        r#""name":"csvx_cache_stats","description":"Materialize-cache and read-window hits/misses, entries and byte usage in this server process.","annotations":{"readOnlyHint":true}"#
    ));
    }
}
