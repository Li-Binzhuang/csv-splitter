use crate::{
    materialize_compressed, parse_delimiter, parse_quotes, print_command_help, report_failure,
    Parser,
};
use csvx_core::search::SearchSpec;
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_search(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut query: Option<String> = None;
    let mut columns: Vec<String> = Vec::new();
    let mut case_sensitive = false;
    let mut regex = false;
    let mut exact = false;
    let mut limit = 1_000usize;
    let mut show = 0usize;
    let mut json = false;
    let mut materialize = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--materialize" => materialize = true,
            "--columns" => match p.value("--columns") {
                Ok(v) => columns = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--regex" => regex = true,
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--case" => case_sensitive = true,
            "--exact" => exact = true,
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
            "--json" => json = true,
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("search");
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
        eprintln!("error: search needs <input> and <text>");
        return 2;
    };
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
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let mut spec = SearchSpec::new(query);
    spec.columns = columns;
    spec.case_sensitive = case_sensitive;
    spec.whole_cell = exact;
    spec.regex = regex;

    let (tx, _rx) = mpsc::channel();
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let result =
        match csvx_core::search::search_fast(&input, delimiter, &spec, limit, None, quotes, &tx) {
            Ok(r) => r,
            Err(e) => {
                return report_failure("search", e);
            }
        };

    if json {
        let rows: Vec<String> = result.row_numbers().iter().map(|n| n.to_string()).collect();
        println!(
            r#"{{"scanned":{},"matched":{},"truncated":{},"columns":["{}"],"rows":[{}]}}"#,
            result.scanned,
            result.matched,
            result.truncated,
            result.columns.join(r#"",""#),
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
    let shown: Vec<String> = result
        .row_numbers()
        .iter()
        .take(50)
        .map(|n| n.to_string())
        .collect();
    println!("rows (1-based, header excluded): {}", shown.join(", "));

    if show > 0 {
        let picked = &result.rows[..result.rows.len().min(show)];
        match csvx_core::filter::read_rows(&input, delimiter, picked) {
            Ok(rows) => {
                println!();
                for r in rows {
                    println!("{:>9}  {}", r.row + 1, r.cells.join(" | "));
                }
            }
            Err(e) => {
                return report_failure("search", format!("reading matched rows: {e}"));
            }
        }
    }
    0
}
