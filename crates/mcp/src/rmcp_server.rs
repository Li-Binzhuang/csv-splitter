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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[derive(Clone)]
struct CsvxServer {
    root: Arc<PathBuf>,
    jobs: Arc<Mutex<super::McpJobRegistry>>,
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

impl CsvxServer {
    async fn job_tool(&self, name: &str, value: &Value) -> Option<CallToolResponse> {
        match name {
            "csvx_job_submit" => {
                let tool = value.get("tool").and_then(Value::as_str)?.to_string();
                if matches!(tool.as_str(), "csvx_cache_stats" | "csvx_cache_clear") {
                    return Some(tool_result(
                        format!("{tool} is a server tool, not a CLI job"),
                        true,
                        "job_start_error",
                    ));
                }
                let args = value.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let args = value_to_mcp(&args);
                let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                Some(match jobs.submit(&self.root, &tool, &args) {
                    Ok(id) => tool_result(
                        format!(r#"{{"job":{},"state":"running"}}"#, json!(id)),
                        false,
                        "ok",
                    ),
                    Err(error) => tool_result(error, true, "job_start_error"),
                })
            }
            "csvx_job_status" => {
                let id = value.get("id").and_then(Value::as_str)?.to_string();
                let wait = value
                    .get("wait")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(30);
                let jobs = Arc::clone(&self.jobs);
                let result = tokio::task::spawn_blocking(move || {
                    jobs.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .status_text(&id, wait)
                })
                .await
                .ok()
                .flatten();
                Some(match result {
                    Some(text) => tool_result(text, false, "ok"),
                    None => tool_result(
                        format!(
                            "unknown job {}",
                            value.get("id").and_then(Value::as_str).unwrap_or("")
                        ),
                        true,
                        "unknown_job",
                    ),
                })
            }
            "csvx_job_cancel" => {
                let id = value.get("id").and_then(Value::as_str)?.to_string();
                let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                Some(match jobs.cancel_text(&id) {
                    Some(text) => tool_result(text, false, "ok"),
                    None => tool_result(format!("unknown job {id}"), true, "unknown_job"),
                })
            }
            "csvx_job_list" => {
                let full = value.get("full").and_then(Value::as_bool).unwrap_or(false);
                let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                Some(tool_result(jobs.list_text(full), false, "ok"))
            }
            "csvx_cache_clear" => {
                let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                Some(tool_result(jobs.cache_clear_text(), false, "ok"))
            }
            "csvx_cache_stats" => {
                let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                Some(tool_result(jobs.cache_stats_text(), false, "ok"))
            }
            _ => None,
        }
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
        let arguments = request.arguments.unwrap_or_default();
        let value = Value::Object(
            arguments
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        if let Some(result) = self.job_tool(&request.name, &value).await {
            return Ok(result);
        }
        let args = value_to_mcp(&value);
        match super::mcp_run_tool(&self.root, &request.name, &args) {
            Ok(text) => Ok(tool_result(text, false, "ok")),
            Err(error) if error.starts_with("unknown tool") => {
                Ok(tool_result(error, true, "unknown_tool"))
            }
            Err(error) if error.contains("outside the MCP root") => {
                Ok(tool_result(error, true, "path_escape"))
            }
            Err(error) => Ok(tool_result(error, true, "cli_error")),
        }
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

fn rewrite_legacy_initialize(line: &str) -> Option<String> {
    let Ok(mut value) = serde_json::from_str::<Value>(line) else {
        return None;
    };
    if value.get("method").and_then(Value::as_str) != Some("initialize") {
        return Some(line.to_string());
    }
    let params = value.as_object_mut().and_then(|object| {
        object
            .entry("params")
            .or_insert_with(|| json!({}))
            .as_object_mut()
    });
    if let Some(params) = params {
        params
            .entry("protocolVersion")
            .or_insert_with(|| json!("2024-11-05"));
        params.entry("capabilities").or_insert_with(|| json!({}));
        params
            .entry("clientInfo")
            .or_insert_with(|| json!({"name":"legacy","version":"0"}));
        params.entry("_meta").or_insert_with(|| {
            json!({
                "io.modelcontextprotocol/protocolVersion": "2024-11-05",
                "io.modelcontextprotocol/clientCapabilities": {}
            })
        });
    }
    Some(serde_json::to_string(&value).unwrap_or_else(|_| line.to_string()))
}

async fn stdin_pump(
    mut tx: tokio::io::DuplexStream,
    synth: tokio::sync::mpsc::Sender<String>,
    mut acks: tokio::sync::mpsc::Receiver<Value>,
) -> io::Result<()> {
    let stdin = tokio::io::stdin();
    let mut lines = tokio::io::BufReader::new(stdin).lines();
    while let Some(line) = lines.next_line().await? {
        match rewrite_legacy_initialize(&line) {
            Some(rewritten) => {
                let parsed = serde_json::from_str::<Value>(&rewritten).ok();
                let request_id = parsed.as_ref().and_then(|value| {
                    value
                        .get("method")
                        .and_then(Value::as_str)
                        .filter(|method| !method.starts_with("notifications/"))
                        .and_then(|_| value.get("id"))
                        .filter(|id| !id.is_null())
                        .cloned()
                });
                tx.write_all(rewritten.as_bytes()).await?;
                tx.write_all(b"\n").await?;
                if let Some(expected) = request_id {
                    loop {
                        match acks.recv().await {
                            Some(id) if id == expected => break,
                            Some(_) => continue,
                            None => return Ok(()),
                        }
                    }
                }
            }
            None => {
                let error = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {"code": -32700, "message": "parse error"}
                });
                let _ = synth.send(error.to_string()).await;
            }
        }
    }
    Ok(())
}

async fn stdout_pump(
    rx: tokio::io::DuplexStream,
    mut synth: tokio::sync::mpsc::Receiver<String>,
    acks: tokio::sync::mpsc::Sender<Value>,
) -> io::Result<()> {
    let mut stdout = tokio::io::stdout();
    let mut lines = tokio::io::BufReader::new(rx).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else {
                    break;
                };
                stdout.write_all(line.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if (value.get("result").is_some() || value.get("error").is_some())
                        && value.get("id").is_some()
                    {
                        let _ = acks.send(value["id"].clone()).await;
                    }
                }
            }
            message = synth.recv() => {
                if let Some(message) = message {
                    stdout.write_all(message.as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                    stdout.flush().await?;
                }
            }
        }
    }
    Ok(())
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
        let server = CsvxServer {
            root: Arc::new(root),
            jobs: Arc::new(Mutex::new(super::McpJobRegistry::new())),
        };
        let service = server
            .serve((input_rx, output_tx))
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        service.waiting().await.map_err(io::Error::other)?;
        Ok(())
    })
}
