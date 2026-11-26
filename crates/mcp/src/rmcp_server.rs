//! Official rmcp server adapter.
//!
//! The transport keeps a small legacy shim: older MCP clients send
//! `initialize` with `params:{}`; rmcp's modern lifecycle wants a protocol
//! version and client capabilities, so the shim fills those defaults before
//! rmcp sees the line. Tool execution still shells out to `csvx`.

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeRequestParams, InitializeResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ProgressNotification, ProgressNotificationParam, ProgressToken,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, ServerConfig, ServerNotification, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
mod audit;
mod jobs;
mod transport;
use audit::AuditLog;
use transport::{stdin_pump, stdout_pump};

#[derive(Clone)]
struct CsvxServer {
    root: Arc<PathBuf>,
    jobs: Arc<Mutex<super::McpJobRegistry>>,
    audit: Arc<Mutex<AuditLog>>,
}

fn tool_result(text: String, is_error: bool, code: &str) -> CallToolResponse {
    let mut result = if is_error {
        CallToolResult::error(vec![ContentBlock::text(text)])
    } else {
        CallToolResult::success(vec![ContentBlock::text(text)])
    };
    result.structured_content = Some(json!({"ok": !is_error, "code": code}));
    result.into()
}

fn tools() -> Vec<Tool> {
    serde_json::from_str(super::mcp_tools()).unwrap_or_default()
}

fn resources() -> Vec<Resource> {
    vec![
        Resource::new("csvx://operations", "operations")
            .with_mime_type("application/json")
            .with_description("Shared CLI/HTTP/MCP operation catalog (JSON)"),
        Resource::new("csvx://agent", "agent")
            .with_mime_type("application/json")
            .with_description("Agent manifest: workflow, tools, safety and limits (JSON)"),
        Resource::new("csvx://audit", "audit")
            .with_mime_type("application/x-ndjson")
            .with_description("Redacted MCP call audit log (JSONL)"),
    ]
}

fn value_to_mcp(value: &Value) -> super::McpJson {
    match value {
        Value::Null => super::McpJson::Null,
        Value::Bool(flag) => super::McpJson::Bool(*flag),
        Value::Number(number) => super::McpJson::Number(number.as_f64().unwrap_or(0.0)),
        Value::String(text) => super::McpJson::String(text.clone()),
        Value::Array(items) => super::McpJson::Array(items.iter().map(value_to_mcp).collect()),
        Value::Object(pairs) => super::McpJson::Object(
            pairs
                .iter()
                .map(|(key, value)| (key.clone(), value_to_mcp(value)))
                .collect(),
        ),
    }
}

fn response_code(response: &CallToolResponse) -> (&str, bool) {
    match response {
        CallToolResponse::Complete(result) => {
            let code = result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("complete");
            (code, !result.is_error.unwrap_or(false))
        }
        _ => ("non_complete", false),
    }
}

impl ServerHandler for CsvxServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_server_info(Implementation::new("csvx", env!("CARGO_PKG_VERSION")))
        .with_instructions(super::AGENT_INSTRUCTIONS.to_string())
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, rmcp::ErrorData> {
        context.peer.set_peer_info(request);
        Ok(self.get_info())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult {
            tools: tools(),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let progress = context.meta.0.get("progressToken").cloned();
        if let Some(token) = progress {
            if request.name == "csvx_job_status" || request.name == "csvx_pipeline_run" {
                if let Ok(token) = serde_json::from_value::<ProgressToken>(token) {
                    let param = ProgressNotificationParam::new(token, 0.0);
                    let notification = ProgressNotification::new(param);
                    if let Err(error) = context
                        .peer
                        .send_notification(ServerNotification::ProgressNotification(notification))
                        .await
                    {
                        eprintln!("progress send failed: {error}");
                    }
                }
            }
        }
        let started = Instant::now();
        let arguments = request.arguments.unwrap_or_default();
        let value = Value::Object(
            arguments
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        let response = if let Some(result) = self.job_tool(&request.name, &value).await {
            result
        } else {
            let args = value_to_mcp(&value);
            match super::mcp_run_tool(&self.root, &request.name, &args) {
                Ok(text) => tool_result(text, false, "ok"),
                Err(error) if error.starts_with("unknown tool") => {
                    tool_result(error, true, "unknown_tool")
                }
                Err(error) if error.contains("outside the MCP root") => {
                    tool_result(error, true, "path_escape")
                }
                Err(error) => tool_result(error, true, "cli_error"),
            }
        };
        let (code, ok) = response_code(&response);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if let Ok(audit) = self.audit.lock() {
            if let Err(error) = audit.record(&request.name, &value, elapsed_ms, ok, code) {
                eprintln!("MCP audit write failed: {error}");
            }
        }
        Ok(response)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, rmcp::ErrorData> {
        Ok(ListResourcesResult {
            resources: resources(),
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, rmcp::ErrorData> {
        let text = match request.uri.as_str() {
            "csvx://operations" => csvx_core::contract::operations_json(),
            "csvx://agent" => super::agent_manifest_json(),
            "csvx://audit" => match self.audit.lock().unwrap_or_else(|e| e.into_inner()).read() {
                Ok(text) => text,
                Err(error) => json!({"error": error.to_string()}).to_string(),
            },
            _ => {
                return Err(rmcp::ErrorData::resource_not_found(
                    "unknown resource",
                    None,
                ))
            }
        };
        Ok(ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)]).into())
    }
}

pub fn run_stdio(root: &Path) -> io::Result<()> {
    let root = root.canonicalize()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let (input_tx, input_rx) = tokio::io::duplex(64 * 1024);
        let (output_tx, output_rx) = tokio::io::duplex(64 * 1024);
        let (synth_tx, synth_rx) = tokio::sync::mpsc::channel(8);
        let (ack_tx, ack_rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = stdin_pump(input_tx, synth_tx, ack_rx).await;
        });
        tokio::spawn(async move {
            let _ = stdout_pump(output_rx, synth_rx, ack_tx).await;
        });
        let audit_path = root.join(".csvx/mcp-audit.jsonl");
        let server = CsvxServer {
            root: Arc::new(root),
            jobs: Arc::new(Mutex::new(super::McpJobRegistry::new())),
            audit: Arc::new(Mutex::new(AuditLog::new(audit_path))),
        };
        let service = server
            .serve((input_rx, output_tx))
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        service.waiting().await.map_err(io::Error::other)?;
        Ok(())
    })
}
