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

/// Open the file as view `t` in a fresh in-memory connection.
fn connect(path: &Path) -> io::Result<Connection> {
    if !path.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "file not found"));
    }
    let conn = Connection::open_in_memory().map_err(to_io)?;
    // DuckDB reads the CSV lazily through the view; the path is escaped as a
    // string literal so a quote in the file name cannot break out.
    let view = format!(
        "CREATE VIEW t AS SELECT * FROM read_csv_auto('{}', header=true, sample_size=20000)",
        path.to_string_lossy().replace('\'', "''")
    );
    conn.execute_batch(&view).map_err(to_io)?;
    Ok(conn)
}

/// Run `sql` against `path` (read-only, results capped at `limit`).
pub fn query(path: &Path, sql: &str, limit: usize) -> io::Result<QueryResult> {
    validate(sql).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let conn = connect(path)?;

    let limit = limit.clamp(1, 100_000);
    let wrapped = format!("SELECT * FROM ({sql}) AS _q LIMIT {}", limit + 1);

    // Column names are only available after execution in duckdb-rs, so ask the
    // planner via DESCRIBE (binds the query without scanning the file).
    let describe = format!("DESCRIBE SELECT * FROM ({sql}) AS _q");
    let mut describe_stmt = conn.prepare(&describe).map_err(to_io)?;
    let mut describe_rows = describe_stmt.query([]).map_err(to_io)?;
    let mut columns: Vec<String> = Vec::new();
    while let Some(row) = describe_rows.next().map_err(to_io)? {
        let name: String = row.get(0).map_err(to_io)?;
        columns.push(name);
    }
    drop(describe_rows);
    drop(describe_stmt);

    let mut stmt = conn.prepare(&wrapped).map_err(to_io)?;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut result_rows = stmt.query([]).map_err(to_io)?;
    while let Some(row) = result_rows.next().map_err(to_io)? {
        let mut cells = Vec::with_capacity(columns.len());
        for i in 0..columns.len() {
            let value: Value = row.get(i).map_err(to_io)?;
            cells.push(value_to_string(&value));
        }
        rows.push(cells);
    }

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
    validate(sql).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let conn = connect(path)?;
    let count = count.clamp(1, 100_000);
    let wrapped = format!(
        "SELECT * FROM ({sql}) AS _q LIMIT {} OFFSET {}",
        count + 1,
        start
    );

    let describe = format!("DESCRIBE SELECT * FROM ({sql}) AS _q");
    let mut describe_stmt = conn.prepare(&describe).map_err(to_io)?;
    let mut describe_rows = describe_stmt.query([]).map_err(to_io)?;
    let mut columns: Vec<String> = Vec::new();
    while let Some(row) = describe_rows.next().map_err(to_io)? {
        let name: String = row.get(0).map_err(to_io)?;
        columns.push(name);
    }
    drop(describe_rows);
    drop(describe_stmt);

    let mut stmt = conn.prepare(&wrapped).map_err(to_io)?;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut result_rows = stmt.query([]).map_err(to_io)?;
    while let Some(row) = result_rows.next().map_err(to_io)? {
        let mut cells = Vec::with_capacity(columns.len());
        for i in 0..columns.len() {
            let value: Value = row.get(i).map_err(to_io)?;
            cells.push(value_to_string(&value));
        }
        rows.push(cells);
    }

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
