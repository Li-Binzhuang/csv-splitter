use crate::{
    check_output, human_bytes, parse_delimiter, parse_quotes, print_command_help, report_failure,
    sheet_path, spawn_reporter, warn_pending_edits_unless, Parser,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_groupby(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut apply_edits = false;
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut keys: Vec<String> = Vec::new();
    let mut agg_spec = String::new();
    let mut order: Option<String> = None;
    let mut descending = false;
    let mut max_groups = csvx_core::groupby::DEFAULT_MAX_GROUPS;
    let mut json = false;
    let mut delim = "auto".to_string();
    let mut engine = csvx_core::engine::Engine::Auto;
    let mut quiet = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--by" => match p.value("--by") {
                Ok(v) => keys = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--engine" => match p.value("--engine") {
                Ok(v) => match csvx_core::engine::Engine::parse(&v) {
                    Ok(e) => engine = e,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--agg" => match p.value("--agg") {
                Ok(v) => agg_spec = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--order" => match p.value("--order") {
                Ok(v) => order = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--desc" => descending = true,
            "--asc" => descending = false,
            "--max-groups" => match p
                .value("--max-groups")
                .and_then(|v| v.parse().map_err(|_| "bad --max-groups".to_string()))
            {
                Ok(v) => max_groups = v,
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
            "--apply-edits" => apply_edits = true,
            "--json" => json = true,
            "-q" | "--quiet" => quiet = true,
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("groupby");
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
        eprintln!("error: groupby needs <input>");
        return 2;
    };
    warn_pending_edits_unless(&input, apply_edits);
    if keys.is_empty() {
        eprintln!("error: groupby needs --by <col[,col]>");
        return 2;
    }
    let aggs = match csvx_core::groupby::AggSpec::parse_list(&agg_spec) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let output = out.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "grouped".to_string());
        input.with_file_name(format!("{stem}.grouped.csv"))
    });
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("groupby", rx, quiet || json);
    let options = csvx_core::groupby::GroupOptions {
        keys,
        aggregates: aggs,
        order_by: order,
        descending,
        max_groups,
    };
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Err(code) = check_output(Some(&input), &output) {
        return code;
    }
    let sheet = match sheet_path(&input, apply_edits, delimiter) {
        Ok(sheet) => sheet,
        Err(code) => return code,
    };
    // `auto` picks DuckDB for large inputs, native for small ones. Below the threshold
    // the engine's start-up costs more than the query itself: 0.207 s vs 0.026 s on the
    // 20 120-row demo, and 0.9 s vs 13.1 s on the 2.5 GB fixture.
    let planned = match csvx_core::engine::resolve(engine, sheet.path()) {
        Ok(e) => e,
        Err(message) => {
            eprintln!("error: {message}");
            return 2;
        }
    };
    let result = match planned {
        csvx_core::engine::Engine::Sql => {
            csvx_core::engine::groupby_sql(sheet.path(), delimiter, &output, &options, &tx)
        }
        _ => csvx_core::groupby::groupby_quotes(
            sheet.path(),
            delimiter,
            &output,
            &options,
            quotes,
            &tx,
        ),
    };
    drop(tx);
    let _ = reporter.join();
    match result {
        Ok(s) => {
            if json {
                println!(
                    r#"{{"rows":{},"groups":{},"bytes":{},"keyColumns":["{}"],"aggregates":["{}"]}}"#,
                    s.rows,
                    s.groups,
                    s.bytes,
                    s.key_columns.join(r#"",""#),
                    s.aggregates.join(r#"",""#)
                );
            } else {
                println!(
                    "✓ {} rows → {} groups → {} ({})",
                    s.rows,
                    s.groups,
                    output.display(),
                    human_bytes(s.bytes)
                );
            }
            0
        }
        Err(e) => report_failure("groupby", e), // engine returned the error itself
    }
}
