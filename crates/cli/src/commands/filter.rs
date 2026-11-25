use crate::{
    check_output, materialize_compressed, parse_delimiter, parse_sort_columns, print_command_help,
    report_failure, self_check, spawn_reporter, warn_pending_edits, Parser,
};
use csvx_core::filter::{self, FilterSpec};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_filter(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut order_by: Option<String> = None;
    let mut order_descending = false;
    let mut input: Option<PathBuf> = None;
    let mut where_exprs: Vec<String> = Vec::new();
    let mut combine = filter::Combine::All;
    let mut out: Option<PathBuf> = None;
    let mut verify = false;
    let mut limit = 10_000usize;
    let mut json = false;
    let mut materialize = false;
    let mut show = 0usize;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--materialize" => materialize = true,
            "--where" => match p.value("--where") {
                Ok(v) => where_exprs.push(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--any" => combine = filter::Combine::Any,
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--order" => match p.value("--order") {
                Ok(v) => order_by = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--desc" => order_descending = true,
            "--verify" => verify = true,
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
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
            "--show" => match p
                .value("--show")
                .and_then(|v| v.parse().map_err(|_| "bad --show".to_string()))
            {
                Ok(v) => show = v,
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
                print_command_help("filter");
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
        eprintln!("error: filter needs <input>");
        return 2;
    };
    warn_pending_edits(&input);
    let _materialized = if materialize {
        match materialize_compressed(&input) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        }
    } else {
        None
    };
    let input = _materialized
        .as_ref()
        .map(|value| value.path.clone())
        .unwrap_or(input);
    if where_exprs.is_empty() {
        eprintln!("error: filter needs at least one --where <expr>");
        return 2;
    }
    let mut specs = Vec::with_capacity(where_exprs.len());
    for expr in &where_exprs {
        match FilterSpec::parse(expr) {
            Ok(s) => specs.push(s),
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        }
    }
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let quotes = match csvx_core::csv_reader::QuoteStyle::parse(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let set = filter::FilterSet { specs, combine };

    // `--out` is the "keep the result" path: stream matches to a file.
    if let Some(out) = out {
        // Ordering needs the whole match set before the first row can be written,
        // so matches go to a temp file and are sorted into place. Filtering stays
        // streaming; only the matched rows touch disk twice.
        let sort_columns = match &order_by {
            None => None,
            Some(spec) => match parse_sort_columns(spec, order_descending) {
                Ok(columns) => Some(columns),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
        };
        let stage = sort_columns.as_ref().map(|_| {
            std::env::temp_dir().join(format!(
                "csvs_filter_{}_{}.csv",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ))
        });
        let target = stage.clone().unwrap_or_else(|| out.clone());

        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("filter", rx, true);
        if let Err(code) = check_output(Some(&input), &target) {
            return code;
        }
        let result = filter::extract_style(&input, delimiter, &set, &target, quotes, &tx);
        drop(tx);
        let _ = reporter.join();
        let sorted = match (result, &sort_columns, &stage) {
            (Ok(summary), Some(columns), Some(stage)) => {
                let (tx, _rx) = mpsc::channel();
                match csvx_core::sort::external_sort_columns(
                    stage, &out, columns, delimiter, 64, &tx,
                ) {
                    Ok(()) => {
                        std::fs::remove_file(stage).ok();
                        Ok(summary)
                    }
                    Err(e) => {
                        std::fs::remove_file(stage).ok();
                        Err(e)
                    }
                }
            }
            (Ok(summary), _, _) => Ok(summary),
            (Err(e), _, _) => Err(e),
        };
        return match sorted {
            Ok(s) => {
                println!(
                    "✓ {} of {} rows matched → {} ({} bytes)",
                    s.matched,
                    s.scanned,
                    out.display(),
                    s.bytes
                );
                if verify {
                    // The claim: exactly the matched rows, same columns, and
                    // no raggedness beyond what the source already had.
                    let ok = self_check(
                        &out,
                        delimiter,
                        csvx_core::check::Expectation::rows(s.matched)
                            .with_ragged(s.matched_ragged),
                        false,
                    );
                    if !ok {
                        return 1;
                    }
                }
                0
            }
            Err(e) => report_failure("filter", e),
        };
    }

    let spec = set.specs.first().cloned();
    let Some(spec) = spec else { return 2 };
    let request = csvx_core::ops::FilterRequest {
        path: input.clone(),
        delimiter,
        spec,
        limit,
        budget: None,
    };
    let result = match request.execute_fast() {
        Ok(r) => r,
        Err(e) => {
            return report_failure("filter", e);
        }
    };

    if json {
        let rows: Vec<String> = result.rows.iter().map(|n| n.to_string()).collect();
        println!(
            r#"{{"scanned":{},"matched":{},"truncated":{},"rows":[{}]}}"#,
            result.scanned,
            result.matched,
            result.truncated,
            rows.join(",")
        );
        return 0;
    }

    println!(
        "scanned {} rows, matched {} ({} shown){}",
        result.scanned,
        result.matched,
        result.rows.len(),
        if result.truncated { ", truncated" } else { "" }
    );
    let shown: Vec<String> = result.rows.iter().take(50).map(|n| n.to_string()).collect();
    println!("rows (1-based, header excluded): {}", shown.join(", "));
    if result.matched > 50 {
        println!(
            "... {} more; use --json for the full list",
            result.matched - 50
        );
    }
    if show > 0 {
        let picked: Vec<u64> = result
            .rows
            .iter()
            .take(show)
            .map(|row| row.saturating_sub(1))
            .collect();
        match csvx_core::filter::read_rows(&input, delimiter, &picked) {
            Ok(rows) => {
                println!();
                for r in rows {
                    println!("{:>9}  {}", r.row + 1, r.cells.join(" | "));
                }
            }
            Err(e) => {
                let _ = report_failure("filter", format!("reading matched rows: {e}"));
                return 1;
            }
        }
    }
    0
}
