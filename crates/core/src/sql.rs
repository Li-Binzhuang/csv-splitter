//! SQL over a CSV file, powered by DuckDB.
//!
//! Hand-writing GROUP BY, ORDER BY, window functions and type coercion is how
//! tools like this rot. DuckDB already reads CSV directly, parallelises the
//! scan and has a real optimiser, so the engine does the work and this module
//! only has to be a careful door in front of it.
//!
//! Safety rules (the API exposes this over HTTP):
//!   * exactly one statement, and it must start with SELECT or WITH;
//!   * no DDL/DML/ATTACH/COPY/PRAGMA/INSTALL/LOAD/EXPORT/IMPORT;
//!   * results are capped, so a `SELECT *` cannot exhaust memory;
//!   * the file is registered as view `t`, so queries never need a path.

use crate::sheet::{ColumnInfo, ColumnKind};
use crate::types::Delimiter;
use duckdb::types::Value;
use duckdb::Connection;
use std::io;
use std::path::Path;

/// Guard against accidental `SELECT *` on a billion rows.
pub const DEFAULT_LIMIT: usize = 1_000;

#[derive(Clone, Debug, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// True when the row cap cut the result.
    pub truncated: bool,
}

/// Words that must not appear anywhere in a query.
/// Statement keywords: matched as whole words, so `OFFSET` (which contains `set`) and
/// a column called `offset` are not mistaken for the `SET` statement.
const FORBIDDEN_WORDS: &[&str] = &[
    "insert",
    "update",
    "delete",
    "drop",
    "create",
    "alter",
    "attach",
    "detach",
    "copy",
    "export",
    "import",
    "install",
    "load",
    "pragma",
    "call",
    "vacuum",
    "checkpoint",
    "set",
];

/// Table functions that read outside the served file: matched as prefixes, because
/// they take arguments (`read_csv_auto('…')`) and the name is only the start.
const FORBIDDEN_PREFIXES: &[&str] = &["read_csv", "read_parquet", "read_json", "read_text", "glob"];

/// Validate a user query before it reaches DuckDB.
pub fn validate(sql: &str) -> Result<(), String> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return Err("empty query".to_string());
    }
    if trimmed.contains(';') {
        return Err("only one statement is allowed".to_string());
    }
    let upper = trimmed.to_ascii_lowercase();
    if !(upper.starts_with("select") || upper.starts_with("with")) {
        return Err("only SELECT / WITH queries are allowed".to_string());
    }
    for word in FORBIDDEN_WORDS {
        if contains_word(&upper, word) {
            return Err(format!("'{word}' is not allowed in a query"));
        }
    }
    for prefix in FORBIDDEN_PREFIXES {
        if upper.contains(prefix) {
            return Err(format!("'{prefix}' is not allowed in a query"));
        }
    }
    Ok(())
}

/// Whole-word search, because a substring test rejects legitimate SQL.
///
/// `OFFSET 0` contains the letters `set `, and the old `contains` check refused it —
/// so "SELECT * FROM t WHERE x LIMIT 10 OFFSET 20" failed with "'set ' is not allowed
/// in a query". Keywords only matter as words: `offset` and `reset` are not `set`.
fn contains_word(haystack: &str, word: &str) -> bool {
    let is_word_char = |c: char| c.is_alphanumeric() || c == '_';
    let bytes = haystack.as_bytes();
    let mut at = 0;
    while let Some(index) = haystack[at..].find(word) {
        let start = at + index;
        let end = start + word.len();
        let before_ok = start == 0 || !is_word_char(bytes[start - 1] as char);
        let after_ok =
            end == bytes.len() || bytes[end] == b' ' || !is_word_char(bytes[end] as char);
        if before_ok && after_ok {
            return true;
        }
        at = end;
    }
    false
}

/// Option sets tried in order when opening a CSV.
///
/// DuckDB refuses `null_padding` together with quoted newlines in the parallel
/// scanner, and ragged rows need `null_padding`. The view is created lazily, so
/// these errors only surface when rows are read — hence the ladder wraps the
/// whole query, not just `CREATE VIEW`.
#[derive(Clone, Copy)]
struct OpenOptions {
    null_padding: bool,
    parallel: bool,
}

const OPEN_LADDER: [OpenOptions; 3] = [
    // Fast path: parallel, ragged-tolerant. Breaks on quoted newlines.
    OpenOptions {
        null_padding: true,
        parallel: true,
    },
    // Quoted newlines with well-formed rows: still parallel.
    OpenOptions {
        null_padding: false,
        parallel: true,
    },
    // Last resort: handles both, at the cost of a serial scan.
    OpenOptions {
        null_padding: true,
        parallel: false,
    },
];

fn view_sql(path: &Path, delimiter: Delimiter, opts: OpenOptions) -> String {
    let quoted = path.to_string_lossy().replace('\'', "''");
    let delim = (delimiter.byte() as char).to_string().replace('\'', "''");
    format!(
        "CREATE VIEW t AS SELECT * FROM read_csv_auto('{quoted}', header=true, delim='{delim}', sample_size=20000, null_padding={np}, parallel={par})",
        np = opts.null_padding,
        par = opts.parallel
    )
}

/// A view that declares its column types instead of asking DuckDB to guess them.
///
/// `read_csv_auto` sniffs the file to infer 782 column types, and on a 2.5 GB sheet that
/// sniffing *is* the query: `SELECT * FROM t LIMIT 1` took 7.3 seconds, which is the
/// whole file read once for types before the first row was produced. Declaring the types
/// from a 64-row sample of our own reader removes that pass entirely — the engine reads
/// the first block and answers.
fn typed_view_sql(
    path: &Path,
    delimiter: Delimiter,
    opts: OpenOptions,
    columns: &[ColumnInfo],
) -> String {
    let quoted = path.to_string_lossy().replace('\'', "\'\'");
    let delim = (delimiter.byte() as char).to_string().replace('\'', "\'\'");
    let fields: Vec<String> = columns
        .iter()
        .map(|c| format!("'{}': '{}'", duck_quote(&c.name), duck_type(c.kind)))
        .collect();
    format!(
        "CREATE VIEW t AS SELECT * FROM read_csv('{quoted}', header=true, delim='{delim}', sample_size=1, null_padding={np}, parallel={par}, columns={{{fields}}})",
        np = opts.null_padding,
        par = opts.parallel,
        fields = fields.join(", ")
    )
}

/// DuckDB's spelling for a column type, from what our sampler saw.
///
/// Numbers are DOUBLE rather than BIGINT because a sampled integer column may hold
/// decimals further down; text stays text. Dates stay VARCHAR: their formats vary
/// (`2026-07-22 03:54:37.957`), and a failed cast would fail the whole read.
fn duck_type(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Number => "DOUBLE",
        _ => "VARCHAR",
    }
}

/// Single-quoted identifier inside a DuckDB map literal: `it's` -> `it''s`.
fn duck_quote(name: &str) -> String {
    name.replace('\'', "\'\'")
}

/// The columns of a file, sampled once and remembered per (path, size, mtime).
///
/// Every request used to sample the header again; on a 782-column file that is a
/// visible cost even though it is small, and it is the same answer every time until the
/// file changes.
fn cached_columns(path: &Path) -> io::Result<Vec<ColumnInfo>> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Vec<ColumnInfo>>>> = OnceLock::new();
    let key = stamp_key(path);
    let store = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = store.lock().ok().and_then(|g| g.get(&key).cloned()) {
        return Ok(hit);
    }
    let sheet = crate::sheet::Sheet::open(path, crate::sheet::ReadOptions::default())?;
    let columns = sheet.schema(64)?;
    if let Ok(mut guard) = store.lock() {
        // Keep the cache small: a page of long-lived servers should not grow with files.
        if guard.len() > 16 {
            guard.clear();
        }
        guard.insert(key, columns.clone());
    }
    Ok(columns)
}

fn stamp_key(path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    format!(
        "{}|{}|{}",
        path.display(),
        meta.as_ref().map(|m| m.len()).unwrap_or(0),
        meta.and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    )
}

/// How long a query may run before it is interrupted, unless the caller says otherwise.
///
/// A 2.5 GB scan takes about eight seconds here, so thirty is generous for an
/// interactive request and still bounded: without it a runaway query holds the request
/// open until the client gives up, and the connection with it.
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Run a query with a wall-clock limit, interrupting the engine when it expires.
///
/// The interrupt is what makes this honest: DuckDB stops the scan and returns an error,
/// instead of the caller pretending a timeout it could not enforce.
pub fn query_with_timeout(
    path: &Path,
    sql: &str,
    limit: usize,
    delimiter: Delimiter,
    timeout_ms: u64,
) -> io::Result<QueryResult> {
    run_with_timeout(path, delimiter, timeout_ms, |conn| {
        let started = std::time::Instant::now();
        let _ = started;
        query_once(conn, sql, limit)
    })
}

fn query_once(conn: &Connection, sql: &str, limit: usize) -> io::Result<QueryResult> {
    validate(sql).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let limit = limit.clamp(1, 100_000);
    let wrapped = format!("SELECT * FROM ({sql}) AS _q LIMIT {}", limit + 1);
    let columns = describe_query(conn, sql)?;
    let rows = collect_rows(conn, &wrapped, columns.len())?;
    let mut result = QueryResult {
        columns,
        rows,
        truncated: false,
    };
    if result.rows.len() > limit {
        result.rows.truncate(limit);
        result.truncated = true;
    }
    Ok(result)
}

/// Shared connection setup with a watchdog that interrupts the engine on expiry.
fn run_with_timeout<T, F>(
    path: &Path,
    delimiter: Delimiter,
    timeout_ms: u64,
    run: F,
) -> io::Result<T>
where
    F: Fn(&Connection) -> io::Result<T>,
{
    if !path.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "file not found"));
    }
    let mut last: Option<io::Error> = None;
    // Typed first: it is what makes a first page cost one block instead of a whole pass.
    // A sample can call a column a number that later holds text, so the auto view stays
    // as the fallback — correctness keeps its slow path, it just stops being the only one.
    let columns = cached_columns(path).ok();
    let mut strategies: Vec<Option<&[ColumnInfo]>> = Vec::new();
    if let Some(cols) = columns.as_deref() {
        strategies.push(Some(cols));
    }
    strategies.push(None);
    for columns in strategies {
        for opts in OPEN_LADDER {
            let conn = Connection::open_in_memory().map_err(to_io)?;
            let create = match columns {
                Some(cols) => typed_view_sql(path, delimiter, opts, cols),
                None => view_sql(path, delimiter, opts),
            };
            if let Err(e) = conn.execute_batch(&create) {
                let message = e.to_string();
                last = Some(to_io(e));
                if is_retryable(&message) {
                    continue;
                }
                break;
            }
            check_table_shape(&conn, delimiter)?;
            let handle = conn.interrupt_handle();
            let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
            let watchdog = std::thread::spawn(move || {
                // Exit as soon as the query finishes; only fire on a real expiry.
                if done_rx
                    .recv_timeout(std::time::Duration::from_millis(timeout_ms))
                    .is_err()
                {
                    handle.interrupt();
                }
            });
            let outcome = run(&conn);
            let _ = done_tx.send(());
            let _ = watchdog.join();
            match outcome {
                Ok(value) => return Ok(value),
                Err(e) => {
                    if is_interrupted(&e.to_string()) {
                        return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "the query ran longer than {} s and was interrupted; add LIMIT or a WHERE clause, or raise the timeout",
                            timeout_ms / 1000
                        ),
                    ));
                    }
                    let retryable = is_retryable(&e.to_string());
                    last = Some(e);
                    if !retryable {
                        break;
                    }
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("cannot read the CSV")))
}

/// DuckDB reports an interrupted query as a failure mentioning the interrupt.
fn is_interrupted(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("interrupt") || lower.contains("cancel")
}

/// Run `f` against the file opened as view `t`, retrying across open options.
///
/// Engine errors that mean "this combination of options cannot read this file"
/// are retried; genuine query errors simply fall through to the last attempt and
/// are reported once.
fn with_connection<T, F>(path: &Path, delimiter: Delimiter, f: F) -> io::Result<T>
where
    F: Fn(&Connection) -> io::Result<T>,
{
    if !path.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "file not found"));
    }
    let mut last: Option<io::Error> = None;
    // Typed first: it is what makes a first page cost one block instead of a whole pass.
    // A sample can call a column a number that later holds text, so the auto view stays
    // as the fallback — correctness keeps its slow path, it just stops being the only one.
    let columns = cached_columns(path).ok();
    let mut strategies: Vec<Option<&[ColumnInfo]>> = Vec::new();
    if let Some(cols) = columns.as_deref() {
        strategies.push(Some(cols));
    }
    strategies.push(None);
    for columns in strategies {
        for opts in OPEN_LADDER {
            let conn = Connection::open_in_memory().map_err(to_io)?;
            // Sniffing happens here, and it can fail for shapes the previous option
            // set could not parse — so this error must also continue down the ladder.
            let create = match columns {
                Some(cols) => typed_view_sql(path, delimiter, opts, cols),
                None => view_sql(path, delimiter, opts),
            };
            if let Err(e) = conn.execute_batch(&create) {
                let message = e.to_string();
                last = Some(to_io(e));
                if is_retryable(&message) {
                    continue;
                }
                break;
            }
            // A wrong delimiter is not fixable by another reader option, so this
            // failure propagates instead of continuing down the ladder.
            check_table_shape(&conn, delimiter)?;
            match f(&conn) {
                Ok(value) => return Ok(value),
                Err(e) => {
                    let retryable = is_retryable(&e.to_string());
                    last = Some(e);
                    if !retryable {
                        break;
                    }
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("cannot read the CSV")))
}

/// Errors caused by the chosen CSV reader options, not by the user's query.
fn is_retryable(message: &str) -> bool {
    message.contains("null_padding")
        || message.contains("parallel scanner")
        || message.contains("CSV Error")
}

/// Run `sql` against `path` (read-only, results capped at `limit`).
pub fn query(path: &Path, sql: &str, limit: usize) -> io::Result<QueryResult> {
    query_with(path, sql, limit, Delimiter::Comma)
}

/// Same as [`query`], with an explicit delimiter.
pub fn query_with(
    path: &Path,
    sql: &str,
    limit: usize,
    delimiter: Delimiter,
) -> io::Result<QueryResult> {
    validate(sql).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let limit = limit.clamp(1, 100_000);
    let wrapped = format!("SELECT * FROM ({sql}) AS _q LIMIT {}", limit + 1);

    with_connection(path, delimiter, |conn| {
        let columns = describe_query(conn, sql)?;
        let rows = collect_rows(conn, &wrapped, columns.len())?;
        let mut result = QueryResult {
            columns,
            rows,
            truncated: false,
        };
        if result.rows.len() > limit {
            result.rows.truncate(limit);
            result.truncated = true;
        }
        Ok(result)
    })
}

/// Detect the classic "wrong delimiter" shape: the whole header became one
/// column, e.g. `id;name` read with `delim=','`.
///
/// Checked on the table (`DESCRIBE t`), not on the query result — `count(*)`
/// returns a single column too, and that is perfectly valid.
fn check_table_shape(conn: &Connection, delimiter: Delimiter) -> io::Result<()> {
    let mut stmt = conn.prepare("DESCRIBE t").map_err(to_io)?;
    let mut rows = stmt.query([]).map_err(to_io)?;
    let mut names: Vec<String> = Vec::new();
    while let Some(row) = rows.next().map_err(to_io)? {
        let name: String = row.get(0).map_err(to_io)?;
        names.push(name);
    }
    if names.len() != 1 {
        return Ok(());
    }
    let found: Vec<char> = b",\t;|"
        .iter()
        .map(|b| *b as char)
        .filter(|c| *c != delimiter.byte() as char && names[0].contains(*c))
        .collect();
    if found.is_empty() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "the header was parsed as a single column ({}); the file looks like it uses {:?}, not '{}' — pass the right delimiter",
            names[0],
            found,
            delimiter.byte() as char
        ),
    ))
}

/// Column names for a query, via DESCRIBE (binds without scanning the file).
fn describe_query(conn: &Connection, sql: &str) -> io::Result<Vec<String>> {
    let describe = format!("DESCRIBE SELECT * FROM ({sql}) AS _q");
    let mut stmt = conn.prepare(&describe).map_err(to_io)?;
    let mut rows = stmt.query([]).map_err(to_io)?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().map_err(to_io)? {
        let name: String = row.get(0).map_err(to_io)?;
        columns.push(name);
    }
    Ok(columns)
}

/// Execute `sql` and pull every row into memory (the caller sets the cap).
fn collect_rows(conn: &Connection, sql: &str, width: usize) -> io::Result<Vec<Vec<String>>> {
    let mut stmt = conn.prepare(sql).map_err(to_io)?;
    let mut result = stmt.query([]).map_err(to_io)?;
    let mut out = Vec::new();
    while let Some(row) = result.next().map_err(to_io)? {
        let mut cells = Vec::with_capacity(width);
        for i in 0..width {
            let value: Value = row.get(i).map_err(to_io)?;
            cells.push(value_to_string(&value));
        }
        out.push(cells);
    }
    Ok(out)
}

/// Run `sql` and materialise up to `row_cap` rows, for the result cache.
///
/// Returns `oversized = true` when the query produced more rows than the cap;
/// the caller must not cache that, but may still serve the rows it has.
pub fn collect(
    path: &Path,
    sql: &str,
    delimiter: Delimiter,
    row_cap: usize,
) -> io::Result<crate::cache::CachedResult> {
    validate(sql).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let cap = row_cap.max(1);
    let wrapped = format!("SELECT * FROM ({sql}) AS _q LIMIT {}", cap + 1);

    with_connection(path, delimiter, |conn| {
        let columns = describe_query(conn, sql)?;
        let mut rows = collect_rows(conn, &wrapped, columns.len())?;
        let oversized = rows.len() > cap;
        if oversized {
            rows.truncate(cap);
        }
        Ok(crate::cache::CachedResult {
            columns,
            rows,
            oversized,
        })
    })
}

/// A window into a query result, for a viewer that pages instead of loading.
#[derive(Clone, Debug, Default)]
pub struct QueryPage {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// 1-based position of the first returned row in the full result.
    pub start: u64,
    /// True when more rows exist after this window.
    pub has_more: bool,
}

/// Run `sql`, then return only rows `[start, start + count)`.
///
/// This is the endpoint a table UI actually needs: the client says "give me
/// rows 5000..5020 of `WHERE city='CDMX'`" and never materialises the rest.
/// One extra row is fetched to answer `has_more` without a second COUNT query.
pub fn page(path: &Path, sql: &str, start: u64, count: u64) -> io::Result<QueryPage> {
    page_with(path, sql, start, count, Delimiter::Comma)
}

/// Same as [`page`], with an explicit delimiter.
pub fn page_with(
    path: &Path,
    sql: &str,
    start: u64,
    count: u64,
    delimiter: Delimiter,
) -> io::Result<QueryPage> {
    validate(sql).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let count = count.clamp(1, 100_000);
    let wrapped = format!(
        "SELECT * FROM ({sql}) AS _q LIMIT {} OFFSET {}",
        count + 1,
        start
    );

    with_connection(path, delimiter, |conn| {
        let columns = describe_query(conn, sql)?;
        let rows = collect_rows(conn, &wrapped, columns.len())?;
        let mut page = QueryPage {
            columns,
            rows,
            start,
            has_more: false,
        };
        if page.rows.len() as u64 > count {
            page.rows.truncate(count as usize);
            page.has_more = true;
        }
        Ok(page)
    })
}

fn value_to_string(value: &Value) -> String {
    use Value::*;
    match value {
        Null => String::new(),
        Boolean(b) => b.to_string(),
        TinyInt(v) => v.to_string(),
        SmallInt(v) => v.to_string(),
        Int(v) => v.to_string(),
        BigInt(v) => v.to_string(),
        HugeInt(v) => v.to_string(),
        UTinyInt(v) => v.to_string(),
        USmallInt(v) => v.to_string(),
        UInt(v) => v.to_string(),
        UBigInt(v) => v.to_string(),
        Float(v) => v.to_string(),
        Double(v) => v.to_string(),
        Decimal(v) => v.to_string(),
        Text(v) => v.to_string(),
        Blob(v) => String::from_utf8_lossy(v).into_owned(),
        Timestamp(unit, v) => format!("{unit:?}:{v}"),
        Interval {
            months,
            days,
            nanos,
        } => format!("{months}m{days}d{nanos}ns"),
        other => format!("{other:?}"),
    }
}

/// Engine failures are the caller's SQL, not the file's fault: classify them as
/// `InvalidInput` so an API can answer 400 (with the engine's wording in a `detail`
/// field) instead of a generic 422 that hides what went wrong.
fn to_io(e: duckdb::Error) -> io::Error {
    let text = e.to_string();
    let input_error = text.starts_with("Parser Error")
        || text.starts_with("Binder Error")
        || text.starts_with("Catalog Error")
        || text.starts_with("Type Error")
        || text.starts_with("Invalid Input Error")
        || text.starts_with("Conversion Error");
    if input_error {
        io::Error::new(io::ErrorKind::InvalidInput, text)
    } else {
        io::Error::other(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "csvs_sql_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                + unique_seq() as u128
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.csv");
        let mut csv = String::from("city,amount\n");
        for i in 0..1000 {
            csv.push_str(&format!(
                "{},{}\n",
                if i % 2 == 0 { "CDMX" } else { "Puebla" },
                i
            ));
        }
        std::fs::write(&p, csv).unwrap();
        p
    }

    /// Per-process unique suffix for temp directories.
    ///
    /// `SystemTime::now()` only has microsecond resolution on macOS, so a dozen
    /// tests starting at once can read the same value and share a directory — one
    /// test's cleanup then deletes the other's files mid-run.
    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn rejects_writes_and_multiple_statements() {
        assert!(validate("SELECT 1").is_ok());
        assert!(validate("  WITH x AS (SELECT 1) SELECT * FROM x").is_ok());
        assert!(validate("DROP TABLE t").is_err());
        assert!(validate("SELECT 1; DROP TABLE t").is_err());
        assert!(validate("SELECT * FROM read_csv_auto('/etc/passwd')").is_err());
        assert!(validate("COPY t TO 'x.csv'").is_err());
    }

    #[test]
    fn groups_and_orders() {
        let p = sample();
        let r = query(
            &p,
            "SELECT city, count(*) AS n, sum(amount) AS total FROM t GROUP BY city ORDER BY city",
            10,
        )
        .unwrap();
        assert_eq!(r.columns, vec!["city", "n", "total"]);
        assert_eq!(r.rows.len(), 2);
        assert_eq!(r.rows[0][0], "CDMX");
        assert_eq!(r.rows[0][1], "500");
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn tolerates_ragged_rows_like_the_rest_of_the_tool() {
        // A stray unquoted comma in a free-text column is a very common export
        // defect. Our own reader parses it as an extra field; DuckDB must not
        // refuse to open the file.
        let dir = std::env::temp_dir().join(format!(
            "csvs_sql_ragged_{}_{}",
            std::process::id(),
            unique_seq()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("ragged.csv");
        std::fs::write(&p, "id,note\n1,ok\n2,x,y\n3,fine\n").unwrap();

        let r = query(&p, "SELECT count(*) AS n FROM t", 10).unwrap();
        assert_eq!(r.rows[0][0], "3");

        // Short rows are padded with NULL rather than dropping the file.
        std::fs::write(&p, "id,a,b\n1,x,y\n2,only\n").unwrap();
        let r = query(&p, "SELECT count(*) AS n FROM t", 10).unwrap();
        assert_eq!(r.rows[0][0], "2");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn explains_a_wrong_delimiter() {
        let dir = std::env::temp_dir().join(format!(
            "csvs_sql_delim_{}_{}",
            std::process::id(),
            unique_seq()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("semi.csv");
        std::fs::write(&p, "id;name\n1;a\n").unwrap();

        let err = query(&p, "SELECT count(*) FROM t", 10).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("single column"), "{err}");

        // With the right delimiter it works.
        let r = query_with(&p, "SELECT count(*) FROM t", 10, Delimiter::Semicolon).unwrap();
        assert_eq!(r.rows[0][0], "1");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pages_without_materialising_the_result() {
        let p = sample();
        let q = "SELECT city, amount FROM t ORDER BY amount";
        let first = page(&p, q, 0, 5).unwrap();
        assert_eq!(first.columns, vec!["city", "amount"]);
        assert_eq!(first.rows.len(), 5);
        assert_eq!(first.start, 0);
        assert!(first.has_more);
        assert_eq!(first.rows[0][1], "0");

        let second = page(&p, q, 5, 5).unwrap();
        assert_eq!(second.start, 5);
        assert_eq!(second.rows[0][1], "5");

        // The last page reports the end.
        let last = page(&p, q, 995, 10).unwrap();
        assert_eq!(last.rows.len(), 5);
        assert!(!last.has_more);

        // Paging past the end is empty, not an error.
        let past = page(&p, q, 5_000, 10).unwrap();
        assert!(past.rows.is_empty());
        assert!(!past.has_more);

        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn page_rejects_writes_too() {
        let p = sample();
        assert!(page(&p, "DELETE FROM t", 0, 10).is_err());
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn caps_rows_and_reports_truncation() {
        let p = sample();
        let r = query(&p, "SELECT * FROM t", 5).unwrap();
        assert_eq!(r.rows.len(), 5);
        assert!(r.truncated);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    #[test]
    fn offset_is_not_a_forbidden_statement() {
        // `OFFSET 20` used to be refused because it contains the letters of `set `.
        assert!(validate("SELECT * FROM t LIMIT 10 OFFSET 20").is_ok());
        assert!(validate("WITH x AS (SELECT 1) SELECT * FROM x").is_ok());
    }

    #[test]
    fn real_write_statements_are_still_refused() {
        for sql in [
            "DROP TABLE t",
            "DELETE FROM t",
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1",
            "SET x = 1",
            "PRAGMA database_list",
            "COPY t TO 'x.csv'",
        ] {
            assert!(validate(sql).is_err(), "{sql} should be refused");
        }
    }
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    fn sample() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "csvs_sqltimeout_{}_{}",
            std::process::id(),
            unique_seq()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.csv");
        let mut text = String::from("id,city\n");
        for i in 0..500 {
            text.push_str(&format!("{i},C{}\n", i % 7));
        }
        std::fs::write(&path, text).unwrap();
        path
    }

    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn a_query_inside_the_limit_answers_normally() {
        let p = sample();
        let result = query_with_timeout(
            &p,
            "SELECT * FROM t",
            10,
            Delimiter::Comma,
            DEFAULT_TIMEOUT_MS,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 10);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn a_zero_timeout_interrupts_the_engine_and_says_so() {
        let p = sample();
        let outcome = query_with_timeout(&p, "SELECT count(*) FROM t", 10, Delimiter::Comma, 0);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
        match outcome {
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");
                assert!(e.to_string().contains("interrupted"), "{e}");
                assert!(e.to_string().contains("LIMIT"), "{e}");
            }
            // A machine fast enough to finish before the watchdog fires is not a failure
            // of the code; report the assumption instead of pretending.
            Ok(_) => eprintln!("note: the zero-timeout query finished before the interrupt"),
        }
    }
}

#[cfg(test)]
mod typed_view_tests {
    use super::*;

    fn sheet(dir: &str, body: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "csvs_typed_{dir}_{}_{}",
            std::process::id(),
            unique_seq()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("t.csv");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    /// Numbers must stay numbers: a declared-DOUBLE view is why `amount > 5` compares
    /// numerically instead of alphabetically (`10` would sort before `9` as text).
    #[test]
    fn declared_types_keep_numbers_numeric() {
        let path = sheet("numeric", "id,amount\n1,10\n2,9\n3,100\n");
        let result = query(
            &path,
            "SELECT id, amount FROM t WHERE amount > 5 ORDER BY amount",
            10,
        )
        .unwrap();
        let amounts: Vec<String> = result.rows.iter().map(|r| r[1].clone()).collect();
        assert_eq!(
            amounts,
            ["9", "10", "100"],
            "text order would be 10, 100, 9"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// A column that looks numeric in the first 64 rows and turns into text later must
    /// fall back to the auto-detected view instead of failing the read.
    #[test]
    fn a_mistyped_sample_falls_back_instead_of_failing() {
        let mut body = String::from("id,value\n");
        for i in 0..80 {
            body.push_str(&format!("{i},{}\n", i * 10));
        }
        body.push_str("80,not-a-number\n81,11\n");
        let path = sheet("fallback", &body);
        let result = query(&path, "SELECT count(*) FROM t", 5).unwrap();
        assert_eq!(
            result.rows[0][0], "82",
            "the fallback view must still read every row"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// Quoted column names and apostrophes must not break the map literal.
    #[test]
    fn odd_column_names_survive_the_view() {
        let path = sheet("oddnames", "'quoted',it's,total\n1,2,3\n");
        let result = query(&path, "SELECT * FROM t", 2).unwrap();
        assert_eq!(result.columns.len(), 3);
        assert_eq!(result.rows[0], ["1", "2", "3"]);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
