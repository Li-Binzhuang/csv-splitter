//! MCP stdio server: the thin agent-facing adapter over csvx.
//!
//! Tool execution deliberately shells out to the `csvx` binary, so this crate
//! owns the protocol and safety envelope but not the CSV engine.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

mod rmcp_server;
pub use rmcp_server::run_stdio as run_rmcp_stdio;

/// The official rmcp transport; the old hand-written protocol loop is gone.
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

struct McpJob {
    child: std::process::Child,
    stdout: PathBuf,
    stderr: PathBuf,
    result: Option<(bool, i32, String)>,
    started: std::time::Instant,
}

fn shutdown_mcp_jobs(jobs: &mut std::collections::HashMap<String, McpJob>) {
    for job in jobs.values_mut() {
        if job.result.is_none() {
            let _ = job.child.kill();
            let _ = job.child.wait();
        }
        drop_mcp_job_files(job);
    }
}

const MCP_JOB_HISTORY: usize = 128;

fn drop_mcp_job_files(job: &McpJob) {
    let _ = std::fs::remove_file(&job.stdout);
    let _ = std::fs::remove_file(&job.stderr);
}

fn prune_mcp_jobs(jobs: &mut std::collections::HashMap<String, McpJob>) {
    while jobs.len() >= MCP_JOB_HISTORY {
        let oldest = jobs
            .iter()
            .filter(|(_, job)| job.result.is_some())
            .min_by_key(|(_, job)| job.started)
            .map(|(id, _)| id.clone());
        let Some(oldest) = oldest else {
            break;
        };
        if let Some(job) = jobs.remove(&oldest) {
            drop_mcp_job_files(&job);
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

fn mcp_quote(text: &str) -> String {
    csvx_core::json::quote(text)
}

pub fn mcp_tools() -> &'static str {
    r#"[{"name":"csvx_schema","description":"Columns, types and row count for a CSV; read-only.","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false},"annotations":{"readOnlyHint":true}},{"name":"csvx_view","description":"A page of rows; start/count cursor paging; optional where.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"start":{"type":"integer","minimum":0},"count":{"type":"integer","minimum":1,"maximum":1000},"where":{"type":"string"}},"required":["path"],"additionalProperties":false},"annotations":{"readOnlyHint":true}},{"name":"csvx_filter","description":"Evaluate a row predicate and return matching rows or row numbers.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"where":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":1000},"show":{"type":"boolean"},"materialize":{"type":"boolean"}},"required":["path","where"],"additionalProperties":false},"annotations":{"readOnlyHint":true}},{"name":"csvx_search","description":"Search text across columns or a chosen column set.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"query":{"type":"string"},"columns":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":1000},"materialize":{"type":"boolean"}},"required":["path","query"],"additionalProperties":false},"annotations":{"readOnlyHint":true}},{"name":"csvx_quality","description":"Quality report: candidate keys, duplicates, mixed types, outliers, groups.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"key":{"type":"string"},"group":{"type":"string"}},"required":["path"],"additionalProperties":false},"annotations":{"readOnlyHint":true}},{"name":"csvx_pipeline_status","description":"Read .pipeline.csv; verify=true re-checks current stamps/hashes.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"verify":{"type":"boolean"},"contentCheck":{"type":"boolean"}},"required":["path"],"additionalProperties":false},"annotations":{"readOnlyHint":true}},{"name":"csvx_edit_preview","description":"Preview a transaction batch; returns row delta, affected ranges and journal hash; never writes.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"ops":{"type":"string"}},"required":["path","ops"],"additionalProperties":false},"annotations":{"readOnlyHint":true}},{"name":"csvx_edit_apply","description":"Apply a validated transaction batch in one journal write; preview first.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"ops":{"type":"string"}},"required":["path","ops"],"additionalProperties":false},"annotations":{"destructiveHint":true}},{"name":"csvx_edit_set","description":"Set one cell (1-based row, column name); uses the journal and write lock.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"row":{"type":"integer","minimum":1},"col":{"type":"string"},"value":{"type":"string"},"expect":{"type":"string"}},"required":["path","row","col","value"],"additionalProperties":false},"annotations":{"destructiveHint":true}},{"name":"csvx_edit_delete","description":"Delete one row by number or every row matching a predicate.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"row":{"type":"integer","minimum":1},"where":{"type":"string"}},"required":["path"],"additionalProperties":false},"annotations":{"destructiveHint":true}},{"name":"csvx_edit_undo","description":"Undo the last journal batch.","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false},"annotations":{"destructiveHint":true}},{"name":"csvx_edit_redo","description":"Redo the last undone batch.","inputSchema":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false},"annotations":{"destructiveHint":true}},{"name":"csvx_concat","description":"Concatenate CSV files or a directory into a new file; destructive write.","inputSchema":{"type":"object","properties":{"paths":{"type":"string"},"dir":{"type":"string"},"out":{"type":"string"},"byName":{"type":"boolean"},"ignoreHeader":{"type":"boolean"}},"required":["out"],"additionalProperties":false},"annotations":{"destructiveHint":true}},{"name":"csvx_convert","description":"Convert to csv/tsv/ndjson/parquet/xlsx; xlsx input needs format=csv and optional sheet; writes a new file.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"format":{"type":"string","enum":["csv","tsv","ndjson","parquet","xlsx"]},"out":{"type":"string"},"sheet":{"type":"string"}},"required":["path","format","out"],"additionalProperties":false},"annotations":{"destructiveHint":true}},{"name":"csvx_job_submit","description":"Run a tool asynchronously; returns a job id for polling.","inputSchema":{"type":"object","properties":{"tool":{"type":"string"},"arguments":{"type":"object"}},"required":["tool","arguments"],"additionalProperties":false},"annotations":{"openWorldHint":true}},{"name":"csvx_job_status","description":"Poll a submitted job; wait>0 long-polls up to N seconds.","inputSchema":{"type":"object","properties":{"id":{"type":"string"},"wait":{"type":"integer","minimum":0,"maximum":30}},"required":["id"],"additionalProperties":false},"annotations":{"readOnlyHint":true}},{"name":"csvx_pipeline_run","description":"Run a declarative CSV pipeline; long runs can be wrapped in a job.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"force":{"type":"boolean"},"dryRun":{"type":"boolean"},"dag":{"type":"boolean"},"jobs":{"type":"integer","minimum":1,"maximum":64},"clean":{"type":"boolean"},"contentCheck":{"type":"boolean"}},"required":["path"],"additionalProperties":false},"annotations":{"destructiveHint":true}},{"name":"csvx_job_cancel","description":"Cancel a running job; returns cancelled, done or failed.","inputSchema":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false},"annotations":{"destructiveHint":true}},{"name":"csvx_report","description":"Render a CSV-spec report; spec may be a file or inline text; returns Markdown or HTML; read-only.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"spec":{"type":"string"},"specText":{"type":"string"},"format":{"type":"string","enum":["md","html"]},"title":{"type":"string"}},"required":["path"],"additionalProperties":false},"annotations":{"readOnlyHint":true}},{"name":"csvx_gate","description":"Quality gate: row/empty/uniqueness thresholds for a file or directory; uniqueGlobal adds cross-file uniqueness; returns JSON; read-only.","inputSchema":{"type":"object","properties":{"path":{"type":"string"},"minRows":{"type":"integer","minimum":0},"maxEmpty":{"type":"integer","minimum":0},"unique":{"type":"string"},"uniqueGlobal":{"type":"boolean"}},"required":["path"],"additionalProperties":false},"annotations":{"readOnlyHint":true}},{"name":"csvx_cache_clear","description":"Delete cached materialized temp files, clear the cache and sweep dead-process temp files; returns removed/stale counts and bytes.","annotations":{"destructiveHint":true,"idempotentHint":true},"inputSchema":{"type":"object","properties":{},"additionalProperties":false}},{"name":"csvx_cache_stats","description":"Materialize-cache and read-window hits/misses, entries and byte usage in this server process.","annotations":{"readOnlyHint":true},"inputSchema":{"type":"object","properties":{},"additionalProperties":false}},{"name":"csvx_job_list","description":"List jobs; full=true includes job output.","inputSchema":{"type":"object","properties":{"full":{"type":"boolean"}},"additionalProperties":false},"annotations":{"readOnlyHint":true}}]"#
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

fn materialize_cache_limits() -> (u64, usize) {
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

static MATERIALIZE_CACHE: std::sync::OnceLock<std::sync::Mutex<Vec<CachedMaterialized>>> =
    std::sync::OnceLock::new();

fn mcp_process_alive(pid: u32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output();
    match output {
        Ok(output) => output.status.success() && !output.stdout.is_empty(),
        Err(_) => true,
    }
}

fn cleanup_stale_mcp_materialized() -> usize {
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

static MATERIALIZE_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static MATERIALIZE_MISSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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

fn mcp_command(root: &Path, name: &str, args: &McpJson) -> Result<std::process::Command, String> {
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

fn mcp_job_tool_allowed(tool: &str) -> bool {
    matches!(
        tool,
        "csvx_schema"
            | "csvx_view"
            | "csvx_filter"
            | "csvx_search"
            | "csvx_quality"
            | "csvx_pipeline_status"
            | "csvx_pipeline_run"
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
    )
}

fn mcp_start_job(
    root: &Path,
    tool: &str,
    arguments: &McpJson,
    job_id: &str,
) -> Result<McpJob, String> {
    if tool.starts_with("csvx_job_") {
        return Err("job tools cannot be submitted as jobs".to_string());
    }
    let mut command = mcp_command(root, tool, arguments)?;
    let stamp = format!("{}-{}", std::process::id(), job_id);
    let stdout = std::env::temp_dir().join(format!("csvx-mcp-{stamp}.out"));
    let stderr = std::env::temp_dir().join(format!("csvx-mcp-{stamp}.err"));
    let out = std::fs::File::create(&stdout).map_err(|e| e.to_string())?;
    let err = std::fs::File::create(&stderr).map_err(|e| e.to_string())?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(out))
        .stderr(std::process::Stdio::from(err));
    let child = command
        .spawn()
        .map_err(|e| format!("cannot start job: {e}"))?;
    Ok(McpJob {
        child,
        stdout,
        stderr,
        result: None,
        started: std::time::Instant::now(),
    })
}

fn mcp_cancel_job(job_id: &str, job: &mut McpJob) -> String {
    if job.result.is_some() {
        return mcp_job_text(job_id, job);
    }
    match job.child.try_wait() {
        Ok(None) => {
            let _ = job.child.kill();
            let _ = job.child.wait();
            let _ = std::fs::remove_file(&job.stdout);
            let _ = std::fs::remove_file(&job.stderr);
            job.result = Some((false, -1, "cancelled".to_string()));
            format!(r#"{{"job":{},"state":"cancelled"}}"#, mcp_quote(job_id))
        }
        _ => mcp_job_text(job_id, job),
    }
}

fn mcp_cache_clear() -> String {
    csvx_core::workbook::clear_window_cache();
    let mut removed = 0usize;
    let mut bytes = 0u64;
    if let Some(cache) = MATERIALIZE_CACHE.get() {
        if let Ok(mut entries) = cache.lock() {
            removed = entries.len();
            bytes = entries.iter().map(|entry| entry.size).sum();
            for entry in entries.drain(..) {
                let _ = std::fs::remove_file(entry.path);
            }
        }
    }
    let stale_removed = cleanup_stale_mcp_materialized();
    format!(r#"{{"removed":{removed},"bytes":{bytes},"staleRemoved":{stale_removed}}}"#)
}

fn mcp_cache_stats() -> String {
    let (max_bytes, max_entries) = materialize_cache_limits();
    let (entries, bytes) = MATERIALIZE_CACHE
        .get()
        .and_then(|cache| {
            cache
                .lock()
                .ok()
                .map(|guard| (guard.len(), guard.iter().map(|entry| entry.size).sum()))
        })
        .unwrap_or((0, 0));
    let (window_hits, window_misses) = csvx_core::workbook::window_cache_stats();
    format!(
        r#"{{"hits":{},"misses":{},"entries":{},"bytes":{},"maxBytes":{},"maxEntries":{},"windowHits":{},"windowMisses":{}}}"#,
        MATERIALIZE_HITS.load(std::sync::atomic::Ordering::Relaxed),
        MATERIALIZE_MISSES.load(std::sync::atomic::Ordering::Relaxed),
        entries,
        bytes,
        max_bytes,
        max_entries,
        window_hits,
        window_misses
    )
}

fn mcp_job_list(jobs: &mut HashMap<String, McpJob>, full: bool) -> String {
    let mut entries = Vec::with_capacity(jobs.len());
    for (job_id, job) in jobs.iter_mut() {
        if full {
            entries.push(mcp_job_text(job_id, job));
            continue;
        }
        // Refresh the cached result first, then summarize without the payload.
        let _ = mcp_job_text(job_id, job);
        let entry = match &job.result {
            Some((success, _, message)) => format!(
                r#"{{"job":{},"state":{},"outputChars":{}}}"#,
                mcp_quote(job_id),
                mcp_quote(if *success { "done" } else { "failed" }),
                message.chars().count()
            ),
            None => format!(
                r#"{{"job":{},"state":"running","elapsedMs":{}}}"#,
                mcp_quote(job_id),
                job.started.elapsed().as_millis()
            ),
        };
        entries.push(entry);
    }
    format!(r#"{{"jobs":[{}]}}"#, entries.join(","))
}

fn mcp_job_text(job_id: &str, job: &mut McpJob) -> String {
    if let Some((success, code, message)) = &job.result {
        return if *success {
            format!(
                r#"{{"job":{},"state":"done","output":{}}}"#,
                mcp_quote(job_id),
                mcp_quote(message)
            )
        } else {
            format!(
                r#"{{"job":{},"state":"failed","exitCode":{},"error":{}}}"#,
                mcp_quote(job_id),
                code,
                mcp_quote(message)
            )
        };
    }
    match job.child.try_wait() {
        Ok(None) => format!(
            r#"{{"job":{},"state":"running","elapsedMs":{}}}"#,
            mcp_quote(job_id),
            job.started.elapsed().as_millis()
        ),
        Ok(Some(status)) => {
            let text = std::fs::read_to_string(&job.stdout)
                .map(|value| value.trim().to_string())
                .unwrap_or_default();
            let error = std::fs::read_to_string(&job.stderr)
                .map(|value| value.trim().to_string())
                .unwrap_or_default();
            let _ = std::fs::remove_file(&job.stdout);
            let _ = std::fs::remove_file(&job.stderr);
            let success = status.success();
            let code = status.code().unwrap_or(-1);
            let message = if success { text } else { error };
            job.result = Some((success, code, message.clone()));
            mcp_job_text(job_id, job)
        }
        Err(error) => format!(
            r#"{{"job":{},"state":"failed","error":{}}}"#,
            mcp_quote(job_id),
            mcp_quote(&error.to_string())
        ),
    }
}

pub fn agent_manifest_json() -> String {
    format!(
        r#"{{"protocol":"csvx-agent/1","transport":["mcp-stdio","http"],"tools":{},"workflow":["schema","view","edit_preview","edit_apply","report","gate"],"safety":{{"preview_before_apply":true,"root_bound":true,"single_user":true,"gui":false}},"endpoints":{{"manifest":"/agent/v1/manifest","sessions":"/agent/v1/sessions","actions":"/agent/v1/sessions/{{id}}/actions","audit":"/agent/v1/sessions/{{id}}/audit","replay":"/agent/v1/sessions/{{id}}/replay","curl":"/agent/v1/sessions/{{id}}/curl","runbook":"/agent/v1/sessions/{{id}}/runbook.md","approvals":"/agent/v1/sessions/{{id}}/approvals"}},"instructions":{}}}"#,
        mcp_tools(),
        mcp_quote(AGENT_INSTRUCTIONS)
    )
}

/// Shared job and cache state used by the official rmcp adapter.
pub struct McpJobRegistry {
    jobs: HashMap<String, McpJob>,
    next: u64,
}

impl Default for McpJobRegistry {
    fn default() -> Self {
        Self {
            jobs: HashMap::new(),
            next: 1,
        }
    }
}

impl McpJobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn submit(
        &mut self,
        root: &Path,
        tool: &str,
        arguments: &McpJson,
    ) -> Result<String, String> {
        if !mcp_job_tool_allowed(tool) {
            return Err(format!("{tool} cannot be submitted as a job"));
        }
        let job_id = format!("job-{}", self.next);
        self.next += 1;
        let job = mcp_start_job(root, tool, arguments, &job_id)?;
        prune_mcp_jobs(&mut self.jobs);
        self.jobs.insert(job_id.clone(), job);
        Ok(job_id)
    }

    pub fn status_text(&mut self, job_id: &str, wait: u64) -> Option<String> {
        let job = self.jobs.get_mut(job_id)?;
        let started = std::time::Instant::now();
        loop {
            let text = mcp_job_text(job_id, job);
            if !text.contains(r#""state":"running""#)
                || started.elapsed() >= std::time::Duration::from_secs(wait)
            {
                return Some(text);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    pub fn cancel_text(&mut self, job_id: &str) -> Option<String> {
        self.jobs
            .get_mut(job_id)
            .map(|job| mcp_cancel_job(job_id, job))
    }

    pub fn list_text(&mut self, full: bool) -> String {
        mcp_job_list(&mut self.jobs, full)
    }

    pub fn cache_clear_text(&self) -> String {
        mcp_cache_clear()
    }

    pub fn cache_stats_text(&self) -> String {
        mcp_cache_stats()
    }

    pub fn shutdown(&mut self) {
        shutdown_mcp_jobs(&mut self.jobs);
    }
}

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
