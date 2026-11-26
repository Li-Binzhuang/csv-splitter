use crate::cache::mcp_materialize;
use crate::McpJson;
use std::path::Path;

mod commands;
mod paths;
use paths::mcp_path;

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
        return commands::concat(&bin, root, args);
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
    commands::build(&bin, root, &path, name, args)
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
