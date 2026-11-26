use crate::McpJson;
use std::path::{Path, PathBuf};

pub(super) fn mcp_path(root: &Path, args: &McpJson) -> Result<std::path::PathBuf, String> {
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

pub(super) fn mcp_named_file(
    root: &Path,
    args: &McpJson,
    key: &str,
) -> Result<Option<PathBuf>, String> {
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

pub(super) fn mcp_output_path(root: &Path, args: &McpJson) -> Result<PathBuf, String> {
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
