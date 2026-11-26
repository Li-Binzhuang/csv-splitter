use crate::McpJson;
use std::path::{Path, PathBuf};

use super::super::paths::mcp_output_path;
use super::args::flag_or;

pub(crate) fn build(
    bin: &Path,
    root: &Path,
    args: &McpJson,
) -> Result<std::process::Command, String> {
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
    if flag_or(args, "byName", false) {
        command.arg("--by-name");
    }
    if flag_or(args, "ignoreHeader", false) {
        command.arg("--ignore-header");
    }
    command.arg("--json");
    Ok(command)
}
