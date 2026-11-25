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
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_BODY: usize = 256 * 1024;
const SESSION_TTL: Duration = Duration::from_secs(8 * 60 * 60);

fn approval_ttl() -> Duration {
    std::env::var("CSVX_APPROVAL_TTL_SECONDS")
        .ok()
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(5 * 60))
}

struct Session {
    root: PathBuf,
    agent: String,
    token: String,
    created: SystemTime,
    approvals: HashMap<String, (String, Instant)>,
    notify: Arc<(Mutex<u64>, Condvar)>,
    allowed: Option<Vec<String>>,
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
    let target = parts.next().unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (target.to_string(), String::new()),
    };
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
    if method == "GET" && path == "/agent/v1/sessions" {
        return sessions_summary(&mut stream, state, authorization.as_deref());
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
    if method == "GET" && action == "actions" {
        return actions_list(&mut stream, state, session_id, token);
    }
    if method == "GET" && (action == "actions.md" || action == "actions.html") {
        return actions_report(
            &mut stream,
            state,
            session_id,
            token,
            action.ends_with(".html"),
        );
    }
    if method == "POST" && action == "actions" {
        return run_action(&mut stream, state, root, session_id, token, &body);
    }
    if method == "GET" && action == "approvals" {
        return approvals_list(&mut stream, state, session_id, token);
    }
    if method == "POST" && action == "approvals" {
        return approval(&mut stream, state, session_id, token, &body);
    }
    if method == "POST" && action == "close" {
        return close(&mut stream, state, session_id, token);
    }
    if method == "GET" && action == "events" {
        let follow = query.split('&').any(|pair| pair == "follow=1");
        return events(&mut stream, state, session_id, token, follow);
    }
    if method == "GET" && (action == "audit" || action == "trace") {
        return audit_log(&mut stream, state, session_id, token);
    }
    if method == "GET" && action == "replay" {
        return replay_log(&mut stream, state, session_id, token);
    }
    if method == "GET" && action == "curl" {
        return curl_script(&mut stream, state, session_id, token);
    }
    if method == "GET" && action == "runbook.md" {
        return runbook(&mut stream, state, session_id, token);
    }
    if method == "GET" && action == "runbook" {
        return runbook_json(&mut stream, state, session_id, token);
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
    let allowed = request.get("allowedTools").and_then(|value| match value {
        McpJson::Array(items) => Some(
            items
                .iter()
                .filter_map(McpJson::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>(),
        ),
        _ => None,
    });
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
        notify: Arc::new((Mutex::new(0), Condvar::new())),
        allowed,
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

fn sessions_summary(
    stream: &mut TcpStream,
    state: &State,
    authorization: Option<&str>,
) -> io::Result<()> {
    let Some(token) = bearer(authorization) else {
        return respond_json(stream, 401, r#"{"error":"missing bearer token"}"#);
    };
    let sessions = match state.sessions.lock() {
        Ok(sessions) => sessions,
        Err(_) => return respond_json(stream, 500, r#"{"error":"state poisoned"}"#),
    };
    let current = sessions
        .iter()
        .find(|(_, session)| constant_eq(&session.token, token))
        .map(|(id, _)| id.clone());
    let Some(current) = current else {
        return respond_json(stream, 401, r#"{"error":"invalid token"}"#);
    };
    let mut rows = Vec::new();
    for (id, session) in sessions.iter() {
        let text = std::fs::read_to_string(&session.audit).unwrap_or_default();
        let approved = approved_tools(&text, id);
        let mut actions = 0usize;
        let mut unapproved = 0usize;
        for line in text
            .lines()
            .filter(|line| line_belongs_to_session(line, id))
        {
            let Ok(value) = McpParser::parse(line) else {
                continue;
            };
            if !matches!(
                value.get("status").and_then(McpJson::as_str),
                Some("ok" | "error")
            ) {
                continue;
            }
            actions += 1;
            if let Some(tool) = value.get("tool").and_then(McpJson::as_str) {
                if action_approval(tool, &approved) == "unapproved" {
                    unapproved += 1;
                }
            }
        }
        let age = SystemTime::now()
            .duration_since(session.created)
            .unwrap_or_default()
            .as_secs();
        rows.push(format!(
            r#"{{"sessionId":{},"agent":{},"root":{},"ageSeconds":{age},"actions":{actions},"unapproved":{unapproved},"pendingApprovals":{}}}"#,
            json_str(id),
            json_str(&session.agent),
            json_str(&session.root.display().to_string()),
            session.approvals.len()
        ));
    }
    let body = format!(
        r#"{{"current":{},"count":{},"sessions":[{}]}}"#,
        json_str(&current),
        rows.len(),
        rows.join(",")
    );
    respond_json(stream, 200, &body)
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
        r#"{{"sessionId":{},"agent":{},"root":{},"ageSeconds":{},"expiresInSeconds":{},"approvals":{},"allowedTools":{}}}"#,
        json_str(id),
        json_str(&session.agent),
        json_str(&session.root.display().to_string()),
        age.as_secs(),
        SESSION_TTL.as_secs(),
        session.approvals.len(),
        session
            .allowed
            .as_ref()
            .map_or("null".to_string(), |tools| format!(
                "[{}]",
                tools
                    .iter()
                    .map(|tool| json_str(tool))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
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
    let (root, audit, notify) = {
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
        if session
            .allowed
            .as_ref()
            .is_some_and(|allowed| !allowed.iter().any(|item| item == &tool))
        {
            return respond_json(
                stream,
                403,
                &format!(
                    r#"{{"error":"tool not allowed","tool":{}}}"#,
                    json_str(&tool)
                ),
            );
        }
        if destructive {
            let mut error = "approval required";
            let approved = match approval_id
                .as_deref()
                .and_then(|approval| session.approvals.remove(approval))
            {
                Some((approved_tool, issued)) if approved_tool == tool => {
                    if issued.elapsed() > approval_ttl() {
                        error = "approval expired";
                        false
                    } else {
                        true
                    }
                }
                Some(_) => {
                    error = "approval tool does not match";
                    false
                }
                None => false,
            };
            if !approved {
                return respond_json(
                    stream,
                    428,
                    &format!(
                        r#"{{"error":{},"tool":{}}}"#,
                        json_str(error),
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
        (
            session.root.clone(),
            session.audit.clone(),
            session.notify.clone(),
        )
    };
    let started = SystemTime::now();
    let args_json = json_render(&args);
    append_audit(&audit, id, &tool, "running", 0, "", &args_json);
    signal(&notify);
    let result = mcp_run_tool(&root, &tool, &args);
    let elapsed_ms = started.elapsed().map(|e| e.as_millis()).unwrap_or(0);
    let (status, output) = match result {
        Ok(output) => ("ok", output),
        Err(error) => ("error", error),
    };
    append_audit(&audit, id, &tool, status, elapsed_ms, &output, &args_json);
    signal(&notify);
    let response = format!(
        r#"{{"status":{},"tool":{},"elapsedMs":{},"output":{}}}"#,
        json_str(status),
        json_str(&tool),
        elapsed_ms,
        json_str(&output)
    );
    respond_json(stream, 200, &response)
}

fn approvals_list(stream: &mut TcpStream, state: &State, id: &str, token: &str) -> io::Result<()> {
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
    let mut approvals = Vec::new();
    for (approval_id, (tool, issued)) in &session.approvals {
        let remaining = approval_ttl().saturating_sub(issued.elapsed()).as_secs();
        approvals.push(format!(
            r#"{{"approvalId":{},"tool":{},"expiresInSeconds":{remaining}}}"#,
            json_str(approval_id),
            json_str(tool)
        ));
    }
    let body = format!(
        r#"{{"session":{},"count":{},"approvals":[{}]}}"#,
        json_str(id),
        approvals.len(),
        approvals.join(",")
    );
    respond_json(stream, 200, &body)
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
        append_audit(
            &session.audit,
            id,
            tool,
            "denied",
            0,
            "approval denied",
            "null",
        );
        return respond_json(
            stream,
            403,
            &format!(r#"{{"error":"approval denied","tool":{}}}"#, json_str(tool)),
        );
    }
    let approval_id = random_id("appr");
    session
        .approvals
        .insert(approval_id.clone(), (tool.to_string(), Instant::now()));
    append_audit(
        &session.audit,
        id,
        tool,
        "approved",
        0,
        &approval_id,
        "null",
    );
    signal(&session.notify);
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
    append_audit(&audit, id, "session.close", "ok", 0, "closed", "null");
    sessions.remove(id);
    respond_json(stream, 200, r#"{"closed":true}"#)
}

fn approved_tools(audit: &str, id: &str) -> Vec<String> {
    audit
        .lines()
        .filter(|line| line_belongs_to_session(line, id))
        .filter_map(|line| McpParser::parse(line).ok())
        .filter(|value| value.get("status").and_then(McpJson::as_str) == Some("approved"))
        .filter_map(|value| {
            value
                .get("tool")
                .and_then(McpJson::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn action_approval(tool: &str, approved: &[String]) -> &'static str {
    if is_destructive(tool) {
        if approved.iter().any(|approved| approved == tool) {
            "approved"
        } else {
            "unapproved"
        }
    } else {
        "not_required"
    }
}

fn actions_report(
    stream: &mut TcpStream,
    state: &State,
    id: &str,
    token: &str,
    html: bool,
) -> io::Result<()> {
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
    let text = std::fs::read_to_string(&session.audit).unwrap_or_default();
    let approved = approved_tools(&text, id);
    let mut entries = Vec::new();
    for line in text
        .lines()
        .filter(|line| line_belongs_to_session(line, id))
    {
        let Ok(value) = McpParser::parse(line) else {
            continue;
        };
        if !matches!(
            value.get("status").and_then(McpJson::as_str),
            Some("ok" | "error")
        ) {
            continue;
        }
        let Some(tool) = value.get("tool").and_then(McpJson::as_str) else {
            continue;
        };
        let arguments = value.get("arguments").cloned().unwrap_or(McpJson::Null);
        let status = value
            .get("status")
            .and_then(McpJson::as_str)
            .unwrap_or("ok");
        let elapsed = value
            .get("elapsedMs")
            .and_then(|value| match value {
                McpJson::Number(number) => Some(*number as u64),
                _ => None,
            })
            .unwrap_or(0);
        entries.push((
            tool.to_string(),
            status.to_string(),
            elapsed,
            json_render(&arguments),
            action_approval(tool, &approved).to_string(),
        ));
    }
    let unapproved = entries
        .iter()
        .filter(|(_, _, _, _, approval)| approval == "unapproved")
        .count();
    let mut body = String::new();
    if html {
        body.push_str(&format!(
            "<p>unapproved destructive actions: {unapproved}</p>\n"
        ));
        body.push_str("<h1>Agent actions</h1><table><tr><th>#</th><th>tool</th><th>status</th><th>approval</th><th>elapsedMs</th><th>arguments</th></tr>\n");
        for (step, (tool, status, elapsed, arguments, approval)) in entries.iter().enumerate() {
            body.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td></tr>\n",
                step + 1,
                html_text(tool),
                html_text(status),
                html_text(approval),
                elapsed,
                html_text(arguments)
            ));
        }
        body.push_str("</table>\n");
    } else {
        body.push_str(&format!(
            "# Agent actions\n\nunapproved destructive actions: {unapproved}\n\n| # | tool | status | approval | elapsedMs | arguments |\n|---|---|---|---|---|---|\n"
        ));
        for (step, (tool, status, elapsed, arguments, approval)) in entries.iter().enumerate() {
            body.push_str(&format!(
                "| {} | {} | {} | {} | {} | `{}` |\n",
                step + 1,
                tool.replace('|', "\\|"),
                status.replace('|', "\\|"),
                approval,
                elapsed,
                arguments.replace('|', "\\|").replace('\n', " ")
            ));
        }
    }
    let content_type = if html { "text/html" } else { "text/markdown" };
    let disposition = if html {
        String::new()
    } else {
        format!("Content-Disposition: attachment; filename=\"agent-actions-{id}.md\"\r\n")
    };
    stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{disposition}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
}

fn html_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn actions_list(stream: &mut TcpStream, state: &State, id: &str, token: &str) -> io::Result<()> {
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
    let text = std::fs::read_to_string(&session.audit).unwrap_or_default();
    let approved = approved_tools(&text, id);
    let mut actions = Vec::new();
    for line in text
        .lines()
        .filter(|line| line_belongs_to_session(line, id))
    {
        let Ok(value) = McpParser::parse(line) else {
            continue;
        };
        if !matches!(
            value.get("status").and_then(McpJson::as_str),
            Some("ok" | "error")
        ) {
            continue;
        }
        let Some(tool) = value.get("tool").and_then(McpJson::as_str) else {
            continue;
        };
        let arguments = value.get("arguments").cloned().unwrap_or(McpJson::Null);
        let status = value
            .get("status")
            .and_then(McpJson::as_str)
            .unwrap_or("ok");
        let elapsed = value
            .get("elapsedMs")
            .and_then(|value| match value {
                McpJson::Number(number) => Some(*number as u64),
                _ => None,
            })
            .unwrap_or(0);
        let step = actions.len() + 1;
        actions.push(format!(
            r#"{{"step":{step},"tool":{},"arguments":{},"status":{},"approval":{},"elapsedMs":{elapsed}}}"#,
            json_str(tool),
            json_render(&arguments),
            json_str(status),
            json_str(action_approval(tool, &approved))
        ));
    }
    let unapproved = actions
        .iter()
        .filter(|action| action.contains(r#""approval":"unapproved""#))
        .count();
    let body = format!(
        r#"{{"session":{},"unapproved":{unapproved},"actions":[{}]}}"#,
        json_str(id),
        actions.join(",")
    );
    respond_json(stream, 200, &body)
}

fn line_belongs_to_session(line: &str, id: &str) -> bool {
    McpParser::parse(line)
        .ok()
        .and_then(|value| {
            value
                .get("session")
                .and_then(McpJson::as_str)
                .map(str::to_string)
        })
        .is_some_and(|session| session == id)
}

fn audit_log(stream: &mut TcpStream, state: &State, id: &str, token: &str) -> io::Result<()> {
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
    let text = std::fs::read_to_string(&session.audit).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| line_belongs_to_session(line, id))
        .collect();
    let body = format!(r#"{{"session":{},"lines":{}}}"#, json_str(id), lines.len());
    let body = format!("{body}\n{}", lines.join("\n"));
    stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
}

fn runbook_json(stream: &mut TcpStream, state: &State, id: &str, token: &str) -> io::Result<()> {
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
    let text = std::fs::read_to_string(&session.audit).unwrap_or_default();
    let approved = approved_tools(&text, id);
    let mut actions = Vec::new();
    let mut unapproved = 0usize;
    for line in text
        .lines()
        .filter(|line| line_belongs_to_session(line, id))
    {
        let Ok(value) = McpParser::parse(line) else {
            continue;
        };
        if !matches!(
            value.get("status").and_then(McpJson::as_str),
            Some("ok" | "error")
        ) {
            continue;
        }
        let Some(tool) = value.get("tool").and_then(McpJson::as_str) else {
            continue;
        };
        let approval = action_approval(tool, &approved);
        if approval == "unapproved" {
            unapproved += 1;
        }
        let arguments = value.get("arguments").cloned().unwrap_or(McpJson::Null);
        let status = value
            .get("status")
            .and_then(McpJson::as_str)
            .unwrap_or("ok");
        let elapsed = value
            .get("elapsedMs")
            .and_then(|value| match value {
                McpJson::Number(number) => Some(*number as u64),
                _ => None,
            })
            .unwrap_or(0);
        actions.push(format!(
            r#"{{"step":{},"tool":{},"arguments":{},"status":{},"approval":{},"elapsedMs":{elapsed}}}"#,
            actions.len() + 1,
            json_str(tool),
            json_render(&arguments),
            json_str(status),
            json_str(approval)
        ));
    }
    let mut pending = Vec::new();
    for (approval_id, (tool, _)) in &session.approvals {
        pending.push(format!(
            r#"{{"approvalId":{},"tool":{}}}"#,
            json_str(approval_id),
            json_str(tool)
        ));
    }
    let body = format!(
        r#"{{"session":{},"root":{},"agent":{},"unapproved":{unapproved},"pendingApprovals":[{}],"actions":[{}]}}"#,
        json_str(id),
        json_str(&session.root.display().to_string()),
        json_str(&session.agent),
        pending.join(","),
        actions.join(",")
    );
    respond_json(stream, 200, &body)
}

fn runbook(stream: &mut TcpStream, state: &State, id: &str, token: &str) -> io::Result<()> {
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
    let text = std::fs::read_to_string(&session.audit).unwrap_or_default();
    let approved = approved_tools(&text, id);
    let mut body = format!(
        "# Session runbook\n\n- session: `{id}`\n- root: `{}`\n- agent: `{}`\n\n## Pending approvals\n\n",
        session.root.display(),
        session.agent
    );
    if session.approvals.is_empty() {
        body.push_str("none\n\n");
    } else {
        body.push_str("| approvalId | tool |\n|---|---|\n");
        for (approval_id, (tool, _)) in &session.approvals {
            body.push_str(&format!("| `{approval_id}` | `{tool}` |\n"));
        }
        body.push('\n');
    }
    body.push_str(
        "## Actions\n\n| # | tool | status | approval | elapsedMs |\n|---|---|---|---|---|\n",
    );
    let mut step = 0usize;
    for line in text
        .lines()
        .filter(|line| line_belongs_to_session(line, id))
    {
        let Ok(value) = McpParser::parse(line) else {
            continue;
        };
        if !matches!(
            value.get("status").and_then(McpJson::as_str),
            Some("ok" | "error")
        ) {
            continue;
        }
        let Some(tool) = value.get("tool").and_then(McpJson::as_str) else {
            continue;
        };
        step += 1;
        let elapsed = value
            .get("elapsedMs")
            .and_then(|value| match value {
                McpJson::Number(number) => Some(*number as u64),
                _ => None,
            })
            .unwrap_or(0);
        body.push_str(&format!(
            "| {step} | `{}` | {} | {} | {elapsed} |\n",
            tool.replace('|', "\\|"),
            value
                .get("status")
                .and_then(McpJson::as_str)
                .unwrap_or("ok"),
            action_approval(tool, &approved)
        ));
    }
    body.push_str(&format!(
        "\n## Replay\n\n```bash\ncurl -sS -H \"Authorization: Bearer $CSVX_TOKEN\" \\\n  \"$CSVX_API/agent/v1/sessions/{id}/replay\" > session.jsonl\ncsvx agent replay --root \"{}\" --replay session.jsonl\n```\n",
        session.root.display()
    ));
    // This is a visible warning, not a replacement for the HTTP 428 gate.
    let unapproved = body.matches("| unapproved |").count();
    body.push_str(&format!(
        "
- action count: {step}
- unapproved: {unapproved}
"
    ));
    if unapproved > 0 {
        body = format!("> **BLOCKED**: {unapproved} unapproved destructive action(s) are listed below; issue /approvals before replay.\n\n{body}");
    }
    stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Disposition: attachment; filename=\"agent-runbook-{id}.md\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn curl_script(stream: &mut TcpStream, state: &State, id: &str, token: &str) -> io::Result<()> {
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
    let text = std::fs::read_to_string(&session.audit).unwrap_or_default();
    let approved = approved_tools(&text, id);
    let mut script = String::from(
        "#!/usr/bin/env bash\nset -euo pipefail\n: \"${CSVX_API:=http://127.0.0.1:8788}\"\n: \"${CSVX_TOKEN:?set CSVX_TOKEN}\"\n: \"${CSVX_SESSION:?set CSVX_SESSION}\"\n",
    );
    let mut actions = 0usize;
    for line in text
        .lines()
        .filter(|line| line_belongs_to_session(line, id))
    {
        let Ok(value) = McpParser::parse(line) else {
            continue;
        };
        if !matches!(
            value.get("status").and_then(McpJson::as_str),
            Some("ok" | "error")
        ) {
            continue;
        }
        let Some(tool) = value.get("tool").and_then(McpJson::as_str) else {
            continue;
        };
        let arguments = value.get("arguments").cloned().unwrap_or(McpJson::Null);
        let mut payload = format!(
            r#"{{"tool":{},"arguments":{}}}"#,
            json_str(tool),
            json_render(&arguments)
        );
        if is_destructive(tool) {
            payload = format!(
                r#"{{"tool":{},"arguments":{},"approvalId":"__APPROVAL_ID__"}}"#,
                json_str(tool),
                json_render(&arguments)
            );
            if action_approval(tool, &approved) == "approved" {
                script.push_str(
                    "# destructive: approval was recorded in this audit; replay still needs a fresh approval; replace __APPROVAL_ID__\n",
                );
            } else {
                script.push_str(
                    "# destructive: NO approval found in this audit; request one below and replace __APPROVAL_ID__\n",
                );
            }
            let approval_body = format!(r#"{{"tool":{},"decision":"approve"}}"#, json_str(tool));
            script.push_str(&format!(
                "# request approval: curl -sS -X POST \"$CSVX_API/agent/v1/sessions/$CSVX_SESSION/approvals\" -H \"Authorization: Bearer $CSVX_TOKEN\" -H 'Content-Type: application/json' --data {}\n",
                shell_quote(&approval_body)
            ));
        }
        script.push_str(&format!(
            "curl -sS -X POST \"$CSVX_API/agent/v1/sessions/$CSVX_SESSION/actions\" -H \"Authorization: Bearer $CSVX_TOKEN\" -H 'Content-Type: application/json' --data {}\n",
            shell_quote(&payload)
        ));
        actions += 1;
    }
    if actions == 0 {
        script.push_str("# no final actions recorded\n");
    }
    stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/x-shellscript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{script}",
            script.len()
        )
        .as_bytes(),
    )
}

fn replay_log(stream: &mut TcpStream, state: &State, id: &str, token: &str) -> io::Result<()> {
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
    let text = std::fs::read_to_string(&session.audit).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| line_belongs_to_session(line, id))
        .filter(|line| {
            McpParser::parse(line)
                .ok()
                .and_then(|value| {
                    value
                        .get("status")
                        .and_then(McpJson::as_str)
                        .map(|status| status == "ok" || status == "error")
                })
                .unwrap_or(false)
        })
        .collect();
    let body = lines.join("\n");
    stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )
}

fn events(
    stream: &mut TcpStream,
    state: &State,
    id: &str,
    token: &str,
    follow: bool,
) -> io::Result<()> {
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
    let notify = session.notify.clone();
    drop(sessions);
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
    )?;
    let mut sent = 0usize;
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let text = std::fs::read_to_string(&audit).unwrap_or_default();
        if text.len() > sent {
            let fresh = &text[sent..];
            for line in fresh
                .lines()
                .filter(|line| !line.is_empty() && line_belongs_to_session(line, id))
            {
                stream.write_all(b"event: audit\ndata: ")?;
                stream.write_all(line.as_bytes())?;
                stream.write_all(b"\n\n")?;
            }
            sent = text.len();
        }
        if !follow || std::time::Instant::now() >= deadline {
            break;
        }
        let guard = match notify.0.lock() {
            Ok(guard) => guard,
            Err(_) => break,
        };
        let _ = notify
            .1
            .wait_timeout(guard, Duration::from_millis(1000))
            .unwrap_or_else(|error| error.into_inner());
    }
    stream.write_all(b"event: end\ndata: {}\n\n")
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

fn signal(notify: &Arc<(Mutex<u64>, Condvar)>) {
    if let Ok(mut generation) = notify.0.lock() {
        *generation += 1;
    }
    notify.1.notify_all();
}

fn append_audit(
    audit: &Path,
    session: &str,
    tool: &str,
    status: &str,
    elapsed_ms: u128,
    output: &str,
    arguments: &str,
) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let preview: String = output.chars().take(240).collect();
    let line = format!(
        r#"{{"ts":{},"session":{},"tool":{},"status":{},"elapsedMs":{},"output":{},"arguments":{}}}"#,
        timestamp,
        json_str(session),
        json_str(tool),
        json_str(status),
        elapsed_ms,
        json_str(&preview),
        arguments
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

// ---------------------------------------------------------------------------
// Local model runner
// ---------------------------------------------------------------------------

const SYSTEM_PROMPT: &str = "You are csvx-agent, a local CSV operator. You must answer with exactly one JSON object on each turn. To call a tool: {\"tool\":\"csvx_schema\",\"arguments\":{\"path\":\"t.csv\"}}. When finished: {\"final\":\"short answer\"}. Do not use markdown fences or prose. Use csvx_schema and csvx_view before editing. Prefer csvx_edit_preview before any destructive change. Available tools: csvx_schema(path), csvx_view(path,start,count), csvx_filter(path,where,limit,show), csvx_quality(path,key,group), csvx_edit_preview(path,ops), csvx_edit_apply(path,ops), csvx_report(path,specText or spec,format), csvx_gate(path,minRows,maxEmpty,unique), csvx_convert(path,format,out,sheet), csvx_job_submit(tool,arguments), csvx_job_status(id).";

enum Action {
    Tool { tool: String, arguments: McpJson },
    Final(String),
}

/// Run a small model-driven loop against an OpenAI-compatible endpoint.
///
/// The API key is read from `CSVX_AGENT_API_KEY` (or `OPENAI_API_KEY`) and is
/// passed to curl on stdin, never in argv or on disk. curl is used instead of
/// adding a TLS client dependency to the whole CLI.
#[allow(clippy::too_many_arguments)]
pub fn run(
    root: &Path,
    endpoint: &str,
    model: &str,
    prompt: &str,
    skill: Option<&Path>,
    max_steps: usize,
    allow_apply: bool,
    trace_path: Option<&Path>,
) -> Result<String, String> {
    let key = std::env::var("CSVX_AGENT_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .map_err(|_| {
            "set CSVX_AGENT_API_KEY (or OPENAI_API_KEY); the key is never written to disk"
                .to_string()
        })?;
    let system_prompt = match skill {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read skill {}: {e}", path.display()))?;
            format!("{SYSTEM_PROMPT}\n\nFollow this skill:\n{text}")
        }
        None => SYSTEM_PROMPT.to_string(),
    };
    let mut messages: Vec<(String, String)> = vec![
        ("system".to_string(), system_prompt),
        ("user".to_string(), prompt.to_string()),
    ];
    let mut trace = match trace_path {
        Some(path) => Some(
            std::fs::File::create(path)
                .map_err(|e| format!("cannot create trace {}: {e}", path.display()))?,
        ),
        None => None,
    };
    trace_line(
        &mut trace,
        &format!(
            r#"{{"event":"run","endpoint":{},"model":{},"root":{},"prompt":{},"skill":{},"maxSteps":{max_steps}}}"#,
            json_str(endpoint),
            json_str(model),
            json_str(&root.display().to_string()),
            json_str(prompt),
            json_str(
                skill
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
                    .as_str()
            )
        ),
    );
    let mut last = String::new();
    for step in 1..=max_steps {
        let content = call_model(endpoint, &key, model, &messages)?;
        trace_line(
            &mut trace,
            &format!(
                r#"{{"event":"model","step":{step},"content":{}}}"#,
                json_str(&content)
            ),
        );
        let action = parse_action(&content)?;
        match action {
            Action::Final(answer) => {
                trace_line(
                    &mut trace,
                    &format!(
                        r#"{{"event":"final","step":{step},"answer":{}}}"#,
                        json_str(&answer)
                    ),
                );
                return Ok(answer);
            }
            Action::Tool { tool, arguments } => {
                if is_destructive(&tool) && !allow_apply {
                    messages.push(("assistant".into(), content));
                    messages.push((
                        "user".into(),
                        format!("{{\"error\":\"{tool} is destructive; rerun with allow-apply or use a preview tool\"}}"),
                    ));
                    continue;
                }
                let started = std::time::Instant::now();
                let result = mcp_run_tool(root, &tool, &arguments);
                let elapsed_ms = started.elapsed().as_millis();
                let observation = match result {
                    Ok(text) => text,
                    Err(error) => format!("{{\"error\":{}}}", json_str(&error)),
                };
                let preview: String = observation.chars().take(500).collect();
                eprintln!(
                    "step {step}: {tool} {} -> {}",
                    json_render(&arguments),
                    preview.replace('\n', " ")
                );
                trace_line(
                    &mut trace,
                    &format!(
                        r#"{{"event":"tool","step":{step},"tool":{},"arguments":{},"observation":{},"elapsedMs":{elapsed_ms}}}"#,
                        json_str(&tool),
                        json_render(&arguments),
                        json_str(&observation.chars().take(20_000).collect::<String>())
                    ),
                );
                // Keep the transcript bounded; a tool result can be huge.
                let observation: String = observation.chars().take(20_000).collect();
                last = observation.clone();
                messages.push(("assistant".into(), content));
                messages.push(("user".into(), format!("observation: {observation}")));
            }
        }
    }
    Err(format!(
        "agent stopped after {max_steps} steps; last observation: {last}"
    ))
}

fn call_model(
    endpoint: &str,
    key: &str,
    model: &str,
    messages: &[(String, String)],
) -> Result<String, String> {
    let mut list = String::from("[");
    for (index, (role, content)) in messages.iter().enumerate() {
        if index > 0 {
            list.push(',');
        }
        list.push_str(&format!(
            r#"{{"role":{},"content":{}}}"#,
            json_str(role),
            json_str(content)
        ));
    }
    list.push(']');
    let body = format!(
        r#"{{"model":{},"temperature":0,"messages":{}}}"#,
        json_str(model),
        list
    );
    let path = std::env::temp_dir().join(format!(
        "csvx_agent_body_{}_{}.json",
        std::process::id(),
        random_id("body")
    ));
    std::fs::write(&path, body).map_err(|e| format!("cannot write request: {e}"))?;
    let config = format!(
        "header = \"Authorization: Bearer {}\"\nheader = \"Content-Type: application/json\"\n",
        key
    );
    let mut child = std::process::Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "180",
            "--config",
            "-",
            "--data-binary",
            &format!("@{}", path.display()),
            endpoint,
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start curl: {e}"))?;
    use std::io::Write as _;
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(config.as_bytes());
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("curl wait: {e}"))?;
    let _ = std::fs::remove_file(&path);
    if !output.status.success() {
        return Err(format!(
            "model endpoint failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let response = String::from_utf8_lossy(&output.stdout);
    let value =
        McpParser::parse(&response).map_err(|e| format!("model response is not JSON: {e}"))?;
    let content = value
        .get("choices")
        .and_then(|choices| match choices {
            McpJson::Array(items) => items.first(),
            _ => None,
        })
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(McpJson::as_str)
        .unwrap_or("");
    if content.trim().is_empty() {
        return Err("model returned empty content (check max_tokens / endpoint)".to_string());
    }
    Ok(content.to_string())
}

fn trace_line(trace: &mut Option<std::fs::File>, line: &str) {
    use std::io::Write as _;
    if let Some(file) = trace {
        let _ = writeln!(file, "{line}");
    }
}

fn json_render(value: &McpJson) -> String {
    match value {
        McpJson::Null => "null".to_string(),
        McpJson::Bool(value) => value.to_string(),
        McpJson::Number(value) => value.to_string(),
        McpJson::String(value) => json_str(value),
        McpJson::Array(items) => format!(
            "[{}]",
            items.iter().map(json_render).collect::<Vec<_>>().join(",")
        ),
        McpJson::Object(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|(key, value)| format!("{}:{}", json_str(key), json_render(value)))
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

/// Replay recorded tool actions without calling a model.
pub fn replay(trace_path: &Path, root: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(trace_path)
        .map_err(|e| format!("cannot read trace {}: {e}", trace_path.display()))?;
    for (line_no, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value =
            McpParser::parse(line).map_err(|e| format!("trace line {}: {e}", line_no + 1))?;
        let event = value.get("event").and_then(McpJson::as_str).unwrap_or("");
        // Runner traces use event:"tool"; HTTP audit lines have no event but do
        // have a tool and arguments, so both formats replay through this path.
        let audit_final = event.is_empty()
            && value.get("tool").is_some()
            && matches!(
                value.get("status").and_then(McpJson::as_str),
                Some("ok" | "error")
            );
        if event == "tool" || audit_final {
            // replay below
        } else {
            continue;
        }
        let tool = value
            .get("tool")
            .and_then(McpJson::as_str)
            .ok_or_else(|| format!("trace line {} has no tool", line_no + 1))?;
        let arguments = value.get("arguments").cloned().unwrap_or(McpJson::Null);
        eprintln!(
            "replay {}: {} {}",
            line_no + 1,
            tool,
            json_render(&arguments)
        );
        let output = mcp_run_tool(root, tool, &arguments)?;
        println!("{output}");
    }
    Ok(())
}

fn parse_action(content: &str) -> Result<Action, String> {
    let text = content.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|inner| inner.strip_suffix("```"))
        .unwrap_or(text)
        .trim();
    let start = text.find('{').ok_or("model did not return a JSON object")?;
    let end = text
        .rfind('}')
        .ok_or("model did not return a JSON object")?;
    let json = &text[start..=end];
    let value = McpParser::parse(json).map_err(|e| format!("invalid action JSON: {e}"))?;
    if let Some(final_text) = value.get("final").and_then(McpJson::as_str) {
        return Ok(Action::Final(final_text.to_string()));
    }
    let tool = value
        .get("tool")
        .and_then(McpJson::as_str)
        .ok_or("action needs tool or final")?;
    let arguments = value.get("arguments").cloned().unwrap_or(McpJson::Null);
    Ok(Action::Tool {
        tool: tool.to_string(),
        arguments,
    })
}

#[cfg(test)]
mod runner_tests {
    use super::*;

    #[test]
    fn parses_tool_and_final_actions() {
        let action =
            parse_action(r#"{"tool":"csvx_schema","arguments":{"path":"t.csv"}}"#).unwrap();
        match action {
            Action::Tool { tool, .. } => assert_eq!(tool, "csvx_schema"),
            _ => panic!("expected tool"),
        }
        let final_action = parse_action("```json\n{\"final\":\"done\"}\n```").unwrap();
        match final_action {
            Action::Final(text) => assert_eq!(text, "done"),
            _ => panic!("expected final"),
        }
        assert!(parse_action("just text").is_err());
    }
}
