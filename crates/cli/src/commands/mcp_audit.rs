//! Read the local MCP audit JSONL without touching the server.
use crate::commands::shared::json_quote;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const DEFAULT_LIMIT: usize = 50;
pub(crate) const MAX_LIMIT: usize = 10_000;

pub(crate) struct AuditOutput {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
    pub(crate) rotated_bytes: u64,
    pub(crate) max_bytes: u64,
    pub(crate) total: usize,
    pub(crate) lines: Vec<String>,
}

pub(crate) fn run(
    root: &Path,
    tool: Option<&str>,
    errors_only: bool,
    limit: usize,
) -> std::io::Result<AuditOutput> {
    let path = root.join(".csvx/mcp-audit.jsonl");
    let rotated = path.with_extension("jsonl.1");
    let bytes = fs::metadata(&path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let rotated_bytes = fs::metadata(&rotated)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let max_bytes = std::env::var("CSVX_MCP_AUDIT_MAX_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(4 * 1024 * 1024);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let all = text.lines().filter(|line| !line.trim().is_empty());
    let matching: Vec<&str> = all
        .filter(|line| {
            if errors_only && !line.contains(r#""ok":false"#) {
                return false;
            }
            match tool {
                Some(tool) => line.contains(&format!(r#""tool":{}"#, json_quote(tool))),
                None => true,
            }
        })
        .collect();
    let total = matching.len();
    let start = total.saturating_sub(limit);
    let lines = matching[start..]
        .iter()
        .map(|line| line.to_string())
        .collect();
    Ok(AuditOutput {
        path,
        bytes,
        rotated_bytes,
        max_bytes,
        total,
        lines,
    })
}

pub(crate) fn field(line: &str, name: &str) -> String {
    let pattern = format!("\"{name}\":");
    let Some(start) = line.find(&pattern) else {
        return "-".to_string();
    };
    let rest = &line[start + pattern.len()..];
    if let Some(quoted) = rest.strip_prefix('"') {
        return quoted.split('"').next().unwrap_or("-").to_string();
    }
    rest.split([',', '}'])
        .next()
        .unwrap_or("-")
        .trim()
        .to_string()
}

pub(crate) fn parse_limit(value: &str) -> Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|_| "bad --limit (expected a positive integer)".to_string())?;
    if limit == 0 || limit > MAX_LIMIT {
        return Err(format!("--limit must be between 1 and {MAX_LIMIT}"));
    }
    Ok(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("csvx-mcp-audit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_log(root: &Path) -> PathBuf {
        let path = root.join(".csvx/mcp-audit.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = concat!(
            r#"{"ts":1,"tool":"csvx_schema","ok":true,"code":"ok","ms":2,"args":{}}"#,
            "\n",
            r#"{"ts":2,"tool":"csvx_view","ok":false,"code":"cli_error","ms":3,"args":{}}"#,
            "\n",
            r#"{"ts":3,"tool":"csvx_schema","ok":false,"code":"path_escape","ms":4,"args":{}}"#,
            "\n"
        );
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn filters_by_tool_and_errors() {
        let root = dir("filter");
        write_log(&root);
        let only_schema = run(&root, Some("csvx_schema"), false, 50).unwrap();
        assert_eq!(only_schema.total, 2);
        assert_eq!(only_schema.lines.len(), 2);
        let errors = run(&root, None, true, 50).unwrap();
        assert_eq!(errors.total, 2);
        assert!(errors
            .lines
            .iter()
            .all(|line| line.contains(r#""ok":false"#)));
        let limited = run(&root, None, false, 1).unwrap();
        assert_eq!(limited.lines.len(), 1);
        assert!(limited.lines[0].contains(r#""ts":3"#));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn missing_log_is_empty_not_an_error() {
        let root = dir("missing");
        let output = run(&root, None, false, 50).unwrap();
        assert_eq!(output.total, 0);
        assert!(output.lines.is_empty());
        assert_eq!(output.bytes, 0);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn limit_rejects_zero_and_non_numeric() {
        assert!(parse_limit("0").is_err());
        assert!(parse_limit("nope").is_err());
        assert!(parse_limit("10001").is_err());
        assert_eq!(parse_limit("1").unwrap(), 1);
    }
}
