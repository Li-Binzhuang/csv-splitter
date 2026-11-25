use crate::{
    parse_delimiter, parse_quotes, print_chart, print_command_help, report_failure, sheet_path,
    spawn_reporter, warn_pending_edits_unless, Parser,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_chart(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut engine = csvx_core::engine::Engine::Auto;
    let mut apply_edits = false;
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut x: Option<String> = None;
    let mut series: Option<String> = None;
    let mut y = String::new();
    let mut kind = "line".to_string();
    let mut sort_by_value = false;
    let mut descending = false;
    let mut max_points = csvx_core::chart::DEFAULT_MAX_POINTS;
    let mut want_json = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--apply-edits" => apply_edits = true,
            "--json" => want_json = true,
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
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
            "--x" => match p.value("--x") {
                Ok(v) => x = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--series" | "--by" => match p.value("--series") {
                Ok(v) => series = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--y" => match p.value("--y") {
                Ok(v) => y = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--type" => match p.value("--type") {
                Ok(v) => kind = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--sort-by-value" => sort_by_value = true,
            "--desc" => descending = true,
            "--max-points" => match p
                .value("--max-points")
                .and_then(|v| v.parse().map_err(|_| "bad --max-points".to_string()))
            {
                Ok(v) => max_points = v,
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
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("chart");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => {
                if input.is_none() {
                    input = Some(PathBuf::from(other));
                } else {
                    eprintln!("error: unexpected argument: {other}");
                    return 2;
                }
            }
        }
    }

    let Some(input) = input else {
        eprintln!("error: chart needs an <input> file");
        return 2;
    };
    warn_pending_edits_unless(&input, apply_edits);
    let Some(x) = x else {
        eprintln!("error: chart needs --x <column>");
        return 2;
    };
    if y.trim().is_empty() {
        eprintln!("error: chart needs --y <column:op>");
        return 2;
    }
    let spec = match csvx_core::aggregate::AggSpec::parse_list(&y) {
        Ok(mut v) if !v.is_empty() => v.remove(0),
        Ok(_) => {
            eprintln!("error: --y needs one column:op measure");
            return 2;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let chart_kind = match csvx_core::chart::ChartKind::parse(&kind) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let options = csvx_core::chart::ChartOptions {
        x,
        series,
        y: spec,
        kind: chart_kind,
        sort_by_value,
        descending,
        max_points,
    };

    let (tx, rx) = mpsc::channel();
    // Writing JSON to stdout means stderr must stay quiet enough to pipe.
    let reporter = spawn_reporter("chart", rx, out.is_none());
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let sheet = match sheet_path(&input, apply_edits, delimiter) {
        Ok(sheet) => sheet,
        Err(code) => return code,
    };
    let planned = match csvx_core::engine::resolve(engine, sheet.path()) {
        Ok(e) => e,
        Err(message) => {
            eprintln!("error: {message}");
            return 2;
        }
    };
    let result = match planned {
        csvx_core::engine::Engine::Sql => {
            csvx_core::engine::chart_sql(sheet.path(), delimiter, &options, &tx)
        }
        _ => csvx_core::chart::chart_quotes(sheet.path(), delimiter, &options, quotes, &tx),
    };
    drop(tx);
    let _ = reporter.join();
    let data = match result {
        Ok(d) => d,
        Err(e) => return report_failure("chart", e),
    };

    let json = data.to_json();
    match out {
        Some(path) => match std::fs::write(&path, &json) {
            Ok(()) => {
                println!(
                    "✓ {} categories × {} series → {}",
                    data.categories.len(),
                    data.series.len(),
                    path.display()
                );
                0
            }
            Err(e) => report_failure("chart", format!("cannot write {}: {e}", path.display())),
        },
        None => {
            if want_json {
                println!("{json}");
            } else {
                print_chart(&data);
            }
            0
        }
    }
}
