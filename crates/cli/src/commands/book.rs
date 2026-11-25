use crate::{human_bytes, print_command_help, report_failure, spawn_reporter, Parser};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_book(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    let mut plan: Option<PathBuf> = None;
    let mut json = false;
    let mut max_sheets = csvx_core::book::DEFAULT_MAX_SHEETS;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--root" => match p.value("--root") {
                Ok(v) => root = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--max" => match p
                .value("--max")
                .and_then(|v| v.parse().map_err(|_| "bad --max".to_string()))
            {
                Ok(v) => max_sheets = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--plan" => match p.value("--plan") {
                Ok(v) => plan = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "-h" | "--help" => {
                print_command_help("book");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => {
                if root.is_none() {
                    root = Some(PathBuf::from(other));
                } else {
                    eprintln!("error: unexpected argument: {other}");
                    return 2;
                }
            }
        }
    }

    if let Some(plan) = plan {
        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("book", rx, true);
        let result = csvx_core::book::apply_manifest(&plan, root.as_deref(), &tx);
        drop(tx);
        let _ = reporter.join();
        return match result {
            Ok(outputs) => {
                if json {
                    let items: Vec<String> = outputs
                        .iter()
                        .map(|output| {
                            format!(
                                r#"{{"name":"{}","path":"{}","rows":{},"bytes":{}}}"#,
                                output.name,
                                output.path.display(),
                                output.rows,
                                output.bytes
                            )
                        })
                        .collect();
                    println!(r#"{{"outputs":[{}]}}"#, items.join(","));
                } else {
                    for output in &outputs {
                        println!(
                            "✓ {} → {} ({} rows, {})",
                            output.name,
                            output.path.display(),
                            output.rows,
                            human_bytes(output.bytes)
                        );
                    }
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        };
    }
    let Some(root) = root else {
        eprintln!("error: book needs a directory (--root <dir>) or a manifest (--plan <file>)");
        return 2;
    };
    let book = match csvx_core::book::scan(&root, max_sheets) {
        Ok(b) => b,
        Err(e) => return report_failure("book", format!("cannot read {}: {e}", root.display())),
    };

    if json {
        let sheets: Vec<String> = book.sheets.iter().map(|s| s.to_json()).collect();
        println!(
            r#"{{"root":"{}","sheets":[{}]}}"#,
            book.root.display(),
            sheets.join(",")
        );
        return 0;
    }

    println!("{}  ({} sheets)", book.root.display(), book.sheets.len());
    println!(
        "  {:<28} {:>9} {:>9} {:>6} {:>6}  state",
        "sheet", "bytes", "rows", "cols", "edits"
    );
    for sheet in &book.sheets {
        let rows = match sheet.rows {
            Some(n) => n.to_string(),
            None => "-".to_string(),
        };
        let mut state = Vec::new();
        if sheet.indexed {
            state.push("indexed");
        }
        if sheet.styled {
            state.push("styled");
        }
        if sheet.edits > 0 {
            state.push("edited");
        }
        println!(
            "  {:<28} {:>9} {:>9} {:>6} {:>6}  {}",
            sheet.name,
            human_bytes(sheet.bytes),
            rows,
            sheet.columns.map(|c| c.to_string()).unwrap_or_default(),
            if sheet.edits == 0 {
                "-".to_string()
            } else {
                sheet.edits.to_string()
            },
            state.join(" ")
        );
    }
    for (path, reason) in &book.unreadable {
        eprintln!("  ! {} — {reason}", path.display());
    }
    0
}
