use serde_json::{json, Value};
use std::io;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

pub(super) fn rewrite_legacy_initialize(line: &str) -> Option<String> {
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

pub(super) async fn stdin_pump(
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

pub(super) async fn stdout_pump(
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
