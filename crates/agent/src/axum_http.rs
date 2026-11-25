//! Axum transport for the Agent API.
//!
//! The session/approval/audit behavior stays in the parent module; this layer
//! only translates typed HTTP requests into the existing response functions and
//! provides the async SSE stream.

use axum::body::Body;
use axum::extract::DefaultBodyLimit;
use axum::extract::{Path, Query, State as AxumState};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use std::collections::HashMap;
use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::{
    actions_report, approval, approvals_list, audit_log, bearer, close, constant_eq,
    create_session, curl_script, line_belongs_to_session, manifest, replay_log, run_action,
    runbook, runbook_json, session_status, sessions_summary, State,
};

#[derive(Clone)]
struct App {
    root: Arc<PathBuf>,
    state: Arc<State>,
}

fn json_response(status: StatusCode, body: &str) -> Response {
    (
        status,
        [(CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn raw_response(raw: Vec<u8>) -> Response {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = raw[split.saturating_add(4)..].to_vec();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(500);
    let content_type = head
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("content-type:"))
        .map(|line| line["content-type:".len()..].trim().to_string())
        .unwrap_or_else(|| "application/json".to_string());
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header(CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn call<F>(f: F) -> Response
where
    F: FnOnce(&mut Vec<u8>) -> io::Result<()>,
{
    let mut raw = Vec::new();
    match f(&mut raw) {
        Ok(()) => raw_response(raw),
        Err(_) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":"write failed"}"#,
        ),
    }
}

fn auth(headers: &HeaderMap) -> Option<String> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

fn token(headers: &HeaderMap) -> Result<String, Box<Response>> {
    bearer(auth(headers).as_deref())
        .map(ToString::to_string)
        .ok_or_else(|| {
            Box::new(json_response(
                StatusCode::UNAUTHORIZED,
                r#"{"error":"missing bearer token"}"#,
            ))
        })
}

async fn h_manifest() -> Response {
    json_response(StatusCode::OK, &manifest())
}

async fn h_sessions_list(AxumState(app): AxumState<App>, headers: HeaderMap) -> Response {
    let authorization = auth(&headers);
    call(|out| sessions_summary(out, &app.state, authorization.as_deref()))
}

async fn h_sessions_create(AxumState(app): AxumState<App>, body: String) -> Response {
    call(|out| create_session(out, &app.root, &app.state, &body))
}

async fn h_session(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| session_status(out, &app.state, &id, &token)),
        Err(response) => *response,
    }
}

async fn h_actions(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| super::actions_list(out, &app.state, &id, &token)),
        Err(response) => *response,
    }
}

async fn h_actions_md(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| actions_report(out, &app.state, &id, &token, false)),
        Err(response) => *response,
    }
}

async fn h_actions_html(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| actions_report(out, &app.state, &id, &token, true)),
        Err(response) => *response,
    }
}

async fn h_action(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| run_action(out, &app.state, &app.root, &id, &token, &body)),
        Err(response) => *response,
    }
}

async fn h_approvals(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| approvals_list(out, &app.state, &id, &token)),
        Err(response) => *response,
    }
}

async fn h_approve(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| approval(out, &app.state, &id, &token, &body)),
        Err(response) => *response,
    }
}

async fn h_close(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| close(out, &app.state, &id, &token)),
        Err(response) => *response,
    }
}

async fn h_audit(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| audit_log(out, &app.state, &id, &token)),
        Err(response) => *response,
    }
}

async fn h_replay(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| replay_log(out, &app.state, &id, &token)),
        Err(response) => *response,
    }
}

async fn h_curl(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| curl_script(out, &app.state, &id, &token)),
        Err(response) => *response,
    }
}

async fn h_runbook_md(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| runbook(out, &app.state, &id, &token)),
        Err(response) => *response,
    }
}

async fn h_runbook(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    match token(&headers) {
        Ok(token) => call(|out| runbook_json(out, &app.state, &id, &token)),
        Err(response) => *response,
    }
}

async fn h_events(
    AxumState(app): AxumState<App>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let token = match token(&headers) {
        Ok(token) => token,
        Err(response) => return *response,
    };
    let (audit, follow) = {
        let sessions = match app.state.sessions.lock() {
            Ok(sessions) => sessions,
            Err(_) => {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    r#"{"error":"state poisoned"}"#,
                )
            }
        };
        let Some(session) = sessions.get(&id) else {
            return json_response(StatusCode::NOT_FOUND, r#"{"error":"unknown session"}"#);
        };
        if !constant_eq(&session.token, &token) {
            return json_response(StatusCode::UNAUTHORIZED, r#"{"error":"invalid token"}"#);
        }
        (
            session.audit.clone(),
            query
                .get("follow")
                .map(|value| value == "1")
                .unwrap_or(false),
        )
    };
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(8);
    tokio::spawn(async move {
        let mut sent = 0usize;
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut last_ping = Instant::now();
        loop {
            let text = std::fs::read_to_string(&audit).unwrap_or_default();
            if text.len() > sent {
                for line in text[sent..]
                    .lines()
                    .filter(|line| !line.is_empty() && line_belongs_to_session(line, &id))
                {
                    if tx
                        .send(Ok(Event::default().event("audit").data(line)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                sent = text.len();
            }
            if !follow || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            if last_ping.elapsed() >= Duration::from_secs(15) {
                if tx.send(Ok(Event::default().comment("ping"))).await.is_err() {
                    return;
                }
                last_ping = Instant::now();
            }
        }
        let _ = tx.send(Ok(Event::default().event("end").data("{}"))).await;
    });
    Sse::new(ReceiverStream::new(rx)).into_response()
}

pub fn serve(root: &FsPath, addr: &str) -> io::Result<()> {
    let root = root.canonicalize()?;
    if !root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "root is not a directory",
        ));
    }
    let app = App {
        root: Arc::new(root.clone()),
        state: Arc::new(State::default()),
    };
    let app = Router::new()
        .route("/agent/v1/manifest", get(h_manifest))
        .route(
            "/agent/v1/sessions",
            get(h_sessions_list).post(h_sessions_create),
        )
        .route("/agent/v1/sessions/:id", get(h_session))
        .route(
            "/agent/v1/sessions/:id/actions",
            get(h_actions).post(h_action),
        )
        .route("/agent/v1/sessions/:id/actions.md", get(h_actions_md))
        .route("/agent/v1/sessions/:id/actions.html", get(h_actions_html))
        .route(
            "/agent/v1/sessions/:id/approvals",
            get(h_approvals).post(h_approve),
        )
        .route("/agent/v1/sessions/:id/close", post(h_close))
        .route("/agent/v1/sessions/:id/events", get(h_events))
        .route("/agent/v1/sessions/:id/audit", get(h_audit))
        .route("/agent/v1/sessions/:id/trace", get(h_audit))
        .route("/agent/v1/sessions/:id/replay", get(h_replay))
        .route("/agent/v1/sessions/:id/curl", get(h_curl))
        .route("/agent/v1/sessions/:id/runbook.md", get(h_runbook_md))
        .route("/agent/v1/sessions/:id/runbook", get(h_runbook))
        .layer(DefaultBodyLimit::max(super::MAX_BODY))
        .with_state(app);
    let address: SocketAddr = addr.parse().map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid address {addr}: {error}"),
        )
    })?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(address).await?;
        eprintln!(
            "csvx agent API on http://{addr}  (root: {})",
            root.display()
        );
        axum::serve(listener, app)
            .await
            .map_err(|error| io::Error::other(error.to_string()))
    })
}
