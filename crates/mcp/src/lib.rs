//! MCP stdio server: the thin agent-facing adapter over csvx.
//!
//! Tool execution deliberately shells out to the `csvx` binary, so this crate
//! owns the protocol and safety envelope but not the CSV engine.

use std::io;
use std::path::Path;

mod rmcp_server;
pub use rmcp_server::run_stdio as run_rmcp_stdio;

/// The official rmcp transport; the old hand-written protocol loop is gone.
mod cache;
mod executor;
mod jobs;
mod json;
mod tools;
#[cfg(test)]
pub(crate) use cache::mcp_materialize;
#[cfg(test)]
pub(crate) use cache::{
    cache_insert, cache_lookup, cleanup_mcp_materialized, cleanup_stale_mcp_materialized,
    CachedMaterialized,
};
pub(crate) use executor::mcp_command;
pub use executor::mcp_run_tool;
pub use jobs::McpJobRegistry;
#[cfg(test)]
pub(crate) use jobs::{
    drop_mcp_job_files, prune_mcp_jobs, shutdown_mcp_jobs, McpJob, MCP_JOB_HISTORY,
};
pub use json::{McpJson, McpParser};
pub use tools::mcp_tools;

pub fn run_mcp_stdio(root: &Path) -> io::Result<()> {
    run_rmcp_stdio(root)
}

/// The one-paragraph contract an agent should receive before it starts calling tools.
pub const AGENT_INSTRUCTIONS: &str = "csvx is a local, single-user CSV backend. Start with csvx_schema and csvx_view; inspect before editing. Use csvx_edit_preview, then csvx_edit_apply only after the preview is acceptable. All paths must stay inside the MCP root. Long work belongs in csvx_job_submit. Reports use csvx_report (CSV spec or specText); paging uses csvx_view; gates use csvx_gate; xlsx goes through csvx_convert. There is no GUI and no multi-user auth; keep destructive actions explicit and auditable. csvx://agent returns the machine-readable manifest.";

pub(crate) fn mcp_quote(text: &str) -> String {
    csvx_core::json::quote(text)
}

pub fn agent_manifest_json() -> String {
    format!(
        r#"{{"protocol":"csvx-agent/1","transport":["mcp-stdio","http"],"tools":{},"workflow":["schema","view","edit_preview","edit_apply","report","gate"],"safety":{{"preview_before_apply":true,"root_bound":true,"single_user":true,"gui":false}},"endpoints":{{"manifest":"/agent/v1/manifest","sessions":"/agent/v1/sessions","actions":"/agent/v1/sessions/{{id}}/actions","audit":"/agent/v1/sessions/{{id}}/audit","replay":"/agent/v1/sessions/{{id}}/replay","curl":"/agent/v1/sessions/{{id}}/curl","runbook":"/agent/v1/sessions/{{id}}/runbook.md","approvals":"/agent/v1/sessions/{{id}}/approvals"}},"instructions":{}}}"#,
        mcp_tools(),
        mcp_quote(AGENT_INSTRUCTIONS)
    )
}

#[cfg(test)]
mod tests;
