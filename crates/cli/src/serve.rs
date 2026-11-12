//! Local HTTP API: the backend half of a front-end/back-end split.
//!
//! A browser cannot page through a 10 GB file, but it can ask for "rows 4,300,000
//! to 4,300,040" — which is what the sparse index makes cheap. This server is
//! deliberately small and dependency-free: GET + query string in, JSON out,
//! bound to loopback, restricted to one directory tree.
//!
//! Endpoints (all read-only):
//!   GET /                          (built-in HTML viewer, no build step)
//!   GET  /edit?path=REL                            (pending edits, or the edited window)
//!   POST /edit?path=REL&action=set&row=N&col=C&value=V   (record an edit)
//!   POST /edit?path=REL&action=undo                (undo the last edit)
//!   POST /edit?path=REL&action=export&out=DEST     (materialise into a CSV job)
//!   GET /health
//!   GET /meta?path=REL
//!   GET /rows?path=REL&start=N&count=M
//!   GET /rows?path=REL&sql=SELECT...&start=N&count=M   (window over a query)
//!   GET /stats?path=REL&sample=N
//!   GET /filter?path=REL&where=col=value&limit=N
//!   GET /search?path=REL&q=text&columns=a,b&limit=N
//!   GET /analyze?path=REL&distinctCap=N
//!   GET /groupby?path=REL&by=city&agg=amount:sum,amount:count&order=sum_amount
//!   GET /files?dir=REL&ext=csv,tsv&limit=N
//!   GET /download?path=REL                         (raw bytes, as attachment)
//!   GET /export?path=REL&sql=...&limit=N           (query result as CSV text)
//!   GET /export?path=REL&where=a=1|b>2             (filtered, kept as CSV)
//!   POST /sort?path=REL&by=col&desc=1&out=DEST     -> {"job":"..."}
//!   GET /query?path=REL&sql=SELECT...&limit=N
//!
//! Write endpoints return a job id instead of blocking:
//!   POST /split?path=REL&splits=N&format=zst&out=REL   -> {"job":"..."}
//!   POST /merge?path=REL&out=REL                        -> {"job":"..."}
//!   POST /warm?path=REL&sql=...                       -> {"job":"..."}
//!   GET  /job?id=...    (poll)      GET /jobs  (list)

use csv_splitter_core::cache::ResultCache;
use csv_splitter_core::dedupe;
use csv_splitter_core::filter::{self, scan as filter_scan, FilterSpec};
use csv_splitter_core::groupby;
use csv_splitter_core::jobs::JobRegistry;
use csv_splitter_core::listing;
use csv_splitter_core::merge::merge_archive;
use csv_splitter_core::partition;
use csv_splitter_core::preview::compute_column_stats;
use csv_splitter_core::profile;
use csv_splitter_core::search::{search as search_scan, SearchSpec};
use csv_splitter_core::splitter::run_split_threads;
use csv_splitter_core::sql;
use csv_splitter_core::viewer::WindowViewer;
use csv_splitter_core::workbook::{self, ColumnSource, Journal, Op};
use csv_splitter_core::{CompressFormat, Delimiter, ProgressEvent, SplitConfig};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_ROWS_PER_REQUEST: u64 = 2_000;
const MAX_SAMPLE: usize = 200_000;

pub struct ServerConfig {
    pub addr: String,
    /// Every request path must resolve inside this directory.
    pub root: PathBuf,
}

pub fn run(cfg: ServerConfig) -> io::Result<()> {
    let root = cfg.root.canonicalize()?;
    // A previous run may have left materialised copies behind (the process was
    // killed before their Drop ran). Sweep anything older than an hour: bounded
    // junk is acceptable, silent accumulation is not.
    sweep_stale_temp();
    let listener = TcpListener::bind(&cfg.addr)?;
    let local = listener.local_addr()?;
    if !local.ip().is_loopback() {
        eprintln!("warning: listening on {local}, not loopback — anyone who can reach this port can read files under {}", root.display());
    }
    println!(
        "csv-splitter API on http://{local}  (root: {})",
        root.display()
    );
    install_signal_handlers();
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // The handler writes the global; this thread owns the copy the loop reads.
    std::thread::spawn({
        let flag = flag.clone();
        move || loop {
            if SHUTDOWN.swap(false, std::sync::atomic::Ordering::SeqCst) {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });
    serve_on_with(listener, root, flag)
}

/// Set once a SIGINT/SIGTERM arrives; the accept loop watches it.
///
/// Written from a signal handler, so it is a plain atomic store and nothing else.
static SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_signal(_signal: libc::c_int) {
    SHUTDOWN.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Install the handlers. Called once by `run`, not by tests, so a test process is not
/// accidentally shut down by another test's signal.
///
/// Unix only: Windows has no SIGTERM to catch, and the default Ctrl+C handling is what a
/// user of a console binary expects there. The graceful path still exists — it is just
/// triggered by process termination rather than by a signal we install.
#[cfg(unix)]
fn install_signal_handlers() {
    unsafe {
        // `signal` wants a numeric handler; going through a pointer keeps the function's
        // ABI instead of an integer cast of the item itself.
        let handler: libc::sighandler_t = {
            let pointer: *const () = on_signal as *const ();
            pointer as libc::sighandler_t
        };
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

#[cfg(not(unix))]
fn install_signal_handlers() {
    // Nothing to install: Ctrl+C terminates the process directly on Windows.
}

/// Remove materialised sheet copies left by an earlier, killed server.
fn sweep_stale_temp() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("csvs_sheet_") || !name.ends_with(".csv") {
            continue;
        }
        let too_old = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age.as_secs() > 3600);
        if too_old {
            std::fs::remove_file(entry.path()).ok();
        }
    }
}

/// Serve until the listener fails. Exposed for tests, which bind port 0.
/// Serve until the flag is set, then stop accepting and let running work finish.
///
/// "Graceful" means three concrete things: no new connections, running jobs get a
/// bounded chance to complete, and in-flight requests are not cut off mid-response.
pub fn serve_on_with(
    listener: TcpListener,
    root: PathBuf,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
) -> io::Result<()> {
    let root = Arc::new(root);
    let jobs = JobRegistry::new();
    let cache = ResultCache::default();
    listener.set_nonblocking(true)?;
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    while !shutdown.load(std::sync::atomic::Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                // Accepted in nonblocking mode; the handler wants a blocking socket.
                stream.set_nonblocking(false)?;
                let root = Arc::clone(&root);
                let jobs = jobs.clone();
                let cache = cache.clone();
                let active = Arc::clone(&active);
                active.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::spawn(move || {
                    let _ = handle(stream, &root, &jobs, &cache);
                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                });
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => continue,
        }
    }
    // No new connections from here on. Give running jobs a bounded chance, then let the
    // process exit; nothing is killed silently, the client polling /job sees the state.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let running = jobs
            .list()
            .iter()
            .filter(|job| matches!(job.state, csv_splitter_core::jobs::JobState::Running))
            .count();
        let in_flight = active.load(std::sync::atomic::Ordering::SeqCst);
        if running == 0 && in_flight == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(())
}

/// How long a client may take to send a request, and how long it may take to read one.
///
/// Without these, a half-open connection holds a thread until the OS gives up — and a
/// browser tab that was closed mid-page is exactly that client.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A request line longer than this is not a real request: it is a body sent to a GET.
///
/// It sits above the query limit on purpose: the line carries the method and path too,
/// so a request whose *query* is over the limit should be told that (`414`), not have all
/// its parameters blamed.
const MAX_REQUEST_LINE: usize = 24 * 1024;
/// Headers beyond this are refused instead of buffered.
const MAX_HEADER_LINES: usize = 64;

fn handle(
    mut stream: TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    cache: &ResultCache,
) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    if request_line.len() > MAX_REQUEST_LINE {
        return respond_json(
            &mut stream,
            400,
            &format!(
                r#"{{"error":{}}}"#,
                json_str("the request line is too long; this API takes its parameters in the URL")
            ),
        );
    }
    // This API has no bodies and no auth, but two headers matter: the validator a
    // client already holds, and whether it only wants the headers of a GET (HEAD).
    let mut if_none_match: Option<String> = None;
    let mut accept_gzip = false;
    let mut header_lines = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        header_lines += 1;
        if header_lines > MAX_HEADER_LINES || line.len() > MAX_REQUEST_LINE {
            return respond_json(
                &mut stream,
                400,
                r#"{"error":"too many or too-long headers; this API takes its parameters in the URL"}"#,
            );
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("accept-encoding:") && lower.contains("gzip") {
            accept_gzip = true;
        }
        if let Some(rest) = lower.strip_prefix("if-none-match:") {
            if_none_match = Some(
                line[lower.len() - rest.len()..]
                    .trim()
                    .trim_start_matches("W/")
                    .to_string(),
            );
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    http_ctx::set(http_ctx::Request {
        head_only: method == "HEAD",
        if_none_match,
        route: format!("{method} {}", target.split('?').next().unwrap_or(target)),
        started: std::time::Instant::now(),
        accept_gzip,
    });
    let (path, query_string) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };

    if method == "OPTIONS" {
        // Preflight: 204 with the allow list; the browser then sends the real request.
        return respond(&mut stream, 204, "");
    }
    const MAX_QUERY: usize = 16 * 1024;
    if query_string.len() > MAX_QUERY {
        return respond_json(
            &mut stream,
            414,
            &format!(
                r#"{{"error":{}}}"#,
                json_str("the query string is longer than 16 KB; narrow the request")
            ),
        );
    }
    let params = parse_query(query_string);

    // Writes are POST; everything else is a read.
    if method == "POST" {
        return match path {
            "/split" => start_split(&mut stream, root, jobs, &params),
            "/merge" => start_merge(&mut stream, root, jobs, &params),
            "/warm" => start_warm(&mut stream, root, jobs, cache, &params),
            "/index" => start_index(&mut stream, root, jobs, &params),
            "/parquet" => start_parquet(&mut stream, root, jobs, &params),
            "/job" => job_cancel(&mut stream, jobs, &params),
            "/sort" => start_sort(&mut stream, root, jobs, &params),
            "/dedupe" => start_dedupe(&mut stream, root, jobs, &params),
            "/partition" => start_partition(&mut stream, root, jobs, &params),
            "/edit" => edit_apply(&mut stream, root, jobs, &params),
            _ => respond_json(&mut stream, 404, r#"{"error":"unknown endpoint"}"#),
        };
    }
    if method != "GET" && method != "HEAD" {
        return respond_json(&mut stream, 405, r#"{"error":"method not allowed"}"#);
    }

    match path {
        "/" | "/index.html" => respond_html(&mut stream, 200, VIEWER_HTML),
        "/health" => health_endpoint(&mut stream, root),
        "/metrics" => metrics_endpoint(&mut stream, jobs),
        "/meta" => meta(&mut stream, root, &params),
        "/rows" => rows(&mut stream, root, &params, cache),
        "/stats" => stats(&mut stream, root, &params),
        "/filter" => filter(&mut stream, root, &params),
        "/search" => search(&mut stream, root, &params),
        "/files" => files(&mut stream, root, &params),
        "/analyze" => analyze(&mut stream, root, &params),
        "/edit" => edit_state(&mut stream, root, &params),
        "/sheet" => sheet_meta(&mut stream, root, &params),
        "/columns" => columns_endpoint(&mut stream, root, &params),
        "/capabilities" => capabilities_endpoint(&mut stream),
        "/spec" => spec_endpoint(&mut stream),
        "/schema" => columns_endpoint(&mut stream, root, &params),
        "/cell" => cell_endpoint(&mut stream, root, &params),
        "/journal" => journal_endpoint(&mut stream, root, &params),
        "/groupby" => group_by(&mut stream, root, cache, &params),
        "/pivot" => pivot_endpoint(&mut stream, root, cache, &params),
        "/style" => style_endpoint(&mut stream, root, &params),
        "/chart" => chart_endpoint(&mut stream, root, &params),
        "/layout" => layout_endpoint(&mut stream, root, &params),
        "/book" => book_endpoint(&mut stream, root, &params),
        "/download" => download(&mut stream, root, &params),
        "/export" => export(&mut stream, root, &params),
        "/query" => query(&mut stream, root, &params),
        "/job" => {
            if params.get("action").map(String::as_str) == Some("cancel") {
                job_cancel(&mut stream, jobs, &params)
            } else {
                job_status(&mut stream, jobs, &params)
            }
        }
        "/jobs" => job_list(&mut stream, jobs),
        _ => {
            // Same list the capabilities call publishes, built the same way.
            let endpoints: Vec<String> = READ_ROUTES.iter().map(|r| json_str(r)).collect();
            let writes: Vec<String> = WRITE_ROUTES.iter().map(|w| json_str(w)).collect();
            respond_json(
                &mut stream,
                404,
                &format!(
                    r#"{{"error":"unknown endpoint","endpoints":[{}],"writes":[{}]}}"#,
                    endpoints.join(","),
                    writes.join(",")
                ),
            )
        }
    }
}

/// The parameters each route reads, and the fields it answers with.
///
/// This is the machine-readable half of the contract: `/spec` publishes it and tests
/// compare it against the live responses, so a field that moves or a parameter that is
/// renamed fails a test instead of a client.
const SPEC_PARAMS: &[(&str, &[&str])] = &[
    (
        "/rows",
        &[
            "path",
            "start",
            "count",
            "where",
            "whereAny",
            "order",
            "desc",
            "columns",
            "start-col",
            "count-cols",
            "keys",
            "delim",
        ],
    ),
    (
        "/columns",
        &["path", "name", "kind", "limit", "sample", "delim"],
    ),
    ("/meta", &["path", "delim"]),
    ("/cell", &["path", "row", "col", "delim"]),
    ("/journal", &["path", "delim"]),
    ("/sheet", &["path", "delim"]),
    ("/health", &[]),
    ("/capabilities", &[]),
    ("/spec", &[]),
    ("/metrics", &[]),
    ("/parquet", &["path", "delim"]),
    (
        "/edit",
        &[
            "path", "action", "row", "col", "value", "expect", "name", "out", "where", "delim",
        ],
    ),
];

/// Response fields, per route. Empty means "not JSON" (files, jobs with their own shape).
const SPEC_FIELDS: &[(&str, &[&str])] = &[
    (
        "/rows",
        &[
            "start",
            "count",
            "requested",
            "limit",
            "total",
            "totalExact",
            "hasMore",
            "pastEnd",
            "columns",
            "frozenColumns",
            "rows",
            "rowKeys",
            "edited",
            "operations",
        ],
    ),
    (
        "/columns",
        &["count", "totalColumns", "shown", "kindFilter", "columns"],
    ),
    ("/cell", &["row", "column", "value", "key", "total"]),
    (
        "/journal",
        &[
            "stamp",
            "operations",
            "count",
            "redo",
            "redoCount",
            "canUndo",
            "canRedo",
            "damaged",
        ],
    ),
    (
        "/sheet",
        &[
            "stamp",
            "columns",
            "rows",
            "edited",
            "operations",
            "parquet",
            "capabilities",
        ],
    ),
    ("/health", &["ok", "ready", "version", "root"]),
    (
        "/metrics",
        &[
            "uptimeSeconds",
            "requests",
            "errors4xx",
            "errors5xx",
            "slowRequests",
            "p50Ms",
            "p95Ms",
            "status",
            "routes",
            "latencyBuckets",
            "jobs",
        ],
    ),
    (
        "/capabilities",
        &[
            "version",
            "reads",
            "pathRequired",
            "writes",
            "editActions",
            "limits",
            "caches",
        ],
    ),
    ("/spec", &[]),
];

/// Every GET route this server answers, in one place.
///
/// The dispatcher, the capabilities response and the 404 fallback all read this list,
/// because three copies of the same route list is how a new endpoint ends up served but
/// unadvertised (insert-column had exactly that problem in 2.8.0).
const READ_ROUTES: &[&str] = &[
    "/",
    "/health",
    "/meta",
    "/rows",
    "/columns",
    "/schema",
    "/cell",
    "/journal",
    "/sheet",
    "/capabilities",
    "/spec",
    "/metrics",
    "/book",
    "/pivot",
    "/style",
    "/chart",
    "/layout",
    "/stats",
    "/analyze",
    "/groupby",
    "/filter",
    "/search",
    "/files",
    "/download",
    "/export",
    "/query",
    "/job",
    "/jobs",
];

/// Reads that need `?path=`; the rest answer without a sheet.
const PATH_REQUIRED: &[&str] = &[
    "/meta",
    "/rows",
    "/columns",
    "/schema",
    "/cell",
    "/journal",
    "/sheet",
    "/pivot",
    "/style",
    "/chart",
    "/layout",
    "/stats",
    "/analyze",
    "/groupby",
    "/filter",
    "/search",
    "/download",
    "/export",
    "/query",
];

/// Every POST route, same rule.
const WRITE_ROUTES: &[&str] = &[
    "POST /split",
    "POST /merge",
    "POST /sort",
    "POST /dedupe",
    "POST /partition",
    "POST /warm",
    "POST /edit",
    "POST /index",
    "POST /parquet",
];

/// A cheap fingerprint of the journal: ops + bytes + mtime.
///
/// Reads are lock-free, so a request can see state that changes a millisecond later.
/// This lets a client answer "is the screen I rendered still current?" without a
/// transaction: fetch it with `/sheet`, compare after a write, re-read if it moved.
/// It is a change detector, not a version clock — two edits that happen to restore the
/// same byte count and mtime second would look identical, and that is acceptable for
/// "should I refresh?" but not for correctness decisions. Those use `expect=`.
fn journal_stamp(path: &Path) -> String {
    let sidecar = Journal::sidecar(path);
    let meta = std::fs::metadata(&sidecar).ok();
    let ops = Journal::load(&sidecar).map(|j| j.len()).unwrap_or(0);
    format!(
        "{ops}:{}:{}",
        meta.as_ref().map(|m| m.len()).unwrap_or(0),
        meta.and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    )
}

/// The query timeout, in milliseconds. Tunable for an operator whose box is slower (or
/// for a test that needs to see the timeout path without waiting thirty seconds).
fn query_timeout_ms() -> u64 {
    std::env::var("CSV_SPLITTER_QUERY_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|ms| *ms > 0)
        .unwrap_or(csv_splitter_core::sql::DEFAULT_TIMEOUT_MS)
}

/// `GET /metrics` — request counts, latency percentiles and error distribution.
///
/// Counts come from one place (`respond`), so a new endpoint is measured without
/// remembering to measure it.
fn metrics_endpoint(stream: &mut TcpStream, jobs: &JobRegistry) -> io::Result<()> {
    let all = jobs.list();
    let running = all
        .iter()
        .filter(|job| matches!(job.state, csv_splitter_core::jobs::JobState::Running))
        .count();
    let body = metrics::shared().to_json((all.len(), running));
    respond_json(stream, 200, &body)
}

/// `GET /health` — liveness and readiness in one answer.
///
/// Ready means "I can answer questions about files right now": the served directory
/// lists. Liveness alone would say `ok` for a process that can no longer read its root.
fn health_endpoint(stream: &mut TcpStream, root: &Path) -> io::Result<()> {
    let ready = std::fs::read_dir(root).is_ok();
    respond_json(
        stream,
        200,
        &format!(
            r#"{{"ok":true,"ready":{},"version":{},"root":{}}}"#,
            ready,
            json_str(env!("CARGO_PKG_VERSION")),
            json_str(&root.to_string_lossy())
        ),
    )
}

/// `GET /spec` — what this backend accepts and answers, as data.
///
/// Pointing a client generator at a prose document is how field names drift; this is
/// generated from the same tables the server uses, and tests compare it with live
/// responses and with `docs/API.md`.
fn spec_endpoint(stream: &mut TcpStream) -> io::Result<()> {
    let routes: Vec<String> = READ_ROUTES.iter().map(|r| json_str(r)).collect();
    let writes: Vec<String> = WRITE_ROUTES.iter().map(|w| json_str(w)).collect();
    let needed: Vec<String> = PATH_REQUIRED.iter().map(|r| json_str(r)).collect();
    let params = |rows: &[(&str, &[&str])]| -> String {
        let items: Vec<String> = rows
            .iter()
            .map(|(route, names)| {
                format!(
                    "{}:[{}]",
                    json_str(route),
                    names
                        .iter()
                        .map(|n| json_str(n))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect();
        format!("{{{}}}", items.join(","))
    };
    let fields = params(SPEC_FIELDS);
    let body = format!(
        r#"{{"version":{},"cors":true,"etag":true,"reads":[{}],"writes":[{}],"pathRequired":[{}],"caches":["parquet"],"limits":{{"rowsPerRequest":{MAX_ROWS_PER_REQUEST},"queryRows":10000,"querySeconds":{}}},"params":{},"fields":{},"errors":{{"400":"your request is wrong (path, column, SQL, parameter)","404":"the target is not there","405":"wrong method","409":"a compare-and-set lost","422":"this file cannot be read","504":"the engine was interrupted after 30 s"}}}}"#,
        json_str(env!("CARGO_PKG_VERSION")),
        routes.join(","),
        writes.join(","),
        needed.join(","),
        query_timeout_ms() / 1000,
        params(SPEC_PARAMS),
        fields
    );
    respond_json(stream, 200, &body)
}

/// `GET /capabilities` — one call that tells a front-end what this backend can do.
///
/// A client should not have to read a document to learn which writes exist, how many
/// rows a window may ask for, or that a write can be made conditional. Everything here
/// is generated from the same constants the handlers use, so it cannot drift.
/// `GET /cell?path=t.csv&row=3&col=city` — one cell, by name.
///
/// The counterpart of `expect=`: read the value, decide, then write conditionally. A
/// front-end that wants only that one value should not pay for a whole window.
fn cell_endpoint(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let (Some(row), Some(column)) = (
        params.get("row").and_then(|v| v.parse::<u64>().ok()),
        params.get("col"),
    ) else {
        return bad_request(
            stream,
            "cell needs row and col",
            "row",
            "row=<n>&col=<name>",
        );
    };
    if row == 0 {
        return bad_request(
            stream,
            "rows are 1-based; row 1 is the first data row",
            "row",
            "pass row=1 for the first row",
        );
    }
    let sheet = match csv_splitter_core::sheet::Sheet::open(
        &path,
        csv_splitter_core::sheet::ReadOptions {
            delimiter: delimiter(params),
        },
    ) {
        Ok(sheet) => sheet,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let columns = match sheet.columns() {
        Ok(columns) => columns,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let Some(slot) = columns.iter().position(|c| c == column) else {
        return bad_request(
            stream,
            "column not found",
            "col",
            &format!("available: {}", columns.join(", ")),
        );
    };
    let window = match sheet.window(row - 1, 1) {
        Ok(window) => window,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let Some(cells) = window.rows.first() else {
        return respond_json(
            stream,
            404,
            &format!(
                r#"{{"error":"row {row} is past the end","parameter":"row","hint":"total is {} data rows"}}"#,
                window.total
            ),
        );
    };
    let value = cells.get(slot).cloned().unwrap_or_default();
    respond_json(
        stream,
        200,
        &format!(
            r#"{{"row":{row},"column":{},"value":{},"key":{},"total":{}}}"#,
            json_str(column),
            json_str(&value),
            json_str(&match window.keys.first() {
                Some(csv_splitter_core::workbook::RowId::Base(i)) => format!("b:{i}"),
                Some(csv_splitter_core::workbook::RowId::Virtual(i)) => format!("v:{i}"),
                None => String::new(),
            }),
            window.total
        ),
    )
}

/// `GET /journal?path=t.csv` — what is pending, and what could be undone.
///
/// A front-end shows an undo stack; this is the stack. Each operation carries its
/// human description (1-based rows, the same text the CLI prints), so a UI can label a
/// button without re-deriving anything.
fn journal_endpoint(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let sidecar = Journal::sidecar(&path);
    let (journal, skipped, skipped_redo) = match Journal::load_reporting(&sidecar) {
        Ok(v) => v,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let ops: Vec<String> = journal
        .ops()
        .iter()
        .map(|op| {
            format!(
                r#"{{"tag":{},"description":{}}}"#,
                json_str(op.tag()),
                json_str(&op.describe())
            )
        })
        .collect();
    let redo: Vec<String> = journal
        .redo_ops()
        .iter()
        .map(|op| {
            format!(
                r#"{{"tag":{},"description":{}}}"#,
                json_str(op.tag()),
                json_str(&op.describe())
            )
        })
        .collect();
    respond_json(
        stream,
        200,
        &format!(
            r#"{{"path":{},"stamp":{},"operations":[{}],"count":{},"redo":[{}],"redoCount":{},"canUndo":{},"canRedo":{},"damaged":{{"operations":{skipped},"redo":{skipped_redo}}}}}"#,
            json_str(&rel),
            json_str(&journal_stamp(&path)),
            ops.join(","),
            journal.len(),
            redo.join(","),
            journal.redo_len(),
            !journal.is_empty(),
            journal.redo_len() > 0
        ),
    )
}

fn capabilities_endpoint(stream: &mut TcpStream) -> io::Result<()> {
    let reads_json: Vec<String> = READ_ROUTES.iter().map(|r| json_str(r)).collect();
    let writes_json: Vec<String> = WRITE_ROUTES.iter().map(|w| json_str(w)).collect();
    let actions: Vec<String> = EDIT_ACTIONS.iter().map(|a| json_str(a)).collect();
    respond_json(
        stream,
        200,
        &format!(
            r#"{{"version":{},"reads":[{}],"pathRequired":[{}],"writes":[{}],"editActions":[{}],"limits":{{"rowsPerRequest":{MAX_ROWS_PER_REQUEST},"columnsProjection":true,"horizontalPaging":true,"queryRows":10000,"querySeconds":{}}},"writeGuards":["expect=compare-and-set"],"caches":["parquet"],"notes":{}}}"#,
            json_str(env!("CARGO_PKG_VERSION")),
            reads_json.join(","),
            PATH_REQUIRED.iter().map(|r| json_str(r)).collect::<Vec<_>>().join(","),
            writes_json.join(","),
            actions.join(","),
            query_timeout_ms() / 1000,
            json_str(
                "rows are 1-based; columns= narrows a window; expect= refuses a write unless the cell still holds that value (409)"
            )
        ),
    )
}

/// `POST /job?action=cancel&id=…` — stop a job, or say honestly that it cannot stop.
///
/// A job only claims to be cancellable when its worker actually checks the flag: the
/// export loop does, DuckDB statements do not. Returning 409 for those is the truthful
/// answer; flipping a status label while the work continues would be worse than no
/// cancel button at all.
fn job_cancel(
    stream: &mut TcpStream,
    jobs: &JobRegistry,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let Some(id) = params.get("id") else {
        return bad_request(
            stream,
            "id is required",
            "id",
            "cancel the job id returned by a write endpoint",
        );
    };
    match jobs.cancel(id) {
        csv_splitter_core::jobs::CancelOutcome::Cancelling => respond_json(
            stream,
            202,
            &format!(
                r#"{{"job":{},"state":"cancelling","hint":{}}}"#,
                json_str(id),
                json_str(
                    "the worker stops at its next check; poll /job until it reports cancelled"
                )
            ),
        ),
        csv_splitter_core::jobs::CancelOutcome::Finished(state) => respond_json(
            stream,
            200,
            &format!(
                r#"{{"job":{},"state":{},"hint":{}}}"#,
                json_str(id),
                json_str(state.as_str()),
                json_str("the job already finished; nothing was stopped")
            ),
        ),
        csv_splitter_core::jobs::CancelOutcome::NotCancellable => respond_json(
            stream,
            409,
            &format!(
                r#"{{"error":"this job cannot be cancelled","parameter":"action","hint":{}}}"#,
                json_str("wait for it to finish, or stop the server (SIGTERM waits five seconds)")
            ),
        ),
        csv_splitter_core::jobs::CancelOutcome::Unknown => {
            respond_json(stream, 404, r#"{"error":"unknown job"}"#)
        }
    }
}

fn job_status(
    stream: &mut TcpStream,
    jobs: &JobRegistry,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let Some(id) = params.get("id") else {
        return bad_request(
            stream,
            "id is required",
            "id",
            "poll the job id returned by a write endpoint",
        );
    };
    match jobs.status(id) {
        Some(status) => respond_json(stream, 200, &job_json(&status)),
        None => respond_json(stream, 404, r#"{"error":"unknown job"}"#),
    }
}

fn job_list(stream: &mut TcpStream, jobs: &JobRegistry) -> io::Result<()> {
    let items: Vec<String> = jobs.list().iter().map(job_json).collect();
    respond_json(stream, 200, &format!(r#"{{"jobs":[{}]}}"#, items.join(",")))
}

fn job_json(status: &csv_splitter_core::jobs::JobStatus) -> String {
    let fraction = match status.fraction() {
        Some(f) => format!("{:.3}", f),
        None => "null".to_string(),
    };
    format!(
        r#"{{"id":{},"kind":{},"state":{},"cancellable":{},"phase":{},"progress":{},"total":{},"fraction":{},"elapsed":{},"output":{},"error":{},"messages":[{}]}}"#,
        json_str(&status.id),
        json_str(&status.kind),
        json_str(status.state.as_str()),
        status.cancellable,
        json_str(&status.phase),
        status.progress,
        status.total,
        fraction,
        status.elapsed_secs(),
        match &status.output {
            Some(p) => json_str(&p.to_string_lossy()),
            None => "null".to_string(),
        },
        match &status.error {
            Some(e) => json_str(e),
            None => "null".to_string(),
        },
        status
            .messages
            .iter()
            .map(|m| json_str(m))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Resolve an output path inside the root, creating nothing yet: the caller
/// writes there. Refuses paths that leave the root.
fn resolve_out(root: &Path, params: &HashMap<String, String>, key: &str) -> Option<PathBuf> {
    let rel = params.get(key)?.trim_start_matches(['/', '\\']).to_string();
    if rel.is_empty() {
        return None;
    }
    let candidate = root.join(&rel);
    // The file may not exist yet, so canonicalise the parent directory instead.
    let parent = candidate.parent()?.canonicalize().ok()?;
    if !parent.starts_with(root) {
        return None;
    }
    Some(candidate)
}

fn start_split(
    stream: &mut TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (input, _rel) = match resolve(root, params) {
        Some(v) => v,
        None => return bad_path(stream),
    };
    let Some(output) = resolve_out(root, params, "out") else {
        return bad_out(stream);
    };
    let splits: usize = params
        .get("splits")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let format = match params.get("format").map(String::as_str) {
        Some("zst") | None => CompressFormat::Zstd,
        Some("gz") => CompressFormat::Gzip,
        Some("bz2") => CompressFormat::Bzip2,
        Some("xz") => CompressFormat::Xz,
        Some("zip") => CompressFormat::Zip,
        Some("none") => CompressFormat::None,
        Some(other) => {
            return respond_json(
                stream,
                400,
                &format!(
                    r#"{{"error":{}}}"#,
                    json_str(&format!("unknown format {other}"))
                ),
            )
        }
    };

    let threads: usize = params
        .get("threads")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    let config = SplitConfig {
        input,
        output: Some(output),
        splits,
        max_bytes: None,
        compress_format: format,
        compress_level: params
            .get("level")
            .and_then(|v| v.parse().ok())
            .unwrap_or(3),
        buf_mb: 32,
        columns_file: None,
        delimiter: delimiter(params),
        sort_column: None,
        sort_descending: false,
    };

    let id = jobs.submit("split", move |tx| run_split_threads(config, threads, tx));
    respond_json(stream, 202, &format!(r#"{{"job":{}}}"#, json_str(&id)))
}

fn start_merge(
    stream: &mut TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (input, _rel) = match resolve(root, params) {
        Some(v) => v,
        None => return bad_path(stream),
    };
    let Some(output) = resolve_out(root, params, "out") else {
        return bad_out(stream);
    };

    let id = jobs.submit("merge", move |tx| {
        match merge_archive(&input, &output, &tx) {
            Ok(report) => {
                tx.send(ProgressEvent::Info(format!(
                    "merged {} chunk(s), {} records",
                    report.chunks, report.records
                )))
                .ok();
                tx.send(ProgressEvent::Finished {
                    output: report.output,
                })
                .ok();
            }
            Err(e) => {
                tx.send(ProgressEvent::Error(e.to_string())).ok();
            }
        }
    });
    respond_json(stream, 202, &format!(r#"{{"job":{}}}"#, json_str(&id)))
}

/// Per-column profile: empties, distinct counts, numeric summary.
fn analyze(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let cap: usize = params
        .get("distinctCap")
        .and_then(|v| v.parse().ok())
        .unwrap_or(profile::DEFAULT_DISTINCT_CAP)
        .max(1);
    let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
    drop(rx);
    match profile::profile(&path, delimiter(params), cap, &tx) {
        Ok(cols) => {
            let items: Vec<String> = cols
                .iter()
                .map(|c| {
                    let numeric = match &c.numeric {
                        Some(n) => format!(
                            r#"{{"count":{},"min":{},"max":{},"sum":{},"mean":{},"median":{}}}"#,
                            n.count,
                            n.min,
                            n.max,
                            n.sum,
                            n.mean,
                            match n.median {
                                Some(m) => m.to_string(),
                                None => "null".to_string(),
                            }
                        ),
                        None => "null".to_string(),
                    };
                    let top: Vec<String> = c
                        .top
                        .iter()
                        .map(|(v, n)| format!(r#"[{},{}]"#, json_str(v), n))
                        .collect();
                    format!(
                        r#"{{"name":{},"rows":{},"ragged":{},"empty":{},"nonEmpty":{},"distinct":{},"distinctTruncated":{},"numeric":{},"mixedTypes":{},"top":[{}]}}"#,
                        json_str(&c.name),
                        c.rows,
                        c.ragged,
                        c.empty,
                        c.non_empty,
                        c.distinct,
                        c.distinct_truncated,
                        numeric,
                        c.mixed_types,
                        top.join(",")
                    )
                })
                .collect();
            respond_json(
                stream,
                200,
                &format!(r#"{{"columns":[{}]}}"#, items.join(",")),
            )
        }
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

/// Deduplicate into `out` as a background job.
fn start_dedupe(
    stream: &mut TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (input, _rel) = match resolve(root, params) {
        Some(v) => v,
        None => return bad_path(stream),
    };
    let Some(out) = resolve_out(root, params, "out") else {
        return bad_out(stream);
    };
    let keys: Vec<String> = params
        .get("by")
        .map(|raw| raw.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let max_keys: usize = params
        .get("maxKeys")
        .and_then(|v| v.parse().ok())
        .unwrap_or(dedupe::DEFAULT_MAX_KEYS);
    let delimiter = delimiter(params);
    let result_path = out.clone();

    let id = jobs.submit("dedupe", move |tx| {
        match dedupe::dedupe(&input, delimiter, &keys, &out, max_keys, &tx) {
            Ok(summary) => {
                tx.send(ProgressEvent::Info(format!(
                    "kept {} of {} rows",
                    summary.kept, summary.scanned
                )))
                .ok();
                tx.send(ProgressEvent::Finished {
                    output: result_path,
                })
                .ok();
            }
            Err(e) => {
                tx.send(ProgressEvent::Error(e.to_string())).ok();
            }
        }
    });
    respond_json(stream, 202, &format!(r#"{{"job":{}}}"#, json_str(&id)))
}

/// Report the pending edits for a file (GET /edit).
fn edit_state(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let sidecar = Journal::sidecar(&path);
    let journal = match Journal::load(&sidecar) {
        Ok(j) => j,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let logical_rows = workbook::logical_rows(&path, journal.ops()).unwrap_or(0);
    let ops: Vec<String> = journal
        .ops()
        .iter()
        .map(|op| {
            format!(
                r#"{{"op":{},"description":{}}}"#,
                json_str(op.tag()),
                json_str(&op.describe())
            )
        })
        .collect();
    respond_json(
        stream,
        200,
        &format!(
            r#"{{"path":{},"journal":{},"operations":{},"logicalRows":{},"ops":[{}]}}"#,
            json_str(&params.get("path").cloned().unwrap_or_default()),
            json_str(&sidecar.to_string_lossy()),
            journal.len(),
            logical_rows,
            ops.join(",")
        ),
    )
}

/// Record an edit (POST /edit?action=...), or start the export job.
/// The write actions `POST /edit` accepts.
///
/// One list, two places: the "action is required" hint and the "unknown action" hint.
/// They used to be written separately and disagreed — each omitted different actions,
/// so the error text could not be trusted as a description of the endpoint.
const EDIT_ACTIONS: &[&str] = &[
    "set",
    "insert",
    "delete",
    "replace",
    "copy",
    "fill",
    "insert-column",
    "split-column",
    "formula",
    "drop",
    "rename",
    "undo",
    "redo",
    "export",
];

fn edit_apply(
    stream: &mut TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let Some(action) = params.get("action") else {
        return bad_request(
            stream,
            "action is required",
            "action",
            &EDIT_ACTIONS.join("|"),
        );
    };
    let sidecar = Journal::sidecar(&path);
    let mut journal = match Journal::load(&sidecar) {
        Ok(j) => j,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };

    if action == "export" {
        let Some(out) = resolve_out(root, params, "out") else {
            return bad_out(stream);
        };
        let ops = journal.ops().to_vec();
        let delimiter = delimiter(params);
        let base = path.clone();
        let result_path = out.clone();
        // `where=` narrows the export, evaluated against the finished row so a filter
        // on a computed column behaves like the spreadsheet.
        let only = match params.get("where").filter(|w| !w.is_empty()) {
            None => None,
            Some(expr) => {
                match workbook::column_layout(&ops, &workbook::base_header(&base, delimiter)?) {
                    Ok(layout) => {
                        let columns: Vec<String> = layout.into_iter().map(|(n, _)| n).collect();
                        let spec = match csv_splitter_core::filter::FilterSpec::parse(expr) {
                            Ok(spec) => spec,
                            Err(e) => {
                                return respond_json(
                                    stream,
                                    400,
                                    &format!(
                                        r#"{{"error":{},"parameter":"where","hint":"pass where=<column><op><value>"}}"#,
                                        json_str(&e)
                                    ),
                                )
                            }
                        };
                        let set = csv_splitter_core::filter::FilterSet::single(spec);
                        match set.compile(&columns) {
                            Ok(compiled) => Some(compiled),
                            Err(e) => {
                                return respond_json(
                                    stream,
                                    400,
                                    &format!(
                                        r#"{{"error":{},"parameter":"where"}}"#,
                                        json_str(&e.to_string())
                                    ),
                                )
                            }
                        }
                    }
                    Err(e) => {
                        return respond_json(
                            stream,
                            422,
                            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                        )
                    }
                }
            }
        };
        let id = jobs.submit_cancellable("edit-export", move |tx, cancel| {
            let cancel_ref = cancel.clone();
            let options = workbook::ExportOptions {
                only: only.as_ref(),
                columns: None,
                cancel: Some(&cancel_ref),
            };
            match workbook::export_selected(
                &base,
                &ops,
                &out,
                delimiter,
                csv_splitter_core::csv_reader::QuoteStyle::Rfc4180,
                options,
                &tx,
            ) {
                Ok(summary) => {
                    tx.send(ProgressEvent::Info(format!(
                        "{} rows × {} columns",
                        summary.rows,
                        summary.columns.len()
                    )))
                    .ok();
                    tx.send(ProgressEvent::Finished {
                        output: result_path,
                    })
                    .ok();
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    // A half-written export that looks complete is worse than none.
                    let _ = std::fs::remove_file(&result_path);
                    tx.send(ProgressEvent::Info("export cancelled".to_string()))
                        .ok();
                }
                Err(e) => {
                    tx.send(ProgressEvent::Error(e.to_string())).ok();
                }
            }
        });
        return respond_json(stream, 202, &format!(r#"{{"job":{}}}"#, json_str(&id)));
    }

    if action == "undo" {
        return match journal.undo() {
            Some(op) => match journal.save(&sidecar) {
                Ok(()) => respond_json(
                    stream,
                    200,
                    &format!(
                        r#"{{"undone":{},"operations":{}}}"#,
                        json_str(&op.describe()),
                        journal.len()
                    ),
                ),
                Err(e) => respond_json(
                    stream,
                    422,
                    &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                ),
            },
            None => respond_json(stream, 409, r#"{"error":"nothing to undo"}"#),
        };
    }

    if action == "redo" {
        return match journal.redo() {
            Some(op) => match journal.save(&sidecar) {
                Ok(()) => respond_json(
                    stream,
                    200,
                    &format!(
                        r#"{{"redone":{},"operations":{}}}"#,
                        json_str(&op.describe()),
                        journal.len()
                    ),
                ),
                Err(e) => respond_json(
                    stream,
                    422,
                    &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                ),
            },
            None => respond_json(
                stream,
                409,
                r#"{"error":"nothing to redo","parameter":"action","hint":"run action=undo first, or make a new edit"}"#,
            ),
        };
    }

    // Rows are 1-based on the wire, the same as the CLI and the same as A1
    // references. The journal stores 0-based physical indices, so the translation
    // happens here and nowhere else.
    // Row numbers arrive 1-based and are stored 0-based; a *count* is just a count.
    let number = |name: &str| -> Option<u64> {
        params
            .get(name)
            .and_then(|v| v.parse::<u64>().ok())
            .and_then(|n| n.checked_sub(1))
    };
    let plain =
        |name: &str| -> Option<u64> { params.get(name).and_then(|v| v.parse::<u64>().ok()) };
    // `row=0` means "before the first row", which does not exist; say so instead of
    // letting it fall through to "set needs row".
    if params.get("row").is_some_and(|v| v.trim() == "0") {
        return bad_request(
            stream,
            "rows are 1-based; row 1 is the first data row",
            "row",
            "pass row=1 for the first row",
        );
    }
    let op = match action.as_str() {
        "set" => {
            let (Some(row), Some(column), Some(value)) =
                (number("row"), params.get("col"), params.get("value"))
            else {
                return bad_request(
                    stream,
                    "set needs row, col and value",
                    "row",
                    "row=<n>&col=<name>&value=<text>",
                );
            };
            // `expect=` makes the write compare-and-set: a client that fetched this
            // window earlier must not overwrite a value it never saw.
            if let Some(expect) = params.get("expect") {
                let sheet = match csv_splitter_core::sheet::Sheet::open(
                    &path,
                    csv_splitter_core::sheet::ReadOptions {
                        delimiter: delimiter(params),
                    },
                ) {
                    Ok(sheet) => sheet,
                    Err(e) => {
                        return respond_json(
                            stream,
                            422,
                            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                        )
                    }
                };
                let columns = match sheet.columns() {
                    Ok(columns) => columns,
                    Err(e) => {
                        return respond_json(
                            stream,
                            422,
                            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                        )
                    }
                };
                let Some(slot) = columns.iter().position(|c| c == column) else {
                    return bad_request(
                        stream,
                        "column not found",
                        "col",
                        &format!("available: {}", columns.join(", ")),
                    );
                };
                let actual = match sheet.window(row, 1) {
                    Ok(window) => window
                        .rows
                        .first()
                        .and_then(|cells| cells.get(slot).cloned())
                        .unwrap_or_default(),
                    Err(e) => {
                        return respond_json(
                            stream,
                            422,
                            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                        )
                    }
                };
                if actual != *expect {
                    return respond_json(
                        stream,
                        409,
                        &format!(
                            r#"{{"error":"the value changed; nothing was written","parameter":"expect","hint":{},"actual":{},"expected":{}}}"#,
                            json_str("re-read the row and retry without expect="),
                            json_str(&actual),
                            json_str(expect)
                        ),
                    );
                }
            }
            Op::SetCell {
                row,
                column: column.clone(),
                value: value.clone(),
            }
        }
        "delete" => {
            let (Some(row), Some(count)) = (number("row"), plain("count")) else {
                return bad_request(
                    stream,
                    "delete needs row and count",
                    "row",
                    "row=<n>&count=<k>",
                );
            };
            if count == 0 {
                return bad_request(
                    stream,
                    "count must be at least 1",
                    "count",
                    "count=<k> with k >= 1",
                );
            }
            Op::DeleteRows { at: row, count }
        }
        "insert" => {
            let Some(row) = number("row") else {
                return bad_request(stream, "insert needs row", "row", "row=<n>&value=a,b,c");
            };
            let raw = params.get("value").cloned().unwrap_or_default();
            let cells = if raw.trim().is_empty() {
                Vec::new()
            } else {
                csv_splitter_core::csv_reader::parse_csv_record(&raw, b',')
            };
            Op::InsertRows {
                at: row,
                rows: vec![cells],
            }
        }
        "replace" => {
            let (Some(from), Some(to)) = (params.get("from"), params.get("to")) else {
                return bad_request(
                    stream,
                    "replace needs from and to",
                    "from",
                    "from=<old>&to=<new>&col=<name>",
                );
            };
            Op::Substitute {
                column: params.get("col").cloned(),
                from: from.clone(),
                to: to.clone(),
            }
        }
        "copy" => {
            let (Some(from), Some(to)) = (params.get("from"), params.get("to")) else {
                return bad_request(stream, "copy needs from and to", "from", "from=A1:B2&to=B3");
            };
            let columns = match workbook::column_layout(
                journal.ops(),
                &workbook::base_header(&path, delimiter(params))?,
            ) {
                Ok(layout) => layout.into_iter().map(|(name, _)| name).collect::<Vec<_>>(),
                Err(e) => {
                    return respond_json(
                        stream,
                        422,
                        &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                    )
                }
            };
            let (start_row, start_col, end_row, end_col) =
                match workbook::parse_a1_range(from, &columns) {
                    Ok(r) => r,
                    Err(e) => {
                        return respond_json(
                            stream,
                            400,
                            &format!(r#"{{"error":{}}}"#, json_str(&e)),
                        )
                    }
                };
            let (target_row, target_col) = match workbook::parse_a1(to, &columns) {
                Ok(r) => r,
                Err(e) => {
                    return respond_json(stream, 400, &format!(r#"{{"error":{}}}"#, json_str(&e)))
                }
            };
            let width = end_col - start_col + 1;
            let height = (end_row - start_row + 1) as usize;
            if width * height > workbook::MAX_BLOCK_CELLS {
                return respond_json(
                    stream,
                    400,
                    r#"{"error":"the block is too large to record as a paste","parameter":"from","hint":"use action=set with where= for bulk changes"}"#,
                );
            }
            if target_col + width > columns.len() {
                return respond_json(
                    stream,
                    400,
                    r#"{"error":"the pasted block does not fit","parameter":"to","hint":"move the target left or widen the sheet"}"#,
                );
            }
            let window = match workbook::window(
                &path,
                journal.ops(),
                start_row,
                height as u64,
                delimiter(params),
            ) {
                Ok(w) => w,
                Err(e) => {
                    return respond_json(
                        stream,
                        422,
                        &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                    )
                }
            };
            if window.rows.len() < height {
                return respond_json(
                    stream,
                    400,
                    r#"{"error":"the source range is taller than the sheet","parameter":"from","hint":"check the range against /sheet"}"#,
                );
            }
            let mut block = Vec::with_capacity(height);
            for row in &window.rows {
                let cells: Vec<String> = (0..width)
                    .map(|offset| row.get(start_col + offset).cloned().unwrap_or_default())
                    .collect();
                block.push(cells);
            }
            Op::PasteBlock {
                row: target_row,
                column: columns[target_col].clone(),
                rows: block,
            }
        }
        "insert-column" | "insertcolumn" => {
            let Some(name) = params.get("name").filter(|v| !v.is_empty()) else {
                return bad_request(
                    stream,
                    "insert-column needs name",
                    "name",
                    "name=<column>&value=<text>|formula=<expr>",
                );
            };
            let source = match (
                params.get("value").filter(|v| !v.is_empty()),
                params.get("formula").filter(|v| !v.is_empty()),
            ) {
                (Some(v), None) => ColumnSource::Literal(v.clone()),
                (None, Some(f)) => ColumnSource::Formula(f.clone()),
                _ => {
                    return bad_request(
                        stream,
                        "insert-column takes value or formula",
                        "value",
                        "value=<text> or formula=<expr>",
                    )
                }
            };
            Op::AddColumnAt {
                after: params.get("after").cloned().filter(|v| !v.is_empty()),
                name: name.clone(),
                source,
            }
        }
        "delete-where" | "deletewhere" => {
            let Some(filter) = params.get("where").cloned() else {
                return bad_request(
                    stream,
                    "delete-where needs where",
                    "where",
                    "where=<column>=<value>",
                );
            };
            let runs =
                match workbook::runs_matching(&path, journal.ops(), &filter, delimiter(params)) {
                    Ok(runs) => runs,
                    Err(e) => {
                        return respond_json(
                            stream,
                            422,
                            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                        )
                    }
                };
            if runs.is_empty() {
                return respond_json(
                    stream,
                    200,
                    r#"{"op":"dels","rows":0,"note":"nothing matched; the journal is unchanged"}"#,
                );
            }
            if runs.len() > 1_000 {
                return bad_request(
                    stream,
                    "the condition matches too many separate stretches",
                    "where",
                    "use a narrower condition, or GET /filter to export the rows you want",
                );
            }
            Op::DeleteRanges { runs }
        }
        "fill" => {
            let (Some(column), Some(from), Some(to)) = (
                params.get("col").cloned(),
                plain("from-row").or_else(|| plain("from")),
                plain("to-row").or_else(|| plain("to")),
            ) else {
                return bad_request(
                    stream,
                    "fill needs col, from-row and to-row",
                    "col",
                    "col=<column>&from-row=<n>&to-row=<n>&value=<text>",
                );
            };
            if from == 0 || to == 0 || from > to {
                return bad_request(
                    stream,
                    "from-row and to-row are 1-based, and from-row must be <= to-row",
                    "from-row",
                    "from-row=1&to-row=10",
                );
            }
            Op::FillRange {
                column,
                from: from - 1,
                to: to - 1,
                value: params.get("value").cloned().unwrap_or_default(),
            }
        }
        "split-column" | "splitcolumn" => {
            let (Some(column), Some(by), Some(into)) = (
                params.get("col").cloned(),
                params.get("by").cloned(),
                params.get("into").cloned(),
            ) else {
                return bad_request(
                    stream,
                    "split-column needs col, by and into",
                    "col",
                    "col=<column>&by=<separator>&into=<name,name,...>",
                );
            };
            let into: Vec<String> = into
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if into.len() < 2 {
                return bad_request(
                    stream,
                    "into needs at least two names",
                    "into",
                    "into=first,last (one name per new column)",
                );
            }
            Op::SplitColumn { column, by, into }
        }
        "drop" => {
            let Some(name) = params.get("name") else {
                return bad_request(stream, "drop needs name", "name", "name=<column>");
            };
            Op::DropColumn { name: name.clone() }
        }
        "rename" => {
            let (Some(from), Some(to)) = (params.get("from"), params.get("to")) else {
                return bad_request(
                    stream,
                    "rename needs from and to",
                    "from",
                    "from=<old>&to=<new>",
                );
            };
            Op::RenameColumn {
                from: from.clone(),
                to: to.clone(),
            }
        }
        other => {
            return respond_json(
                stream,
                400,
                &format!(
                    r#"{{"error":{},"parameter":"action","hint":{}}}"#,
                    json_str(&format!("unknown action {other}")),
                    json_str(&EDIT_ACTIONS.join("|"))
                ),
            )
        }
    };

    journal.push(op.clone());
    match journal.save(&sidecar) {
        Ok(()) => respond_json(
            stream,
            200,
            &format!(
                r#"{{"op":{},"description":{},"operations":{}}}"#,
                json_str(op.tag()),
                json_str(&op.describe()),
                journal.len()
            ),
        ),
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

/// Group and aggregate, returning the result directly (not a file).
///
/// Writes to a rate-limited cache of recent results: a dashboard polling the
/// same query should not rescan the file each refresh, and the file is small
/// (one row per group) by construction.
/// The workbook view: every sheet under the served root (GET /book).
fn book_endpoint(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    // A named directory that does not exist must not silently become the served root:
    // a client asking for "/other" would get this directory's sheets and believe it.
    let scan_root: PathBuf = match params
        .get("root")
        .filter(|r| !r.trim().is_empty() && r.trim() != ".")
    {
        None => root.to_path_buf(),
        Some(requested) => {
            let candidate = root.join(requested.trim_start_matches('/'));
            if !candidate.is_dir() {
                return bad_request(
                    stream,
                    "no such directory inside the served root",
                    "root",
                    "root=. for the whole tree, or root=<subdirectory>",
                );
            }
            candidate
        }
    };
    // A malformed number is a bad request, not something to ignore: silently
    // falling back to the default would make `max=nope` look like it worked.
    let max = match params.get("max") {
        None => csv_splitter_core::book::DEFAULT_MAX_SHEETS,
        Some(raw) => match raw.parse::<usize>() {
            Ok(value) => value,
            Err(_) => {
                return bad_request(
                    stream,
                    "max must be a number",
                    "max",
                    "pass max=<number of sheets to list>",
                )
            }
        },
    };
    match csv_splitter_core::book::scan(&scan_root, max) {
        Ok(book) => {
            let sheets: Vec<String> = book.sheets.iter().map(|s| s.to_json()).collect();
            let unreadable: Vec<String> = book
                .unreadable
                .iter()
                .map(|(path, reason)| {
                    format!(
                        r#"{{"path":{},"error":{}}}"#,
                        json_str(&path.to_string_lossy()),
                        json_str(reason)
                    )
                })
                .collect();
            respond_json(
                stream,
                200,
                &format!(
                    r#"{{"root":{},"sheets":[{}],"hidden":{},"total":{},"truncated":{},"unreadable":[{}]}}"#,
                    json_str(&scan_root.to_string_lossy()),
                    sheets.join(","),
                    book.hidden,
                    book.total,
                    book.hidden > 0,
                    unreadable.join(",")
                ),
            )
        }
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

/// Column metadata: names, widths, frozen columns, number formats.
fn layout_endpoint(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let mut layout = match csv_splitter_core::layout::Layout::load(&path) {
        Ok(l) => l,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };

    // A POST-shaped call carries column/key/value as query parameters here, so the
    // router stays one code path for reads and writes.
    if let Some(column) = params.get("column").filter(|v| !v.is_empty()) {
        let key = params.get("key").map(String::as_str).unwrap_or("width");
        let value = params.get("value").cloned().unwrap_or_default();
        if let Err(e) = layout.set(column, key, &value) {
            return respond_json(
                stream,
                400,
                &format!(
                    r#"{{"error":{},"parameter":"value","hint":"name, width, frozen, format"}}"#,
                    json_str(&e)
                ),
            );
        }
        if let Err(e) = layout.save(&path) {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            );
        }
    }

    let base_columns = match workbook::base_header(&path, delimiter(params)) {
        Ok(c) => c,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let journal = Journal::load(&Journal::sidecar(&path)).unwrap_or_default();
    let columns: Vec<String> = match workbook::column_layout(journal.ops(), &base_columns) {
        Ok(layout) => layout.into_iter().map(|(name, _)| name).collect(),
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let resolved = layout.resolve(&columns);
    let items: Vec<String> = resolved
        .iter()
        .zip(&columns)
        .map(|(meta, original)| meta.to_json(original))
        .collect();
    respond_json(
        stream,
        200,
        &format!(r#"{{"columns":[{}]}}"#, items.join(",")),
    )
}

/// Chart data for the client to draw (GET /chart).
fn chart_endpoint(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let Some(x) = params.get("x").filter(|v| !v.is_empty()) else {
        return bad_request(stream, "x is required", "x", "pass x=<category column>");
    };
    let Some(y) = params.get("y").filter(|v| !v.is_empty()) else {
        return bad_request(stream, "y is required", "y", "pass y=<column:op>");
    };
    let spec = match csv_splitter_core::aggregate::AggSpec::parse_list(y) {
        Ok(mut v) if !v.is_empty() => v.remove(0),
        _ => {
            return respond_json(
                stream,
                400,
                r#"{"error":"y must be one column:op measure","parameter":"y","hint":"pass y=amount:sum"}"#,
            )
        }
    };
    let kind = match csv_splitter_core::chart::ChartKind::parse(
        params.get("type").map(String::as_str).unwrap_or("line"),
    ) {
        Ok(k) => k,
        Err(e) => return respond_json(stream, 400, &format!(r#"{{"error":{}}}"#, json_str(&e))),
    };
    let options = csv_splitter_core::chart::ChartOptions {
        x: x.clone(),
        series: params.get("series").cloned().filter(|v| !v.is_empty()),
        y: spec,
        kind,
        sort_by_value: params
            .get("sortByValue")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false),
        descending: params
            .get("desc")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false),
        max_points: params
            .get("maxPoints")
            .and_then(|v| v.parse().ok())
            .unwrap_or(csv_splitter_core::chart::DEFAULT_MAX_POINTS),
    };
    let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
    drop(rx);
    match csv_splitter_core::chart::chart(&path, delimiter(params), &options, &tx) {
        Ok(data) => respond_json(stream, 200, &data.to_json()),
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

/// Conditional formatting for a window: which cells to paint (GET /style).
fn style_endpoint(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let Some(rules_text) = params.get("rules").filter(|v| !v.is_empty()) else {
        return bad_request(
            stream,
            "rules is required",
            "rules",
            "pass rules=amount>1000:bg=red;status=late:fg=orange",
        );
    };
    let mut rules = Vec::new();
    for text in rules_text.split(';').filter(|s| !s.trim().is_empty()) {
        match csv_splitter_core::format::Rule::parse(text) {
            Ok(rule) => rules.push(rule),
            Err(e) => {
                return respond_json(stream, 400, &format!(r#"{{"error":{}}}"#, json_str(&e)))
            }
        }
    }
    let Some(start) = number_param(stream, params, "start", 0)? else {
        return Ok(());
    };
    let Some(count) = number_param(stream, params, "count", 100)? else {
        return Ok(());
    };
    let count = count.min(MAX_ROWS_PER_REQUEST);
    let sidecar = Journal::sidecar(&path);
    let journal = match Journal::load(&sidecar) {
        Ok(j) => j,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    match csv_splitter_core::format::styled_window(
        &path,
        journal.ops(),
        &rules,
        start,
        count,
        delimiter(params),
    ) {
        Ok(cells) => {
            let body: Vec<String> = cells.iter().map(|c| c.to_json()).collect();
            let rule_text: Vec<String> = rules.iter().map(|r| r.to_json()).collect();
            respond_json(
                stream,
                200,
                &format!(
                    r#"{{"start":{start},"count":{count},"cells":[{}],"rules":[{}]}}"#,
                    body.join(","),
                    rule_text.join(",")
                ),
            )
        }
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

/// Pivot a sheet into a rows × columns matrix (GET /pivot).
fn pivot_endpoint(
    stream: &mut TcpStream,
    root: &Path,
    cache: &ResultCache,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let Some(rows) = params.get("rows").filter(|v| !v.is_empty()) else {
        return bad_request(
            stream,
            "rows is required",
            "rows",
            "pass rows=<column[,column]>",
        );
    };
    let Some(values) = params.get("values").filter(|v| !v.is_empty()) else {
        return bad_request(
            stream,
            "values is required",
            "values",
            "pass values=column:sum[,column:count]",
        );
    };
    let specs = match csv_splitter_core::aggregate::AggSpec::parse_list(values) {
        Ok(v) => v,
        Err(e) => return respond_json(stream, 400, &format!(r#"{{"error":{}}}"#, json_str(&e))),
    };
    let row_columns: Vec<String> = rows.split(',').map(|k| k.trim().to_string()).collect();
    let columns = params.get("cols").cloned().filter(|v| !v.is_empty());
    let qualify = params
        .get("qualify")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(true);
    let max_columns: usize = params
        .get("maxColumns")
        .and_then(|v| v.parse().ok())
        .unwrap_or(csv_splitter_core::pivot::DEFAULT_MAX_COLUMNS);
    let max_cells: usize = params
        .get("maxCells")
        .and_then(|v| v.parse().ok())
        .unwrap_or(csv_splitter_core::pivot::DEFAULT_MAX_CELLS);

    let delimiter = delimiter(params);
    let options = csv_splitter_core::pivot::PivotOptions {
        rows: row_columns.clone(),
        columns: columns.clone(),
        values: specs.clone(),
        order_by: None,
        descending: false,
        qualify_names: qualify,
        max_columns,
        max_cells,
    };
    let cache_key = format!(
        "pivot|{rows}|{}|{values}|{qualify}|{max_columns}|{max_cells}",
        columns.clone().unwrap_or_default()
    );

    let loaded = cache.get_or_load(&path, &cache_key, delimiter, || {
        let out = std::env::temp_dir().join(format!(
            "csvs_pivot_{}_{}.csv",
            std::process::id(),
            unique_nanos()
        ));
        let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
        drop(rx);
        csv_splitter_core::pivot::pivot(&path, delimiter, &out, &options, &tx)?;
        let text = std::fs::read_to_string(&out)?;
        std::fs::remove_file(&out).ok();
        let mut rows = Vec::new();
        let mut columns = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let fields: Vec<String> = parse_csv_line(line);
            if index == 0 {
                columns = fields;
            } else {
                rows.push(fields);
            }
        }
        Ok(csv_splitter_core::cache::CachedResult {
            columns,
            rows,
            oversized: false,
        })
    });

    match loaded {
        Ok(result) => {
            let cols: Vec<String> = result.columns.iter().map(|c| json_str(c)).collect();
            let body: Vec<String> = result
                .rows
                .iter()
                .map(|row| {
                    let cells: Vec<String> = row.iter().map(|c| json_str(c)).collect();
                    format!("[{}]", cells.join(","))
                })
                .collect();
            respond_json(
                stream,
                200,
                &format!(
                    r#"{{"columns":[{}],"rows":[{}],"rowKeys":{}}}"#,
                    cols.join(","),
                    body.join(","),
                    result.rows.len()
                ),
            )
        }
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

/// Nanosecond stamp for temp file names (unique across processes and threads).
fn unique_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn group_by(
    stream: &mut TcpStream,
    root: &Path,
    cache: &ResultCache,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let Some(by) = params.get("by").filter(|b| !b.is_empty()) else {
        return bad_request(stream, "by is required", "by", "pass by=<column name>");
    };
    let Some(agg) = params.get("agg").filter(|a| !a.is_empty()) else {
        return respond_json(
            stream,
            400,
            r#"{"error":"agg is required","parameter":"agg","hint":"pass agg=column:sum,column:count"}"#,
        );
    };
    let keys: Vec<String> = by.split(',').map(|k| k.trim().to_string()).collect();
    let aggs = match groupby::AggSpec::parse_list(agg) {
        Ok(a) => a,
        Err(e) => return respond_json(stream, 400, &format!(r#"{{"error":{}}}"#, json_str(&e))),
    };
    let order = params.get("order").cloned();
    let descending = params
        .get("desc")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    let max_groups: usize = params
        .get("maxGroups")
        .and_then(|v| v.parse().ok())
        .unwrap_or(groupby::DEFAULT_MAX_GROUPS);

    // Reuse the query cache: the key is path+sql, so we synthesise a stable
    // pseudo-query that encodes every parameter.
    let sql_key = format!(
        "groupby|{by}|{agg}|{}|{descending}|{max_groups}",
        order.clone().unwrap_or_default()
    );
    let delimiter = delimiter(params);
    let out_dir = std::env::temp_dir();

    let loaded = cache.get_or_load(&path, &sql_key, delimiter, || {
        let unique = format!(
            "csvs_group_{}_{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let out = out_dir.join(unique);
        let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
        drop(rx);
        let summary = groupby::groupby(
            &path,
            delimiter,
            &out,
            &groupby::GroupOptions {
                keys: keys.clone(),
                aggregates: aggs.clone(),
                order_by: order.clone(),
                descending,
                max_groups,
            },
            &tx,
        )?;
        let text = std::fs::read_to_string(&out)?;
        std::fs::remove_file(&out).ok();
        let mut rows = Vec::new();
        let mut columns = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let fields: Vec<String> = parse_csv_line(line);
            if index == 0 {
                columns = fields;
            } else {
                rows.push(fields);
            }
        }
        let _ = summary;
        Ok(csv_splitter_core::cache::CachedResult {
            columns,
            rows,
            oversized: false,
        })
    });

    match loaded {
        Ok(result) => {
            let cols: Vec<String> = result.columns.iter().map(|c| json_str(c)).collect();
            let body: Vec<String> = result
                .rows
                .iter()
                .map(|row| {
                    let cells: Vec<String> = row.iter().map(|c| json_str(c)).collect();
                    format!("[{}]", cells.join(","))
                })
                .collect();
            respond_json(
                stream,
                200,
                &format!(
                    r#"{{"columns":[{}],"rows":[{}]}}"#,
                    cols.join(","),
                    body.join(",")
                ),
            )
        }
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => respond_json(
            stream,
            400,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

/// Split a CSV line into cells (the aggregate output is small and machine-made,
/// so a plain split after unquoting suffices).
fn parse_csv_line(line: &str) -> Vec<String> {
    csv_splitter_core::csv_reader::parse_csv_record(line, b',')
}

/// Partition the file by a column into an archive, as a background job.
fn start_partition(
    stream: &mut TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (input, _rel) = match resolve(root, params) {
        Some(v) => v,
        None => return bad_path(stream),
    };
    let Some(out) = resolve_out(root, params, "out") else {
        return bad_out(stream);
    };
    let Some(by) = params.get("by").filter(|b| !b.is_empty()) else {
        return bad_request(stream, "by is required", "by", "pass by=<column name>");
    };
    let format = match params.get("format").map(String::as_str) {
        Some("zst") | None => CompressFormat::Zstd,
        Some("gz") => CompressFormat::Gzip,
        Some("bz2") => CompressFormat::Bzip2,
        Some("xz") => CompressFormat::Xz,
        Some("zip") => CompressFormat::Zip,
        Some("none") => CompressFormat::None,
        Some(other) => {
            return respond_json(
                stream,
                400,
                &format!(
                    r#"{{"error":{}}}"#,
                    json_str(&format!("unknown format {other}"))
                ),
            )
        }
    };
    let max_partitions: usize = params
        .get("maxPartitions")
        .and_then(|v| v.parse().ok())
        .unwrap_or(partition::DEFAULT_MAX_PARTITIONS);

    let config = partition::PartitionConfig {
        input,
        output: Some(out.clone()),
        column: by.clone(),
        compress_format: format,
        compress_level: 3,
        buf_mb: 64,
        delimiter: delimiter(params),
        max_partitions,
    };
    let id = jobs.submit("partition", move |tx| {
        partition::run_partition(config, tx);
    });
    respond_json(stream, 202, &format!(r#"{{"job":{}}}"#, json_str(&id)))
}

/// Pre-load a query into the result cache so the first scroll is already fast.
///
/// On files with quoted newlines the CSV reader falls back to a serial scan
/// (a 10 GB query then costs seconds), so a client should start warming as soon
/// as it knows the filter, not when the user scrolls.
/// Sort the whole file into `out` as a background job.
fn start_sort(
    stream: &mut TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (input, _rel) = match resolve(root, params) {
        Some(v) => v,
        None => return bad_path(stream),
    };
    let Some(out) = resolve_out(root, params, "out") else {
        return bad_out(stream);
    };
    let Some(by) = params.get("by").filter(|b| !b.is_empty()) else {
        return bad_request(stream, "by is required", "by", "pass by=<column name>");
    };
    let descending = params
        .get("desc")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    let buf_mb: usize = params
        .get("bufMb")
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);
    let delimiter = delimiter(params);
    let by_owned = by.to_string();
    let result_path = out.clone();

    let id = jobs.submit(
        "sort",
        move |tx| match csv_splitter_core::sort::external_sort(
            &input, &out, &by_owned, descending, delimiter, buf_mb, &tx,
        ) {
            // A job only reaches `done` on Finished; without this it would stay
            // "running" forever even though the file is already complete.
            Ok(()) => {
                tx.send(ProgressEvent::Finished {
                    output: result_path,
                })
                .ok();
            }
            Err(e) => {
                tx.send(ProgressEvent::Error(e.to_string())).ok();
            }
        },
    );
    respond_json(stream, 202, &format!(r#"{{"job":{}}}"#, json_str(&id)))
}

/// `POST /parquet?path=REL` — build the columnar cache as a job.
///
/// A filtered page on a wide file costs about a second even when it needs ten rows,
/// because a CSV row has to be parsed whole. With this cache the same page is a column
/// read: about ten milliseconds. The cache is a sidecar (`t.csv.parquet`), never a
/// rewrite of the sheet, and it goes stale the moment the file changes.
fn start_parquet(
    stream: &mut TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    if csv_splitter_core::sql_cache::is_fresh(&path) {
        return respond_json(
            stream,
            200,
            &format!(
                r#"{{"job":null,"cached":true,"path":{},"bytes":{}}}"#,
                json_str(&rel),
                std::fs::metadata(csv_splitter_core::sql_cache::sidecar(&path))
                    .map(|m| m.len())
                    .unwrap_or(0)
            ),
        );
    }
    let delim = delimiter(params);
    let id = jobs.submit("parquet", move |tx| {
        tx.send(ProgressEvent::PhaseStart {
            phase: "building columnar cache".into(),
            total_bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        })
        .ok();
        let columns = match csv_splitter_core::sheet::Sheet::open(
            &path,
            csv_splitter_core::sheet::ReadOptions { delimiter: delim },
        ) {
            Ok(sheet) => sheet.schema(64),
            Err(e) => Err(e),
        };
        let outcome = match columns {
            Ok(columns) => {
                csv_splitter_core::sql_cache::build(&path, delim, &columns, query_timeout_ms() * 20)
            }
            Err(e) => Err(e),
        };
        match outcome {
            Ok(out) => {
                let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                tx.send(ProgressEvent::Info(format!(
                    "columnar cache ready: {bytes} bytes"
                )))
                .ok();
                tx.send(ProgressEvent::Finished { output: out }).ok();
            }
            Err(e) => {
                tx.send(ProgressEvent::Error(e.to_string())).ok();
            }
        }
    });
    respond_json(stream, 202, &format!(r#"{{"job":{}}}"#, json_str(&id)))
}

/// `POST /index?path=REL` — build the sparse row index as a job.
///
/// Paging a file without an index pays for a full scan inside the first request. A
/// client that knows it will page a big file can call this first, watch the job, and
/// then request pages without eating the scan under a timeout.
fn start_index(
    stream: &mut TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let interval = params
        .get("interval")
        .and_then(|v| v.parse().ok())
        .unwrap_or(csv_splitter_core::index::DEFAULT_INTERVAL)
        .max(1);
    let id = jobs.submit("index", move |tx| {
        let sidecar = csv_splitter_core::index::SparseIndex::sidecar_path(&path);
        let result = if sidecar.exists() {
            Ok((0, 0))
        } else {
            match csv_splitter_core::index::SparseIndex::build(&path, interval) {
                Ok(index) => match index.save(&sidecar) {
                    Ok(()) => {
                        let bytes = std::fs::metadata(&sidecar).map(|m| m.len()).unwrap_or(0);
                        Ok((index.data_rows(), bytes))
                    }
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            }
        };
        match result {
            Ok((rows, bytes)) => {
                tx.send(ProgressEvent::Info(format!(
                    "index ready: {rows} rows, {bytes} bytes"
                )))
                .ok();
                tx.send(ProgressEvent::Finished { output: sidecar }).ok();
            }
            Err(e) => {
                tx.send(ProgressEvent::Error(e.to_string())).ok();
            }
        }
    });
    respond_json(stream, 202, &format!(r#"{{"job":{}}}"#, json_str(&id)))
}

fn start_warm(
    stream: &mut TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    cache: &ResultCache,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let Some(statement) = params.get("sql") else {
        return bad_request(stream, "sql is required", "sql", "pass sql=<SELECT ...>");
    };
    // Fail fast on a bad query instead of hiding it inside a job.
    if let Err(e) = sql::validate(statement) {
        return respond_json(stream, 400, &format!(r#"{{"error":{}}}"#, json_str(&e)));
    }
    if cache.contains(&path, statement, delimiter(params)) {
        return respond_json(stream, 200, r#"{"job":null,"cached":true}"#);
    }

    let cache = cache.clone();
    let sql_owned = statement.to_string();
    let delim = delimiter(params);
    let id = jobs.submit("warm", move |tx| {
        tx.send(ProgressEvent::PhaseStart {
            phase: "warming query".into(),
            total_bytes: 0,
        })
        .ok();
        match cache.get_or_load(&path, &sql_owned, delim, || {
            sql::collect(&path, &sql_owned, delim, cache.row_cap())
        }) {
            Ok(result) => {
                let note = if result.oversized {
                    " (prefix only)"
                } else {
                    ""
                };
                tx.send(ProgressEvent::Info(format!(
                    "materialised {} rows{note}",
                    result.total()
                )))
                .ok();
                tx.send(ProgressEvent::Finished {
                    output: path.clone(),
                })
                .ok();
            }
            Err(e) => {
                tx.send(ProgressEvent::Error(e.to_string())).ok();
            }
        }
    });
    respond_json(stream, 202, &format!(r#"{{"job":{}}}"#, json_str(&id)))
}

fn meta(stream: &mut TcpStream, root: &Path, params: &HashMap<String, String>) -> io::Result<()> {
    let (path, rel) = match resolve(root, params) {
        Some(v) => v,
        None => return bad_path(stream),
    };
    match WindowViewer::open(&path, delimiter(params)) {
        Ok(viewer) => {
            let headers = viewer.headers().unwrap_or_default();
            let headers_json: Vec<String> = headers.iter().map(|h| json_str(h)).collect();
            let body = format!(
                r#"{{"path":{},"rows":{},"bytes":{},"headers":[{}],"delimiter":{},"encoding":{}}}"#,
                json_str(&rel),
                viewer.total_rows(),
                viewer.file_len(),
                headers_json.join(","),
                json_str(&(delimiter(params).byte() as char).to_string()),
                json_str(viewer.encoding_label())
            );
            respond_json(stream, 200, &body)
        }
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

/// A materialised copy plus the journal fingerprint it was built from.
type MaterialEntry = (Stamp, csv_splitter_core::sheet::Materialized);
type MaterialTable = std::sync::Mutex<HashMap<String, MaterialEntry>>;

/// Materialised copies of edited sheets, at most one per source file.
///
/// SQL needs a real file, and a temp copy is discarded as soon as the journal
/// changes. Keeping exactly one is deliberate: the entry exists so paging stays
/// cheap, not so disk fills up.
fn material_cache() -> &'static MaterialTable {
    static CACHE: std::sync::OnceLock<MaterialTable> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Identity of a file, for cache invalidation.
///
/// Two caches ask the same question — "has this changed since I built it?" — so the
/// answer lives in one place. The journal is hashed (it is small and append-only);
/// the base file is not, because it can be 10 GB, and its size plus modification time
/// is enough: the base is supposed to be immutable.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Stamp {
    hash: u64,
    len: u64,
}

impl Stamp {
    /// A journal's identity: the hash of its contents, so two writes inside one
    /// timestamp tick are still distinguished.
    fn journal(path: &Path) -> Self {
        let sidecar = Journal::sidecar(path);
        let Ok(bytes) = std::fs::read(&sidecar) else {
            return Self { hash: 0, len: 0 };
        };
        Self {
            hash: fnv1a(&bytes),
            len: bytes.len() as u64,
        }
    }

    /// A base file's identity: size and modification time, without reading it.
    fn base(path: &Path) -> Self {
        match std::fs::metadata(path) {
            Ok(meta) => {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                Self {
                    hash: modified,
                    len: meta.len(),
                }
            }
            Err(_) => Self { hash: 0, len: 0 },
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Serve a window of the edited sheet (base + journal).
/// Open sheets kept across requests.
///
/// Building a pager means reading the journal, the sparse index and compiling the
/// formulas — a few milliseconds, but the viewer asks for a page on every scroll.
/// The cache key carries the journal fingerprint and the base file's size/mtime, so
/// a stale entry can never be served: it is compared before use, and rebuilt if it
/// differs. Prefer a rebuild over a wrong page.
type PagerCache = std::sync::Mutex<HashMap<PagerKey, std::sync::Arc<workbook::Pager>>>;

#[derive(Clone, PartialEq, Eq, Hash)]
struct PagerKey {
    path: String,
    journal: Stamp,
    base: Stamp,
    delimiter: String,
}

fn pager_cache() -> &'static PagerCache {
    static CACHE: std::sync::OnceLock<PagerCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Keep the cache small: an editor opens a handful of files, not hundreds.
const MAX_CACHED_PAGERS: usize = 8;

fn pager_key(path: &Path, journal: Stamp, delimiter: &str) -> PagerKey {
    PagerKey {
        path: path.to_string_lossy().to_string(),
        journal,
        base: Stamp::base(path),
        delimiter: delimiter.to_string(),
    }
}

fn cached_pager(
    path: &Path,
    ops: &[workbook::Op],
    delimiter: csv_splitter_core::types::Delimiter,
    delimiter_name: &str,
) -> io::Result<std::sync::Arc<workbook::Pager>> {
    let key = pager_key(path, Stamp::journal(path), delimiter_name);
    {
        let guard = pager_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(pager) = guard.get(&key) {
            return Ok(pager.clone());
        }
    }
    let pager = std::sync::Arc::new(workbook::Pager::open(path, ops, delimiter)?);
    let mut guard = pager_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard.len() >= MAX_CACHED_PAGERS {
        // Any eviction is fine: the next request rebuilds what it needs.
        if let Some(stale) = guard.keys().next().cloned() {
            guard.remove(&stale);
        }
    }
    guard.insert(key, pager.clone());
    Ok(pager)
}

fn respond_sheet_window(
    stream: &mut TcpStream,
    path: &Path,
    start: u64,
    count: u64,
    requested: u64,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    // A cached pager is reused across scrolls; the key includes the journal
    // fingerprint and the base file's stamp, so an edit invalidates it rather than
    // serving the previous page from a stale sheet.
    let ops = match Journal::load(&Journal::sidecar(path)) {
        Ok(journal) => journal.ops().to_vec(),
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let delimiter_name = params.get("delim").cloned().unwrap_or_default();
    let pager = match cached_pager(path, &ops, delimiter(params), &delimiter_name) {
        Ok(p) => p,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let window = match pager.page(start, count) {
        Ok(w) => w,
        Err(e) => {
            return respond_json(
                stream,
                500,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let projected = match requested_columns(params) {
        None => (window.columns.clone(), window.rows.clone()),
        Some(wanted) => match project_window(&window.columns, &window.rows, &wanted) {
            Ok(pair) => pair,
            Err(e) => return bad_request(stream, &e, "columns", "columns=a,b,c"),
        },
    };
    let (display_columns, display_rows) = slice_columns(projected.0, projected.1, params);
    let row_keys = row_keys_json(&window.keys);
    let cols: Vec<String> = display_columns.iter().map(|c| json_str(c)).collect();
    let rows: Vec<String> = display_rows
        .iter()
        .map(|row| {
            let cells: Vec<String> = row.iter().map(|c| json_str(c)).collect();
            format!("[{}]", cells.join(","))
        })
        .collect();
    let rows_count = display_rows.len();
    let frozen: Vec<String> = csv_splitter_core::layout::Layout::load(path)
        .map(|l| {
            let count = l.frozen_prefix(&display_columns);
            display_columns
                .iter()
                .take(count)
                .map(|c| json_str(c))
                .collect()
        })
        .unwrap_or_default();
    respond_json(
        stream,
        200,
        &format!(
            r#"{{"start":{start},"count":{},"requested":{requested},"limit":{MAX_ROWS_PER_REQUEST},"total":{},"pastEnd":{},"columns":[{}],"frozenColumns":[{}],"rows":[{}],"rowKeys":{},"edited":true,"operations":{}}}"#,
            rows_count,
            window.total,
            window.total > 0 && start >= window.total,
            cols.join(","),
            frozen.join(","),
            rows.join(","),
            row_keys,
            ops.len()
        ),
    )
}

/// Everything a front-end needs to render the sheet chrome (GET /sheet).
/// `GET /columns?path=f.csv[&name=city][&sample=100]`
///
/// The column list a front-end renders from, and the existence check it asks before
/// drawing a column it was told about. `name` answers one question and nothing else,
/// so a client does not have to fetch 782 columns to learn that one is missing.
fn columns_endpoint(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let options = csv_splitter_core::sheet::ReadOptions {
        delimiter: delimiter(params),
    };
    let sheet = match csv_splitter_core::sheet::Sheet::open(&path, options) {
        Ok(s) => s,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };

    if let Some(name) = params.get("name") {
        let index = match sheet.column_index(name) {
            Ok(index) => index,
            Err(e) => {
                return respond_json(
                    stream,
                    422,
                    &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                )
            }
        };
        let body = match index {
            Some(index) => format!(
                r#"{{"name":{},"exists":true,"index":{index}}}"#,
                json_str(name)
            ),
            None => format!(r#"{{"name":{},"exists":false}}"#, json_str(name)),
        };
        return respond_json(stream, 200, &body);
    }

    let sample: u64 = params
        .get("sample")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let known_kinds = ["number", "date", "text", "empty", "mixed"];
    let kind_filter: Option<String> = params.get("kind").map(|k| k.trim().to_ascii_lowercase());
    if let Some(kind) = &kind_filter {
        if !known_kinds.contains(&kind.as_str()) {
            return bad_request(
                stream,
                &format!("unknown kind '{kind}'"),
                "kind",
                &format!("one of: {}", known_kinds.join(", ")),
            );
        }
    }
    let limit: Option<usize> = match params.get("limit").and_then(|v| v.parse::<usize>().ok()) {
        Some(0) => return bad_request(stream, "limit must be positive", "limit", "limit=50"),
        other => other,
    };
    // A pristine sheet is sampled directly: 782 column shapes must not cost a 2.5 GB
    // row index. Edited sheets go through the pager, because their columns may include
    // added ones and their rows are not the base file's.
    let pristine = !csv_splitter_core::sheet::has_edits(&path);
    let mut schema = if pristine {
        match csv_splitter_core::sheet::schema_sample(&path, delimiter(params), sample) {
            Ok(schema) => schema,
            Err(e) => {
                return respond_json(
                    stream,
                    422,
                    &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                )
            }
        }
    } else {
        match sheet.schema(sample) {
            Ok(schema) => schema,
            Err(e) => {
                return respond_json(
                    stream,
                    422,
                    &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                )
            }
        }
    };
    let total_columns = schema.len();
    if let Some(kind) = &kind_filter {
        schema.retain(|column| column.kind.as_str() == kind);
    }
    let matched = schema.len();
    let shown = match limit {
        Some(limit) if limit < schema.len() => {
            schema.truncate(limit);
            limit
        }
        _ => schema.len(),
    };
    // The row count needs the sparse index, and building it is a full scan. A client
    // that has not asked for /meta yet gets `null` rather than silently paying 0.3 s.
    let rows = if pristine {
        let sidecar = csv_splitter_core::index::SparseIndex::sidecar_path(&path);
        csv_splitter_core::index::SparseIndex::load(&sidecar)
            .ok()
            .filter(|index| index.matches(&path))
            .map(|index| index.data_rows())
            .map(|rows| rows.to_string())
            .unwrap_or_else(|| "null".to_string())
    } else {
        sheet.rows().unwrap_or(0).to_string()
    };
    let body: Vec<String> = schema
        .iter()
        .map(|column| {
            format!(
                r#"{{"name":{},"index":{},"kind":{},"empty":{},"sampled":{},"distinct":{},"distinctCapped":{}}}"#,
                json_str(&column.name),
                column.index,
                json_str(column.kind.as_str()),
                column.empty,
                column.sampled,
                column.distinct,
                column.distinct_capped
            )
        })
        .collect();
    respond_json(
        stream,
        200,
        &format!(
            r#"{{"path":{},"rows":{rows},"count":{},"totalColumns":{total_columns},"shown":{shown},"kindFilter":{},"columns":[{}]}}"#,
            json_str(&params.get("path").cloned().unwrap_or_default()),
            matched,
            match &kind_filter {
                Some(kind) => json_str(kind),
                None => "null".to_string(),
            },
            body.join(",")
        ),
    )
}

fn sheet_meta(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let options = csv_splitter_core::sheet::ReadOptions {
        delimiter: delimiter(params),
    };
    let sheet = match csv_splitter_core::sheet::Sheet::open(&path, options) {
        Ok(s) => s,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let columns = match sheet.columns() {
        Ok(c) => c,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let rows = sheet.rows().unwrap_or(0);
    let cols: Vec<String> = columns.iter().map(|c| json_str(c)).collect();
    let caps: Vec<String> = sheet.capabilities().iter().map(|c| json_str(c)).collect();
    let last_op: String = match sheet.ops().last() {
        Some(op) => format!(
            r#"{{"tag":{},"description":{}}}"#,
            json_str(op.tag()),
            json_str(&op.describe())
        ),
        None => "null".to_string(),
    };
    // Chrome travels with the data: a client should not need a second request to
    // learn a column's width or how its numbers print.
    let layout_json = match csv_splitter_core::layout::Layout::load(&path) {
        Ok(layout) => {
            let resolved = layout.resolve(&columns);
            let items: Vec<String> = resolved
                .iter()
                .zip(&columns)
                .map(|(meta, original)| meta.to_json(original))
                .collect();
            format!("[{}]", items.join(","))
        }
        Err(_) => "[]".to_string(),
    };
    let frozen_count = csv_splitter_core::layout::Layout::load(&path)
        .map(|l| l.frozen_prefix(&columns))
        .unwrap_or(0);
    let frozen_names: Vec<String> = columns
        .iter()
        .take(frozen_count)
        .map(|c| json_str(c))
        .collect();
    // Whether random access is already paid for. A client that pages a big file can
    // look here first and pre-warm instead of eating the scan inside a request.
    let index_sidecar = csv_splitter_core::index::SparseIndex::sidecar_path(&path);
    let indexed = index_sidecar.exists();
    let index_bytes = if indexed {
        std::fs::metadata(&index_sidecar)
            .map(|m| m.len())
            .unwrap_or(0)
    } else {
        0
    };
    respond_json(
        stream,
        200,
        &format!(
            r#"{{"path":{},"stamp":{},"columns":[{}],"frozenColumns":[{}],"rows":{},"edited":{},"operations":{},"lastOperation":{},"capabilities":[{}],"layout":{},"journal":{},"indexed":{},"indexBytes":{},"parquet":{{"fresh":{},"bytes":{}}},"cost":{}}}"#,
            json_str(&params.get("path").cloned().unwrap_or_default()),
            json_str(&journal_stamp(&path)),
            cols.join(","),
            frozen_names.join(","),
            rows,
            !sheet.is_pristine(),
            sheet.op_count(),
            last_op,
            caps.join(","),
            layout_json,
            json_str(&sheet.journal_path().to_string_lossy()),
            indexed,
            index_bytes,
            csv_splitter_core::sql_cache::is_fresh(&path),
            std::fs::metadata(csv_splitter_core::sql_cache::sidecar(&path))
                .map(|m| m.len())
                .unwrap_or(0),
            {
                // Published costs, not measured ones: these are the tiers the docs
                // promise, so a client can decide before it asks for a page.
                let warm = csv_splitter_core::sql_cache::is_fresh(&path);
                let tier = if warm { "warm" } else { "cold" };
                let filter_ms = if warm { 260 } else { 1000 };
                let order_ms = if warm { 340 } else { 1400 };
                let hint = if warm {
                    "the columnar cache is present: filtered and ordered pages are on the fast tier"
                } else {
                    "POST /parquet builds the columnar cache (about 11 s for 2.5 GB) and moves this sheet to the fast tier"
                };
                format!(
                    r#"{{"tier":{},"filterFirstPageMs":{filter_ms},"orderFirstPageMs":{order_ms},"hint":{}}}"#,
                    json_str(tier),
                    json_str(hint)
                )
            }
        ),
    )
}

/// `GET /rows?keys=b:12,v:3` — resolve stable row keys to their current rows.
///
/// The order is the order asked for. A key that no longer exists is reported in
/// `missing`, not silently dropped, so a front-end can clear its selection.
fn rows_by_keys_endpoint(
    stream: &mut TcpStream,
    path: &Path,
    params: &HashMap<String, String>,
    keys: &str,
) -> io::Result<()> {
    const MAX_KEYS: usize = 200;
    let mut wanted: Vec<csv_splitter_core::workbook::RowId> = Vec::new();
    let mut malformed: Vec<String> = Vec::new();
    for raw in keys.split(',').map(str::trim).filter(|k| !k.is_empty()) {
        match raw.split_once(':') {
            Some(("b", n)) => match n.parse::<u64>() {
                Ok(index) => wanted.push(csv_splitter_core::workbook::RowId::Base(index)),
                Err(_) => malformed.push(raw.to_string()),
            },
            Some(("v", n)) => match n.parse::<u64>() {
                Ok(id) => wanted.push(csv_splitter_core::workbook::RowId::Virtual(id)),
                Err(_) => malformed.push(raw.to_string()),
            },
            _ => malformed.push(raw.to_string()),
        }
    }
    if !malformed.is_empty() {
        return bad_request(
            stream,
            &format!("unreadable key(s): {}", malformed.join(", ")),
            "keys",
            "keys=b:12,b:13 or keys=v:3",
        );
    }
    if wanted.len() > MAX_KEYS {
        return bad_request(
            stream,
            &format!("at most {MAX_KEYS} keys per request"),
            "keys",
            "ask for a smaller selection",
        );
    }
    let sidecar = Journal::sidecar(path);
    let journal = match Journal::load(&sidecar) {
        Ok(journal) => journal,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let resolved = match workbook::rows_by_keys(path, journal.ops(), delimiter(params), &wanted) {
        Ok(rows) => rows,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let columns: Vec<String> = match workbook::column_layout(
        journal.ops(),
        &workbook::base_header(path, delimiter(params))?,
    ) {
        Ok(layout) => layout.into_iter().map(|(name, _)| name).collect(),
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let found_keys: Vec<String> = resolved
        .iter()
        .map(|(_, key, _)| match key {
            csv_splitter_core::workbook::RowId::Base(i) => format!("b:{i}"),
            csv_splitter_core::workbook::RowId::Virtual(i) => format!("v:{i}"),
        })
        .collect();
    let row_numbers: Vec<String> = resolved
        .iter()
        .map(|(logical, _, _)| (logical + 1).to_string())
        .collect();
    let missing: Vec<String> = wanted
        .iter()
        .filter(|key| !resolved.iter().any(|(_, found, _)| *found == **key))
        .map(|key| match key {
            csv_splitter_core::workbook::RowId::Base(i) => format!("b:{i}"),
            csv_splitter_core::workbook::RowId::Virtual(i) => format!("v:{i}"),
        })
        .collect();
    // The same projection and horizontal slice every other window offers.
    let cells: Vec<Vec<String>> = resolved.iter().map(|(_, _, cells)| cells.clone()).collect();
    let projected = match requested_columns(params) {
        None => (columns, cells),
        Some(wanted) => match project_window(&columns, &cells, &wanted) {
            Ok(pair) => pair,
            Err(e) => return bad_request(stream, &e, "columns", "columns=a,b,c"),
        },
    };
    let (display_columns, display_rows) = slice_columns(projected.0, projected.1, params);
    let rows_json: Vec<String> = display_rows
        .iter()
        .map(|cells| {
            let items: Vec<String> = cells.iter().map(|c| json_str(c)).collect();
            format!("[{}]", items.join(","))
        })
        .collect();
    let cols_json: Vec<String> = display_columns.iter().map(|c| json_str(c)).collect();
    respond_json(
        stream,
        200,
        &format!(
            r#"{{"columns":[{}],"rowKeys":[{}],"rowNumbers":[{}],"rows":[{}],"missing":[{}]}}"#,
            cols_json.join(","),
            found_keys
                .iter()
                .map(|k| json_str(k))
                .collect::<Vec<_>>()
                .join(","),
            row_numbers.join(","),
            rows_json.join(","),
            missing
                .iter()
                .map(|k| json_str(k))
                .collect::<Vec<_>>()
                .join(",")
        ),
    )
}

fn rows(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
    cache: &ResultCache,
) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let Some(start) = number_param(stream, params, "start", 0)? else {
        return Ok(());
    };
    let Some(count) = number_param(stream, params, "count", 100)? else {
        return Ok(());
    };
    let requested = count;
    let count = count.min(MAX_ROWS_PER_REQUEST);

    // `keys=` answers "where did my selection go?" — the inverse of `rowKeys`. A client
    // that kept `b:12` through an insert asks for it and gets the row number back.
    if let Some(keys) = params.get("keys") {
        return rows_by_keys_endpoint(stream, &path, params, keys);
    }

    // A filter without an order has no need to materialise the whole result: SQL
    // LIMIT/OFFSET lets the engine stop as soon as it has the page (plus one, to know
    // whether there is another). On a 2.5 GB file this is the difference between a
    // 15-second first page and a two-second one for a selective filter.
    if !csv_splitter_core::sheet::has_edits(&path)
        && (params.contains_key("where") || params.contains_key("order"))
    {
        let filter = filter_clause(params);
        let mut where_sql = String::new();
        if let Some(clause) = filter.as_ref() {
            where_sql = format!(" WHERE {clause}");
        }
        let order_sql = match params.get("order") {
            None => String::new(),
            Some(raw) => match order_clause(
                raw,
                params
                    .get("desc")
                    .map(|v| v == "1" || v == "true")
                    .unwrap_or(false),
            )
            .and_then(|(clause, used)| match known_columns(&path, params) {
                Ok(known) => {
                    if let Some(missing) = used.iter().find(|c| !known.contains(c)) {
                        return Err(format!(
                            "cannot order by '{missing}': available columns: {}",
                            known.join(", ")
                        ));
                    }
                    Ok(clause)
                }
                Err(e) => Err(e.to_string()),
            }) {
                Ok(clause) => format!(" {clause}"),
                Err(e) => return bad_request(stream, &e, "order", "order=col[:asc|desc]"),
            },
        };
        if filter.is_some() || !order_sql.is_empty() {
            // LIMIT/OFFSET goes to the engine, so a first page costs the rows it shows
            // (plus one). Without it an ordered first page sorted 684 917 wide rows and
            // collected 200 000 of them before answering: 15.5 seconds for ten rows.
            let statement = format!(
                "SELECT * FROM t{where_sql}{order_sql} LIMIT {} OFFSET {start}",
                count.saturating_add(1)
            );
            let parameter = if filter.is_none() { "order" } else { "where" };
            return respond_filtered_page(
                stream,
                &path,
                params,
                &statement,
                Page {
                    start,
                    count,
                    requested,
                },
                parameter,
            );
        }
    }

    // `where` / `order` turn a raw scan into a filtered, ordered view. They are
    // synonyms for the SQL endpoint, so the two cannot drift apart.
    //
    // A file with a journal makes this harder: SQL reads the base bytes, so it
    // would answer with pre-edit values. The edited sheet is materialised to a temp
    // file first (and refused above a size ceiling rather than quietly filling the
    // disk), so filtering and sorting agree with what /rows shows.
    if (params.contains_key("where") || params.contains_key("order"))
        && csv_splitter_core::sheet::has_edits(&path)
    {
        let options = csv_splitter_core::sheet::ReadOptions {
            delimiter: delimiter(params),
        };
        let fingerprint = Stamp::journal(&path);
        let key = path.to_string_lossy().to_string();
        {
            let mut guard = material_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let stale = match guard.get(&key) {
                Some((stamp, _)) => *stamp != fingerprint,
                None => true,
            };
            if stale {
                // Dropping the old entry removes its temp file.
                guard.remove(&key);
                let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
                drop(rx);
                match csv_splitter_core::sheet::materialize(&path, &options, &tx) {
                    Ok(materialised) => {
                        guard.insert(key.clone(), (fingerprint, materialised));
                    }
                    Err(e) => {
                        return respond_json(
                            stream,
                            422,
                            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                        )
                    }
                }
            }
        }

        let mut query = String::from("SELECT * FROM t");
        if let Some(clause) = filter_clause(params) {
            query.push_str(" WHERE ");
            query.push_str(&clause);
        }
        if let Some(order) = params.get("order").filter(|o| !o.is_empty()) {
            let default_desc = params
                .get("desc")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false);
            match order_clause(order, default_desc) {
                Ok((clause, used)) => match known_columns(&path, params) {
                    Ok(known) => {
                        if let Some(missing) = used.iter().find(|c| !known.contains(c)) {
                            return bad_request(
                                stream,
                                &format!(
                                    "cannot order by '{missing}': {}",
                                    csv_splitter_core::csv_reader::column_candidates(
                                        missing, &known
                                    )
                                ),
                                "order",
                                "order=<one of the columns in /columns>",
                            );
                        }
                        query.push_str(&clause);
                    }
                    Err(e) => {
                        return respond_json(
                            stream,
                            422,
                            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                        )
                    }
                },
                Err(e) => {
                    return bad_request(
                        stream,
                        &e,
                        "order",
                        "order=amount or order=amount:desc,id:asc",
                    )
                }
            }
        }
        let guard = material_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let path_buf = guard
            .get(&key)
            .map(|(_, m)| m.path().to_path_buf())
            .unwrap_or_else(|| path.clone());
        drop(guard);
        let statement = query;
        return respond_rows_from_cache(
            stream,
            cache,
            RowWindow {
                path: &path_buf,
                statement: &statement,
                start,
                count,
                requested,
            },
            params,
        );
    }
    if params.contains_key("where") || params.contains_key("order") {
        let mut query = String::from("SELECT * FROM t");
        if let Some(clause) = filter_clause(params) {
            query.push_str(" WHERE ");
            query.push_str(&clause);
        }
        if let Some(order) = params.get("order").filter(|o| !o.is_empty()) {
            let default_desc = params
                .get("desc")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false);
            match order_clause(order, default_desc) {
                Ok((clause, used)) => match known_columns(&path, params) {
                    Ok(known) => {
                        if let Some(missing) = used.iter().find(|c| !known.contains(c)) {
                            return bad_request(
                                stream,
                                &format!(
                                    "cannot order by '{missing}': {}",
                                    csv_splitter_core::csv_reader::column_candidates(
                                        missing, &known
                                    )
                                ),
                                "order",
                                "order=<one of the columns in /columns>",
                            );
                        }
                        query.push_str(&clause);
                    }
                    Err(e) => {
                        return respond_json(
                            stream,
                            422,
                            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
                        )
                    }
                },
                Err(e) => {
                    return bad_request(
                        stream,
                        &e,
                        "order",
                        "order=amount or order=amount:desc,id:asc",
                    )
                }
            }
        }
        return respond_rows_from_cache(
            stream,
            cache,
            RowWindow {
                path: &path,
                statement: &query,
                start,
                count,
                requested,
            },
            params,
        );
    }

    // A file with a journal is a sheet: the window must show the edits, not the
    // raw bytes. This is the same code path the CLI uses, so the two cannot
    // disagree about what a row means.
    if csv_splitter_core::sheet::has_edits(&path) {
        return respond_sheet_window(stream, &path, start, count, requested, params);
    }

    let mut viewer = match WindowViewer::open(&path, delimiter(params)) {
        Ok(v) => v,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    // With `sql=`, the window is taken from the query result instead of the
    // raw file: a table can page a filtered/sorted view directly. The result is
    // materialised once (up to a cap) and later pages are slices, so scrolling
    // does not re-scan a 10 GB file per frame.
    if let Some(statement) = params.get("sql") {
        return respond_rows_from_cache(
            stream,
            cache,
            RowWindow {
                path: &path,
                statement,
                start,
                count,
                requested,
            },
            params,
        );
    }

    if let Err(e) = viewer.ensure_range(start, count) {
        return respond_json(
            stream,
            500,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        );
    }

    let offset = start.saturating_sub(viewer.window_start()) as usize;
    let headers = viewer.headers().unwrap_or_default();
    let windowed: Vec<Vec<String>> = viewer
        .rows()
        .iter()
        .skip(offset)
        .take(count as usize)
        .cloned()
        .collect();
    let projected = match requested_columns(params) {
        None => (headers.clone(), windowed),
        Some(wanted) => match project_window(&headers, &windowed, &wanted) {
            Ok(pair) => pair,
            Err(e) => return bad_request(stream, &e, "columns", "columns=a,b,c"),
        },
    };
    let (display_columns, display_rows) = slice_columns(projected.0, projected.1, params);
    let base_keys: Vec<csv_splitter_core::workbook::RowId> = (0..display_rows.len())
        .map(|i| csv_splitter_core::workbook::RowId::Base(start + i as u64))
        .collect();
    let row_keys = row_keys_json(&base_keys);
    let rows: Vec<String> = display_rows
        .iter()
        .map(|row| {
            let cells: Vec<String> = row.iter().map(|c| json_str(c)).collect();
            format!("[{}]", cells.join(","))
        })
        .collect();

    // Column names travel with every window: a client that only pages rows
    // would otherwise have to make a second request to render a header.
    let headers = display_columns.clone();
    let cols: Vec<String> = headers.iter().map(|c| json_str(c)).collect();
    let frozen: Vec<String> = csv_splitter_core::layout::Layout::load(&path)
        .map(|l| {
            let count = l.frozen_prefix(&headers);
            headers.iter().take(count).map(|c| json_str(c)).collect()
        })
        .unwrap_or_default();
    let total = viewer.total_rows();
    // Every page answers these, whether it was filtered or not: a client that has to
    // branch on the shape of a response is a client that will get it wrong.
    let past_end = total > 0 && start >= start.saturating_add(rows.len() as u64);
    let body = format!(
        r#"{{"start":{start},"count":{},"requested":{requested},"limit":{MAX_ROWS_PER_REQUEST},"total":{total},"totalExact":true,"hasMore":{},"pastEnd":{past_end},"columns":[{}],"frozenColumns":[{}],"rows":[{}],"rowKeys":{},"edited":false,"operations":0}}"#,
        rows.len(),
        (start + rows.len() as u64) < total,
        cols.join(","),
        frozen.join(","),
        rows.join(","),
        row_keys
    );
    respond_json(stream, 200, &body)
}

fn query(stream: &mut TcpStream, root: &Path, params: &HashMap<String, String>) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let Some(statement) = params.get("sql") else {
        return bad_request(stream, "sql is required", "sql", "pass sql=<SELECT ...>");
    };
    const QUERY_LIMIT: usize = 10_000;
    let asked: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(sql::DEFAULT_LIMIT);
    // Truncation that hides itself is how a client ends up believing it saw every row.
    let limit = asked.min(QUERY_LIMIT);
    match sql::query_with_timeout(
        &path,
        statement,
        limit,
        delimiter(params),
        query_timeout_ms(),
    ) {
        Ok(result) => {
            let cols: Vec<String> = result.columns.iter().map(|c| json_str(c)).collect();
            let requested_limit = asked;
            let rows: Vec<String> = result
                .rows
                .iter()
                .map(|row| {
                    let cells: Vec<String> = row.iter().map(|c| json_str(c)).collect();
                    format!("[{}]", cells.join(","))
                })
                .collect();
            respond_json(
                stream,
                200,
                &format!(
                    r#"{{"columns":[{}],"rows":[{}],"truncated":{},"limit":{limit},"requestedLimit":{requested_limit}}}"#,
                    cols.join(","),
                    rows.join(","),
                    result.truncated
                ),
            )
        }
        // A timeout is neither the caller's mistake nor a broken file: 504 says the
        // engine was stopped, and the hint names the knob that produced it.
        Err(e) if e.kind() == io::ErrorKind::TimedOut => respond_json(
            stream,
            504,
            &format!(
                r#"{{"error":"the query was interrupted","parameter":"sql","hint":{}}}"#,
                json_str("add LIMIT or a WHERE clause, or ask for less data; the engine was stopped at the time limit")
            ),
        ),
        // An SQL mistake is the caller's mistake: say so with the same three-field
        // shape every other 400 uses, and keep the engine's own wording in `detail` —
        // DuckDB's message is the useful part, but it is not a whole response.
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => respond_json(
            stream,
            400,
            &format!(
                r#"{{"error":"the query could not be run","parameter":"sql","hint":{},"detail":{}}}"#,
                json_str("pass a SELECT statement over the file, e.g. sql=SELECT count(*) FROM t"),
                json_str(&e.to_string())
            ),
        ),
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

/// Stream a file under the root as a download.
///
/// Deliberately no range support and no content sniffing beyond the extension:
/// a local single-user tool needs "give me the bytes", not a CDN.
fn download(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let name = rel.rsplit('/').next().unwrap_or("download");
    respond_bytes(
        stream,
        200,
        "application/octet-stream",
        &format!("attachment; filename=\"{}\"", name.replace('"', "'")),
        &bytes,
    )
}

/// Run a query and return its rows as CSV text (not JSON) so it can be piped
/// straight into another tool.
/// Build a WHERE clause from `where` (AND) / `whereAny` (OR), `|`-separated.
///
/// Predicates are parsed with the same grammar as the CLI filter
/// (`col=value`, `col~text`, `col>number`, `col empty`) and then rendered as
/// SQL, which keeps string values quoted instead of letting them be read as
/// column names — and keeps a value can never break out of its literal.
fn filter_clause(params: &HashMap<String, String>) -> Option<String> {
    let render = |raw: &str, op: &str| -> Option<String> {
        let parts: Vec<String> = raw
            .split('|')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .filter_map(|p| FilterSpec::parse(p).ok())
            .filter_map(|spec| sql_predicate(&spec))
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(format!("({})", parts.join(&format!(" {op} "))))
        }
    };
    params
        .get("where")
        .and_then(|raw| render(raw, "AND"))
        .or_else(|| params.get("whereAny").and_then(|raw| render(raw, "OR")))
}

fn sql_predicate(spec: &FilterSpec) -> Option<String> {
    let column = &spec.column;
    let quote = |v: &str| format!("'{}'", v.replace('\'', "''"));
    let numeric = spec.value.trim().parse::<f64>().is_ok();
    let value = if numeric {
        spec.value.trim().to_string()
    } else {
        quote(&spec.value)
    };
    let sql = match spec.op {
        filter::Op::Eq => format!("{column} = {}", quote(&spec.value)),
        filter::Op::Ne => format!("{column} <> {}", quote(&spec.value)),
        filter::Op::Contains => format!(
            "{column} LIKE {}",
            quote(&format!("%{}%", spec.value.replace('%', "\\%")))
        ),
        filter::Op::Gt => format!("{column} > {value}"),
        filter::Op::Gte => format!("{column} >= {value}"),
        filter::Op::Lt => format!("{column} < {value}"),
        filter::Op::Lte => format!("{column} <= {value}"),
        filter::Op::Empty => format!("({column} IS NULL OR {column} = '')"),
        filter::Op::NotEmpty => format!("({column} IS NOT NULL AND {column} <> '')"),
        // `in a,b,c` becomes a quoted SQL list.
        filter::Op::In => {
            let list: Vec<String> = spec.value.split(',').map(|v| quote(v.trim())).collect();
            format!("{column} IN ({})", list.join(", "))
        }
        // DuckDB's regexp_match is RE2-like; the CLI validated the pattern.
        filter::Op::Regex => format!("regexp_matches({column}, {})", quote(&spec.value)),
    };
    Some(sql)
}

fn export(stream: &mut TcpStream, root: &Path, params: &HashMap<String, String>) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let limit: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(sql::DEFAULT_LIMIT)
        .min(100_000);

    // Either a query, or a `where` alone (which means "SELECT * FROM t WHERE").
    // The latter is the "filter, then keep the result" workflow.
    let statement = match (filter_clause(params), params.get("sql")) {
        (Some(clause), _) => format!("SELECT * FROM t WHERE {clause}"),
        (None, Some(query)) => query.clone(),
        (None, None) => {
            return bad_request(
                stream,
                "sql or where is required",
                "sql",
                "pass sql=<SELECT ...>, or where=<column=value>",
            )
        }
    };
    match sql::query_with(&path, &statement, limit, delimiter(params)) {
        Ok(result) => {
            let mut text = String::with_capacity(result.rows.len() * 32);
            text.push_str(&csv_join(&result.columns));
            text.push('\n');
            for row in &result.rows {
                text.push_str(&csv_join(row));
                text.push('\n');
            }
            respond_bytes(
                stream,
                200,
                "text/csv; charset=utf-8",
                "attachment; filename=\"result.csv\"",
                text.as_bytes(),
            )
        }
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => respond_json(
            stream,
            400,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

/// Quote cells the way a CSV reader expects.
fn csv_join(cells: &[String]) -> String {
    cells
        .iter()
        .map(|c| {
            if c.contains(',') || c.contains('"') || c.contains('\n') || c.contains('\r') {
                format!("\"{}\"", c.replace('"', "\"\""))
            } else {
                c.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// The window a caller asked for, plus everything needed to render it.
struct RowWindow<'a> {
    path: &'a Path,
    statement: &'a str,
    start: u64,
    count: u64,
    /// What the client asked for, before the page cap — reported so a truncated
    /// answer is not mistaken for a complete one.
    requested: u64,
}

/// Render a window of a materialised query result, caching it by (path, SQL).
/// Horizontal paging: `start-col=` and `count-cols=` slice the columns of a window.
///
/// A 782-column sheet does not fit on any screen, so a front-end scrolls sideways. Slicing
/// here means the bytes for the columns off-screen are never built.
fn slice_columns(
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    params: &HashMap<String, String>,
) -> (Vec<String>, Vec<Vec<String>>) {
    let start = params
        .get("start-col")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let count = params
        .get("count-cols")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    if start == 0 && count == usize::MAX {
        return (columns, rows);
    }
    let end = columns.len().min(start.saturating_add(count));
    if start >= end {
        return (Vec::new(), rows.into_iter().map(|_| Vec::new()).collect());
    }
    let kept_columns = columns[start..end].to_vec();
    let kept_rows = rows
        .into_iter()
        .map(|row| {
            let from = start.min(row.len());
            let to = end.min(row.len());
            row[from..to].to_vec()
        })
        .collect();
    (kept_columns, kept_rows)
}

fn row_keys_json(keys: &[csv_splitter_core::workbook::RowId]) -> String {
    use csv_splitter_core::workbook::RowId;
    let items: Vec<String> = keys
        .iter()
        .map(|key| match key {
            RowId::Base(index) => format!("\"b:{index}\""),
            RowId::Virtual(id) => format!("\"v:{id}\""),
        })
        .collect();
    format!("[{}]", items.join(","))
}

/// Build `ORDER BY` from `order=amount:desc,id` and the legacy `desc=1`.
///
/// Two callers had their own copy of this, and one of them passed `amount:desc` straight
/// to DuckDB — the client got a raw parser error naming an SQL statement it never wrote.
/// Accepted forms: `col`, `col:asc`, `col:desc`, comma-separated. Anything with a
/// character that is not part of a column name, a comma or a direction is a 400.
fn order_clause(order: &str, default_desc: bool) -> Result<(String, Vec<String>), String> {
    let mut parts: Vec<String> = Vec::new();
    let mut columns_used: Vec<String> = Vec::new();
    for raw in order.split(',') {
        let item = raw.trim();
        if item.is_empty() {
            continue;
        }
        let (column, direction) = match item.split_once(':') {
            Some((column, direction)) => {
                let direction = direction.trim().to_ascii_lowercase();
                if !matches!(direction.as_str(), "asc" | "desc") {
                    return Err(format!(
                        "order direction must be asc or desc, not {direction:?}"
                    ));
                }
                (column.trim(), Some(direction))
            }
            None => (item, None),
        };
        let allowed = column
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'));
        if column.is_empty() || !allowed {
            return Err(format!(
                "order must name a column: {item:?} is not a column name (order=amount or order=amount:desc)"
            ));
        }
        let suffix = match direction {
            Some(direction) => format!(" {}", direction.to_ascii_uppercase()),
            None if default_desc => " DESC".to_string(),
            None => String::new(),
        };
        parts.push(format!("{column}{suffix}"));
        columns_used.push(column.to_string());
    }
    if parts.is_empty() {
        return Err("order is empty".to_string());
    }
    Ok((format!(" ORDER BY {}", parts.join(", ")), columns_used))
}

/// The columns this sheet has, for checking names before they reach the engine.
fn known_columns(path: &Path, params: &HashMap<String, String>) -> io::Result<Vec<String>> {
    let sidecar = Journal::sidecar(path);
    let journal = Journal::load(&sidecar)?;
    let header = workbook::base_header(path, delimiter(params))?;
    Ok(workbook::column_layout(journal.ops(), &header)?
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

/// Where a page starts and how much of it was asked for.
struct Page {
    start: u64,
    count: u64,
    requested: u64,
}

/// One filtered page, without materialising the rest.
///
/// `total` is exact only when the page reached the end of the result: a caller that gets
/// `totalExact: false` knows `total` is "at least this many", not "this many".
fn respond_filtered_page(
    stream: &mut TcpStream,
    path: &Path,
    params: &HashMap<String, String>,
    statement: &str,
    page: Page,
    parameter: &str,
) -> io::Result<()> {
    let Page {
        start,
        count,
        requested,
    } = page;
    let result = match sql::query_with_timeout(
        path,
        statement,
        count as usize + 1,
        delimiter(params),
        query_timeout_ms(),
    ) {
        Ok(result) => result,
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            return respond_json(
                stream,
                504,
                &format!(
                    r#"{{"error":"the query exceeded its time limit","parameter":{},"hint":{}}}"#,
                    json_str(parameter),
                    json_str("narrow the filter or page without one; the engine was stopped at the time limit")
                ),
            )
        }
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => {
            return respond_json(
                stream,
                400,
                &format!(
                    r#"{{"error":"the filter could not be run","parameter":{},"hint":{},"detail":{}}}"#,
                    json_str(parameter),
                    json_str(if parameter == "order" {
                        "order=col[:asc|desc], comma-separated"
                    } else {
                        "where=<column=value>, repeat with | for AND"
                    }),
                    json_str(&e.to_string())
                ),
            )
        }
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let mut rows = result.rows;
    let has_more = rows.len() as u64 > count;
    if has_more {
        rows.truncate(count as usize);
    }
    let exact = !has_more;
    let total = if exact {
        start + rows.len() as u64
    } else {
        start + count
    };
    let projected = match requested_columns(params) {
        None => (result.columns.clone(), rows.clone()),
        Some(wanted) => match project_window(&result.columns, &rows, &wanted) {
            Ok(pair) => pair,
            Err(e) => return bad_request(stream, &e, "columns", "columns=a,b,c"),
        },
    };
    let (display_columns, display_rows) = slice_columns(projected.0, projected.1, params);
    let cols: Vec<String> = display_columns.iter().map(|c| json_str(c)).collect();
    let body: Vec<String> = display_rows
        .iter()
        .map(|row| {
            let cells: Vec<String> = row.iter().map(|c| json_str(c)).collect();
            format!("[{}]", cells.join(","))
        })
        .collect();
    let frozen: Vec<String> = csv_splitter_core::layout::Layout::load(path)
        .map(|l| {
            let prefix = l.frozen_prefix(&display_columns);
            display_columns
                .iter()
                .take(prefix)
                .map(|c| json_str(c))
                .collect()
        })
        .unwrap_or_default();
    // No `rowKeys` here: a filtered page's rows come from the engine and are not
    // `start + i` of the base file. Pretending otherwise would hand a client keys that
    // point at the wrong rows; `/rows` without a filter is where keys are meaningful.
    respond_json(
        stream,
        200,
        &format!(
            r#"{{"start":{start},"count":{},"requested":{requested},"limit":{MAX_ROWS_PER_REQUEST},"total":{total},"totalExact":{exact},"hasMore":{has_more},"pastEnd":{},"columns":[{}],"frozenColumns":[{}],"rows":[{}],"edited":false,"operations":0}}"#,
            display_rows.len(),
            exact && total > 0 && start >= total,
            cols.join(","),
            frozen.join(","),
            body.join(",")
        ),
    )
}

/// `columns=a,b,c` narrows a window to those columns, in that order.
///
/// A 782-column row is 3.6 KB of JSON; a client that renders three columns should not
/// pay for the other 779. Unknown names are refused rather than silently dropped.
fn requested_columns(params: &HashMap<String, String>) -> Option<Vec<String>> {
    params.get("columns").map(|value| {
        value
            .split(',')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
    })
}

fn project_window(
    columns: &[String],
    rows: &[Vec<String>],
    wanted: &[String],
) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    let mut slots = Vec::with_capacity(wanted.len());
    for name in wanted {
        match columns.iter().position(|c| c == name) {
            Some(slot) => slots.push(slot),
            None => {
                return Err(format!(
                    "column '{name}' is not in this sheet; available: {}",
                    columns.join(", ")
                ))
            }
        }
    }
    let picked_columns = slots.iter().map(|i| columns[*i].clone()).collect();
    let picked_rows = rows
        .iter()
        .map(|row| {
            slots
                .iter()
                .map(|slot| row.get(*slot).cloned().unwrap_or_default())
                .collect()
        })
        .collect();
    Ok((picked_columns, picked_rows))
}

fn respond_rows_from_cache(
    stream: &mut TcpStream,
    cache: &ResultCache,
    window: RowWindow<'_>,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let RowWindow {
        path,
        statement,
        start,
        count,
        requested,
    } = window;
    let delimiter = delimiter(params);
    let loaded = cache.get_or_load(path, statement, delimiter, || {
        sql::collect(path, statement, delimiter, cache.row_cap())
    });
    match loaded {
        Ok(cached) => {
            let (rows, has_more) = match cached.try_window(start as usize, count as usize) {
                Some(w) => w,
                None => {
                    return respond_json(
                        stream,
                        422,
                        &format!(
                            r#"{{"error":"result is larger than the cache prefix ({} rows); rerun the query or narrow it"}}"#,
                            cached.total()
                        ),
                    )
                }
            };
            let projected = match requested_columns(params) {
                None => (cached.columns.clone(), rows.clone()),
                Some(wanted) => match project_window(&cached.columns, &rows, &wanted) {
                    Ok(pair) => pair,
                    Err(e) => return bad_request(stream, &e, "columns", "columns=a,b,c"),
                },
            };
            let (display_columns, display_rows) = slice_columns(projected.0, projected.1, params);
            let cols: Vec<String> = display_columns.iter().map(|c| json_str(c)).collect();
            // The same context the plain window carries: a client should be able to
            // render frozen columns and say "edits applied" whichever path answered.
            let frozen: Vec<String> = csv_splitter_core::layout::Layout::load(path)
                .map(|l| {
                    let prefix = l.frozen_prefix(&cached.columns);
                    cached
                        .columns
                        .iter()
                        .take(prefix)
                        .map(|c| json_str(c))
                        .collect()
                })
                .unwrap_or_default();
            let edited = csv_splitter_core::sheet::has_edits(path);
            let operations = Journal::load(&Journal::sidecar(path))
                .map(|j| j.len())
                .unwrap_or(0);
            let body_rows: Vec<String> = display_rows
                .iter()
                .map(|row| {
                    let cells: Vec<String> = row.iter().map(|c| json_str(c)).collect();
                    format!("[{}]", cells.join(","))
                })
                .collect();
            respond_json(
                stream,
                200,
                &format!(
                    // Same fields the plain path reports: a client that reads one window
                    // shape should not need a second parser for the filtered one.
                    r#"{{"start":{start},"count":{},"requested":{requested},"limit":{MAX_ROWS_PER_REQUEST},"total":{},"hasMore":{},"columns":[{}],"frozenColumns":[{}],"rows":[{}],"edited":{edited},"operations":{operations}}}"#,
                    rows.len(),
                    cached.total(),
                    has_more,
                    cols.join(","),
                    frozen.join(","),
                    body_rows.join(",")
                ),
            )
        }
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => respond_json(
            stream,
            400,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

fn search(stream: &mut TcpStream, root: &Path, params: &HashMap<String, String>) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let Some(query) = params.get("q").filter(|q| !q.is_empty()) else {
        return bad_request(stream, "q is required", "q", "pass q=<text to search for>");
    };
    let mut spec = SearchSpec::new(query.clone());
    if let Some(columns) = params.get("columns").filter(|c| !c.is_empty()) {
        spec.columns = columns
            .split(',')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();
    }
    spec.regex = params.get("regex").is_some_and(|v| v == "1" || v == "true");
    spec.case_sensitive = params
        .get("case")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    spec.whole_cell = params
        .get("exact")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);

    let limit: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000)
        .min(MAX_ROWS_PER_REQUEST as usize * 10);
    let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
    drop(rx);
    match search_scan(&path, delimiter(params), &spec, limit, &tx) {
        Ok(result) => {
            let rows: Vec<String> = result.row_numbers().iter().map(|n| n.to_string()).collect();
            let cols: Vec<String> = result.columns.iter().map(|c| json_str(c)).collect();
            respond_json(
                stream,
                200,
                &format!(
                    r#"{{"scanned":{},"matched":{},"truncated":{},"columns":[{}],"rows":[{}]}}"#,
                    result.scanned,
                    result.matched,
                    result.truncated,
                    cols.join(","),
                    rows.join(",")
                ),
            )
        }
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => respond_json(
            stream,
            400,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

fn files(stream: &mut TcpStream, root: &Path, params: &HashMap<String, String>) -> io::Result<()> {
    // `dir` is optional and defaults to the root itself.
    let dir = match params.get("dir") {
        None => root.to_path_buf(),
        Some(rel) => {
            let rel = rel.trim_start_matches(['/', '\\']);
            let candidate = if rel.is_empty() {
                root.to_path_buf()
            } else {
                root.join(rel)
            };
            match candidate.canonicalize() {
                Ok(p) if p.starts_with(root) && p.is_dir() => p,
                _ => {
                    return respond_json(
                        stream,
                        400,
                        r#"{"error":"dir must be a directory inside root"}"#,
                    )
                }
            }
        }
    };
    let exts = params
        .get("ext")
        .cloned()
        .unwrap_or_else(|| "csv,tsv,txt,gz,zst,zip,bz2,xz".to_string());
    let limit: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(200)
        .min(10_000);

    match listing::list(&dir, &exts, limit) {
        Ok(list) => {
            let items: Vec<String> = list
                .files
                .iter()
                .map(|f| {
                    format!(
                        r#"{{"path":{},"bytes":{},"modified":{}}}"#,
                        json_str(&f.relative),
                        f.bytes,
                        f.modified_secs
                    )
                })
                .collect();
            respond_json(
                stream,
                200,
                &format!(
                    r#"{{"root":{},"truncated":{},"total":{},"files":[{}]}}"#,
                    json_str(&list.root.to_string_lossy()),
                    list.truncated,
                    list.total,
                    items.join(",")
                ),
            )
        }
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

fn filter(stream: &mut TcpStream, root: &Path, params: &HashMap<String, String>) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let Some(expr) = params.get("where") else {
        return respond_json(stream, 400, r#"{"error":"where is required"}"#);
    };
    let spec = match FilterSpec::parse(expr) {
        Ok(s) => s,
        Err(e) => return respond_json(stream, 400, &format!(r#"{{"error":{}}}"#, json_str(&e))),
    };
    let limit: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000)
        .min(MAX_ROWS_PER_REQUEST as usize * 10);
    let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
    drop(rx);
    match filter_scan(&path, delimiter(params), &spec, limit, &tx) {
        Ok(result) => {
            let rows: Vec<String> = result.row_numbers().iter().map(|n| n.to_string()).collect();
            respond_json(
                stream,
                200,
                &format!(
                    r#"{{"scanned":{},"matched":{},"truncated":{},"rows":[{}]}}"#,
                    result.scanned,
                    result.matched,
                    result.truncated,
                    rows.join(",")
                ),
            )
        }
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

fn stats(stream: &mut TcpStream, root: &Path, params: &HashMap<String, String>) -> io::Result<()> {
    let (path, _rel) = match resolve_reason(root, params) {
        Ok(v) => v,
        Err(reason) => return bad_path_reason(stream, reason),
    };
    let sample: usize = params
        .get("sample")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50_000)
        .min(MAX_SAMPLE);
    let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();
    drop(rx);
    match compute_column_stats(&path, delimiter(params), sample, 100_000, &tx) {
        Ok(cols) => {
            let items: Vec<String> = cols
                .iter()
                .map(|c| {
                    let top: Vec<String> = c
                        .top_values
                        .iter()
                        .take(10)
                        .map(|(v, n)| format!("[{},{}]", json_str(v), n))
                        .collect();
                    format!(
                        r#"{{"name":{},"unique":{},"empty":{},"sampled":{},"top":[{}]}}"#,
                        json_str(&c.name),
                        c.unique_count,
                        c.empty_count,
                        c.total_sampled,
                        top.join(",")
                    )
                })
                .collect();
            respond_json(
                stream,
                200,
                &format!(r#"{{"columns":[{}]}}"#, items.join(",")),
            )
        }
        Err(e) => respond_json(
            stream,
            422,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        ),
    }
}

fn delimiter(params: &HashMap<String, String>) -> Delimiter {
    match params.get("delim").map(String::as_str) {
        Some("tab") | Some("\\t") => Delimiter::Tab,
        Some("semicolon") | Some(";") => Delimiter::Semicolon,
        Some("pipe") | Some("|") => Delimiter::Pipe,
        _ => Delimiter::Comma,
    }
}

/// Resolve `?path=` against the root and refuse anything that escapes it.
fn resolve(root: &Path, params: &HashMap<String, String>) -> Option<(PathBuf, String)> {
    let rel = params
        .get("path")?
        .trim_start_matches(['/', '\\'])
        .to_string();
    if rel.is_empty() {
        return None;
    }
    let candidate = root.join(&rel);
    let canonical = candidate.canonicalize().ok()?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return None;
    }
    Some((canonical, rel))
}

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The built-in viewer: one file, no build step, no dependencies.
///
/// It exists so "the backend is done" is something you can *look at*: it calls
/// only the public endpoints, so anything it can do, a real front-end can too.
const VIEWER_HTML: &str = include_str!("web/index.html");

/// Parse an optional numeric parameter, refusing garbage instead of ignoring it.
///
/// `?count=abc` silently becoming the default is how a caller ends up paging with
/// the wrong window and blaming the data.
fn number_param(
    stream: &mut TcpStream,
    params: &HashMap<String, String>,
    name: &str,
    default: u64,
) -> io::Result<Option<u64>> {
    match params.get(name) {
        None => Ok(Some(default)),
        Some(raw) => match raw.parse::<u64>() {
            Ok(value) => Ok(Some(value)),
            Err(_) => {
                bad_request(
                    stream,
                    &format!("{name} must be a number"),
                    name,
                    &format!("pass {name}=<non-negative integer>"),
                )?;
                Ok(None)
            }
        },
    }
}

/// A 400 with a machine-readable reason.
///
/// Every client-side mistake gets the same shape:
/// `{"error":"<sentence>","parameter":"<name>","hint":"<what to pass>"}`.
/// A front-end can point at the offending input instead of showing a wall of
/// text, and a test can assert on `parameter` rather than on wording.
fn bad_request(stream: &mut TcpStream, error: &str, parameter: &str, hint: &str) -> io::Result<()> {
    respond_json(
        stream,
        400,
        &format!(
            r#"{{"error":{},"parameter":{},"hint":{}}}"#,
            json_str(error),
            json_str(parameter),
            json_str(hint)
        ),
    )
}

/// Why a `path` did not resolve: missing, not found, or outside the root.
///
/// Three different mistakes deserve three different sentences — "outside root" for a
/// typo'd filename sends people looking for a sandbox problem that does not exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathError {
    Missing,
    NotFound,
    OutsideRoot,
    NotAFile,
}

impl PathError {
    fn message(self) -> &'static str {
        match self {
            Self::Missing => "path is required",
            Self::NotFound => "no such file inside the served directory",
            Self::OutsideRoot => "path is outside the served directory",
            Self::NotAFile => "path is a directory, not a file",
        }
    }
}

fn resolve_reason(
    root: &Path,
    params: &HashMap<String, String>,
) -> Result<(PathBuf, String), PathError> {
    let rel = params
        .get("path")
        .map(|p| p.trim_start_matches(['/', '\\']).to_string())
        .ok_or(PathError::Missing)?;
    if rel.is_empty() {
        return Err(PathError::Missing);
    }
    let candidate = root.join(&rel);
    let canonical = candidate.canonicalize().map_err(|_| PathError::NotFound)?;
    if !canonical.starts_with(root) {
        return Err(PathError::OutsideRoot);
    }
    if !canonical.is_file() {
        return Err(PathError::NotAFile);
    }
    Ok((canonical, rel))
}

fn bad_path_reason(stream: &mut TcpStream, reason: PathError) -> io::Result<()> {
    bad_request(
        stream,
        reason.message(),
        "path",
        "pass path=<relative path inside the served directory>",
    )
}

/// `path` missing, or pointing outside `--root`.
fn bad_path(stream: &mut TcpStream) -> io::Result<()> {
    bad_request(
        stream,
        "path missing or outside root",
        "path",
        "pass path=<relative path inside the served directory>",
    )
}

fn bad_out(stream: &mut TcpStream) -> io::Result<()> {
    bad_request(
        stream,
        "out must stay inside root",
        "out",
        "pass out=<relative path inside the served directory>",
    )
}

fn respond_html(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    let request = http_ctx::current();
    let elapsed_ms = request.started.elapsed().as_secs_f64() * 1000.0;
    metrics::shared().record(&request.route, status, elapsed_ms);
    let reason = reason_for(status);
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         X-Response-Time: {elapsed_ms:.1}ms\r\n\
         {}Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{}",
        body.len(),
        cors_headers(),
        if request.head_only { "" } else { body }
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn respond_bytes(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    disposition: &str,
    body: &[u8],
) -> io::Result<()> {
    let reason = reason_for(status);
    let request = http_ctx::current();
    let elapsed_ms = request.started.elapsed().as_secs_f64() * 1000.0;
    metrics::shared().record(&request.route, status, elapsed_ms);
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         X-Response-Time: {elapsed_ms:.1}ms\r\n\
         Content-Type: {content_type}\r\n\
         Content-Disposition: {disposition}\r\n\
         Content-Length: {}\r\n\
         {}Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len(),
        cors_headers()
    );
    stream.write_all(head.as_bytes())?;
    if !request.head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

fn reason_for(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        414 => "URI Too Long",
        422 => "Unprocessable Entity",
        504 => "Gateway Timeout",
        _ => "Internal Server Error",
    }
}

fn respond_json(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    respond(stream, status, body)
}

/// Process-wide request accounting, so an operator can answer "what is slow" without a
/// profiler attached.
///
/// It is a global rather than a threaded parameter because every response goes through
/// one place already (`respond`); adding an argument to forty handlers to record a
/// millisecond would be the sixth time this phase duplicated a rule to avoid one shared
/// object.
mod metrics {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    /// Latency bucket upper bounds, in milliseconds; the last one is open-ended.
    const BUCKETS: [u64; 13] = [
        1, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000, 30000,
    ];

    #[derive(Clone, Default)]
    struct RouteStat {
        count: u64,
        total_ms: f64,
        max_ms: f64,
    }

    pub struct Metrics {
        started: Instant,
        requests: AtomicU64,
        slow: AtomicU64,
        errors_4xx: AtomicU64,
        errors_5xx: AtomicU64,
        by_status: Mutex<BTreeMap<u16, u64>>,
        by_route: Mutex<BTreeMap<String, RouteStat>>,
        buckets: Mutex<[u64; BUCKETS.len() + 1]>,
    }

    impl Metrics {
        fn new() -> Self {
            Self {
                started: Instant::now(),
                requests: AtomicU64::new(0),
                slow: AtomicU64::new(0),
                errors_4xx: AtomicU64::new(0),
                errors_5xx: AtomicU64::new(0),
                by_status: Mutex::new(BTreeMap::new()),
                by_route: Mutex::new(BTreeMap::new()),
                buckets: Mutex::new([0; BUCKETS.len() + 1]),
            }
        }

        pub fn record(&self, route: &str, status: u16, elapsed_ms: f64) {
            self.requests.fetch_add(1, Ordering::Relaxed);
            if (400..500).contains(&status) {
                self.errors_4xx.fetch_add(1, Ordering::Relaxed);
            }
            if status >= 500 {
                self.errors_5xx.fetch_add(1, Ordering::Relaxed);
            }
            if elapsed_ms >= 1000.0 {
                self.slow.fetch_add(1, Ordering::Relaxed);
            }
            if let Ok(mut map) = self.by_status.lock() {
                *map.entry(status).or_insert(0) += 1;
            }
            if let Ok(mut map) = self.by_route.lock() {
                let entry = map.entry(route.to_string()).or_default();
                entry.count += 1;
                entry.total_ms += elapsed_ms;
                entry.max_ms = entry.max_ms.max(elapsed_ms);
            }
            if let Ok(mut buckets) = self.buckets.lock() {
                let slot = BUCKETS
                    .iter()
                    .position(|upper| elapsed_ms <= *upper as f64)
                    .unwrap_or(BUCKETS.len());
                buckets[slot] += 1;
            }
        }

        /// Approximate percentile from the bucket counts: the upper bound of the bucket
        /// the target rank falls in. Approximate on purpose — the alternative is storing
        /// every sample forever in a server that is meant to run unattended.
        fn percentile(&self, quota: f64) -> u64 {
            let Ok(buckets) = self.buckets.lock() else {
                return 0;
            };
            let total: u64 = buckets.iter().sum();
            if total == 0 {
                return 0;
            }
            let target = (total as f64 * quota).ceil() as u64;
            let mut seen = 0u64;
            for (slot, count) in buckets.iter().enumerate() {
                seen += count;
                if seen >= target {
                    return BUCKETS
                        .get(slot)
                        .copied()
                        .unwrap_or(BUCKETS[BUCKETS.len() - 1]);
                }
            }
            BUCKETS[BUCKETS.len() - 1]
        }

        pub fn to_json(&self, jobs: (usize, usize)) -> String {
            let uptime = self.started.elapsed().as_secs();
            let status = match self.by_status.lock() {
                Ok(map) => map
                    .iter()
                    .map(|(code, count)| format!("\"{code}\":{count}"))
                    .collect::<Vec<_>>()
                    .join(","),
                Err(_) => String::new(),
            };
            let routes = match self.by_route.lock() {
                Ok(map) => map
                    .iter()
                    .map(|(route, stat)| {
                        format!(
                            "{}:{{\"count\":{},\"avgMs\":{:.1},\"maxMs\":{:.1}}}",
                            super::json_str(route),
                            stat.count,
                            if stat.count > 0 {
                                stat.total_ms / stat.count as f64
                            } else {
                                0.0
                            },
                            stat.max_ms
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                Err(_) => String::new(),
            };
            let buckets = match self.buckets.lock() {
                Ok(values) => values
                    .iter()
                    .enumerate()
                    .map(|(slot, count)| {
                        let label = BUCKETS
                            .get(slot)
                            .map(|b| format!("<={b}ms"))
                            .unwrap_or(">30000ms".to_string());
                        format!("{}:{count}", super::json_str(&label))
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                Err(_) => String::new(),
            };
            format!(
                r#"{{"uptimeSeconds":{uptime},"requests":{},"errors4xx":{},"errors5xx":{},"slowRequests":{},"p50Ms":{},"p95Ms":{},"status":{{{status}}},"routes":{{{routes}}},"latencyBuckets":{{{buckets}}},"jobs":{{"total":{},"running":{}}}}}"#,
                self.requests.load(Ordering::Relaxed),
                self.errors_4xx.load(Ordering::Relaxed),
                self.errors_5xx.load(Ordering::Relaxed),
                self.slow.load(Ordering::Relaxed),
                self.percentile(0.50),
                self.percentile(0.95),
                jobs.0,
                jobs.1
            )
        }
    }

    pub fn shared() -> &'static Metrics {
        static METRICS: OnceLock<Metrics> = OnceLock::new();
        METRICS.get_or_init(Metrics::new)
    }
}

/// What the current connection asked for that every response must honour.
///
/// Handlers take `&mut TcpStream` and nothing else, and threading a context through
/// forty of them would be churn; each connection is its own thread, so a thread-local is
/// exactly the right scope. It is set in `handle` and read by the two response helpers —
/// one place to add "the browser needs this header".
mod http_ctx {
    use std::cell::RefCell;

    #[derive(Clone)]
    pub struct Request {
        /// `HEAD`: send the headers a `GET` would, and no body.
        pub head_only: bool,
        /// `If-None-Match`: the validator the client already has.
        pub if_none_match: Option<String>,
        /// `METHOD /route`, for the metrics table.
        pub route: String,
        /// When this request started, for `X-Response-Time`.
        pub started: std::time::Instant,
        /// `Accept-Encoding` mentions gzip, so a large body may be compressed.
        pub accept_gzip: bool,
    }

    impl Default for Request {
        fn default() -> Self {
            Self {
                head_only: false,
                if_none_match: None,
                route: String::new(),
                started: std::time::Instant::now(),
                accept_gzip: false,
            }
        }
    }

    thread_local! {
        static CURRENT: RefCell<Request> = RefCell::new(Request::default());
    }

    pub fn set(request: Request) {
        CURRENT.with(|c| *c.borrow_mut() = request);
    }

    pub fn current() -> Request {
        CURRENT.with(|c| c.borrow().clone())
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    let request = http_ctx::current();
    let elapsed_ms = request.started.elapsed().as_secs_f64() * 1000.0;
    metrics::shared().record(&request.route, status, elapsed_ms);
    let timing = format!("X-Response-Time: {elapsed_ms:.1}ms\r\n");

    // A 782-column window is 3.6 KB per row; gzip is the difference between a page that
    // loads and one that does not. Compress first, because a compressed body is a
    // different representation and therefore carries a different validator.
    let payload: std::borrow::Cow<'_, [u8]> = if request.accept_gzip && body.len() > 512 {
        match gzip(body.as_bytes()) {
            Some(compressed) => std::borrow::Cow::Owned(compressed),
            None => std::borrow::Cow::Borrowed(body.as_bytes()),
        }
    } else {
        std::borrow::Cow::Borrowed(body.as_bytes())
    };
    let compressed = payload.len() < body.len();
    let etag = if compressed {
        format!("\"fnv-{:016x}-gz\"", fnv1a(body.as_bytes()))
    } else {
        format!("\"fnv-{:016x}\"", fnv1a(body.as_bytes()))
    };
    // Only a successful body is worth revalidating; a preflight or an error is not.
    let taggable = status == 200;
    if taggable && request.if_none_match.as_deref() == Some(etag.as_str()) {
        let head = format!(
            "HTTP/1.1 304 Not Modified\r\n\
             ETag: {etag}\r\n\
             Content-Length: 0\r\n\
             {timing}\
             {}Connection: close\r\n\r\n",
            cors_headers()
        );
        stream.write_all(head.as_bytes())?;
        return stream.flush();
    }
    let extra = if taggable {
        format!(
            "ETag: {etag}\r\n\
             Access-Control-Expose-Headers: ETag, X-Response-Time\r\n"
        )
    } else {
        String::new()
    };
    let encoding = if compressed {
        "Content-Encoding: gzip\r\nVary: Accept-Encoding\r\n"
    } else {
        "Vary: Accept-Encoding\r\n"
    };
    let reason = reason_for(status);
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         {encoding}{extra}{timing}{}Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        payload.len(),
        cors_headers()
    );
    stream.write_all(response.as_bytes())?;
    if !request.head_only {
        stream.write_all(&payload)?;
    }
    stream.flush()
}

/// Gzip a JSON body, or `None` when compression would not help.
fn gzip(bytes: &[u8]) -> Option<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    let mut encoder = GzEncoder::new(Vec::with_capacity(bytes.len() / 4), Compression::fast());
    std::io::Write::write_all(&mut encoder, bytes).ok()?;
    encoder.finish().ok()
}

/// The CORS answer, in one place: browsers need `POST`, `HEAD` and the preflight.
fn cors_headers() -> String {
    "Access-Control-Allow-Origin: *\r\n\
     Access-Control-Allow-Methods: GET, POST, HEAD, OPTIONS\r\n\
     Access-Control-Allow-Headers: *\r\n\
     Access-Control-Max-Age: 600\r\n\
     Vary: Origin\r\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_json_and_keeps_unicode() {
        assert_eq!(json_str("a\"b\\c\n中"), "\"a\\\"b\\\\c\\n中\"");
    }

    #[test]
    fn decodes_percent_encoding() {
        assert_eq!(percent_decode("a%20b%2Fc+d"), "a b/c d");
    }

    #[test]
    fn parses_query_string() {
        let q = parse_query("path=a.csv&start=10&count=5");
        assert_eq!(q.get("path").unwrap(), "a.csv");
        assert_eq!(q.get("start").unwrap(), "10");
    }

    #[test]
    fn refuses_paths_outside_root() {
        let root = std::env::temp_dir().join(format!("csvs_srv_root_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("ok.csv"), "a\n1\n").unwrap();
        let root = root.canonicalize().unwrap();

        let mut p = HashMap::new();
        p.insert("path".to_string(), "../../etc/passwd".to_string());
        assert!(resolve(&root, &p).is_none());

        p.insert("path".to_string(), "ok.csv".to_string());
        assert!(resolve(&root, &p).is_some());

        std::fs::remove_dir_all(&root).ok();
    }
}
