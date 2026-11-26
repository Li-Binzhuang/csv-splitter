use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct AuditLog {
    path: PathBuf,
    max_bytes: u64,
}

impl AuditLog {
    pub(super) fn new(path: PathBuf) -> Self {
        let max_bytes = std::env::var("CSVX_MCP_AUDIT_MAX_BYTES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_MAX_BYTES);
        Self { path, max_bytes }
    }

    pub(super) fn record(
        &self,
        tool: &str,
        args: &Value,
        elapsed_ms: u64,
        ok: bool,
        code: &str,
    ) -> std::io::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| std::io::Error::other("audit path has no parent"))?;
        fs::create_dir_all(parent)?;
        self.rotate_if_needed()?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let line = json!({
            "ts": timestamp,
            "tool": tool,
            "ok": ok,
            "code": code,
            "ms": elapsed_ms,
            "args": summarize(args),
        });
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&line).unwrap_or_default())
    }

    pub(super) fn read(&self) -> std::io::Result<String> {
        match fs::read_to_string(&self.path) {
            Ok(text) => Ok(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(error),
        }
    }

    fn rotate_if_needed(&self) -> std::io::Result<()> {
        let size = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if size < self.max_bytes {
            return Ok(());
        }
        let rotated = self.path.with_extension("jsonl.1");
        if rotated.exists() {
            fs::remove_file(&rotated)?;
        }
        fs::rename(&self.path, &rotated)
    }
}

fn summarize(args: &Value) -> Value {
    let Value::Object(pairs) = args else {
        return json!({"type": kind(args)});
    };
    let mut summary = serde_json::Map::new();
    for (key, value) in pairs {
        summary.insert(key.clone(), describe(value));
    }
    Value::Object(summary)
}

fn describe(value: &Value) -> Value {
    match value {
        Value::String(text) => json!({"type": "string", "chars": text.chars().count()}),
        Value::Array(items) => json!({"type": "array", "items": items.len()}),
        Value::Object(pairs) => json!({"type": "object", "keys": pairs.len()}),
        other => json!({"type": kind(other)}),
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("csvx-audit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn redacts_argument_values() {
        let args = json!({"path": "secret.csv", "where": "amount>10", "force": true});
        let summary = summarize(&args);
        let text = serde_json::to_string(&summary).unwrap();
        assert!(!text.contains("secret.csv"));
        assert!(!text.contains("amount>10"));
        assert!(text.contains("\"chars\""));
    }

    #[test]
    fn appends_and_rotates_at_size_cap() {
        let root = dir("rotate");
        let log = AuditLog {
            path: root.join("mcp-audit.jsonl"),
            max_bytes: 120,
        };
        for _ in 0..20 {
            log.record("csvx_schema", &json!({"path": "x.csv"}), 1, true, "ok")
                .unwrap();
        }
        assert!(log.path.exists());
        assert!(log.path.with_extension("jsonl.1").exists());
        let lines = fs::read_to_string(&log.path).unwrap().lines().count();
        assert!(lines >= 1);
        let kept = fs::read_to_string(log.path.with_extension("jsonl.1")).unwrap();
        assert!(kept.contains("csvx_schema"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reports_unwritable_path() {
        let root = dir("error");
        let blocker = root.join("blocked");
        fs::write(&blocker, b"x").unwrap();
        let log = AuditLog {
            path: blocker.join("audit.jsonl"),
            max_bytes: 1024,
        };
        assert!(log
            .record("csvx_schema", &json!({}), 1, true, "ok")
            .is_err());
        fs::remove_dir_all(root).ok();
    }
}
