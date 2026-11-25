use crate::{
    check_output, human_bytes, parse_delimiter, print_command_help, report_failure, spawn_reporter,
    Parser,
};
use csvx_core::sort::SortColumn;
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_sort(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut by: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut descending = false;
    let mut verify = false;
    let mut buf_mb = 64usize;
    let mut delim = "auto".to_string();
    let mut quiet = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--by" | "-b" => match p.value("--by") {
                Ok(v) => by = Some(v),
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
            "--desc" => descending = true,
            "--asc" => descending = false,
            "--verify" => verify = true,
            // Accepting `--engine` and quietly using the native path would be worse than
            // refusing: someone who asked for SQL would believe they got it.
            "--engine" => {
                eprintln!(
                    "error: sort has no sql engine; its ordering is numeric only when both \
                     sides parse as numbers, which no SQL ORDER BY on a typed column reproduces"
                );
                return 2;
            }
            "--buf-mb" => match p
                .value("--buf-mb")
                .and_then(|v| v.parse().map_err(|_| "bad --buf-mb".to_string()))
            {
                Ok(v) => buf_mb = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-q" | "--quiet" => quiet = true,
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("sort");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => input = Some(PathBuf::from(other)),
        }
    }

    let (Some(input), Some(by)) = (input, by) else {
        eprintln!("error: sort needs <input> and --by <column>");
        return 2;
    };
    let out = match out {
        Some(o) => o,
        None => {
            let stem = input
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "sorted".to_string());
            let ext = input
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_else(|| "csv".to_string());
            input.with_file_name(format!("{stem}.sorted.{ext}"))
        }
    };
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    // `--by a,b` sorts by both; `--by a:desc,b:asc` sets direction per column.
    let columns: Vec<SortColumn> = match parse_sort_columns(&by, descending) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    if let Err(code) = check_output(Some(&input), &out) {
        return code;
    }
    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("sort", rx, quiet);
    let result =
        csvx_core::sort::external_sort_columns(&input, &out, &columns, delimiter, buf_mb, &tx);
    drop(tx);
    let _ = reporter.join();
    match result {
        Ok(()) => {
            let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            let described = columns
                .iter()
                .map(|c| format!("{} {}", c.name, if c.descending { "desc" } else { "asc" }))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "✓ sorted by {described} → {} ({})",
                out.display(),
                human_bytes(bytes)
            );
            if verify {
                // Sorting promises "same rows, reordered": check the counts on
                // both sides rather than trusting the merge step.
                let (tx, _rx) = mpsc::channel();
                let before = match csvx_core::check::count(&input, delimiter, &tx) {
                    Ok(c) => c,
                    Err(e) => {
                        return report_failure("sort", format!("cannot read the source: {e}"))
                    }
                };
                let after = match csvx_core::check::count(&out, delimiter, &tx) {
                    Ok(c) => c,
                    Err(e) => {
                        return report_failure("sort", format!("cannot read the result: {e}"))
                    }
                };
                if before.rows != after.rows || before.columns != after.columns {
                    let _ = report_failure(
                        "sort",
                        format!(
                            "self-check failed: {} rows × {} columns became {} rows × {} columns",
                            before.rows, before.columns, after.rows, after.columns
                        ),
                    );
                    return 1;
                }
                if !quiet {
                    println!(
                        "  ✓ checked: {} rows, {} columns",
                        after.rows, after.columns
                    );
                }
            }
            0
        }
        Err(e) => {
            // Same as merge: sorting returns an error without sending an event.
            eprintln!("error: {e}");
            1
        }
    }
}

/// `a` / `a,b` / `a:desc,b:asc`. A bare `--desc` sets the default direction.
pub(crate) fn parse_sort_columns(
    spec: &str,
    default_descending: bool,
) -> Result<Vec<SortColumn>, String> {
    let mut columns = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, descending) = match part.split_once(':') {
            Some((n, dir)) => match dir.trim().to_ascii_lowercase().as_str() {
                "asc" | "a" => (n.trim().to_string(), false),
                "desc" | "d" => (n.trim().to_string(), true),
                other => return Err(format!("unknown sort direction '{other}' (use asc/desc)")),
            },
            None => (part.to_string(), default_descending),
        };
        if name.is_empty() {
            return Err("empty sort column".to_string());
        }
        columns.push(SortColumn::new(name, descending));
    }
    if columns.is_empty() {
        return Err("no sort columns".to_string());
    }
    Ok(columns)
}
