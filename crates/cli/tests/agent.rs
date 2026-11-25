//! End-to-end test for the HTTP Agent API.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_csvx")
}

fn free_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("{addr}")
}

fn request(addr: &str, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let status = response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

fn extract(body: &str, key: &str) -> String {
    let marker = format!("\"{key}\":\"");
    let start = body.find(&marker).unwrap() + marker.len();
    let rest = &body[start..];
    let end = rest.find('"').unwrap();
    rest[..end].to_string()
}

struct Server {
    child: Child,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(root: &std::path::Path, addr: &str) -> Server {
    let child = Command::new(bin())
        .args([
            "agent",
            "serve",
            "--root",
            root.to_str().unwrap(),
            "--addr",
            addr,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut child = child;
    while Instant::now() < deadline {
        if TcpStream::connect(addr).is_ok() {
            return Server { child };
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("agent server did not start on {addr}");
}

#[test]
fn agent_replay_runs_recorded_tool_calls_without_a_model() {
    let dir = std::env::temp_dir().join(format!("csvx_agent_replay_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n").unwrap();
    let trace = dir.join("trace.jsonl");
    // HTTP audit format: no event field, final status is what gets replayed.
    std::fs::write(
        &trace,
        "{\"tool\":\"csvx_schema\",\"arguments\":{\"path\":\"t.csv\"},\"status\":\"ok\"}\n",
    )
    .unwrap();
    let output = Command::new(bin())
        .args([
            "agent",
            "replay",
            "--root",
            dir.to_str().unwrap(),
            "--replay",
            trace.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("columns"), "{stdout}");
    assert!(stdout.contains("city"), "{stdout}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn agent_api_honors_the_tool_allowlist() {
    let dir = std::env::temp_dir().join(format!("csvx_agent_allow_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n").unwrap();
    let addr = free_addr();
    let _server = start(&dir, &addr);

    let (status, body) = request(
        &addr,
        "POST",
        "/agent/v1/sessions",
        None,
        r#"{"agent":"allow","allowedTools":["csvx_schema"]}"#,
    );
    assert_eq!(status, 201, "{body}");
    let token = extract(&body, "token");
    let session = extract(&body, "sessionId");
    let path = format!("/agent/v1/sessions/{session}/actions");

    let (status, body) = request(
        &addr,
        "POST",
        &path,
        Some(&token),
        r#"{"tool":"csvx_schema","arguments":{"path":"t.csv"}}"#,
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = request(
        &addr,
        "POST",
        &path,
        Some(&token),
        r#"{"tool":"csvx_view","arguments":{"path":"t.csv"}}"#,
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("not allowed"), "{body}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn agent_audit_is_scoped_to_the_session() {
    let dir = std::env::temp_dir().join(format!("csvx_agent_scope_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.csv"), "id,city\n1,A\n").unwrap();
    let addr = free_addr();
    let _server = start(&dir, &addr);

    let mut sessions = Vec::new();
    for agent in ["a", "b"] {
        let (status, body) = request(
            &addr,
            "POST",
            "/agent/v1/sessions",
            None,
            &format!(r#"{{"agent":"{agent}"}}"#),
        );
        assert_eq!(status, 201, "{body}");
        sessions.push((extract(&body, "token"), extract(&body, "sessionId")));
    }
    let path_a = format!("/agent/v1/sessions/{}/actions", sessions[0].1);
    let path_b = format!("/agent/v1/sessions/{}/actions", sessions[1].1);
    let (status, body) = request(
        &addr,
        "POST",
        &path_a,
        Some(&sessions[0].0),
        r#"{"tool":"csvx_schema","arguments":{"path":"t.csv"}}"#,
    );
    assert_eq!(status, 200, "{body}");
    let (status, body) = request(
        &addr,
        "POST",
        &path_b,
        Some(&sessions[1].0),
        r#"{"tool":"csvx_view","arguments":{"path":"t.csv"}}"#,
    );
    assert_eq!(status, 200, "{body}");

    let (status, body) = request(
        &addr,
        "GET",
        &format!("/agent/v1/sessions/{}/audit", sessions[0].1),
        Some(&sessions[0].0),
        "",
    );
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("csvx_schema"), "{body}");
    assert!(!body.contains("csvx_view"), "{body}");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn agent_api_creates_session_actions_approves_and_audits() {
    let dir = std::env::temp_dir().join(format!("csvx_agent_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.csv"), "id,city,amount\n1,A,10\n2,B,20\n").unwrap();
    let addr = free_addr();
    let _server = start(&dir, &addr);

    let (status, body) = request(
        &addr,
        "POST",
        "/agent/v1/sessions",
        None,
        r#"{"agent":"test"}"#,
    );
    assert_eq!(status, 201, "{body}");
    let token = extract(&body, "token");
    let session = extract(&body, "sessionId");

    let path = format!("/agent/v1/sessions/{session}/actions");
    let schema = r#"{"tool":"csvx_schema","arguments":{"path":"t.csv"}}"#;
    let (status, body) = request(&addr, "POST", &path, Some(&token), schema);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""status":"ok""#), "{body}");
    assert!(body.contains("city"), "{body}");

    // Destructive action without approval is refused.
    let apply = r#"{"tool":"csvx_edit_apply","arguments":{"path":"t.csv","ops":"set:1:city:Z"}}"#;
    let (status, body) = request(&addr, "POST", &path, Some(&token), apply);
    assert_eq!(status, 428, "{body}");
    assert!(body.contains("approval"), "{body}");

    let approvals = format!("/agent/v1/sessions/{session}/approvals");
    let (status, body) = request(
        &addr,
        "POST",
        &approvals,
        Some(&token),
        r#"{"tool":"csvx_edit_apply","decision":"approve"}"#,
    );
    assert_eq!(status, 200, "{body}");
    let approval = extract(&body, "approvalId");
    let apply = format!(
        r#"{{"tool":"csvx_edit_apply","arguments":{{"path":"t.csv","ops":"set:1:city:Z"}},"approvalId":"{approval}"}}"#
    );
    let (status, body) = request(&addr, "POST", &path, Some(&token), &apply);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""status":"ok""#), "{body}");

    let events = format!("/agent/v1/sessions/{session}/events");
    let (status, body) = request(&addr, "GET", &events, Some(&token), "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("event: audit"), "{body}");
    assert!(body.contains("csvx_edit_apply"), "{body}");

    let audit = std::fs::read_to_string(dir.join(".csvx-agent/audit.jsonl")).unwrap();
    assert!(audit.contains("csvx_edit_apply"), "{audit}");
    assert!(
        audit.contains("approval denied") || audit.contains("csvx_schema"),
        "{audit}"
    );

    let audit_endpoint = format!("/agent/v1/sessions/{session}/audit");
    let (status, body) = request(&addr, "GET", &audit_endpoint, Some(&token), "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("csvx_edit_apply"), "{body}");
    assert!(body.contains("running"), "{body}");
    assert!(body.contains("approved"), "{body}");
    assert!(body.contains("ok"), "{body}");
    assert!(body.contains("\"arguments\""), "{body}");
    assert!(
        body.contains("csvx_report") || body.contains("t.csv"),
        "{body}"
    );

    let replay_endpoint = format!("/agent/v1/sessions/{session}/replay");
    let (status, body) = request(&addr, "GET", &replay_endpoint, Some(&token), "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("csvx_edit_apply"), "{body}");
    assert!(!body.contains("running"), "{body}");
    assert!(!body.contains("approved"), "{body}");

    let actions_md = format!("/agent/v1/sessions/{session}/actions.md");
    let (status, body) = request(&addr, "GET", &actions_md, Some(&token), "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("# Agent actions"), "{body}");
    assert!(body.contains("| # | tool | status | approval |"), "{body}");
    assert!(
        body.contains("| 1 | csvx_schema | ok | not_required |"),
        "{body}"
    );
    assert!(body.contains("csvx_edit_apply"), "{body}");
    let actions_html = format!("/agent/v1/sessions/{session}/actions.html");
    let (status, body) = request(&addr, "GET", &actions_html, Some(&token), "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("<table>"), "{body}");
    assert!(body.contains("<td>csvx_schema</td>"), "{body}");

    let actions_endpoint = format!("/agent/v1/sessions/{session}/actions");
    let (status, body) = request(&addr, "GET", &actions_endpoint, Some(&token), "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("csvx_edit_apply"), "{body}");
    assert!(body.contains("\"step\":1"), "{body}");
    assert!(body.contains("\"approval\":\"approved\""), "{body}");
    assert!(body.contains("\"arguments\""), "{body}");

    let curl_endpoint = format!("/agent/v1/sessions/{session}/curl");
    let (status, body) = request(&addr, "GET", &curl_endpoint, Some(&token), "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("curl -sS"), "{body}");
    assert!(body.contains("csvx_edit_apply"), "{body}");
    assert!(body.contains("__APPROVAL_ID__"), "{body}");
    assert!(body.contains("approval was recorded"), "{body}");
    assert!(body.contains("/approvals"), "{body}");
    assert!(!body.contains(&token), "token leaked into script");

    // follow=1 keeps the connection open and pushes the first audit frame.
    let mut follow = TcpStream::connect(&addr).unwrap();
    follow
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let follow_request = format!(
        "GET /agent/v1/sessions/{session}/events?follow=1 HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    follow.write_all(follow_request.as_bytes()).unwrap();
    let mut prefix = String::new();
    for _ in 0..20 {
        let mut chunk = vec![0u8; 4096];
        let read = follow.read(&mut chunk).unwrap_or(0);
        prefix.push_str(&String::from_utf8_lossy(&chunk[..read]));
        if prefix.contains("event: audit") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(prefix.contains("event: audit"), "{prefix}");
    drop(follow);

    let (status, body) = request(
        &addr,
        "POST",
        &format!("/agent/v1/sessions/{session}/close"),
        Some(&token),
        "{}",
    );
    assert_eq!(status, 200, "{body}");
    std::fs::remove_dir_all(dir).ok();
}
