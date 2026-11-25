use crate::{
    check_output, human_bytes, parse_delimiter, parse_quotes, print_command_help, report_failure,
    self_check, spawn_reporter, Parser,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_columns(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut keep: Vec<String> = Vec::new();
    let mut drop_cols: Vec<String> = Vec::new();
    let mut rename: Vec<(String, String)> = Vec::new();
    let mut json = false;
    let mut verify = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--verify" => verify = true,
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--keep" => match p.value("--keep") {
                Ok(v) => keep = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--drop" => match p.value("--drop") {
                Ok(v) => drop_cols = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--rename" => match p
                .value("--rename")
                .and_then(|v| csvx_core::reshape::ReshapeSpec::parse_renames(&v))
            {
                Ok(v) => rename = v,
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
            "--json" => json = true,
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("columns");
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
        eprintln!("error: columns needs <input>");
        return 2;
    };
    let spec = csvx_core::reshape::ReshapeSpec {
        keep,
        drop: drop_cols,
        rename,
    };
    if spec.is_empty() {
        eprintln!("error: give at least one of --keep, --drop, --rename");
        return 2;
    }
    let output = out.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "columns".to_string());
        input.with_file_name(format!("{stem}.reshaped.csv"))
    });
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    if let Err(code) = check_output(Some(&input), &output) {
        return code;
    }
    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("columns", rx, json);
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let result = csvx_core::reshape::reshape_quotes(&input, delimiter, &spec, &output, quotes, &tx);
    drop(tx);
    let _ = reporter.join();
    match result {
        Ok(s) => {
            if json {
                println!(
                    r#"{{"rows":{},"bytes":{},"columns":["{}"],"dropped":["{}"]}}"#,
                    s.rows,
                    s.bytes,
                    s.columns.join(r#"",""#),
                    s.dropped.join(r#"",""#)
                );
            } else {
                println!(
                    "✓ {} rows × {} columns → {} ({})",
                    s.rows,
                    s.columns.len(),
                    output.display(),
                    human_bytes(s.bytes)
                );
                println!("    columns: {}", s.columns.join(", "));
                if !s.dropped.is_empty() {
                    println!("    dropped: {}", s.dropped.join(", "));
                }
            }
            if verify {
                // The claim: every row kept, exactly the surviving columns.
                let ok = self_check(
                    &output,
                    delimiter,
                    csvx_core::check::Expectation::rows(s.rows).with_columns(s.columns.len()),
                    json,
                );
                if !ok {
                    return 1;
                }
            }
            0
        }
        Err(e) => report_failure("lookup", e), // engine returned the error itself
    }
}
