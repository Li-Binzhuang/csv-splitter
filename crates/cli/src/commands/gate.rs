use crate::{json_escape, parse_delimiter, print_command_help, report_failure, Parser};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

fn gate_csv_files(dir: &Path, output: Option<&Path>) -> std::io::Result<Vec<PathBuf>> {
    let output_abs = output.map(|path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if !path.is_file()
            || !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
        {
            continue;
        }
        let absolute = path.canonicalize().unwrap_or_else(|_| path.clone());
        if output_abs.as_ref() == Some(&absolute) {
            continue;
        }
        files.push(path);
    }
    files.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    Ok(files)
}

fn json_number(text: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\":");
    let rest = text.split(&needle).nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Apply the same thresholds to every `.csv` directly inside a directory.
///
/// Files are independent gates: a duplicate id in one file cannot be excused by a
/// unique one in another. The aggregate report keeps each child report verbatim.
fn gate_directory(
    dir: &Path,
    min_rows: Option<u64>,
    max_empty: Option<u64>,
    unique: Option<&str>,
    unique_global: bool,
    out: Option<&Path>,
    json: bool,
) -> i32 {
    let files = match gate_csv_files(dir, out) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("error: cannot read {}: {error}", dir.display());
            return 2;
        }
    };
    if files.is_empty() {
        eprintln!("error: {} has no .csv files", dir.display());
        return 2;
    }
    let bin = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("error: cannot locate csvx: {error}");
            return 2;
        }
    };
    let mut reports = Vec::with_capacity(files.len());
    let mut failed = 0usize;
    let mut total_rows = 0u64;
    let mut total_empty = 0u64;
    for file in &files {
        let mut command = std::process::Command::new(&bin);
        command.args(["gate", file.to_str().unwrap_or(""), "--json"]);
        if let Some(minimum) = min_rows {
            command.args(["--min-rows", &minimum.to_string()]);
        }
        if let Some(maximum) = max_empty {
            command.args(["--max-empty", &maximum.to_string()]);
        }
        if let Some(name) = unique {
            command.args(["--unique", name]);
        }
        let output = match command.output() {
            Ok(output) => output,
            Err(error) => {
                eprintln!("error: cannot gate {}: {error}", file.display());
                return 2;
            }
        };
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            failed += 1;
        }
        total_rows += json_number(&text, "rows").unwrap_or(0);
        total_empty += json_number(&text, "empty").unwrap_or(0);
        if !json && !output.status.success() {
            eprintln!("✗ {}: {}", file.display(), stderr.trim());
        } else if !json {
            println!("✓ {}: {text}", file.display());
        }
        reports.push(text);
    }
    let mut global_failure = String::new();
    let mut global_passed = true;
    if unique_global && files.len() > 1 {
        let stamp = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        );
        let temp = std::env::temp_dir().join(format!("csvx-gate-global-{stamp}.csv"));
        let mut concat = std::process::Command::new(&bin);
        concat.arg("concat");
        for file in &files {
            concat.arg(file);
        }
        concat.args(["-o", temp.to_str().unwrap_or(""), "--json"]);
        let concatenated = concat.output();
        match concatenated {
            Ok(result) if result.status.success() => {
                let mut check = std::process::Command::new(&bin);
                check.args([
                    "gate",
                    temp.to_str().unwrap_or(""),
                    "--unique",
                    unique.unwrap_or(""),
                    "--json",
                ]);
                let checked = check.output();
                match checked {
                    Ok(result) if result.status.success() => {}
                    Ok(result) => {
                        global_passed = false;
                        let detail = String::from_utf8_lossy(&result.stdout);
                        global_failure = if detail.contains("\"uniqueDuplicates\":true") {
                            "unique column has duplicates across files".to_string()
                        } else {
                            format!("unique check failed: {}", detail.trim())
                        };
                    }
                    Err(error) => {
                        global_passed = false;
                        global_failure = error.to_string();
                    }
                }
            }
            Ok(result) => {
                global_passed = false;
                global_failure = String::from_utf8_lossy(&result.stderr).trim().to_string();
            }
            Err(error) => {
                global_passed = false;
                global_failure = error.to_string();
            }
        }
        let _ = std::fs::remove_file(&temp);
    }
    let passed = failed == 0 && global_passed;
    let global = if unique_global {
        format!(
            ",\"uniqueGlobal\":{{\"passed\":{},\"failure\":\"{}\"}}",
            global_passed,
            json_escape(&global_failure)
        )
    } else {
        String::new()
    };
    let aggregate = format!(
        r#"{{"passed":{},"files":{},"failed":{},"rows":{},"empty":{},"reports":[{}]{}}}"#,
        passed,
        files.len(),
        failed,
        total_rows,
        total_empty,
        reports.join(","),
        global
    );
    if let Some(path) = out {
        if let Err(error) = std::fs::write(path, format!("{aggregate}\n")) {
            eprintln!("error: cannot write {}: {error}", path.display());
            return 2;
        }
    }
    if json {
        println!("{aggregate}");
    } else {
        println!(
            "gate: {} file(s), {} failed; rows {}, empty {}",
            files.len(),
            failed,
            total_rows,
            total_empty
        );
    }
    if passed {
        0
    } else {
        1
    }
}

/// A quality gate for scripts and pipelines: exit 1 when a threshold fails.
///
/// Thresholds are counts, not ratios: "at least 100 rows, at most 5 empty cells,
/// column id unique". The gate writes its own JSON report when `--out` is used, so
/// a pipeline step can keep the evidence while a failing exit stops the run.
pub(crate) fn cmd_gate(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut min_rows: Option<u64> = None;
    let mut max_empty: Option<u64> = None;
    let mut unique: Option<String> = None;
    let mut unique_global = false;
    let mut out: Option<PathBuf> = None;
    let mut json = false;
    let mut delim = "auto".to_string();
    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--min-rows" => match p
                .value("--min-rows")
                .and_then(|v| v.parse().map_err(|_| "bad --min-rows".to_string()))
            {
                Ok(v) => min_rows = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--max-empty" => match p
                .value("--max-empty")
                .and_then(|v| v.parse().map_err(|_| "bad --max-empty".to_string()))
            {
                Ok(v) => max_empty = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--unique" => match p.value("--unique") {
                Ok(v) => unique = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--unique-global" => unique_global = true,
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
                print_command_help("gate");
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
        eprintln!("error: gate needs <input>");
        return 2;
    };
    if min_rows.is_none() && max_empty.is_none() && unique.is_none() {
        eprintln!("error: gate needs at least one of --min-rows, --max-empty, --unique");
        return 2;
    }
    if let Some(out) = &out {
        if out == &input {
            eprintln!("error: --out must not be the input file");
            return 2;
        }
    }
    if unique_global && unique.is_none() {
        eprintln!("error: --unique-global needs --unique <column>");
        return 2;
    }
    if input.is_dir() {
        return gate_directory(
            &input,
            min_rows,
            max_empty,
            unique.as_deref(),
            unique_global,
            out.as_deref(),
            json,
        );
    }
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let sheet =
        match csvx_core::sheet::Sheet::open(&input, csvx_core::sheet::ReadOptions { delimiter }) {
            Ok(sheet) => sheet,
            Err(e) => return report_failure("gate", e.to_string()),
        };
    let total = match sheet.rows() {
        Ok(rows) => rows,
        Err(e) => return report_failure("gate", e.to_string()),
    };
    let columns = match sheet.columns() {
        Ok(columns) => columns,
        Err(e) => return report_failure("gate", e.to_string()),
    };
    let unique_index = match &unique {
        None => None,
        Some(name) => match sheet.column_index(name) {
            Ok(Some(index)) => Some(index - 1),
            Ok(None) => {
                eprintln!("error: --unique names unknown column {name:?}");
                return 2;
            }
            Err(e) => return report_failure("gate", e.to_string()),
        },
    };
    // A uniqueness check over 100 GB cannot hold every value in memory. Sort the
    // edited sheet by that column once, then adjacent duplicates are exact and the
    // working set is whatever the external sorter budgets. The other metrics reuse
    // the sorted stream, so the file is read once for the gate.
    let mut sorted_path: Option<PathBuf> = None;
    let mut _materialized = None;
    let metrics_sheet = if let Some(name) = &unique {
        if total == 0 {
            sheet
        } else {
            let (tx, rx) = mpsc::channel();
            drop(rx);
            let materialized = match csvx_core::sheet::materialize(
                &input,
                &csvx_core::sheet::ReadOptions { delimiter },
                &tx,
            ) {
                Ok(value) => value,
                Err(e) => return report_failure("gate", e.to_string()),
            };
            let source = materialized.path().to_path_buf();
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let sorted = std::env::temp_dir().join(format!(
                "csvx-gate-{}-{}-{stamp}.csv",
                std::process::id(),
                name
            ));
            let result = csvx_core::sort::external_sort_columns(
                &source,
                &sorted,
                &[csvx_core::sort::SortColumn::new(name.clone(), false)],
                delimiter,
                256,
                &tx,
            );
            drop(tx);
            if let Err(e) = result {
                let _ = std::fs::remove_file(&sorted);
                return report_failure("gate", format!("cannot sort for uniqueness: {e}"));
            }
            let sorted_sheet = match csvx_core::sheet::Sheet::open(
                &sorted,
                csvx_core::sheet::ReadOptions { delimiter },
            ) {
                Ok(value) => value,
                Err(e) => {
                    let _ = std::fs::remove_file(&sorted);
                    return report_failure("gate", e.to_string());
                }
            };
            sorted_path = Some(sorted);
            _materialized = Some(materialized);
            drop(sheet);
            sorted_sheet
        }
    } else {
        sheet
    };
    let mut empty: u64 = 0;
    let mut seen: u64 = 0;
    let mut unique_values: u64 = 0;
    let mut unique_duplicate = false;
    let mut previous_unique: Option<String> = None;
    let mut start: u64 = 0;
    let chunk = 5_000u64;
    while start < total {
        let window = match metrics_sheet.window(start, chunk) {
            Ok(window) => window,
            Err(e) => return report_failure("gate", e.to_string()),
        };
        if window.rows.is_empty() {
            break;
        }
        for row in &window.rows {
            seen += 1;
            empty += (columns.len().saturating_sub(row.len())) as u64;
            for cell in row.iter().take(columns.len()) {
                if cell.is_empty() {
                    empty += 1;
                }
            }
            if let Some(index) = unique_index {
                let value = row.get(index).cloned().unwrap_or_default();
                if previous_unique.as_deref() == Some(value.as_str()) {
                    unique_duplicate = true;
                } else {
                    unique_values += 1;
                    previous_unique = Some(value);
                }
            }
        }
        start += window.rows.len() as u64;
    }
    if let Some(path) = &sorted_path {
        let _ = std::fs::remove_file(path);
    }
    let min_ok = min_rows.is_none_or(|minimum| total >= minimum);
    let empty_ok = max_empty.is_none_or(|maximum| empty <= maximum);
    let unique_ok = unique
        .as_ref()
        .is_none_or(|_| !unique_duplicate && unique_values == seen);
    let passed = min_ok && empty_ok && unique_ok;
    let mut failures = Vec::new();
    if !min_ok {
        failures.push(format!("rows {total} < minimum {}", min_rows.unwrap_or(0)));
    }
    if !empty_ok {
        failures.push(format!(
            "empty {empty} > maximum {}",
            max_empty.unwrap_or(0)
        ));
    }
    if !unique_ok {
        if unique_duplicate {
            failures.push("unique column has duplicates".to_string());
        } else {
            failures.push("unique column could not be proven unique".to_string());
        }
    }
    let failures_json = failures
        .iter()
        .map(|failure| format!("\"{}\"", json_escape(failure)))
        .collect::<Vec<_>>()
        .join(",");
    if let Some(path) = &out {
        let mut text = String::from("{\"passed\":");
        text.push_str(if passed { "true" } else { "false" });
        text.push_str(&format!(
            ",\"rows\":{total},\"empty\":{empty},\"uniqueValues\":{},\"uniqueDuplicates\":{},\"failures\":[{}],\"checks\":{{\"minRows\":{:?},\"maxEmpty\":{:?},\"unique\":{:?}}}",
            unique_values,
            unique_duplicate,
            failures_json,
            min_rows,
            max_empty,
            unique.as_deref().map(json_escape)
        ));
        if let Err(e) = std::fs::write(path, format!("{text}\n")) {
            eprintln!("error: cannot write {}: {e}", path.display());
            return 2;
        }
    }
    if json {
        println!(
            "{{\"passed\":{},\"rows\":{},\"empty\":{},\"uniqueValues\":{},\"uniqueDuplicates\":{},\"failures\":[{}]}}",
            passed,
            total,
            empty,
            unique_values,
            unique_duplicate,
            failures_json
        );
    } else {
        println!(
            "gate: rows {total}, empty {empty}, unique {}",
            if unique.is_some() {
                format!(
                    "{}{}",
                    unique_values,
                    if unique_duplicate { "+" } else { "" }
                )
            } else {
                "-".to_string()
            }
        );
        if !min_ok {
            eprintln!("✗ rows {total} < minimum {}", min_rows.unwrap_or(0));
        }
        if !empty_ok {
            eprintln!("✗ empty {empty} > maximum {}", max_empty.unwrap_or(0));
        }
        if !unique_ok {
            eprintln!("✗ unique column has duplicates");
        }
        if passed {
            println!("✓ gate passed");
        }
    }
    if passed {
        0
    } else {
        1
    }
}
