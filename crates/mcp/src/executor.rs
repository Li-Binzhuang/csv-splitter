use crate::cache::mcp_materialize;
use crate::McpJson;
use std::path::{Path, PathBuf};

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
