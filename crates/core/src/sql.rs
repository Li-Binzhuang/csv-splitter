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
const FORBIDDEN: &[&str] = &[
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
    "set ",
    "read_csv",
    "read_parquet",
    "read_json",
    "read_text",
    "glob",
];

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
    for word in FORBIDDEN {
        if upper.contains(word) {
            return Err(format!("'{word}' is not allowed in a query"));
        }
    }
    Ok(())
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
    for opts in OPEN_LADDER {
        let conn = Connection::open_in_memory().map_err(to_io)?;
        conn.execute_batch(&view_sql(path, delimiter, opts))
            .map_err(to_io)?;
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

fn to_io(e: duckdb::Error) -> io::Error {
    io::Error::other(e.to_string())
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
