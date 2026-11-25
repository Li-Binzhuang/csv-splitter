use crate::{
    human_bytes, parse_delimiter, print_command_help, report_failure, spawn_reporter, Parser,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_concat(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut ignore_header = false;
    let mut by_name = false;
    let mut engine = csvx_core::engine::Engine::Auto;
    let mut json = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
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
            "--ignore-header" => ignore_header = true,
            "--by-name" => by_name = true,
            "--json" => json = true,
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("concat");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => inputs.push(PathBuf::from(other)),
        }
    }

    // A directory is a convenient shorthand for "every .csv directly inside it".
    // The output is excluded: re-running into the same directory must not append the
    // previous result to the next one.
    let single_dir = inputs.len() == 1 && inputs[0].is_dir();
    let output = out.unwrap_or_else(|| {
        if single_dir {
            let name = inputs[0]
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("combined");
            inputs[0].with_file_name(format!("{name}-combined.csv"))
        } else {
            inputs[0].with_file_name("combined.csv")
        }
    });
    if single_dir {
        let dir = &inputs[0];
        let output_abs = output.canonicalize().unwrap_or_else(|_| output.clone());
        let mut found = Vec::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", dir.display());
                return 2;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
            {
                continue;
            }
            let absolute = path.canonicalize().unwrap_or_else(|_| path.clone());
            if absolute == output_abs {
                continue;
            }
            found.push(path);
        }
        found.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
        if found.len() < 2 {
            eprintln!(
                "error: {} has fewer than two .csv files to concatenate",
                dir.display()
            );
            return 2;
        }
        inputs = found;
    }
    if inputs.len() < 2 {
        eprintln!("error: concat needs at least two <input> files");
        return 2;
    }
    let delimiter = match parse_delimiter(&delim, &inputs[0]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("concat", rx, json);

    // `--by-name` aligns same-named columns first, so files exported in a different column
    // order concatenate instead of failing on a header mismatch.
    let aligned_guard;
    let aligned: Vec<PathBuf> = if by_name {
        match csvx_core::concat::align_by_name(&inputs, delimiter) {
            Ok(guard) => {
                aligned_guard = guard;
                aligned_guard.paths().to_vec()
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    } else {
        inputs.clone()
    };
    let planned = match csvx_core::engine::resolve_for_copy(engine, &inputs[0]) {
        Ok(e) => e,
        Err(message) => {
            eprintln!("error: {message}");
            return 2;
        }
    };
    let result = match planned {
        csvx_core::engine::Engine::Sql => {
            csvx_core::engine::concat_sql(&aligned, &output, delimiter, ignore_header, &tx)
        }
        _ => csvx_core::concat::concat(&aligned, &output, delimiter, ignore_header, &tx),
    };
    drop(tx);
    let _ = reporter.join();
    match result {
        Ok(s) => {
            if json {
                println!(
                    r#"{{"files":{},"rows":{},"bytes":{},"columns":["{}"],"mismatched":{}}}"#,
                    s.files,
                    s.rows,
                    s.bytes,
                    s.columns.join(r#"",""#),
                    s.mismatched.len()
                );
            } else {
                println!(
                    "✓ {} files, {} rows → {} ({})",
                    s.files,
                    s.rows,
                    output.display(),
                    human_bytes(s.bytes)
                );
                for f in &s.mismatched {
                    eprintln!("  ⚠ header differs, appended anyway: {}", f.display());
                }
            }
            0
        }
        Err(e) => report_failure("chart", e), // engine returned the error itself
    }
}
