use crate::{
    check_output, human_bytes, parse_delimiter, parse_quotes, print_command_help, print_table,
    report_failure, sheet_path, spawn_reporter, warn_pending_edits_unless, Parser,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_pivot(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut apply_edits = false;
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut rows: Vec<String> = Vec::new();
    let mut columns: Option<String> = None;
    let mut values = String::new();
    let mut order: Option<String> = None;
    let mut descending = false;
    let mut qualify = false;
    let mut max_columns = csvx_core::pivot::DEFAULT_MAX_COLUMNS;
    let mut max_cells = csvx_core::pivot::DEFAULT_MAX_CELLS;
    let mut engine = csvx_core::engine::Engine::Auto;
    let mut json = false;
    let mut delim = "auto".to_string();
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
            "--rows" => match p.value("--rows") {
                Ok(v) => rows = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--cols" | "--columns" => match p.value("--cols") {
                Ok(v) => columns = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--values" => match p.value("--values") {
                Ok(v) => values = v,
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
            "--qualify" => qualify = true,
            "--max-columns" => match p
                .value("--max-columns")
                .and_then(|v| v.parse().map_err(|_| "bad --max-columns".to_string()))
            {
                Ok(v) => max_columns = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--max-cells" => match p
                .value("--max-cells")
                .and_then(|v| v.parse().map_err(|_| "bad --max-cells".to_string()))
            {
                Ok(v) => max_cells = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--apply-edits" => apply_edits = true,
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
                print_command_help("pivot");
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
        eprintln!("error: pivot needs an <input> file");
        return 2;
    };
    warn_pending_edits_unless(&input, apply_edits);
    if rows.is_empty() {
        eprintln!("error: pivot needs --rows <column[,column]>");
        return 2;
    }
    if values.trim().is_empty() {
        eprintln!("error: pivot needs --values <column:op[,...]>");
        return 2;
    }
    let specs = match csvx_core::aggregate::AggSpec::parse_list(&values) {
        Ok(v) => v,
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
    // Keep the requested path, if any: printing to the terminal is the default.
    let out_was_given: Option<PathBuf> = out;
    let options = csvx_core::pivot::PivotOptions {
        rows,
        columns,
        values: specs,
        order_by: order,
        descending,
        qualify_names: qualify,
        max_columns,
        max_cells,
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("pivot", rx, quiet || json);
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Some(out) = &out_was_given {
        if let Err(code) = check_output(Some(&input), out) {
            return code;
        }
    }
    let sheet = match sheet_path(&input, apply_edits, delimiter) {
        Ok(sheet) => sheet,
        Err(code) => return code,
    };
    // Without `-o` the matrix goes to the terminal: a pivot is something you look at,
    // and writing a file only to open it again is a step nobody asked for.
    let table = if let Some(output) = out_was_given.clone() {
        let planned = match csvx_core::engine::resolve(engine, sheet.path()) {
            Ok(e) => e,
            Err(message) => {
                eprintln!("error: {message}");
                return 2;
            }
        };
        let result = match planned {
            csvx_core::engine::Engine::Sql => {
                csvx_core::engine::pivot_sql(sheet.path(), delimiter, &output, &options, &tx)
            }
            _ => csvx_core::pivot::pivot_quotes(
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
            Ok(summary) => Some((summary, None)),
            // The engine returns the error itself; nothing else printed it.
            Err(e) => return report_failure("pivot", e),
        }
    } else {
        let result = csvx_core::pivot::pivot_rows(sheet.path(), delimiter, &options, quotes, &tx);
        drop(tx);
        let _ = reporter.join();
        match result {
            Ok(table) => Some((table.summary.clone(), Some(table))),
            Err(e) => return report_failure("pivot", e),
        }
    };

    let Some((summary, table)) = table else {
        return 1;
    };

    if json {
        println!(
            r#"{{"inputRows":{},"rowKeys":{},"columnKeys":{},"bytes":{},"rowColumns":["{}"],"columnValues":["{}"],"measures":["{}"]}}"#,
            summary.input_rows,
            summary.row_keys,
            summary.column_keys,
            summary.bytes,
            summary.row_columns.join(r#"",""#),
            summary.column_values.join(r#"",""#),
            summary.measures.join(r#"",""#)
        );
    } else if let Some(table) = table {
        print_table(&table.header, &table.rows);
        println!(
            "\n({} row(s) × {} column(s), {} input rows)",
            summary.row_keys, summary.column_keys, summary.input_rows
        );
    } else {
        println!(
            "✓ {} rows × {} columns → {} ({}, {} cells)",
            summary.row_keys,
            summary.column_values.len() * summary.measures.len(),
            out_was_given
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            human_bytes(summary.bytes),
            summary.row_keys * (summary.column_keys * summary.measures.len() as u64)
        );
    }
    0
}
