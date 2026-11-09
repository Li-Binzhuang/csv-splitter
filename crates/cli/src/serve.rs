//! Local HTTP API: the backend half of a front-end/back-end split.
//!
//! A browser cannot page through a 10 GB file, but it can ask for "rows 4,300,000
//! to 4,300,040" — which is what the sparse index makes cheap. This server is
//! deliberately small and dependency-free: GET + query string in, JSON out,
//! bound to loopback, restricted to one directory tree.
//!
//! Endpoints (all read-only):
//!   GET /health
//!   GET /meta?path=REL
//!   GET /rows?path=REL&start=N&count=M
//!   GET /rows?path=REL&sql=SELECT...&start=N&count=M   (window over a query)
//!   GET /stats?path=REL&sample=N
//!   GET /filter?path=REL&where=col=value&limit=N
//!   GET /search?path=REL&q=text&columns=a,b&limit=N
//!   GET /files?dir=REL&ext=csv,tsv&limit=N
//!   GET /query?path=REL&sql=SELECT...&limit=N
//!
//! Write endpoints return a job id instead of blocking:
//!   POST /split?path=REL&splits=N&format=zst&out=REL   -> {"job":"..."}
//!   POST /merge?path=REL&out=REL                        -> {"job":"..."}
//!   POST /warm?path=REL&sql=...                       -> {"job":"..."}
//!   GET  /job?id=...    (poll)      GET /jobs  (list)

use csv_splitter_core::cache::ResultCache;
use csv_splitter_core::filter::{scan as filter_scan, FilterSpec};
use csv_splitter_core::jobs::JobRegistry;
use csv_splitter_core::listing;
use csv_splitter_core::merge::merge_archive;
use csv_splitter_core::preview::compute_column_stats;
use csv_splitter_core::search::{search as search_scan, SearchSpec};
use csv_splitter_core::splitter::run_split_threads;
use csv_splitter_core::sql;
use csv_splitter_core::viewer::WindowViewer;
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
    let listener = TcpListener::bind(&cfg.addr)?;
    let local = listener.local_addr()?;
    if !local.ip().is_loopback() {
        eprintln!("warning: listening on {local}, not loopback — anyone who can reach this port can read files under {}", root.display());
    }
    println!(
        "csv-splitter API on http://{local}  (root: {})",
        root.display()
    );
    serve_on(listener, root)
}

/// Serve until the listener fails. Exposed for tests, which bind port 0.
pub fn serve_on(listener: TcpListener, root: PathBuf) -> io::Result<()> {
    let root = Arc::new(root);
    let jobs = JobRegistry::new();
    let cache = ResultCache::default();
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let root = Arc::clone(&root);
        let jobs = jobs.clone();
        let cache = cache.clone();
        std::thread::spawn(move || {
            let _ = handle(stream, &root, &jobs, &cache);
        });
    }
    Ok(())
}

fn handle(
    mut stream: TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    cache: &ResultCache,
) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    // Headers are read and ignored: this API has no bodies and no auth.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let (path, query_string) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };

    if method == "OPTIONS" {
        return respond(&mut stream, 204, "");
    }
    let params = parse_query(query_string);

    // Writes are POST; everything else is a read.
    if method == "POST" {
        return match path {
            "/split" => start_split(&mut stream, root, jobs, &params),
            "/merge" => start_merge(&mut stream, root, jobs, &params),
            "/warm" => start_warm(&mut stream, root, jobs, cache, &params),
            _ => respond_json(&mut stream, 404, r#"{"error":"unknown endpoint"}"#),
        };
    }
    if method != "GET" {
        return respond_json(&mut stream, 405, r#"{"error":"method not allowed"}"#);
    }

    match path {
        "/health" => respond_json(&mut stream, 200, r#"{"ok":true,"service":"csv-splitter"}"#),
        "/meta" => meta(&mut stream, root, &params),
        "/rows" => rows(&mut stream, root, &params, cache),
        "/stats" => stats(&mut stream, root, &params),
        "/filter" => filter(&mut stream, root, &params),
        "/search" => search(&mut stream, root, &params),
        "/files" => files(&mut stream, root, &params),
        "/query" => query(&mut stream, root, &params),
        "/job" => job_status(&mut stream, jobs, &params),
        "/jobs" => job_list(&mut stream, jobs),
        _ => respond_json(
            &mut stream,
            404,
            r#"{"error":"unknown endpoint","endpoints":["/health","/meta","/rows","/stats","/filter","/search","/files","/query","/job","/jobs"],"writes":["POST /split","POST /merge"]}"#,
        ),
    }
}

fn job_status(
    stream: &mut TcpStream,
    jobs: &JobRegistry,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let Some(id) = params.get("id") else {
        return respond_json(stream, 400, r#"{"error":"id is required"}"#);
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
        r#"{{"id":{},"kind":{},"state":{},"phase":{},"progress":{},"total":{},"fraction":{},"elapsed":{},"output":{},"error":{},"messages":[{}]}}"#,
        json_str(&status.id),
        json_str(&status.kind),
        json_str(status.state.as_str()),
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
        None => return respond_json(stream, 400, r#"{"error":"path missing or outside root"}"#),
    };
    let Some(output) = resolve_out(root, params, "out") else {
        return respond_json(stream, 400, r#"{"error":"out must stay inside root"}"#);
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
        None => return respond_json(stream, 400, r#"{"error":"path missing or outside root"}"#),
    };
    let Some(output) = resolve_out(root, params, "out") else {
        return respond_json(stream, 400, r#"{"error":"out must stay inside root"}"#);
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

/// Pre-load a query into the result cache so the first scroll is already fast.
///
/// On files with quoted newlines the CSV reader falls back to a serial scan
/// (a 10 GB query then costs seconds), so a client should start warming as soon
/// as it knows the filter, not when the user scrolls.
fn start_warm(
    stream: &mut TcpStream,
    root: &Path,
    jobs: &JobRegistry,
    cache: &ResultCache,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, _rel) = match resolve(root, params) {
        Some(v) => v,
        None => return respond_json(stream, 400, r#"{"error":"path missing or outside root"}"#),
    };
    let Some(statement) = params.get("sql") else {
        return respond_json(stream, 400, r#"{"error":"sql is required"}"#);
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
        None => return respond_json(stream, 400, r#"{"error":"path missing or outside root"}"#),
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

fn rows(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
    cache: &ResultCache,
) -> io::Result<()> {
    let (path, _rel) = match resolve(root, params) {
        Some(v) => v,
        None => return respond_json(stream, 400, r#"{"error":"path missing or outside root"}"#),
    };
    let start: u64 = params
        .get("start")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let count: u64 = params
        .get("count")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100)
        .min(MAX_ROWS_PER_REQUEST);

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
        let loaded = cache.get_or_load(&path, statement, delimiter(params), || {
            sql::collect(&path, statement, delimiter(params), cache.row_cap())
        });
        return match loaded {
            Ok(cached) => {
                let (rows, has_more) = match cached.try_window(start as usize, count as usize) {
                    Some(w) => w,
                    // The request starts past what was materialised: answer
                    // honestly rather than pretending the result ended.
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
                let cols: Vec<String> = cached.columns.iter().map(|c| json_str(c)).collect();
                let body_rows: Vec<String> = rows
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
                        r#"{{"start":{start},"hasMore":{},"rowsSoFar":{},"columns":[{}],"rows":[{}]}}"#,
                        has_more,
                        cached.total(),
                        cols.join(","),
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
        };
    }

    if let Err(e) = viewer.ensure_range(start, count) {
        return respond_json(
            stream,
            500,
            &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
        );
    }

    let offset = start.saturating_sub(viewer.window_start()) as usize;
    let rows: Vec<String> = viewer
        .rows()
        .iter()
        .skip(offset)
        .take(count as usize)
        .map(|row| {
            let cells: Vec<String> = row.iter().map(|c| json_str(c)).collect();
            format!("[{}]", cells.join(","))
        })
        .collect();

    let body = format!(
        r#"{{"start":{start},"total":{},"rows":[{}]}}"#,
        viewer.total_rows(),
        rows.join(",")
    );
    respond_json(stream, 200, &body)
}

fn query(stream: &mut TcpStream, root: &Path, params: &HashMap<String, String>) -> io::Result<()> {
    let (path, _rel) = match resolve(root, params) {
        Some(v) => v,
        None => return respond_json(stream, 400, r#"{"error":"path missing or outside root"}"#),
    };
    let Some(statement) = params.get("sql") else {
        return respond_json(stream, 400, r#"{"error":"sql is required"}"#);
    };
    let limit: usize = params
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(sql::DEFAULT_LIMIT)
        .min(10_000);
    match sql::query_with(&path, statement, limit, delimiter(params)) {
        Ok(result) => {
            let cols: Vec<String> = result.columns.iter().map(|c| json_str(c)).collect();
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
                    r#"{{"columns":[{}],"rows":[{}],"truncated":{}}}"#,
                    cols.join(","),
                    rows.join(","),
                    result.truncated
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
    let (path, _rel) = match resolve(root, params) {
        Some(v) => v,
        None => return respond_json(stream, 400, r#"{"error":"path missing or outside root"}"#),
    };
    let Some(query) = params.get("q").filter(|q| !q.is_empty()) else {
        return respond_json(stream, 400, r#"{"error":"q is required"}"#);
    };
    let mut spec = SearchSpec::new(query.clone());
    if let Some(columns) = params.get("columns").filter(|c| !c.is_empty()) {
        spec.columns = columns
            .split(',')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect();
    }
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
                    r#"{{"root":{},"truncated":{},"files":[{}]}}"#,
                    json_str(&list.root.to_string_lossy()),
                    list.truncated,
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
    let (path, _rel) = match resolve(root, params) {
        Some(v) => v,
        None => return respond_json(stream, 400, r#"{"error":"path missing or outside root"}"#),
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
    let (path, _rel) = match resolve(root, params) {
        Some(v) => v,
        None => return respond_json(stream, 400, r#"{"error":"path missing or outside root"}"#),
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

fn json_str(s: &str) -> String {
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

fn respond_json(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    respond(stream, status, body)
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        422 => "Unprocessable Entity",
        _ => "Internal Server Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, OPTIONS\r\n\
         Access-Control-Allow-Headers: *\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
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
