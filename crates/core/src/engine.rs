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
/// Below this, DuckDB's fixed startup (~30 ms) costs more than the scan it saves.
///
/// Measured on one machine with `groupby` (7 groups, one sum):
///   0.9 MB  native 0.01 s  sql 0.03 s
///   1.8 MB  native 0.02 s  sql 0.03 s
///   3.6 MB  native 0.04 s  sql 0.04 s
///   9.1 MB  native 0.08 s  sql 0.06 s
///  18.1 MB  native 0.16 s  sql 0.07 s
/// The old 64 MB was chosen without data and left a 7x gap where the native path is slower.
pub const SQL_AUTO_THRESHOLD_BYTES: u64 = 8 * 1024 * 1024;

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

/// Resolve an engine for a command whose work is mostly *copying bytes* rather than
/// reading values.
///
/// `concat` is the example: the native path copies records and only touches headers, while
/// DuckDB parses every column, writes the file back and scans it again for the row count.
/// Measured: 18 MB x 2 files, native 0.07 s vs sql 0.20 s; 90 MB x 2, 0.26 s vs 0.88 s.
/// So `auto` stays native here; asking for `sql` explicitly is still allowed.
pub fn resolve_for_copy(engine: Engine, path: &Path) -> Result<Engine, String> {
    match engine {
        Engine::Auto => Ok(Engine::Native),
        other => resolve(other, path),
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

/// DuckDB-backed lookup: the reference table becomes view `ref`, the main file view `t`,
/// and the match is a LEFT JOIN. A missing key keeps the native `#N/A`, so the output
/// means the same thing whichever engine produced it.
#[cfg(feature = "sql")]
pub fn lookup_sql(
    main: &Path,
    delimiter: crate::types::Delimiter,
    output: &Path,
    options: &crate::lookup::LookupOptions,
    tx: &crate::types::ProgressSender,
) -> std::io::Result<crate::lookup::LookupSummary> {
    use std::io::{self, Write};

    let total_bytes = std::fs::metadata(main).map(|m| m.len()).unwrap_or(0);
    tx.send(crate::types::ProgressEvent::PhaseStart {
        phase: "Looking up (duckdb)".into(),
        total_bytes,
    })
    .ok();

    let main_header = crate::workbook::base_header(main, delimiter)?;
    if main_header.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} has no header", main.display()),
        ));
    }

    // Same clash rule as the native path: a copied name that already exists in the main
    // file gets `_ref`, repeatedly, until it is unique.
    let mut columns = main_header.clone();
    let mut output_names: Vec<String> = Vec::with_capacity(options.take.len());
    for wanted in &options.take {
        let mut name = wanted.clone();
        while columns.iter().any(|c| c == &name) {
            name = format!("{name}_ref");
        }
        columns.push(name.clone());
        output_names.push(name);
    }

    let keys: Vec<String> = options
        .key
        .split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    let reference_keys: Vec<String> = options
        .reference_key
        .split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    if keys.len() != reference_keys.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "key and from-key must have the same number of columns ({} vs {})",
                keys.len(),
                reference_keys.len()
            ),
        ));
    }

    let on = keys
        .iter()
        .zip(reference_keys.iter())
        .map(|(l, r)| format!("m.{} = r.{}", ident(l), ident(r)))
        .collect::<Vec<_>>()
        .join(" AND ");
    let projected = output_names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            format!(
                "COALESCE(r.{}, '#N/A') AS {}",
                ident(&options.take[index]),
                ident(name)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!("SELECT m.*, {projected} FROM t m LEFT JOIN ref r ON {on}");

    let result =
        crate::sql::query_with_reference(main, &options.reference, &query, 100_000, delimiter)?;

    let mut out = std::io::BufWriter::new(std::fs::File::create(output)?);
    writeln!(out, "{}", quote_cells(&columns).join(","))?;
    let mut missing = 0u64;
    for row in &result.rows {
        let cells: Vec<String> = row.clone();
        if cells
            .iter()
            .rev()
            .take(output_names.len())
            .all(|v| v == "#N/A")
        {
            missing += 1;
        }
        writeln!(out, "{}", quote_cells(&cells).join(","))?;
    }
    out.flush()?;

    let rows = result.rows.len() as u64;
    let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(crate::types::ProgressEvent::PhaseDone {
        phase: "Looking up (duckdb)".into(),
    })
    .ok();
    Ok(crate::lookup::LookupSummary {
        main_rows: rows,
        matched: rows.saturating_sub(missing),
        missing,
        reference_keys: 0,
        bytes,
        columns,
    })
}

#[cfg(not(feature = "sql"))]
pub fn lookup_sql(
    _main: &Path,
    _delimiter: crate::types::Delimiter,
    _output: &Path,
    _options: &crate::lookup::LookupOptions,
    _tx: &crate::types::ProgressSender,
) -> std::io::Result<crate::lookup::LookupSummary> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this build has no `sql` feature; rebuild with --features sql",
    ))
}

/// Translate one filter into a SQL predicate, or `None` when the native semantics have no
/// exact SQL equivalent.
///
/// Supported: equality, inequality, contains, `in` (case-insensitive like native), empty
/// and non-empty, and regex. The relational operators (`> >= < <=`) are NOT translated:
/// the native path compares numbers when both sides parse and strings otherwise, and a
/// single SQL predicate cannot express that without a CASE that would be harder to trust
/// than the native comparison.
#[cfg(feature = "sql")]
pub fn filter_predicate(spec: &crate::filter::FilterSpec) -> Option<String> {
    use crate::filter::Op;
    // COALESCE matters: DuckDB reads a blank CSV field as NULL, and `NULL = ''` is
    // false, which would make `note empty` select nothing.
    let text = format!("COALESCE(CAST({} AS VARCHAR), '')", ident(&spec.column));
    let quoted = spec.value.replace('\'', "''");
    Some(match spec.op {
        Op::Eq => format!("{text} = '{quoted}'"),
        Op::Ne => format!("{text} <> '{quoted}'"),
        Op::Contains => format!("contains({text}, '{quoted}')"),
        Op::In => {
            let items: Vec<String> = spec
                .value
                .split(',')
                .map(|v| format!("'{}'", v.trim().to_lowercase().replace('\'', "''")))
                .collect();
            if items.is_empty() {
                return None;
            }
            format!("lower(trim({text})) IN ({})", items.join(", "))
        }
        Op::Empty => format!("trim({text}) = ''"),
        Op::NotEmpty => format!("trim({text}) <> ''"),
        Op::Regex => format!("regexp_matches({text}, '{quoted}')"),
        // Spreadsheet wildcards stay on the native matcher: translating them to LIKE
        // would silently change case sensitivity, and native is correct.
        Op::Wildcard => return None,
        Op::Gt | Op::Gte | Op::Lt | Op::Lte => return None,
    })
}

#[cfg(not(feature = "sql"))]
pub fn filter_predicate(_spec: &crate::filter::FilterSpec) -> Option<String> {
    None
}

/// DuckDB-backed concat: one view over the file list, one `COPY` to the output.
///
/// Headers are still validated natively first. The database's `union_by_name` would happily
/// glue a column of amounts under a column of ids, which is the corruption the native path
/// exists to refuse, so the check stays where it can name the differing file and columns.
#[cfg(feature = "sql")]
pub fn concat_sql(
    inputs: &[std::path::PathBuf],
    output: &Path,
    delimiter: crate::types::Delimiter,
    ignore_header: bool,
    tx: &crate::types::ProgressSender,
) -> std::io::Result<crate::concat::ConcatSummary> {
    use std::io;

    if inputs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no input files",
        ));
    }
    let total_bytes: u64 = inputs
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    tx.send(crate::types::ProgressEvent::PhaseStart {
        phase: "Concatenating (duckdb)".into(),
        total_bytes,
    })
    .ok();

    let header_of = |path: &std::path::Path| -> io::Result<Vec<String>> {
        let file = std::fs::File::open(path)?;
        let mut reader = crate::csv_reader::RecordReader::new(
            std::io::BufReader::with_capacity(256 * 1024, file),
            256 * 1024,
        );
        let mut record = Vec::new();
        if !reader.next_record(&mut record)? {
            return Ok(Vec::new());
        }
        Ok(crate::csv_reader::parse_csv_record(
            String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
            delimiter.byte(),
        ))
    };

    let expected = header_of(&inputs[0])?;
    if expected.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} has no header", inputs[0].display()),
        ));
    }
    let mut mismatched: Vec<std::path::PathBuf> = Vec::new();
    for path in inputs.iter().skip(1) {
        let header = header_of(path)?;
        if header != expected {
            if !ignore_header {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "{}: header differs from {} (use --ignore-header to append anyway)",
                        path.display(),
                        inputs[0].display()
                    ),
                ));
            }
            mismatched.push(path.clone());
        }
    }

    let rows = crate::sql::copy_to_csv(inputs, "SELECT * FROM t", output, delimiter)?;
    let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(crate::types::ProgressEvent::PhaseDone {
        phase: "Concatenating (duckdb)".into(),
    })
    .ok();
    Ok(crate::concat::ConcatSummary {
        files: inputs.len() as u64,
        rows,
        bytes,
        columns: expected,
        mismatched,
    })
}

#[cfg(not(feature = "sql"))]
pub fn concat_sql(
    _inputs: &[std::path::PathBuf],
    _output: &Path,
    _delimiter: crate::types::Delimiter,
    _ignore_header: bool,
    _tx: &crate::types::ProgressSender,
) -> std::io::Result<crate::concat::ConcatSummary> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this build has no `sql` feature; rebuild with --features sql",
    ))
}

/// User-level format conversion: CSV/TSV/NDJSON/Parquet in any direction. DuckDB does
/// the parsing and writing, so quoted fields, unicode and nested JSON stay correct.
/// xlsx is deliberately absent.
pub fn convert_format(
    input: &Path,
    output: &Path,
    target: &str,
    delimiter: crate::types::Delimiter,
    tx: &crate::types::ProgressSender,
) -> std::io::Result<()> {
    let target = target.trim().to_ascii_lowercase();
    if !matches!(target.as_str(), "csv" | "tsv" | "ndjson" | "parquet") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "unsupported --format {target:?}; use csv/tsv/ndjson/parquet (xlsx is disabled)"
            ),
        ));
    }
    if !sql_available() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this build cannot convert formats (no DuckDB feature)",
        ));
    }
    let source = source_expr(input, delimiter)?;
    let options = match target.as_str() {
        "csv" => "(HEADER, DELIMITER ',')",
        "tsv" => "(HEADER, DELIMITER '\\t')",
        "ndjson" => "(FORMAT JSON)",
        "parquet" => "(FORMAT PARQUET)",
        _ => unreachable!(),
    };
    tx.send(crate::types::ProgressEvent::PhaseStart {
        phase: "Converting".into(),
        total_bytes: std::fs::metadata(input).map(|m| m.len()).unwrap_or(0),
    })
    .ok();
    let sql = format!(
        "COPY (SELECT * FROM {source}) TO '{}' {options};",
        sql_path(output)
    );
    crate::sql::run_copy_with_timeout(input, &sql, 600_000)?;
    tx.send(crate::types::ProgressEvent::PhaseDone {
        phase: "Converting".into(),
    })
    .ok();
    Ok(())
}

fn sql_path(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}

fn source_expr(input: &Path, delimiter: crate::types::Delimiter) -> std::io::Result<String> {
    let name = input
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let path = sql_path(input);
    let kind = if name.ends_with(".parquet") {
        "parquet"
    } else if name.ends_with(".ndjson") || name.ends_with(".jsonl") || name.ends_with(".json") {
        "ndjson"
    } else if name.ends_with(".tsv") {
        "tsv"
    } else {
        "csv"
    };
    Ok(match kind {
        "parquet" => format!("read_parquet('{path}')"),
        "ndjson" => format!("read_json_auto('{path}')"),
        "tsv" => format!("read_csv_auto('{path}', delim='\\t')"),
        _ => {
            let delim = delimiter.byte() as char;
            format!("read_csv_auto('{path}', delim='{delim}')")
        }
    })
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
    /// Result parity is tested elsewhere; error parity is not. A missing column must fail the
    /// same way on both engines, or a caller that handles one status gets the other.
    /// concat's error path: two files whose headers disagree. Native concatenation has to
    /// refuse or align them; the SQL engine must answer the same way, or a caller switching
    /// engines gets a file where one expects a failure.
    /// With `--by-name` both engines receive the aligned inputs, so their outputs must agree.
    #[test]
    #[cfg(feature = "sql")]
    fn concat_engines_agree_after_by_name_alignment() {
        use crate::types::Delimiter;

        let dir = std::env::temp_dir().join(format!("csvs_engine_align_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.csv");
        let b = dir.join("b.csv");
        std::fs::write(&a, "a,b,c\n1,2,3\n").unwrap();
        std::fs::write(&b, "c,a,b\n30,10,20\n").unwrap();
        let inputs = vec![a.clone(), b.clone()];
        let (tx, _rx) = std::sync::mpsc::channel::<crate::types::ProgressEvent>();

        let aligned = crate::concat::align_by_name(&inputs, Delimiter::Comma).unwrap();
        let native_out = dir.join("n.csv");
        crate::concat::concat(aligned.paths(), &native_out, Delimiter::Comma, false, &tx).unwrap();
        let sql_out = dir.join("s.csv");
        concat_sql(aligned.paths(), &sql_out, Delimiter::Comma, false, &tx).unwrap();
        let native = std::fs::read_to_string(&native_out).unwrap();
        let sql = std::fs::read_to_string(&sql_out).unwrap();
        assert_eq!(native, sql);
    }

    #[test]
    #[cfg(feature = "sql")]
    fn concat_engines_agree_on_how_mismatched_headers_fail() {
        use crate::types::Delimiter;

        let dir = std::env::temp_dir().join(format!("csvs_engine_err4_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.csv");
        let b = dir.join("b.csv");
        std::fs::write(&a, "city,amount\nCDMX,10\n").unwrap();
        std::fs::write(&b, "city,total\nPuebla,20\n").unwrap();
        let inputs = vec![a.clone(), b.clone()];
        let (tx, _rx) = std::sync::mpsc::channel::<crate::types::ProgressEvent>();

        let native_out = dir.join("n.csv");
        let native =
            crate::concat::concat(&inputs, &native_out, Delimiter::Comma, false, &tx).unwrap_err();
        let sql_out = dir.join("s.csv");
        let sql = concat_sql(&inputs, &sql_out, Delimiter::Comma, false, &tx).unwrap_err();
        assert_eq!(
            native.kind(),
            sql.kind(),
            "native {native:?} vs sql {sql:?}"
        );
    }

    /// The last engine pair: lookup over two files. A key column that does not exist must fail
    /// the same way on both engines; the reference side is checked too because a missing
    /// reference key is the mistake a caller actually makes.
    #[test]
    #[cfg(feature = "sql")]
    fn lookup_engines_agree_on_how_a_missing_column_fails() {
        use crate::lookup::LookupOptions;
        use crate::types::Delimiter;

        let dir = std::env::temp_dir().join(format!("csvs_engine_err3_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("main.csv");
        let reference = dir.join("ref.csv");
        std::fs::write(&main, "city,amount\nCDMX,10\n").unwrap();
        std::fs::write(&reference, "city,region\nCDMX,Bajio\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel::<crate::types::ProgressEvent>();

        for (key, rkey, label) in [
            ("nope", "city", "main key"),
            ("city", "nope", "reference key"),
        ] {
            let out = dir.join(format!("out_{label}.csv"));
            let options = LookupOptions {
                key: key.into(),
                reference: reference.clone(),
                reference_key: rkey.into(),
                take: vec!["region".into()],
                max_keys: 1000,
            };
            let native =
                crate::lookup::lookup(&main, Delimiter::Comma, &out, &options, &tx).unwrap_err();
            let sql = lookup_sql(&main, Delimiter::Comma, &out, &options, &tx).unwrap_err();
            assert_eq!(native.kind(), sql.kind(), "{label}: {native:?} vs {sql:?}");
        }
    }

    /// Same check as chart, for the two aggregations whose error paths were still untested.
    #[test]
    #[cfg(feature = "sql")]
    fn groupby_and_pivot_agree_on_how_a_missing_column_fails() {
        use crate::aggregate::AggSpec;
        use crate::pivot::PivotOptions;
        use crate::types::Delimiter;

        let dir = std::env::temp_dir().join(format!("csvs_engine_err2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("t.csv");
        std::fs::write(&input, "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel::<crate::types::ProgressEvent>();

        // groupby: a key column that does not exist.
        let out = dir.join("g.csv");
        let options = crate::groupby::GroupOptions {
            keys: vec!["nope".into()],
            aggregates: AggSpec::parse_list("amount:sum").unwrap(),
            ..Default::default()
        };
        let native = crate::groupby::groupby_quotes(
            &input,
            Delimiter::Comma,
            &out,
            &options,
            crate::csv_reader::QuoteStyle::Rfc4180,
            &tx,
        )
        .unwrap_err();
        let sql = groupby_sql(&input, Delimiter::Comma, &out, &options, &tx).unwrap_err();
        assert_eq!(native.kind(), sql.kind(), "groupby: {native:?} vs {sql:?}");

        // pivot: same mistake on the row key.
        let out = dir.join("p.csv");
        let options = PivotOptions {
            rows: vec!["nope".into()],
            values: AggSpec::parse_list("amount:sum").unwrap(),
            ..Default::default()
        };
        let native = crate::pivot::pivot_quotes(
            &input,
            Delimiter::Comma,
            &out,
            &options,
            crate::csv_reader::QuoteStyle::Rfc4180,
            &tx,
        )
        .unwrap_err();
        let sql = pivot_sql(&input, Delimiter::Comma, &out, &options, &tx).unwrap_err();
        assert_eq!(native.kind(), sql.kind(), "pivot: {native:?} vs {sql:?}");
    }

    #[test]
    #[cfg(feature = "sql")]
    fn chart_engines_agree_on_how_a_missing_column_fails() {
        use crate::chart::ChartOptions;
        use crate::types::Delimiter;

        let dir = std::env::temp_dir().join(format!("csvs_engine_err_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("t.csv");
        std::fs::write(&input, "city,amount\nCDMX,10\nPuebla,20\n").unwrap();
        let options = ChartOptions {
            x: "nope".into(),
            y: crate::aggregate::AggSpec::parse_list("amount:sum")
                .unwrap()
                .remove(0),
            ..Default::default()
        };
        let (tx, _rx) = std::sync::mpsc::channel::<crate::types::ProgressEvent>();

        let native = crate::chart::chart(&input, Delimiter::Comma, &options, &tx).unwrap_err();
        let sql = chart_sql(&input, Delimiter::Comma, &options, &tx).unwrap_err();
        assert_eq!(
            native.kind(),
            sql.kind(),
            "native said {native:?}, sql said {sql:?}"
        );
        assert!(
            sql.to_string().contains("nope"),
            "the sql error should name the column: {sql}"
        );
    }

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
            aggregates: [
                "amount:sum",
                "amount:mean",
                "amount:min",
                "amount:max",
                "qty:count",
                "qty:max",
            ]
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
        std::fs::write(
            &input,
            "city,kind,amount,qty\nA,x,1,10\nA,y,2,20\nB,x,3,30\nB,,4,40\n",
        )
        .unwrap();
        let options = PivotOptions {
            rows: vec!["city".into()],
            columns: Some("kind".into()),
            values: AggSpec::parse_list(
                "amount:sum,amount:mean,amount:min,amount:max,amount:count,qty:max",
            )
            .unwrap(),
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

    /// The SQL lookup must reproduce the native file byte for byte, including `#N/A` for
    /// a key that is not in the reference and the `_ref` suffix on a clashing name.
    #[cfg(feature = "sql")]
    #[test]
    fn lookup_engines_agree_byte_for_byte() {
        use crate::lookup::{lookup_quotes, LookupOptions};
        use crate::types::{Delimiter, ProgressSender};

        let dir = std::env::temp_dir().join(format!("csvs_engine6_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("m.csv");
        let reference = dir.join("r.csv");
        std::fs::write(&main, "id,city\n1,CDMX\n9,NOWHERE\n").unwrap();
        std::fs::write(&reference, "id,city,region\n1,Puebla,SUR\n1,CDMX,NORTH\n").unwrap();
        let options = LookupOptions {
            key: "id,city".into(),
            reference: reference.clone(),
            reference_key: "id,city".into(),
            take: vec!["region".into()],
            max_keys: 1000,
        };
        let native_out = dir.join("native.csv");
        let sql_out = dir.join("sql.csv");
        let (tx, _rx) = std::sync::mpsc::channel::<crate::types::ProgressEvent>();
        let tx_native: ProgressSender = tx.clone();
        lookup_quotes(
            &main,
            Delimiter::Comma,
            &native_out,
            &options,
            Default::default(),
            &tx_native,
        )
        .unwrap();
        let tx_sql: ProgressSender = tx.clone();
        lookup_sql(&main, Delimiter::Comma, &sql_out, &options, &tx_sql).unwrap();
        let native = std::fs::read_to_string(&native_out).unwrap();
        let sql = std::fs::read_to_string(&sql_out).unwrap();
        assert_eq!(native, sql, "native:\n{native}\nsql:\n{sql}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The translated predicate must select the same rows the native matcher selects.
    #[cfg(feature = "sql")]
    #[test]
    fn filter_predicate_matches_the_native_rows() {
        use crate::filter::{matches, FilterSpec};
        use crate::types::Delimiter;

        let dir = std::env::temp_dir().join(format!("csvs_engine7_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("t.csv");
        std::fs::write(
            &input,
            "city,note\nPuebla,refund\nCDMX,\nPuebla,ok\nTijuana,refund issued\n",
        )
        .unwrap();

        for expr in [
            "city=Puebla",
            "note~refund",
            "note empty",
            "city in puebla,tijuana",
        ] {
            let spec = FilterSpec::parse(expr).unwrap();
            let predicate = filter_predicate(&spec).expect("translatable");
            let query = format!("SELECT \"city\", \"note\" FROM t WHERE {predicate}");
            let sql = crate::sql::query_with(&input, &query, 100, Delimiter::Comma).unwrap();

            // The native matcher, applied to the spec's own column, in file order.
            let text = std::fs::read_to_string(&input).unwrap();
            let mut lines = text.lines();
            let header: Vec<String> = lines
                .next()
                .unwrap()
                .split(',')
                .map(|v| v.to_string())
                .collect();
            let index = header.iter().position(|c| c == &spec.column).unwrap();
            let mut native: Vec<Vec<String>> = Vec::new();
            for line in lines {
                let cells: Vec<String> = line.split(',').map(|v| v.to_string()).collect();
                if matches(&cells[index], &spec) {
                    native.push(cells);
                }
            }
            assert_eq!(sql.rows, native, "{expr}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Both concat engines must write the same file and agree on the summary's row count.
    #[cfg(feature = "sql")]
    #[test]
    fn concat_engines_agree_byte_for_byte() {
        use crate::types::{Delimiter, ProgressSender};

        let dir = std::env::temp_dir().join(format!("csvs_engine8_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.csv");
        let b = dir.join("b.csv");
        std::fs::write(&a, "id,city\n1,Puebla\n").unwrap();
        std::fs::write(&b, "id,city\n2,CDMX\n3,Tijuana\n").unwrap();
        let inputs = vec![a.clone(), b.clone()];
        let native_out = dir.join("native.csv");
        let sql_out = dir.join("sql.csv");
        let (tx, _rx) = std::sync::mpsc::channel::<crate::types::ProgressEvent>();
        let tx_native: ProgressSender = tx.clone();
        let native =
            crate::concat::concat(&inputs, &native_out, Delimiter::Comma, false, &tx_native)
                .unwrap();
        let tx_sql: ProgressSender = tx.clone();
        let sql = concat_sql(&inputs, &sql_out, Delimiter::Comma, false, &tx_sql).unwrap();
        assert_eq!(
            std::fs::read_to_string(&native_out).unwrap(),
            std::fs::read_to_string(&sql_out).unwrap()
        );
        assert_eq!(native.rows, sql.rows);
        assert_eq!(native.files, sql.files);
        std::fs::remove_dir_all(&dir).ok();
    }
}
