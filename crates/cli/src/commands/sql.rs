use crate::commands::pipeline::process_alive;
use crate::{fit_to_terminal, note_hidden_columns, print_command_help, report_failure, Parser};
use csvx_core::sql::{self, DEFAULT_LIMIT};
use std::path::PathBuf;
use std::time::Instant;

pub(crate) fn cmd_sql(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut query: Option<String> = None;
    let mut limit = DEFAULT_LIMIT;
    let mut json = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--limit" => match p
                .value("--limit")
                .and_then(|v| v.parse().map_err(|_| "bad --limit".to_string()))
            {
                Ok(v) => limit = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "-h" | "--help" => {
                print_command_help("sql");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => {
                if input.is_none() {
                    input = Some(PathBuf::from(other));
                } else if query.is_none() {
                    query = Some(other.to_string());
                } else {
                    eprintln!("error: unexpected argument: {other}");
                    return 2;
                }
            }
        }
    }

    let (Some(input), Some(query)) = (input, query) else {
        eprintln!("error: sql needs <input> and a query");
        return 2;
    };

    let started = Instant::now();
    let result = match sql::query(&input, &query, limit) {
        Ok(r) => r,
        Err(e) => {
            return report_failure("sql", e);
        }
    };

    if json {
        let rows: Vec<String> = result
            .rows
            .iter()
            .map(|row| {
                let cells: Vec<String> = row.iter().map(|c| json_quote(c)).collect();
                format!("[{}]", cells.join(","))
            })
            .collect();
        let cols: Vec<String> = result.columns.iter().map(|c| json_quote(c)).collect();
        println!(
            r#"{{"columns":[{}],"rows":[{}],"truncated":{}}}"#,
            cols.join(","),
            rows.join(","),
            result.truncated
        );
        return 0;
    }

    println!("{}", result.columns.join(" | "));
    println!("{}", "-".repeat(40));
    for row in &result.rows {
        println!("{}", row.join(" | "));
    }
    println!(
        "{} row(s){} in {:.2}s",
        result.rows.len(),
        if result.truncated { " (truncated)" } else { "" },
        started.elapsed().as_secs_f64()
    );
    0
}

pub(crate) fn json_quote(s: &str) -> String {
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

/// Width of a string as a terminal shows it: CJK and other wide characters take
/// two columns, combining marks take none. Padding with `chars().count()` would
/// misalign every table that has Chinese in it, which is most of them here.
pub(crate) fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| {
            let code = ch as u32;
            let wide = (0x1100..=0x115F).contains(&code)
                || (0x2E80..=0xA4CF).contains(&code)
                || (0xAC00..=0xD7A3).contains(&code)
                || (0xF900..=0xFAFF).contains(&code)
                || (0xFE30..=0xFE4F).contains(&code)
                || (0xFF00..=0xFF60).contains(&code)
                || (0xFFE0..=0xFFE6).contains(&code)
                || (0x1F300..=0x1F64F).contains(&code)
                || (0x20000..=0x3FFFD).contains(&code);
            if wide {
                2
            } else {
                1
            }
        })
        .sum()
}

/// Print rows as an aligned table, with padding measured in terminal columns.
pub(crate) fn print_table(columns: &[String], rows: &[Vec<String>]) {
    let (columns, rows, hidden) = fit_to_terminal(columns, rows);
    let columns = &columns;
    let rows = &rows;
    let mut widths: Vec<usize> = columns.iter().map(|c| display_width(c)).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(i) {
                *width = (*width).max(display_width(cell));
            }
        }
    }
    let render = |cells: &[String]| {
        let mut line = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                line.push_str(" | ");
            }
            line.push_str(cell);
            let pad = widths.get(i).copied().unwrap_or(0) - display_width(cell);
            if i + 1 < cells.len() {
                line.push_str(&" ".repeat(pad));
            }
        }
        line
    };
    println!("{}", render(columns));
    let total: usize = widths.iter().sum::<usize>() + 3 * widths.len().saturating_sub(1);
    println!("{}", "-".repeat(total.max(20)));
    for row in rows {
        println!("{}", render(row));
    }
    note_hidden_columns(hidden);
}

/// `view`: paged look at the edited sheet, optionally filtered.
///
/// Reads the file in windows instead of loading it, so the same command works on a
/// 10 GB file. Filtering streams: each window is tested and matches are printed until
/// the requested count is reached, which means a selective filter stays fast and an
/// unselective one stops early anyway.
pub(crate) struct MaterializedInput {
    pub(crate) path: std::path::PathBuf,
}

impl Drop for MaterializedInput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) fn cleanup_stale_materialized() {
    let dir = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("csvx-materialized-") else {
            continue;
        };
        let Some(pid) = rest
            .split('-')
            .next()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != std::process::id() && !process_alive(pid) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub(crate) fn materialize_compressed(
    input: &std::path::Path,
) -> std::io::Result<Option<MaterializedInput>> {
    if !csvx_core::encoding::is_compressed(input) {
        return Ok(None);
    }
    cleanup_stale_materialized();
    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    );
    let path = std::env::temp_dir().join(format!("csvx-materialized-{stamp}.csv"));
    let mut reader = csvx_core::encoding::open_decoded(input)?;
    let mut writer = std::fs::File::create(&path)?;
    std::io::copy(&mut reader, &mut writer)?;
    Ok(Some(MaterializedInput { path }))
}
