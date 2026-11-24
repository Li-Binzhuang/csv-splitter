//! Minimal HTTP Agent API on loopback.
//!
//! This is deliberately smaller than the main HTTP API: JSON bodies, one
//! bearer token per session, an audit file, and explicit approval for
//! destructive tools. It reuses the MCP tool executor, so there is one action
//! implementation instead of a second command surface.

use crate::serve::{agent_manifest_json, json_str, mcp_run_tool, McpJson, McpParser};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_BODY: usize = 256 * 1024;
const SESSION_TTL: Duration = Duration::from_secs(8 * 60 * 60);

struct Session {
    root: PathBuf,
    agent: String,
    token: String,
    created: SystemTime,
    approvals: HashMap<String, String>,
    audit: PathBuf,
}

#[derive(Default)]
struct State {
    sessions: Mutex<HashMap<String, Session>>,
}

/// Run the Agent API until the process is stopped.
pub fn serve(root: &Path, addr: &str) -> io::Result<()> {
    let root = root.canonicalize()?;
    if !root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "root is not a directory",
        ));
    }
    let listener = TcpListener::bind(addr)?;
    eprintln!(
        "csvx agent API on http://{addr}  (root: {})",
        root.display()
    );
    let state = Arc::new(State::default());
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        let root = root.clone();
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            let _ = handle(stream, &root, &state);
        });
    }
    Ok(())
}

/// Print the manifest without starting a server.
pub fn manifest() -> String {
    agent_manifest_json()
}

fn handle(mut stream: TcpStream, root: &Path, state: &State) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    let mut authorization: Option<String> = None;
    let mut content_length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("content-length:") {
            content_length = header[header.len() - rest.len()..]
                .trim()
                .parse()
                .unwrap_or(0);
        }
        if let Some(rest) = lower.strip_prefix("authorization:") {
            authorization = Some(header[header.len() - rest.len()..].trim().to_string());
        }
    }
    if content_length > MAX_BODY {
        return respond_json(&mut stream, 413, r#"{"error":"body too large"}"#);
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    let body = String::from_utf8_lossy(&body).into_owned();

    if method == "GET" && path == "/agent/v1/manifest" {
        return respond_json(&mut stream, 200, &manifest());
    }
    if method == "POST" && path == "/agent/v1/sessions" {
        return create_session(&mut stream, root, state, &body);
    }
    let Some(rest) = path.strip_prefix("/agent/v1/sessions/") else {
        return respond_json(&mut stream, 404, r#"{"error":"not found"}"#);
    };
    let (session_id, action) = match rest.split_once('/') {
        Some(pair) => pair,
        None => (rest, ""),
    };
    if session_id.is_empty() {
        return respond_json(&mut stream, 404, r#"{"error":"not found"}"#);
    }
    let token = match bearer(authorization.as_deref()) {
        Some(token) => token,
        None => return respond_json(&mut stream, 401, r#"{"error":"missing bearer token"}"#),
    };
    if method == "GET" && action.is_empty() {
        return session_status(&mut stream, state, session_id, token);
    }
    if method == "POST" && action == "actions" {
        return run_action(&mut stream, state, root, session_id, token, &body);
    }
    if method == "POST" && action == "approvals" {
        return approval(&mut stream, state, session_id, token, &body);
    }
    if method == "POST" && action == "close" {
        return close(&mut stream, state, session_id, token);
    }
    if method == "GET" && action == "events" {
        return events(&mut stream, state, session_id, token);
    }
    respond_json(&mut stream, 404, r#"{"error":"not found"}"#)
}

fn create_session(
    stream: &mut TcpStream,
    root: &Path,
    state: &State,
    body: &str,
) -> io::Result<()> {
    let request = match McpParser::parse(body) {
        Ok(value) => value,
        Err(error) => {
            return respond_json(stream, 400, &format!(r#"{{"error":{}}}"#, json_str(&error)))
        }
    };
    let requested_root = request
        .get("root")
        .and_then(McpJson::as_str)
        .map(PathBuf::from);
    let selected_root = match requested_root {
        Some(path) => {
            let canonical = match path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    return respond_json(
                        stream,
                        400,
                        &format!(r#"{{"error":{}}}"#, json_str(&format!("bad root: {error}"))),
                    )
                }
            };
            if !canonical.starts_with(root) {
                return respond_json(
                    stream,
                    403,
                    r#"{"error":"session root must stay inside the served root"}"#,
                );
            }
            canonical
        }
        None => root.to_path_buf(),
    };
    let agent = request
        .get("agent")
        .and_then(McpJson::as_str)
        .unwrap_or("anonymous")
        .to_string();
    let id = random_id("sess");
    let token = random_id("tok");
    let audit = selected_root.join(".csvx-agent").join("audit.jsonl");
    if let Some(parent) = audit.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let session = Session {
        root: selected_root.clone(),
        agent: agent.clone(),
        token: token.clone(),
        created: SystemTime::now(),
        approvals: HashMap::new(),
        audit,
    };
    if let Ok(mut sessions) = state.sessions.lock() {
        sessions.insert(id.clone(), session);
    }
    let response = format!(
        r#"{{"sessionId":{},"token":{},"root":{},"agent":{},"expiresInSeconds":{}}}"#,
        json_str(&id),
        json_str(&token),
        json_str(&selected_root.display().to_string()),
        json_str(&agent),
        SESSION_TTL.as_secs()
    );
    respond_json(stream, 201, &response)
}

fn session_status(stream: &mut TcpStream, state: &State, id: &str, token: &str) -> io::Result<()> {
    let sessions = match state.sessions.lock() {
        Ok(sessions) => sessions,
        Err(_) => return respond_json(stream, 500, r#"{"error":"state poisoned"}"#),
    };
    let Some(session) = sessions.get(id) else {
        return respond_json(stream, 404, r#"{"error":"unknown session"}"#);
    };
    if !constant_eq(&session.token, token) {
        return respond_json(stream, 401, r#"{"error":"invalid token"}"#);
    }
    let age = SystemTime::now()
        .duration_since(session.created)
        .unwrap_or_default();
    let response = format!(
        r#"{{"sessionId":{},"agent":{},"root":{},"ageSeconds":{},"expiresInSeconds":{},"approvals":{}}}"#,
        json_str(id),
        json_str(&session.agent),
        json_str(&session.root.display().to_string()),
        age.as_secs(),
        SESSION_TTL.as_secs(),
        session.approvals.len()
    );
    respond_json(stream, 200, &response)
}

fn run_action(
    stream: &mut TcpStream,
    state: &State,
    served_root: &Path,
    id: &str,
    token: &str,
    body: &str,
) -> io::Result<()> {
    let request = match McpParser::parse(body) {
        Ok(value) => value,
        Err(error) => {
            return respond_json(stream, 400, &format!(r#"{{"error":{}}}"#, json_str(&error)))
        }
    };
    let tool = match request.get("tool").and_then(McpJson::as_str) {
        Some(tool) if !tool.is_empty() => tool.to_string(),
        _ => return respond_json(stream, 400, r#"{"error":"tool is required"}"#),
    };
    let args = request.get("arguments").cloned().unwrap_or(McpJson::Null);
    let approval_id = request
        .get("approvalId")
        .and_then(McpJson::as_str)
        .map(str::to_string);
    let destructive = is_destructive(&tool);
    let (root, audit) = {
        let mut sessions = match state.sessions.lock() {
            Ok(sessions) => sessions,
            Err(_) => return respond_json(stream, 500, r#"{"error":"state poisoned"}"#),
        };
        let Some(session) = sessions.get_mut(id) else {
            return respond_json(stream, 404, r#"{"error":"unknown session"}"#);
        };
        if !constant_eq(&session.token, token) {
            return respond_json(stream, 401, r#"{"error":"invalid token"}"#);
        }
        if destructive {
            let approved = approval_id
                .as_deref()
                .and_then(|approval| session.approvals.remove(approval))
                .is_some_and(|approved_tool| approved_tool == tool);
            if !approved {
                return respond_json(
                    stream,
                    428,
                    &format!(
                        r#"{{"error":"approval required","tool":{}}}"#,
                        json_str(&tool)
                    ),
                );
            }
        }
        // A session may narrow its own root but never escape the served root.
        if !session.root.starts_with(served_root) {
            return respond_json(
                stream,
                403,
                r#"{"error":"session root escaped the served root"}"#,
            );
        }
        (session.root.clone(), session.audit.clone())
    };
    let started = SystemTime::now();
    let result = mcp_run_tool(&root, &tool, &args);
    let elapsed_ms = started.elapsed().map(|e| e.as_millis()).unwrap_or(0);
    let (status, output) = match result {
        Ok(output) => ("ok", output),
        Err(error) => ("error", error),
    };
    append_audit(&audit, id, &tool, status, elapsed_ms, &output);
    let response = format!(
        r#"{{"status":{},"tool":{},"elapsedMs":{},"output":{}}}"#,
        json_str(status),
        json_str(&tool),
        elapsed_ms,
        json_str(&output)
    );
    respond_json(stream, 200, &response)
}

fn approval(
    stream: &mut TcpStream,
    state: &State,
    id: &str,
    token: &str,
    body: &str,
) -> io::Result<()> {
    let request = match McpParser::parse(body) {
        Ok(value) => value,
        Err(error) => {
            return respond_json(stream, 400, &format!(r#"{{"error":{}}}"#, json_str(&error)))
        }
    };
    let tool = request.get("tool").and_then(McpJson::as_str).unwrap_or("");
    let decision = request
        .get("decision")
        .and_then(McpJson::as_str)
        .unwrap_or("deny");
    if tool.is_empty() {
        return respond_json(stream, 400, r#"{"error":"tool is required"}"#);
    }
    let mut sessions = match state.sessions.lock() {
        Ok(sessions) => sessions,
        Err(_) => return respond_json(stream, 500, r#"{"error":"state poisoned"}"#),
    };
    let Some(session) = sessions.get_mut(id) else {
        return respond_json(stream, 404, r#"{"error":"unknown session"}"#);
    };
    if !constant_eq(&session.token, token) {
        return respond_json(stream, 401, r#"{"error":"invalid token"}"#);
    }
    if decision != "approve" {
        append_audit(&session.audit, id, tool, "denied", 0, "approval denied");
        return respond_json(stream, 200, r#"{"approved":false}"#);
    }
    let approval_id = random_id("appr");
    session
        .approvals
        .insert(approval_id.clone(), tool.to_string());
    let response = format!(
        r#"{{"approved":true,"approvalId":{}}}"#,
        json_str(&approval_id)
    );
    respond_json(stream, 200, &response)
}

fn close(stream: &mut TcpStream, state: &State, id: &str, token: &str) -> io::Result<()> {
    let mut sessions = match state.sessions.lock() {
        Ok(sessions) => sessions,
        Err(_) => return respond_json(stream, 500, r#"{"error":"state poisoned"}"#),
    };
    let Some(session) = sessions.get(id) else {
        return respond_json(stream, 404, r#"{"error":"unknown session"}"#);
    };
    if !constant_eq(&session.token, token) {
        return respond_json(stream, 401, r#"{"error":"invalid token"}"#);
    }
    let audit = session.audit.clone();
    append_audit(&audit, id, "session.close", "ok", 0, "closed");
    sessions.remove(id);
    respond_json(stream, 200, r#"{"closed":true}"#)
}

fn events(stream: &mut TcpStream, state: &State, id: &str, token: &str) -> io::Result<()> {
    let sessions = match state.sessions.lock() {
        Ok(sessions) => sessions,
        Err(_) => return respond_json(stream, 500, r#"{"error":"state poisoned"}"#),
    };
    let Some(session) = sessions.get(id) else {
        return respond_json(stream, 404, r#"{"error":"unknown session"}"#);
    };
    if !constant_eq(&session.token, token) {
        return respond_json(stream, 401, r#"{"error":"invalid token"}"#);
    }
    let audit = session.audit.clone();
    drop(sessions);
    let mut response = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n");
    if let Ok(text) = std::fs::read_to_string(&audit) {
        for line in text.lines() {
            response.push_str("event: audit\ndata: ");
            response.push_str(line);
            response.push_str("\n\n");
        }
    }
    response.push_str("event: end\ndata: {}\n\n");
    stream.write_all(response.as_bytes())
}

fn is_destructive(tool: &str) -> bool {
    matches!(
        tool,
        "csvx_edit_apply"
            | "csvx_edit_set"
            | "csvx_edit_delete"
            | "csvx_edit_undo"
            | "csvx_edit_redo"
            | "csvx_concat"
            | "csvx_convert"
            | "csvx_job_submit"
            | "csvx_pipeline_run"
            | "csvx_job_cancel"
            | "csvx_cache_clear"
    )
}

fn append_audit(
    audit: &Path,
    session: &str,
    tool: &str,
    status: &str,
    elapsed_ms: u128,
    output: &str,
) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let preview: String = output.chars().take(240).collect();
    let line = format!(
        r#"{{"ts":{},"session":{},"tool":{},"status":{},"elapsedMs":{},"output":{}}}"#,
        timestamp,
        json_str(session),
        json_str(tool),
        json_str(status),
        elapsed_ms,
        json_str(&preview)
    );
    if let Some(parent) = audit.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(audit)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn bearer(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| value.strip_prefix("Bearer "))
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn random_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{nanos:x}-{counter:x}", std::process::id())
}

fn constant_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn respond_json(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        428 => "Precondition Required",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_tools_need_approval() {
        assert!(is_destructive("csvx_edit_apply"));
        assert!(!is_destructive("csvx_view"));
    }

    #[test]
    fn tokens_compare_without_early_exit() {
        assert!(constant_eq("a", "a"));
        assert!(!constant_eq("a", "b"));
        assert!(!constant_eq("a", "aa"));
    }
}
