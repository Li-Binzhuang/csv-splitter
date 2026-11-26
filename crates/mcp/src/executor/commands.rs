use crate::McpJson;
use std::path::Path;

use super::paths::{mcp_named_file, mcp_output_path};

mod args;
mod concat;

use args::{bounded, flag_or, optional_bounded};
pub(crate) use concat::build as concat;

pub(super) fn build(
    bin: &Path,
    root: &Path,
    path: &Path,
    name: &str,
    args: &McpJson,
) -> Result<std::process::Command, String> {
    let mut command = std::process::Command::new(bin);
    match name {
        "csvx_schema" => {
            command.args(["schema", path.to_str().unwrap_or(""), "--json"]);
        }
        "csvx_view" => {
            command.args(["view", path.to_str().unwrap_or(""), "--json"]);
            let start = args.get("start").and_then(McpJson::as_u64).unwrap_or(0);
            let count = bounded(args, "count", 1, 1000, 100)?;
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
            let limit = bounded(args, "limit", 1, 1000, 100)?;
            command.args([
                "filter",
                path.to_str().unwrap_or(""),
                "--where",
                where_clause,
                "--json",
                "--limit",
                &limit.to_string(),
            ]);
            if flag_or(args, "show", true)? {
                command.args(["--show", &limit.to_string()]);
            }
        }
        "csvx_search" => {
            let query = args
                .get("query")
                .and_then(McpJson::as_str)
                .ok_or("query is required")?;
            let limit = bounded(args, "limit", 1, 1000, 100)?;
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
            if flag_or(args, "verify", false)? {
                command.arg("--verify");
            }
            if flag_or(args, "contentCheck", false)? {
                command.arg("--content-check");
            }
        }
        "csvx_report" => {
            let format = match args.get("format").and_then(McpJson::as_str) {
                None => "md",
                Some(value) if value.eq_ignore_ascii_case("md") => "md",
                Some(value) if value.eq_ignore_ascii_case("html") => "html",
                Some(value) => {
                    return Err(format!("format must be md or html, got {value}"));
                }
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
            if flag_or(args, "force", false)? {
                command.arg("--force");
            }
            if flag_or(args, "dryRun", false)? {
                command.arg("--dry-run");
            }
            if flag_or(args, "dag", false)? {
                command.arg("--dag");
            }
            if let Some(jobs) = optional_bounded(args, "jobs", 1, 64)? {
                command.args(["--jobs", &jobs.to_string()]);
            }
            if flag_or(args, "clean", false)? {
                command.arg("--clean");
            }
            if flag_or(args, "contentCheck", false)? {
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
                let count = args.get("count").and_then(McpJson::as_u64).unwrap_or(1);
                command.args(["--count", &count.to_string()]);
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
            if let Some(minimum) = optional_bounded(args, "minRows", 0, u64::MAX)? {
                command.args(["--min-rows", &minimum.to_string()]);
            }
            if let Some(maximum) = optional_bounded(args, "maxEmpty", 0, u64::MAX)? {
                command.args(["--max-empty", &maximum.to_string()]);
            }
            if let Some(name) = args.get("unique").and_then(McpJson::as_str) {
                command.args(["--unique", name]);
            }
            if flag_or(args, "uniqueGlobal", false)? {
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
