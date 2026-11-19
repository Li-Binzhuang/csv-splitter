//! Which engine runs an analysis: the native one, or DuckDB.
//!
//! The project has two kinds of work and they want different machinery:
//!
//! * **rendering / editing** — a window of rows, journal edits, formula columns. It must
//!   see `base + journal` per row, so it stays native. Handing it to DuckDB would require
//!   materialising the whole file first, which is the one thing this tool refuses to do.
//! * **analysis** — `groupby`, `pivot`, `chart`, `sort`, `dedupe`, joins. "Scan the file and
//!   answer" is exactly a database's job, and DuckDB reads a 2.5 GB / 782-column CSV in
//!   about a second while the native reader needs thirteen. Those belong in DuckDB.
//!
//! `Engine::Auto` is the default and chooses by input size: DuckDB above the threshold,
//! native below it (its startup is ~200 ms, which is eight times the whole small query).

use std::path::Path;

/// Inputs at least this large go to DuckDB under `Engine::Auto`.
///
/// Below it the native path wins: measured 20 120 rows in 0.026 s natively against
/// 0.207 s through DuckDB, almost all of it engine start-up.
pub const SQL_AUTO_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// Pick by input size.
    Auto,
    /// The in-repo streaming implementation.
    Native,
    /// DuckDB.
    Sql,
}

impl Engine {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Ok(Self::Auto),
            "native" | "rust" => Ok(Self::Native),
            "sql" | "duckdb" => Ok(Self::Sql),
            other => Err(format!("unknown engine '{other}' (use auto/native/sql)")),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Native => "native",
            Self::Sql => "sql",
        }
    }
}

/// Whether this build can talk to DuckDB at all.
pub fn sql_available() -> bool {
    cfg!(feature = "sql")
}

/// Resolve `Auto` into the engine that will actually run, and refuse `Sql` in a build
/// without the feature instead of silently falling back — a silent fallback would make
/// "I chose sql" and "the build ignored me" look identical.
pub fn resolve(engine: Engine, input: &Path) -> Result<Engine, String> {
    match engine {
        Engine::Sql if !sql_available() => {
            Err("this build has no `sql` feature; rebuild with --features sql".to_string())
        }
        Engine::Sql => Ok(Engine::Sql),
        Engine::Native => Ok(Engine::Native),
        Engine::Auto => {
            let bytes = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
            if bytes >= SQL_AUTO_THRESHOLD_BYTES && sql_available() {
                Ok(Engine::Sql)
            } else {
                Ok(Engine::Native)
            }
        }
    }
}

/// Quote an identifier for DuckDB without letting a column name close the quote.
#[cfg(feature = "sql")]
fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Present in every build so callers do not need their own `cfg`: a build without the
/// feature answers with an error instead of failing to compile.
#[cfg(not(feature = "sql"))]
pub fn groupby_sql(
    _path: &Path,
    _delimiter: crate::types::Delimiter,
    _output: &Path,
    _options: &crate::groupby::GroupOptions,
    _tx: &crate::types::ProgressSender,
) -> std::io::Result<crate::groupby::GroupSummary> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this build has no `sql` feature; rebuild with --features sql",
    ))
}

/// DuckDB-backed groupby. Same options and the same output bytes as the native path
/// where the semantics overlap.
///
/// It goes through `sql::query_with` rather than opening its own connection: that path
/// already declares every column from a small sample (`auto_detect=false`), which is why
/// it reads a 782-column file in a second while `read_csv_auto` spends that long inferring
/// 782 types.
///
/// Deliberate difference from native: numeric aggregates over a column that mixes text
/// keep SQL arithmetic and ignore the text, where native reports `<mixed>`.
#[cfg(feature = "sql")]
pub fn groupby_sql(
    path: &Path,
    delimiter: crate::types::Delimiter,
    output: &Path,
    options: &crate::groupby::GroupOptions,
    tx: &crate::types::ProgressSender,
) -> std::io::Result<crate::groupby::GroupSummary> {
    use crate::aggregate::{format_number, AggOp};
    use std::io::{self, Write};

    let total_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    tx.send(crate::types::ProgressEvent::PhaseStart {
        phase: "Grouping (duckdb)".into(),
        total_bytes,
    })
    .ok();

    if options.keys.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no key columns",
        ));
    }
    if options.aggregates.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no aggregates"));
    }

    let mut select: Vec<String> = options.keys.iter().map(|k| ident(k)).collect();
    for spec in &options.aggregates {
        let column = ident(&spec.column);
        let as_double = format!("try_cast({column} AS DOUBLE)");
        let expr = match spec.op {
            AggOp::Count => format!("count({column})"),
            AggOp::Sum => format!("sum({as_double})"),
            AggOp::Mean => format!("avg({as_double})"),
            AggOp::Min => format!("min({column})"),
            AggOp::Max => format!("max({column})"),
        };
        select.push(format!("{expr} AS {}", ident(&spec.output_name())));
    }
    let keys_sql = options
        .keys
        .iter()
        .map(|k| ident(k))
        .collect::<Vec<_>>()
        .join(", ");
    let order = match options.order_by.as_deref() {
        Some(name) => format!(
            "{}{}",
            ident(name),
            if options.descending { " DESC" } else { "" }
        ),
        None => format!(
            "{}{}",
            keys_sql,
            if options.descending { " DESC" } else { "" }
        ),
    };
    let query = format!(
        "SELECT {} FROM t GROUP BY {} ORDER BY {}",
        select.join(", "),
        keys_sql,
        order
    );
    let limit = options.max_groups.max(1).saturating_add(1);
    let result = crate::sql::query_with(path, &query, limit, delimiter)?;
    if result.truncated {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "more than {} groups; raise --max-groups or narrow the key",
                options.max_groups
            ),
        ));
    }

    let mut out = std::io::BufWriter::new(std::fs::File::create(output)?);
    let mut header: Vec<String> = options.keys.clone();
    header.extend(options.aggregates.iter().map(|a| a.output_name()));
    writeln!(out, "{}", header.join(","))?;
    for row in &result.rows {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, value)| {
                // sum/mean come back as full-precision doubles; render them the way the
                // native path does so both engines produce the same bytes.
                let numeric_agg = i >= options.keys.len()
                    && options.aggregates[i - options.keys.len()].op.is_numeric();
                match (numeric_agg, value.trim().parse::<f64>()) {
                    (true, Ok(n)) if value.trim() != "" => format_number(n),
                    _ => value.clone(),
                }
            })
            .collect();
        writeln!(out, "{}", quote_cells(&cells).join(","))?;
    }
    out.flush()?;

    let rows_query = "SELECT count(*) AS n FROM t";
    let rows: u64 = crate::sql::query_with(path, rows_query, 1, delimiter)
        .ok()
        .and_then(|r| r.rows.first().and_then(|row| row.first().cloned()))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(crate::types::ProgressEvent::PhaseDone {
        phase: "Grouping (duckdb)".into(),
    })
    .ok();
    Ok(crate::groupby::GroupSummary {
        rows,
        groups: result.rows.len() as u64,
        bytes,
        key_columns: options.keys.clone(),
        aggregates: options.aggregates.iter().map(|a| a.output_name()).collect(),
    })
}

/// Minimal CSV quoting: only when the cell needs it, matching the native writer.
#[cfg(feature = "sql")]
fn quote_cells(cells: &[String]) -> Vec<String> {
    cells
        .iter()
        .map(|cell| {
            if cell.contains(',') || cell.contains('"') || cell.contains('\n') {
                format!("\"{}\"", cell.replace('"', "\"\""))
            } else {
                cell.clone()
            }
        })
        .collect()
}

/// DuckDB-backed chart data. Same shape as the native path: categories plus one value
/// per category for each series, with `None` where a series has no point.
#[cfg(feature = "sql")]
pub fn chart_sql(
    path: &Path,
    delimiter: crate::types::Delimiter,
    options: &crate::chart::ChartOptions,
    tx: &crate::types::ProgressSender,
) -> std::io::Result<crate::chart::ChartData> {
    use crate::aggregate::AggOp;
    use crate::chart::Series;
    use std::io;

    let total_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    tx.send(crate::types::ProgressEvent::PhaseStart {
        phase: "Charting (duckdb)".into(),
        total_bytes,
    })
    .ok();

    let x = ident(&options.x);
    let y = ident(&options.y.column);
    let as_double = format!("try_cast({y} AS DOUBLE)");
    let measure = match options.y.op {
        AggOp::Count => format!("count({y})"),
        AggOp::Sum => format!("sum({as_double})"),
        AggOp::Mean => format!("avg({as_double})"),
        AggOp::Min => format!("min({as_double})"),
        AggOp::Max => format!("max({as_double})"),
    };
    let (select, group) = match options.series.as_deref() {
        Some(name) => {
            let series = ident(name);
            (
                format!("SELECT {x}, {series}, {measure} AS v FROM t"),
                format!(" GROUP BY {x}, {series}"),
            )
        }
        None => (
            format!("SELECT {x}, {measure} AS v FROM t"),
            format!(" GROUP BY {x}"),
        ),
    };
    let order = if options.sort_by_value {
        format!(
            " ORDER BY v{}",
            if options.descending { " DESC" } else { "" }
        )
    } else {
        format!(
            " ORDER BY {x}{}",
            if options.descending { " DESC" } else { "" }
        )
    };
    let query = format!("{select}{group}{order}");
    let result = crate::sql::query_with(path, &query, options.max_points.max(1), delimiter)?;
    if result.truncated {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "more points than --max-points {}; narrow the x column or raise the limit",
                options.max_points
            ),
        ));
    }

    let mut categories: Vec<String> = Vec::new();
    let mut series: Vec<Series> = Vec::new();
    let mut points: Vec<(usize, usize, Option<f64>)> = Vec::new();
    for row in &result.rows {
        let (category, series_name, value) = match options.series.as_deref() {
            Some(_) => (
                row.first().cloned().unwrap_or_default(),
                row.get(1).cloned().unwrap_or_default(),
                row.get(2).and_then(|v| v.trim().parse::<f64>().ok()),
            ),
            None => (
                row.first().cloned().unwrap_or_default(),
                String::new(),
                row.get(1).and_then(|v| v.trim().parse::<f64>().ok()),
            ),
        };
        let category_index = match categories.iter().position(|c| c == &category) {
            Some(i) => i,
            None => {
                categories.push(category);
                categories.len() - 1
            }
        };
        let series_index = match series.iter().position(|s| s.name == series_name) {
            Some(i) => i,
            None => {
                series.push(Series {
                    name: series_name,
                    points: Vec::new(),
                });
                series.len() - 1
            }
        };
        points.push((category_index, series_index, value));
    }
    for entry in &mut series {
        entry.points = vec![None; categories.len()];
    }
    for (category_index, series_index, value) in points {
        if let Some(series) = series.get_mut(series_index) {
            series.points[category_index] = value;
        }
    }

    let rows_query = "SELECT count(*) AS n FROM t";
    let rows: u64 = crate::sql::query_with(path, rows_query, 1, delimiter)
        .ok()
        .and_then(|r| r.rows.first().and_then(|row| row.first().cloned()))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    tx.send(crate::types::ProgressEvent::PhaseDone {
        phase: "Charting (duckdb)".into(),
    })
    .ok();
    Ok(crate::chart::ChartData {
        kind: options.kind.name(),
        categories,
        series,
        rows,
    })
}

#[cfg(not(feature = "sql"))]
pub fn chart_sql(
    _path: &Path,
    _delimiter: crate::types::Delimiter,
    _options: &crate::chart::ChartOptions,
    _tx: &crate::types::ProgressSender,
) -> std::io::Result<crate::chart::ChartData> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this build has no `sql` feature; rebuild with --features sql",
    ))
}

/// DuckDB-backed pivot, for the file output path. The terminal matrix keeps the native
/// path: it is built for three-column output, not for a database to stream.
#[cfg(feature = "sql")]
pub fn pivot_sql(
    path: &Path,
    delimiter: crate::types::Delimiter,
    output: &Path,
    options: &crate::pivot::PivotOptions,
    tx: &crate::types::ProgressSender,
) -> std::io::Result<crate::pivot::PivotSummary> {
    use crate::aggregate::{format_number, AggOp};
    use std::io::{self, Write};

    let total_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    tx.send(crate::types::ProgressEvent::PhaseStart {
        phase: "Pivoting (duckdb)".into(),
        total_bytes,
    })
    .ok();

    let Some(column_key) = options.columns.as_deref() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pivot needs a column key",
        ));
    };
    if options.rows.is_empty() || options.values.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pivot needs row keys and measures",
        ));
    }

    let distinct_query = format!(
        "SELECT DISTINCT COALESCE({col}, '') FROM t ORDER BY 1 LIMIT {}",
        options.max_columns.max(1) + 1,
        col = ident(column_key)
    );
    let distinct =
        crate::sql::query_with(path, &distinct_query, options.max_columns + 1, delimiter)?;
    let mut column_values: Vec<String> = distinct
        .rows
        .iter()
        .filter_map(|row| row.first().cloned())
        .collect();
    if column_values.len() > options.max_columns {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "more than {} distinct values in '{}'; raise --max-columns",
                options.max_columns, column_key
            ),
        ));
    }
    column_values.sort();

    let mut header: Vec<String> = options.rows.clone();
    for value in &column_values {
        for spec in &options.values {
            header.push(if options.qualify_names {
                format!("{value} \u{00b7} {}", spec.output_name())
            } else {
                value.clone()
            });
        }
    }

    let mut select: Vec<String> = options.rows.iter().map(|r| ident(r)).collect();
    for value in &column_values {
        // An empty cell is a legitimate pivot column, which is why COALESCE appears on
        // both sides: DuckDB reads a blank CSV field as NULL, the native path as "".
        let condition = format!(
            "COALESCE({}, '') = '{}'",
            ident(column_key),
            value.replace('\'', "''")
        );
        for spec in &options.values {
            let measure = ident(&spec.column);
            let expr = match spec.op {
                AggOp::Count => format!("count(CASE WHEN {condition} THEN {measure} END)"),
                AggOp::Sum => {
                    format!("sum(CASE WHEN {condition} THEN try_cast({measure} AS DOUBLE) END)")
                }
                AggOp::Mean => {
                    format!("avg(CASE WHEN {condition} THEN try_cast({measure} AS DOUBLE) END)")
                }
                AggOp::Min => {
                    format!("min(CASE WHEN {condition} THEN try_cast({measure} AS DOUBLE) END)")
                }
                AggOp::Max => {
                    format!("max(CASE WHEN {condition} THEN try_cast({measure} AS DOUBLE) END)")
                }
            };
            // The alias is positional, not the display name: an empty pivot column has an
            // empty header (legal in CSV) but SQL rejects a zero-length identifier. The
            // real header is written from `header`, which carries the display names.
            select.push(format!(
                "{expr} AS {}",
                ident(&format!("c{}", select.len()))
            ));
        }
    }
    let keys_sql = options
        .rows
        .iter()
        .map(|r| ident(r))
        .collect::<Vec<_>>()
        .join(", ");
    let order = match options.order_by.as_deref() {
        Some(name) => format!(
            "{}{}",
            ident(name),
            if options.descending { " DESC" } else { "" }
        ),
        None => format!(
            "{}{}",
            keys_sql,
            if options.descending { " DESC" } else { "" }
        ),
    };
    let query = format!(
        "SELECT {} FROM t GROUP BY {} ORDER BY {}",
        select.join(", "),
        keys_sql,
        order
    );
    let limit = options.max_cells.max(1).saturating_add(1);
    let result = crate::sql::query_with(path, &query, limit, delimiter)?;
    if result.truncated
        || result.rows.len() * column_values.len() * options.values.len() > options.max_cells
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "more than {} cells; raise --max-cells or narrow the keys",
                options.max_cells
            ),
        ));
    }

    let mut out = std::io::BufWriter::new(std::fs::File::create(output)?);
    writeln!(out, "{}", quote_cells(&header).join(","))?;
    for row in &result.rows {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, value)| {
                let measure_index = if i < options.rows.len() {
                    None
                } else {
                    Some((i - options.rows.len()) % options.values.len())
                };
                match measure_index {
                    Some(index) => {
                        let op = options.values[index].op;
                        if value.trim().is_empty() {
                            // A cell with no contributing row is not NULL in the native
                            // path: sum and count print 0, mean/min/max print nothing.
                            match op {
                                AggOp::Sum | AggOp::Count => "0".to_string(),
                                _ => String::new(),
                            }
                        } else if op.is_numeric() || matches!(op, AggOp::Count) {
                            match value.trim().parse::<f64>() {
                                Ok(n) => format_number(n),
                                Err(_) => value.clone(),
                            }
                        } else {
                            value.clone()
                        }
                    }
                    None => value.clone(),
                }
            })
            .collect();
        writeln!(out, "{}", quote_cells(&cells).join(","))?;
    }
    out.flush()?;

    let rows_query = "SELECT count(*) AS n FROM t";
    let input_rows: u64 = crate::sql::query_with(path, rows_query, 1, delimiter)
        .ok()
        .and_then(|r| r.rows.first().and_then(|row| row.first().cloned()))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(crate::types::ProgressEvent::PhaseDone {
        phase: "Pivoting (duckdb)".into(),
    })
    .ok();
    Ok(crate::pivot::PivotSummary {
        input_rows,
        row_keys: result.rows.len() as u64,
        column_keys: column_values.len() as u64,
        output_rows: result.rows.len() as u64,
        bytes,
        row_columns: options.rows.clone(),
        column_values,
        measures: options.values.iter().map(|v| v.output_name()).collect(),
    })
}

#[cfg(not(feature = "sql"))]
pub fn pivot_sql(
    _path: &Path,
    _delimiter: crate::types::Delimiter,
    _output: &Path,
    _options: &crate::pivot::PivotOptions,
    _tx: &crate::types::ProgressSender,
) -> std::io::Result<crate::pivot::PivotSummary> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this build has no `sql` feature; rebuild with --features sql",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_picks_by_size() {
        let dir = std::env::temp_dir().join(format!("csvs_engine_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.csv");
        std::fs::write(&small, "a,b\n1,2\n").unwrap();
        let big = dir.join("big.csv");
        std::fs::write(&big, vec![b'x'; SQL_AUTO_THRESHOLD_BYTES as usize]).unwrap();

        let small_engine = resolve(Engine::Auto, &small).unwrap();
        assert_eq!(small_engine, Engine::Native);
        let big_engine = resolve(Engine::Auto, &big).unwrap();
        assert_eq!(
            big_engine,
            if sql_available() {
                Engine::Sql
            } else {
                Engine::Native
            }
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn explicit_sql_is_refused_without_the_feature() {
        let dir = std::env::temp_dir().join(format!("csvs_engine2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.csv");
        std::fs::write(&file, "a\n1\n").unwrap();
        let result = resolve(Engine::Sql, &file);
        assert_eq!(result.is_ok(), sql_available());
        if let Err(message) = result {
            assert!(message.contains("no `sql` feature"), "{message}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The two engines must produce the same file for data both can handle. Without this
    /// test the "reuse DuckDB" refactor would silently change numbers.
    #[cfg(feature = "sql")]
    #[test]
    fn json_and_native_groupby_agree_byte_for_byte() {
        use crate::aggregate::AggSpec;
        use crate::groupby::{groupby_quotes, GroupOptions};
        use crate::types::{Delimiter, ProgressSender};

        let dir = std::env::temp_dir().join(format!("csvs_engine3_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("t.csv");
        std::fs::write(
            &input,
            "city,amount,qty\nCDMex,10,2\nPuebla,7,3\nCDMex,5,4\nPuebla,1,5\n",
        )
        .unwrap();
        let options = GroupOptions {
            keys: vec!["city".into()],
            aggregates: ["amount:sum", "amount:mean", "qty:count"]
                .iter()
                .map(|s| AggSpec::parse_list(s).unwrap().remove(0))
                .collect(),
            order_by: None,
            descending: false,
            max_groups: 1000,
        };

        let native_out = dir.join("native.csv");
        let sql_out = dir.join("sql.csv");
        let (tx, _rx) = std::sync::mpsc::channel::<crate::types::ProgressEvent>();
        let tx_native: ProgressSender = tx.clone();
        groupby_quotes(
            &input,
            Delimiter::Comma,
            &native_out,
            &options,
            Default::default(),
            &tx_native,
        )
        .unwrap();
        let tx_sql: ProgressSender = tx.clone();
        groupby_sql(&input, Delimiter::Comma, &sql_out, &options, &tx_sql).unwrap();

        let native = std::fs::read_to_string(&native_out).unwrap();
        let sql = std::fs::read_to_string(&sql_out).unwrap();
        assert_eq!(native, sql, "native:\n{native}\nsql:\n{sql}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Chart engines must agree on every category's number. Order is allowed to differ:
    /// SQL's GROUP BY has no "first appearance" order, while the native path deliberately
    /// keeps the order categories appear in (a date axis must not become alphabetical).
    #[cfg(feature = "sql")]
    #[test]
    fn chart_engines_agree_on_values() {
        use crate::aggregate::AggSpec;
        use crate::chart::{chart_quotes, ChartOptions};
        use crate::types::{Delimiter, ProgressSender};

        let dir = std::env::temp_dir().join(format!("csvs_engine4_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("t.csv");
        std::fs::write(
            &input,
            "day,amount\n2026-01-02,10\n2026-01-01,4\n2026-01-02,6\n2026-01-01,8\n",
        )
        .unwrap();
        let options = ChartOptions {
            x: "day".into(),
            series: None,
            y: AggSpec::parse_list("amount:sum").unwrap().remove(0),
            kind: crate::chart::ChartKind::Bar,
            sort_by_value: false,
            descending: false,
            max_points: 100,
        };

        let (tx, _rx) = std::sync::mpsc::channel::<crate::types::ProgressEvent>();
        let tx_native: ProgressSender = tx.clone();
        let native = chart_quotes(
            &input,
            Delimiter::Comma,
            &options,
            Default::default(),
            &tx_native,
        )
        .unwrap();
        let tx_sql: ProgressSender = tx.clone();
        let sql = chart_sql(&input, Delimiter::Comma, &options, &tx_sql).unwrap();

        let mut native_map = std::collections::BTreeMap::new();
        for (index, category) in native.categories.iter().enumerate() {
            native_map.insert(category.clone(), native.series[0].points[index]);
        }
        let mut sql_map = std::collections::BTreeMap::new();
        for (index, category) in sql.categories.iter().enumerate() {
            sql_map.insert(category.clone(), sql.series[0].points[index]);
        }
        assert_eq!(native_map, sql_map);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The pivot path must reproduce the native file byte for byte, empty column values
    /// included: that `""` header is a real bucket, not a bug to filter out.
    #[cfg(feature = "sql")]
    #[test]
    fn pivot_engines_agree_byte_for_byte() {
        use crate::aggregate::AggSpec;
        use crate::pivot::{pivot_quotes, PivotOptions};
        use crate::types::{Delimiter, ProgressSender};

        let dir = std::env::temp_dir().join(format!("csvs_engine5_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("t.csv");
        std::fs::write(&input, "city,kind,amount\nA,x,1\nA,y,2\nB,x,3\nB,,4\n").unwrap();
        let options = PivotOptions {
            rows: vec!["city".into()],
            columns: Some("kind".into()),
            values: vec![AggSpec::parse_list("amount:sum").unwrap().remove(0)],
            order_by: None,
            descending: false,
            qualify_names: false,
            max_columns: 100,
            max_cells: 10_000,
        };
        let native_out = dir.join("native.csv");
        let sql_out = dir.join("sql.csv");
        let (tx, _rx) = std::sync::mpsc::channel::<crate::types::ProgressEvent>();
        let tx_native: ProgressSender = tx.clone();
        pivot_quotes(
            &input,
            Delimiter::Comma,
            &native_out,
            &options,
            Default::default(),
            &tx_native,
        )
        .unwrap();
        let tx_sql: ProgressSender = tx.clone();
        pivot_sql(&input, Delimiter::Comma, &sql_out, &options, &tx_sql).unwrap();
        let native = std::fs::read_to_string(&native_out).unwrap();
        let sql = std::fs::read_to_string(&sql_out).unwrap();
        assert_eq!(native, sql, "native:\n{native}\nsql:\n{sql}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
