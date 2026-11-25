use crate::{
    check_output, human_bytes, parse_delimiter, parse_quotes, print_command_help, report_failure,
    spawn_reporter, warn_pending_edits, Parser,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_lookup(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut key: Option<String> = None;
    let mut reference: Option<PathBuf> = None;
    let mut reference_key = String::new();
    let mut take = String::new();
    let mut max_keys = csvx_core::lookup::DEFAULT_MAX_KEYS;
    let mut engine = csvx_core::engine::Engine::Auto;
    let mut json = false;
    let mut quiet = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
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
            "--key" => match p.value("--key") {
                Ok(v) => key = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--from" | "--reference" => match p.value("--from") {
                Ok(v) => reference = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--from-key" => match p.value("--from-key") {
                Ok(v) => reference_key = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--get" => match p.value("--get") {
                Ok(v) => take = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--max-keys" => match p
                .value("--max-keys")
                .and_then(|v| v.parse().map_err(|_| "bad --max-keys".to_string()))
            {
                Ok(v) => max_keys = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "--quiet" | "-q" => quiet = true,
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
                print_command_help("lookup");
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
        eprintln!("error: lookup needs an <input> file");
        return 2;
    };
    let Some(key) = key else {
        eprintln!("error: lookup needs --key <column>");
        return 2;
    };
    let Some(reference) = reference else {
        eprintln!("error: lookup needs --from <reference.csv>");
        return 2;
    };
    if reference_key.trim().is_empty() {
        // Default: the reference key has the same name as the main key. That is
        // the common case, and demanding it explicitly is pointless ceremony.
        reference_key = key.clone();
    }
    if take.trim().is_empty() {
        eprintln!("error: lookup needs --get <columns from the reference>");
        return 2;
    }
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let output = out.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "lookup".to_string());
        input.with_file_name(format!("{stem}.lookup.csv"))
    });
    warn_pending_edits(&input);

    let options = csvx_core::lookup::LookupOptions {
        key,
        reference,
        reference_key,
        take: take.split(',').map(|s| s.trim().to_string()).collect(),
        max_keys,
    };
    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("lookup", rx, quiet || json);
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
    let result = {
        let planned = match csvx_core::engine::resolve(engine, &input) {
            Ok(e) => e,
            Err(message) => {
                eprintln!("error: {message}");
                return 2;
            }
        };
        match planned {
            csvx_core::engine::Engine::Sql => {
                csvx_core::engine::lookup_sql(&input, delimiter, &output, &options, &tx)
            }
            _ => {
                csvx_core::lookup::lookup_quotes(&input, delimiter, &output, &options, quotes, &tx)
            }
        }
    };
    drop(tx);
    let _ = reporter.join();
    let summary = match result {
        Ok(s) => s,
        Err(e) => return report_failure("lookup", e),
    };

    if json {
        println!(
            r#"{{"mainRows":{},"matched":{},"missing":{},"referenceKeys":{},"bytes":{},"columns":["{}"]}}"#,
            summary.main_rows,
            summary.matched,
            summary.missing,
            summary.reference_keys,
            summary.bytes,
            summary.columns.join(r#"",""#)
        );
    } else {
        println!(
            "✓ {} of {} rows matched ({} missing) → {} ({})",
            summary.matched,
            summary.main_rows,
            summary.missing,
            output.display(),
            human_bytes(summary.bytes)
        );
    }
    0
}

/// Columns as the sheet presents them (journal applied), for layout commands.
pub(crate) fn layout_columns(input: &std::path::Path) -> Result<Vec<String>, String> {
    let base_columns = csvx_core::workbook::base_header(input, csvx_core::types::Delimiter::Comma)
        .map_err(|e| format!("cannot read the header: {e}"))?;
    csvx_core::workbook::Journal::load(&csvx_core::workbook::Journal::sidecar(input))
        .map(|journal| {
            csvx_core::workbook::column_layout(journal.ops(), &base_columns)
                .map(|layout| layout.into_iter().map(|(name, _)| name).collect())
        })
        .unwrap_or_else(|_| Ok(base_columns))
        .map_err(|e: std::io::Error| e.to_string())
}
