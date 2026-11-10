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
    serve_on(listener, root)
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
            "/index" => start_index(&mut stream, root, jobs, &params),
            "/sort" => start_sort(&mut stream, root, jobs, &params),
            "/dedupe" => start_dedupe(&mut stream, root, jobs, &params),
            "/partition" => start_partition(&mut stream, root, jobs, &params),
            "/edit" => edit_apply(&mut stream, root, jobs, &params),
            _ => respond_json(&mut stream, 404, r#"{"error":"unknown endpoint"}"#),
        };
    }
    if method != "GET" {
        return respond_json(&mut stream, 405, r#"{"error":"method not allowed"}"#);
    }

    match path {
        "/" | "/index.html" => respond_html(&mut stream, 200, VIEWER_HTML),
        "/health" => respond_json(&mut stream, 200, r#"{"ok":true,"service":"csv-splitter"}"#),
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
        "/groupby" => group_by(&mut stream, root, cache, &params),
        "/pivot" => pivot_endpoint(&mut stream, root, cache, &params),
        "/style" => style_endpoint(&mut stream, root, &params),
        "/chart" => chart_endpoint(&mut stream, root, &params),
        "/layout" => layout_endpoint(&mut stream, root, &params),
        "/book" => book_endpoint(&mut stream, root, &params),
        "/download" => download(&mut stream, root, &params),
        "/export" => export(&mut stream, root, &params),
        "/query" => query(&mut stream, root, &params),
        "/job" => job_status(&mut stream, jobs, &params),
        "/jobs" => job_list(&mut stream, jobs),
        _ => respond_json(
            &mut stream,
            404,
            r#"{"error":"unknown endpoint","endpoints":["/","/health","/meta","/rows","/columns","/sheet","/book","/pivot","/style","/chart","/layout","/stats","/analyze","/groupby","/filter","/search","/files","/download","/export","/edit","/query","/job","/jobs"],"writes":["POST /split","POST /merge","POST /sort","POST /dedupe","POST /partition","POST /warm","POST /edit"]}"#,
        ),
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
    "set", "insert", "delete", "replace", "copy", "formula", "drop", "rename", "undo", "redo",
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
        let id = jobs.submit("edit-export", move |tx| {
            match workbook::export_filtered(
                &base,
                &ops,
                &out,
                delimiter,
                csv_splitter_core::csv_reader::QuoteStyle::Rfc4180,
                only.as_ref(),
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
    match csv_splitter_core::book::scan(root, max) {
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
                    r#"{{"root":{},"sheets":[{}],"unreadable":[{}]}}"#,
                    json_str(&root.to_string_lossy()),
                    sheets.join(","),
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
    let cols: Vec<String> = window.columns.iter().map(|c| json_str(c)).collect();
    let rows: Vec<String> = window
        .rows
        .iter()
        .map(|row| {
            let cells: Vec<String> = row.iter().map(|c| json_str(c)).collect();
            format!("[{}]", cells.join(","))
        })
        .collect();
    let rows_count = window.rows.len();
    let frozen: Vec<String> = csv_splitter_core::layout::Layout::load(path)
        .map(|l| {
            let count = l.frozen_prefix(&window.columns);
            window
                .columns
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
            r#"{{"start":{start},"count":{},"requested":{requested},"limit":{MAX_ROWS_PER_REQUEST},"total":{},"columns":[{}],"frozenColumns":[{}],"rows":[{}],"edited":true,"operations":{}}}"#,
            rows_count,
            window.total,
            cols.join(","),
            frozen.join(","),
            rows.join(","),
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
    let schema = match sheet.schema(sample) {
        Ok(schema) => schema,
        Err(e) => {
            return respond_json(
                stream,
                422,
                &format!(r#"{{"error":{}}}"#, json_str(&e.to_string())),
            )
        }
    };
    let rows = sheet.rows().unwrap_or(0);
    let body: Vec<String> = schema
        .iter()
        .map(|column| {
            format!(
                r#"{{"name":{},"index":{},"kind":{},"empty":{},"sampled":{}}}"#,
                json_str(&column.name),
                column.index,
                json_str(column.kind.as_str()),
                column.empty,
                column.sampled
            )
        })
        .collect();
    respond_json(
        stream,
        200,
        &format!(
            r#"{{"path":{},"rows":{rows},"count":{},"columns":[{}]}}"#,
            json_str(&params.get("path").cloned().unwrap_or_default()),
            schema.len(),
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
    let ops: Vec<String> = sheet
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
            r#"{{"path":{},"columns":[{}],"frozenColumns":[{}],"rows":{},"edited":{},"operations":{},"capabilities":[{}],"layout":{},"journal":{},"indexed":{},"indexBytes":{},"ops":[{}]}}"#,
            json_str(&params.get("path").cloned().unwrap_or_default()),
            cols.join(","),
            frozen_names.join(","),
            rows,
            !sheet.is_pristine(),
            ops.len(),
            caps.join(","),
            layout_json,
            json_str(&sheet.journal_path().to_string_lossy()),
            indexed,
            index_bytes,
            ops.join(",")
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
            let clean: String = order
                .chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '_' | '-' | ',' | '.' | ':'))
                .collect();
            if clean.trim().is_empty() {
                return respond_json(stream, 400, r#"{"error":"order must name a column"}"#);
            }
            query.push_str(" ORDER BY ");
            query.push_str(&clean);
            if params
                .get("desc")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false)
            {
                query.push_str(" DESC");
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
            // Column names come from the caller; strip anything that could end
            // the clause early rather than passing it through.
            let clean: String = order
                .chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '_' | '-' | ',' | '.' | ':'))
                .collect();
            if clean.trim().is_empty() {
                return respond_json(stream, 400, r#"{"error":"order must name a column"}"#);
            }
            query.push_str(" ORDER BY ");
            query.push_str(&clean);
            if params
                .get("desc")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false)
            {
                query.push_str(" DESC");
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

    // Column names travel with every window: a client that only pages rows
    // would otherwise have to make a second request to render a header.
    let headers = viewer.headers().unwrap_or_default();
    let cols: Vec<String> = headers.iter().map(|c| json_str(c)).collect();
    let frozen: Vec<String> = csv_splitter_core::layout::Layout::load(&path)
        .map(|l| {
            let count = l.frozen_prefix(&headers);
            headers.iter().take(count).map(|c| json_str(c)).collect()
        })
        .unwrap_or_default();
    let body = format!(
        r#"{{"start":{start},"count":{},"requested":{requested},"limit":{MAX_ROWS_PER_REQUEST},"total":{},"columns":[{}],"frozenColumns":[{}],"rows":[{}]}}"#,
        rows.len(),
        viewer.total_rows(),
        cols.join(","),
        frozen.join(","),
        rows.join(",")
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

/// Stream a file under the root as a download.
///
/// Deliberately no range support and no content sniffing beyond the extension:
/// a local single-user tool needs "give me the bytes", not a CDN.
fn download(
    stream: &mut TcpStream,
    root: &Path,
    params: &HashMap<String, String>,
) -> io::Result<()> {
    let (path, rel) = match resolve(root, params) {
        Some(v) => v,
        None => return bad_path(stream),
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
            let cols: Vec<String> = cached.columns.iter().map(|c| json_str(c)).collect();
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
    let reason = reason_for(status);
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n{}",
        body.len(),
        body
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
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Disposition: {disposition}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, OPTIONS\r\n\
         Access-Control-Allow-Headers: *\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn reason_for(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        422 => "Unprocessable Entity",
        _ => "Internal Server Error",
    }
}

fn respond_json(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    respond(stream, status, body)
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> io::Result<()> {
    let reason = reason_for(status);
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
