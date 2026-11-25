use crate::{
    json_escape, parse_delimiter, print_command_help, quality_checks, report_failure, serve, Parser,
};
use csvx_core::csv_reader::parse_csv_record;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

pub(crate) fn date_counts(
    sheet: &csvx_core::sheet::Sheet,
    columns: &[String],
    spec: &str,
) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("datecounts needs a column name".to_string());
    }
    let (date_column, group_column) = match spec.split_once(':') {
        Some((date, group)) => (date.trim(), Some(group.trim())),
        None => (spec, None),
    };
    let date_index = columns
        .iter()
        .position(|name| name == date_column)
        .ok_or_else(|| format!("datecounts: unknown column {date_column:?}"))?;
    let group_index = group_column
        .map(|group| {
            columns
                .iter()
                .position(|name| name == group)
                .ok_or_else(|| format!("datecounts: unknown group column {group:?}"))
        })
        .transpose()?;
    let window = sheet
        .window(0, 1_000_000)
        .map_err(|e| format!("datecounts: {e}"))?;
    let mut counts = std::collections::BTreeMap::<(String, String), u64>::new();
    for row in window.rows {
        let value = row.get(date_index).map(String::as_str).unwrap_or("");
        let date: String = value.chars().take(10).collect();
        if date.is_empty() {
            continue;
        }
        let group = group_index
            .and_then(|index| row.get(index).cloned())
            .unwrap_or_default();
        *counts.entry((date, group)).or_insert(0) += 1;
    }
    let mut headers = vec![date_column.to_string()];
    if let Some(group) = group_column {
        headers.push(group.to_string());
    }
    headers.push("requests".to_string());
    let rows = counts
        .into_iter()
        .map(|((date, group), count)| {
            let mut row = vec![date];
            if group_column.is_some() {
                row.push(group);
            }
            row.push(count.to_string());
            row
        })
        .collect();
    Ok((headers, rows))
}

pub(crate) fn truncate_for_display(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

type ReportGroupTable = (String, Vec<String>, Vec<Vec<String>>);

fn report_group_table(
    input: &Path,
    delimiter: csvx_core::types::Delimiter,
    key: &str,
    measure: &str,
    op: csvx_core::aggregate::AggOp,
    limit: usize,
) -> Result<ReportGroupTable, String> {
    let mut materialized = None;
    let target = if csvx_core::sheet::has_edits(input) {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        let value =
            csvx_core::sheet::materialize(input, &csvx_core::sheet::ReadOptions { delimiter }, &tx)
                .map_err(|error| error.to_string())?;
        let path = value.path().to_path_buf();
        materialized = Some(value);
        path
    } else {
        input.to_path_buf()
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let out = std::env::temp_dir().join(format!(
        "csvx-report-groups-{}-{stamp}.csv",
        std::process::id()
    ));
    let agg = csvx_core::aggregate::AggSpec {
        column: measure.to_string(),
        op,
    };
    let order_name = agg.output_name();
    let options = csvx_core::groupby::GroupOptions {
        keys: vec![key.to_string()],
        aggregates: vec![agg],
        order_by: Some(order_name),
        descending: true,
        max_groups: 100_000,
    };
    let (tx, rx) = mpsc::channel();
    drop(rx);
    let result = csvx_core::groupby::groupby_quotes(
        &target,
        delimiter,
        &out,
        &options,
        csvx_core::csv_reader::QuoteStyle::Rfc4180,
        &tx,
    );
    drop(tx);
    if let Err(error) = result {
        let _ = std::fs::remove_file(&out);
        return Err(error.to_string());
    }
    let sheet =
        match csvx_core::sheet::Sheet::open(&out, csvx_core::sheet::ReadOptions { delimiter }) {
            Ok(sheet) => sheet,
            Err(error) => {
                let _ = std::fs::remove_file(&out);
                return Err(error.to_string());
            }
        };
    let columns = sheet.columns().map_err(|error| error.to_string())?;
    let window = sheet
        .window(0, limit.min(1_000) as u64)
        .map_err(|error| error.to_string())?;
    let rows = window.rows;
    drop(sheet);
    let _ = std::fs::remove_file(&out);
    drop(materialized);
    let label = if op == csvx_core::aggregate::AggOp::Count {
        key.to_string()
    } else {
        format!("{key} · {}({measure})", op.name())
    };
    Ok((label, columns, rows))
}

fn report_profiles(
    input: &Path,
    delimiter: csvx_core::types::Delimiter,
) -> Result<Vec<csvx_core::profile::ColumnProfile>, String> {
    // Report sections must see pending edits, like the rows section does.
    let mut materialized = None;
    let target = if csvx_core::sheet::has_edits(input) {
        let (tx, rx) = mpsc::channel();
        drop(rx);
        let value =
            csvx_core::sheet::materialize(input, &csvx_core::sheet::ReadOptions { delimiter }, &tx)
                .map_err(|error| error.to_string())?;
        let path = value.path().to_path_buf();
        materialized = Some(value);
        path
    } else {
        input.to_path_buf()
    };
    let (tx, rx) = mpsc::channel();
    drop(rx);
    let profiles = csvx_core::profile::profile_quotes(
        &target,
        delimiter,
        64,
        None,
        csvx_core::csv_reader::QuoteStyle::Rfc4180,
        &tx,
    )
    .map_err(|error| error.to_string())?;
    drop(materialized);
    Ok(profiles)
}

fn report_quality(
    input: &Path,
    delimiter: csvx_core::types::Delimiter,
    keys: &[String],
    columns: &[String],
) -> Result<Vec<(String, String, String)>, String> {
    let profiles = report_profiles(input, delimiter)?;
    let keys = keys
        .iter()
        .filter(|key| columns.iter().any(|column| column == *key))
        .cloned()
        .collect::<Vec<_>>();
    let (_checks, findings) = quality_checks(&profiles, &keys, &[]);
    Ok(findings)
}

/// A small deterministic report renderer: the spec is CSV rows, not a GUI.
///
/// The spec is intentionally dull: `section,arg` (summary/columns/rows), plus an
/// optional `title,...` row. Markdown is the default; `--format html` gives a
/// standalone file that can be opened locally.
pub(crate) fn cmd_report(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut spec: Option<PathBuf> = None;
    let mut spec_text: Option<String> = None;
    let mut report_root: Option<PathBuf> = None;
    let mut title = "CSV report".to_string();
    let mut out: Option<PathBuf> = None;
    let mut format = "md".to_string();
    let mut format_explicit = false;
    let mut json = false;
    let mut delim = "auto".to_string();
    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--spec" => match p.value("--spec") {
                Ok(v) => spec = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--spec-text" => match p.value("--spec-text") {
                Ok(v) => spec_text = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--root" => match p.value("--root") {
                Ok(v) => report_root = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--title" => match p.value("--title") {
                Ok(v) => title = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--format" => match p.value("--format") {
                Ok(v) => {
                    format = v;
                    format_explicit = true;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("report");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => input = Some(PathBuf::from(other)),
        }
    }
    let Some(input) = input else {
        eprintln!("error: report needs <input>");
        return 2;
    };
    if !format_explicit {
        if let Some(path) = &out {
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("");
            if extension.eq_ignore_ascii_case("html") {
                format = "html".to_string();
            } else if extension.eq_ignore_ascii_case("xlsx") {
                format = "xlsx".to_string();
            }
        }
    }
    if !matches!(format.as_str(), "md" | "html" | "xlsx") {
        eprintln!("error: --format must be md, html or xlsx");
        return 2;
    }
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let mut sections: Vec<(String, String)> = vec![
        ("summary".to_string(), String::new()),
        ("columns".to_string(), String::new()),
        ("rows".to_string(), "20".to_string()),
    ];
    if spec.is_some() && spec_text.is_some() {
        eprintln!("error: use either --spec or --spec-text, not both");
        return 2;
    }
    let spec_source = if let Some(text) = spec_text {
        Some(text)
    } else if let Some(path) = &spec {
        match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!("error: cannot read spec {}: {e}", path.display());
                return 2;
            }
        }
    } else {
        None
    };
    if let Some(text) = spec_source {
        let mut parsed = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let cells = parse_csv_record(line, b',');
            let Some(kind) = cells.first().map(|cell| cell.trim().to_ascii_lowercase()) else {
                continue;
            };
            if kind.is_empty() {
                continue;
            }
            let arg = cells
                .get(1)
                .map(|cell| cell.trim().to_string())
                .unwrap_or_default();
            if kind == "title" {
                if !arg.is_empty() {
                    title = arg;
                }
                continue;
            }
            if !matches!(
                kind.as_str(),
                "summary"
                    | "columns"
                    | "rows"
                    | "quality"
                    | "groups"
                    | "chart"
                    | "compare"
                    | "datecounts"
            ) {
                eprintln!("error: unknown report section {kind:?}");
                return 2;
            }
            parsed.push((kind, arg));
        }
        if parsed.is_empty() {
            eprintln!("error: the report spec has no sections");
            return 2;
        }
        sections = parsed;
    }

    let sheet =
        match csvx_core::sheet::Sheet::open(&input, csvx_core::sheet::ReadOptions { delimiter }) {
            Ok(sheet) => sheet,
            Err(e) => return report_failure("report", e.to_string()),
        };
    let rows = match sheet.rows() {
        Ok(rows) => rows,
        Err(e) => return report_failure("report", e.to_string()),
    };
    let columns = match sheet.columns() {
        Ok(columns) => columns,
        Err(e) => return report_failure("report", e.to_string()),
    };
    let schema = match sheet.schema(10) {
        Ok(schema) => schema,
        Err(e) => return report_failure("report", e.to_string()),
    };
    if format == "xlsx" {
        let Some(out) = out.as_ref() else {
            eprintln!("error: report --format xlsx needs -o/--out");
            return 2;
        };
        if sections
            .iter()
            .any(|(kind, _)| !matches!(kind.as_str(), "summary" | "columns" | "rows"))
        {
            eprintln!("error: xlsx report supports summary, columns and rows sections");
            return 2;
        }
        let mut sheets = Vec::new();
        for (kind, arg) in &sections {
            match kind.as_str() {
                "summary" => sheets.push(csvx_core::xlsx::Sheet {
                    name: "Summary".to_string(),
                    headers: vec!["metric".to_string(), "value".to_string()],
                    rows: vec![
                        vec!["title".to_string(), title.clone()],
                        vec!["source".to_string(), input.display().to_string()],
                        vec!["rows".to_string(), rows.to_string()],
                        vec!["columns".to_string(), columns.len().to_string()],
                    ],
                }),
                "columns" => sheets.push(csvx_core::xlsx::Sheet {
                    name: "Columns".to_string(),
                    headers: vec![
                        "#".to_string(),
                        "name".to_string(),
                        "kind".to_string(),
                        "empty".to_string(),
                        "sampled".to_string(),
                        "distinct".to_string(),
                    ],
                    rows: schema
                        .iter()
                        .map(|column| {
                            vec![
                                column.index.to_string(),
                                column.name.clone(),
                                column.kind.as_str().to_string(),
                                column.empty.to_string(),
                                column.sampled.to_string(),
                                column.distinct.to_string(),
                            ]
                        })
                        .collect(),
                }),
                "rows" => {
                    let limit: u64 = arg.parse().unwrap_or(20);
                    let window = match sheet.window(0, limit.min(1_000)) {
                        Ok(window) => window,
                        Err(e) => return report_failure("report", e.to_string()),
                    };
                    sheets.push(csvx_core::xlsx::Sheet {
                        name: "Rows".to_string(),
                        headers: window.columns.clone(),
                        rows: window.rows,
                    });
                }
                "datecounts" => {
                    let (headers, rows) = match date_counts(&sheet, &columns, arg) {
                        Ok(value) => value,
                        Err(e) => {
                            eprintln!("error: {e}");
                            return 2;
                        }
                    };
                    sheets.push(csvx_core::xlsx::Sheet {
                        name: "DateCounts".to_string(),
                        headers,
                        rows,
                    });
                }
                _ => unreachable!(),
            }
        }
        return match csvx_core::xlsx::write_sheets(out, &sheets) {
            Ok(()) => {
                if json {
                    println!(
                        r#"{{"ok":true,"out":"{}","sheets":{}}}"#,
                        serve::json_str(&out.display().to_string()),
                        sheets.len()
                    );
                } else {
                    println!("✓ {} sheet(s) → {}", sheets.len(), out.display());
                }
                0
            }
            Err(e) => report_failure("report", e.to_string()),
        };
    }
    let mut body = String::new();
    if format == "md" {
        body.push_str(&format!("# {title}\n\n"));
        body.push_str(&format!("source: `{}`\n\n", input.display()));
    } else {
        body.push_str("<h1>");
        body.push_str(&html_escape(&title));
        body.push_str("</h1>\n");
        body.push_str(&format!(
            "<p>source: <code>{}</code></p>\n",
            html_escape(&input.display().to_string())
        ));
    }
    for (kind, arg) in &sections {
        match kind.as_str() {
            "summary" => {
                if format == "md" {
                    body.push_str("## Summary\n\n");
                    body.push_str(&format!("- rows: {rows}\n- columns: {}\n\n", columns.len()));
                } else {
                    body.push_str(&format!(
                        "<h2>Summary</h2><ul><li>rows: {rows}</li><li>columns: {}</li></ul>\n",
                        columns.len()
                    ));
                }
            }
            "columns" => {
                if format == "md" {
                    body.push_str("## Columns\n\n| # | name | kind | empty | sampled | distinct |\n|---|---|---|---|---|---|\n");
                    for column in &schema {
                        body.push_str(&format!(
                            "| {} | {} | {} | {} | {} | {} |\n",
                            column.index,
                            md_cell(&column.name),
                            column.kind.as_str(),
                            column.empty,
                            column.sampled,
                            column.distinct
                        ));
                    }
                    body.push('\n');
                } else {
                    body.push_str("<h2>Columns</h2><table><tr><th>#</th><th>name</th><th>kind</th><th>empty</th><th>sampled</th><th>distinct</th></tr>\n");
                    for column in &schema {
                        body.push_str(&format!(
                            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                            column.index,
                            html_escape(&column.name),
                            column.kind.as_str(),
                            column.empty,
                            column.sampled,
                            column.distinct
                        ));
                    }
                    body.push_str("</table>\n");
                }
            }
            "rows" => {
                let limit: u64 = arg.parse().unwrap_or(20);
                let window = match sheet.window(0, limit.min(1_000)) {
                    Ok(window) => window,
                    Err(e) => return report_failure("report", e.to_string()),
                };
                if format == "md" {
                    body.push_str(&format!("## Rows (first {})\n\n", window.rows.len()));
                    body.push('|');
                    for column in &window.columns {
                        body.push_str(&format!(" {} |", md_cell(column)));
                    }
                    body.push('\n');
                    body.push('|');
                    for _ in &window.columns {
                        body.push_str(" --- |");
                    }
                    body.push('\n');
                    for row in &window.rows {
                        body.push('|');
                        for column in window.columns.iter().enumerate() {
                            let cell = row.get(column.0).cloned().unwrap_or_default();
                            body.push_str(&format!(" {} |", md_cell(&cell)));
                        }
                        body.push('\n');
                    }
                    body.push('\n');
                } else {
                    body.push_str("<h2>Rows</h2><table><tr>");
                    for column in &window.columns {
                        body.push_str(&format!("<th>{}</th>", html_escape(column)));
                    }
                    body.push_str("</tr>\n");
                    for row in &window.rows {
                        body.push_str("<tr>");
                        for cell in row {
                            body.push_str(&format!("<td>{}</td>", html_escape(cell)));
                        }
                        body.push_str("</tr>\n");
                    }
                    body.push_str("</table>\n");
                }
            }
            "datecounts" => {
                let (headers, rows) = match date_counts(&sheet, &columns, arg) {
                    Ok(value) => value,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                };
                if format == "md" {
                    body.push_str("## Date counts\n\n|");
                    for header in &headers {
                        body.push_str(&format!(" {} |", md_cell(header)));
                    }
                    body.push('\n');
                    body.push('|');
                    for _ in &headers {
                        body.push_str(" --- |");
                    }
                    body.push('\n');
                    for row in &rows {
                        body.push('|');
                        for cell in row {
                            body.push_str(&format!(" {} |", md_cell(cell)));
                        }
                        body.push('\n');
                    }
                    body.push('\n');
                } else {
                    body.push_str("<h2>Date counts</h2><table><tr>");
                    for header in &headers {
                        body.push_str(&format!("<th>{}</th>", html_escape(header)));
                    }
                    body.push_str("</tr>\n");
                    for row in &rows {
                        body.push_str("<tr>");
                        for cell in row {
                            body.push_str(&format!("<td>{}</td>", html_escape(cell)));
                        }
                        body.push_str("</tr>\n");
                    }
                    body.push_str("</table>\n");
                }
            }
            "quality" => {
                let keys: Vec<String> = arg
                    .split(',')
                    .map(|key| key.trim().to_string())
                    .filter(|key| !key.is_empty())
                    .collect();
                for key in &keys {
                    if !columns.iter().any(|column| column == key) {
                        eprintln!("error: quality names unknown key column {key:?}");
                        return 2;
                    }
                }
                let findings = match report_quality(&input, delimiter, &keys, &columns) {
                    Ok(findings) => findings,
                    Err(e) => return report_failure("report", e),
                };
                if format == "md" {
                    body.push_str("## Quality\n\n");
                    body.push_str("| severity | column | finding |\n|---|---|---|\n");
                    for (severity, column, message) in &findings {
                        body.push_str(&format!(
                            "| {} | {} | {} |\n",
                            severity,
                            md_cell(column),
                            md_cell(message)
                        ));
                    }
                    body.push('\n');
                } else {
                    body.push_str("<h2>Quality</h2><table><tr><th>severity</th><th>column</th><th>finding</th></tr>\n");
                    for (severity, column, message) in &findings {
                        body.push_str(&format!(
                            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                            html_escape(severity),
                            html_escape(column),
                            html_escape(message)
                        ));
                    }
                    body.push_str("</table>\n");
                }
            }
            "groups" => {
                let pieces: Vec<&str> = arg.split(':').map(str::trim).collect();
                let key = pieces.first().copied().unwrap_or_default();
                let (measure, op, limit) = match pieces.as_slice() {
                    [_key] => (
                        columns.first().cloned().unwrap_or_default(),
                        csvx_core::aggregate::AggOp::Count,
                        10usize,
                    ),
                    [key, second] => {
                        if let Ok(limit) = second.parse::<usize>() {
                            (
                                columns.first().cloned().unwrap_or_default(),
                                csvx_core::aggregate::AggOp::Count,
                                limit,
                            )
                        } else {
                            let _ = key;
                            eprintln!(
                                "error: groups needs <key>[:limit] or <key>:<measure>:<op>[:limit]"
                            );
                            return 2;
                        }
                    }
                    [key, measure, op] => {
                        let _ = key;
                        let op = match csvx_core::aggregate::AggOp::parse(op) {
                            Ok(op) => op,
                            Err(e) => {
                                eprintln!("error: {e}");
                                return 2;
                            }
                        };
                        ((*measure).to_string(), op, 10)
                    }
                    [key, measure, op, fourth] => {
                        let _ = key;
                        let op = match csvx_core::aggregate::AggOp::parse(op) {
                            Ok(op) => op,
                            Err(e) => {
                                eprintln!("error: {e}");
                                return 2;
                            }
                        };
                        let limit = match fourth.parse::<usize>() {
                            Ok(limit) if limit > 0 => limit,
                            _ => {
                                eprintln!("error: groups limit must be a positive integer");
                                return 2;
                            }
                        };
                        ((*measure).to_string(), op, limit)
                    }
                    _ => {
                        eprintln!(
                            "error: groups needs <key>[:limit] or <key>:<measure>:<op>[:limit]"
                        );
                        return 2;
                    }
                };
                if !columns.iter().any(|name| name == key) {
                    eprintln!("error: groups names unknown key column {key:?}");
                    return 2;
                }
                if !columns.iter().any(|name| name == &measure) {
                    eprintln!("error: groups names unknown measure column {measure:?}");
                    return 2;
                }
                let (label, header, rows) =
                    match report_group_table(&input, delimiter, key, &measure, op, limit) {
                        Ok(value) => value,
                        Err(e) => return report_failure("report", e),
                    };
                if format == "md" {
                    body.push_str(&format!("## Groups: {label}\n\n"));
                    body.push('|');
                    for column in &header {
                        body.push_str(&format!(" {} |", md_cell(column)));
                    }
                    body.push('\n');
                    body.push('|');
                    for _ in &header {
                        body.push_str(" --- |");
                    }
                    body.push('\n');
                    for row in &rows {
                        body.push('|');
                        for cell in row {
                            body.push_str(&format!(" {} |", md_cell(cell)));
                        }
                        body.push('\n');
                    }
                    body.push('\n');
                } else {
                    body.push_str(&format!(
                        "<h2>Groups: {}</h2><table><tr>",
                        html_escape(&label)
                    ));
                    for column in &header {
                        body.push_str(&format!("<th>{}</th>", html_escape(column)));
                    }
                    body.push_str("</tr>\n");
                    for row in &rows {
                        body.push_str("<tr>");
                        for cell in row {
                            body.push_str(&format!("<td>{}</td>", html_escape(cell)));
                        }
                        body.push_str("</tr>\n");
                    }
                    body.push_str("</table>\n");
                }
            }
            "chart" => {
                let pieces: Vec<&str> = arg.split(':').map(str::trim).collect();
                if pieces.is_empty() || pieces.len() > 3 {
                    eprintln!("error: chart needs <x>[:<measure>[:limit]]");
                    return 2;
                }
                let x = pieces[0];
                let (measure, op) = match pieces.as_slice() {
                    [_] => (
                        columns.first().cloned().unwrap_or_default(),
                        csvx_core::aggregate::AggOp::Count,
                    ),
                    [_, measure] => ((*measure).to_string(), csvx_core::aggregate::AggOp::Sum),
                    [_, measure, limit] => {
                        if limit.parse::<usize>().ok().is_none_or(|value| value == 0) {
                            eprintln!("error: chart limit must be positive");
                            return 2;
                        }
                        ((*measure).to_string(), csvx_core::aggregate::AggOp::Sum)
                    }
                    _ => unreachable!(),
                };
                let limit: usize = pieces
                    .get(2)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(10);
                if !columns.iter().any(|name| name == x) {
                    eprintln!("error: chart names unknown x column {x:?}");
                    return 2;
                }
                if op != csvx_core::aggregate::AggOp::Count
                    && !columns.iter().any(|name| name == &measure)
                {
                    eprintln!("error: chart names unknown measure column {measure:?}");
                    return 2;
                }
                let (label, header, rows) =
                    match report_group_table(&input, delimiter, x, &measure, op, limit) {
                        Ok(value) => value,
                        Err(e) => return report_failure("report", e),
                    };
                if format == "md" {
                    body.push_str(&format!("## Chart: {label}\n\n"));
                    body.push('|');
                    for column in &header {
                        body.push_str(&format!(" {} |", md_cell(column)));
                    }
                    body.push('\n');
                    body.push('|');
                    for _ in &header {
                        body.push_str(" --- |");
                    }
                    body.push('\n');
                    for row in &rows {
                        body.push('|');
                        for cell in row {
                            body.push_str(&format!(" {} |", md_cell(cell)));
                        }
                        body.push('\n');
                    }
                    body.push('\n');
                } else {
                    let max = rows
                        .iter()
                        .filter_map(|row| row.last().and_then(|value| value.parse::<f64>().ok()))
                        .fold(0.0, f64::max);
                    let height = (rows.len() as u64 * 26).saturating_add(12);
                    body.push_str(&format!("<h2>Chart: {}</h2>\n", html_escape(&label)));
                    body.push_str(&format!(
                        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"640\" height=\"{height}\" viewBox=\"0 0 640 {height}\" role=\"img\" aria-label=\"{}\" style=\"max-width:100%\">\n",
                        html_escape(&label)
                    ));
                    for (index, row) in rows.iter().enumerate() {
                        let y = 6 + index as u64 * 26;
                        let value = row
                            .last()
                            .and_then(|value| value.parse::<f64>().ok())
                            .unwrap_or(0.0);
                        let width = if max > 0.0 {
                            ((value / max) * 300.0).round() as u64
                        } else {
                            0
                        };
                        let name = row.first().cloned().unwrap_or_default();
                        body.push_str(&format!(
                            "<text x=\"180\" y=\"{}\">{}</text><rect x=\"196\" y=\"{}\" width=\"{width}\" height=\"16\" fill=\"#4a90d9\"></rect><text x=\"{}\">{}</text>\n",
                            y + 13,
                            html_escape(&name),
                            y,
                            196 + width.saturating_add(6),
                            html_escape(row.last().map(String::as_str).unwrap_or(""))
                        ));
                    }
                    body.push_str("</svg>\n");
                }
            }
            "compare" => {
                let other_arg = arg.trim();
                if other_arg.is_empty() {
                    eprintln!("error: compare needs <other.csv>");
                    return 2;
                }
                let other = if Path::new(other_arg).is_absolute() {
                    std::path::PathBuf::from(other_arg)
                } else {
                    input
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(other_arg)
                };
                if let Some(root) = &report_root {
                    let canonical_root = match root.canonicalize() {
                        Ok(root) => root,
                        Err(e) => {
                            eprintln!("error: cannot resolve --root {}: {e}", root.display());
                            return 2;
                        }
                    };
                    let canonical_other = match other.canonicalize() {
                        Ok(path) if path.starts_with(&canonical_root) => path,
                        _ => {
                            eprintln!("error: compare path {other_arg} is outside --root");
                            return 2;
                        }
                    };
                    if !canonical_other.is_file() {
                        eprintln!("error: compare path {other_arg} is not a file");
                        return 2;
                    }
                }
                let base = match csvx_core::sheet::Sheet::open(
                    &input,
                    csvx_core::sheet::ReadOptions { delimiter },
                ) {
                    Ok(sheet) => sheet,
                    Err(e) => return report_failure("report", e),
                };
                let base_rows = base.rows().unwrap_or(0);
                let base_columns = match base.columns() {
                    Ok(columns) => columns,
                    Err(e) => return report_failure("report", e),
                };
                let viewer = match csvx_core::viewer::WindowViewer::open(&other, delimiter) {
                    Ok(viewer) => viewer,
                    Err(e) => return report_failure("report", e),
                };
                let other_columns = match viewer.headers() {
                    Ok(columns) => columns,
                    Err(e) => return report_failure("report", e),
                };
                let other_rows = viewer.total_rows();
                let diff = csvx_core::schema::diff(&base_columns, &other_columns);
                let columns_delta = base_columns.len() as i64 - other_columns.len() as i64;
                let rows_delta = base_rows as i64 - other_rows as i64;
                let list = |items: &[String]| {
                    if items.is_empty() {
                        "-".to_string()
                    } else {
                        items.join(", ")
                    }
                };
                let moved = diff
                    .moved
                    .iter()
                    .map(|item| format!("{} ({}→{})", item.name, item.here, item.there))
                    .collect::<Vec<_>>();
                let base_bytes = std::fs::metadata(&input).map(|m| m.len()).unwrap_or(0);
                let other_bytes = std::fs::metadata(&other).map(|m| m.len()).unwrap_or(0);
                let table = vec![
                    vec![
                        "rows".to_string(),
                        base_rows.to_string(),
                        other_rows.to_string(),
                        format!("{rows_delta:+}"),
                    ],
                    vec![
                        "columns".to_string(),
                        base_columns.len().to_string(),
                        other_columns.len().to_string(),
                        format!("{columns_delta:+}"),
                    ],
                    vec![
                        "added".to_string(),
                        list(&diff.added),
                        "-".to_string(),
                        "-".to_string(),
                    ],
                    vec![
                        "removed".to_string(),
                        list(&diff.removed),
                        "-".to_string(),
                        "-".to_string(),
                    ],
                    vec![
                        "moved".to_string(),
                        list(&moved),
                        "-".to_string(),
                        "-".to_string(),
                    ],
                    vec![
                        "bytes".to_string(),
                        base_bytes.to_string(),
                        other_bytes.to_string(),
                        format!("{:+}", base_bytes as i64 - other_bytes as i64),
                    ],
                ];
                let title = format!("Compare: {}", other_arg);
                if format == "md" {
                    body.push_str(&format!("## {title}\n\n"));
                    body.push_str("| property | base | other | delta |\n|---|---|---|---|\n");
                    for row in &table {
                        body.push_str(&format!(
                            "| {} | {} | {} | {} |\n",
                            md_cell(&row[0]),
                            md_cell(&row[1]),
                            md_cell(&row[2]),
                            md_cell(&row[3])
                        ));
                    }
                    body.push('\n');
                } else {
                    body.push_str(&format!("<h2>{}</h2>\n<table><tr><th>property</th><th>base</th><th>other</th><th>delta</th></tr>\n", html_escape(&title)));
                    for row in &table {
                        body.push_str("<tr>");
                        for cell in row {
                            body.push_str(&format!("<td>{}</td>", html_escape(cell)));
                        }
                        body.push_str("</tr>\n");
                    }
                    body.push_str("</table>\n");
                }
            }
            _ => {}
        }
    }
    if format == "html" {
        body = format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>body{{font-family:system-ui,sans-serif;margin:2rem}}table{{border-collapse:collapse}}td,th{{border:1px solid #ccc;padding:4px 8px}}code{{background:#f4f4f4}}</style></head><body>{}</body></html>\n",
            html_escape(&title),
            body
        );
    }
    if let Some(path) = &out {
        if let Err(e) = std::fs::write(path, &body) {
            eprintln!("error: cannot write {}: {e}", path.display());
            return 2;
        }
        if json {
            let out_json = format!("\"{}\"", json_escape(&path.to_string_lossy()));
            let format_json = format!("\"{}\"", json_escape(&format));
            println!(
                r#"{{"report":true,"out":{},"format":{}}}"#,
                out_json, format_json
            );
        } else {
            println!(
                "✓ report written to {} ({} bytes)",
                path.display(),
                body.len()
            );
        }
    } else {
        print!("{body}");
    }
    0
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn md_cell(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ")
}
