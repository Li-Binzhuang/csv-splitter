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
    PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ServiceExt;
use serde_json::{json, Value};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
struct CsvxServer {
    root: Arc<PathBuf>,
}

static TOOL_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let arguments = request.arguments.unwrap_or_default();
        let value = Value::Object(
            arguments
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        );
        let _guard = TOOL_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let args = value_to_mcp(&value);
        match super::mcp_run_tool(&self.root, &request.name, &args) {
            Ok(text) => Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into()),
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(error)]).into()),
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
) -> io::Result<()> {
    let stdin = tokio::io::stdin();
    let mut lines = tokio::io::BufReader::new(stdin).lines();
    while let Some(line) = lines.next_line().await? {
        match rewrite_legacy_initialize(&line) {
            Some(rewritten) => {
                tx.write_all(rewritten.as_bytes()).await?;
                tx.write_all(b"\n").await?;
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
    mut rx: tokio::io::DuplexStream,
    mut synth: tokio::sync::mpsc::Receiver<String>,
) -> io::Result<()> {
    let mut stdout = tokio::io::stdout();
    let mut buffer = vec![0u8; 8192];
    loop {
        tokio::select! {
            read = rx.read(&mut buffer) => {
                let read = read?;
                if read == 0 {
                    break;
                }
                stdout.write_all(&buffer[..read]).await?;
                stdout.flush().await?;
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
        tokio::spawn(async move {
            let _ = stdin_pump(input_tx, synth_tx).await;
        });
        tokio::spawn(async move {
            let _ = stdout_pump(output_rx, synth_rx).await;
        });
        let server = CsvxServer {
            root: Arc::new(root),
        };
        let service = server
            .serve((input_rx, output_tx))
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        service.waiting().await.map_err(io::Error::other)?;
        Ok(())
    })
}
