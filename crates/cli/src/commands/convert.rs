//! Transcoding and workbook extraction. `--all-sheets` writes one CSV per
//! worksheet; the single-sheet path stays the default.
use crate::{
    check_output, parse_delimiter, print_command_help, report_failure, spawn_reporter, Parser,
};
use csvx_core::encoding::{parse_name, TextEncoding};
use csvx_core::*;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Instant;

fn parse_column_formats(
    text: &str,
) -> Result<Vec<(String, csvx_core::layout::NumberFormat)>, String> {
    let mut formats = Vec::new();
    for raw in split_format_specs(text) {
        let (column, code) = raw
            .split_once(':')
            .ok_or_else(|| format!("number format {raw:?} needs column:format-code"))?;
        let column = column.trim().to_string();
        if column.is_empty() {
            return Err(format!("number format {raw:?} needs a column"));
        }
        if formats.iter().any(|(name, _)| name == &column) {
            return Err(format!("number format column {column:?} is repeated"));
        }
        let format = csvx_core::layout::NumberFormat::parse(code.trim())
            .map_err(|error| format!("number format {column:?}: {error}"))?;
        formats.push((column, format));
    }
    if formats.is_empty() {
        return Err("--number-format needs at least one column:format-code".to_string());
    }
    Ok(formats)
}

/// Split `a:fmt;b:fmt` on semicolons that are not inside an accounting `(...)`.
fn split_format_specs(text: &str) -> Vec<&str> {
    let mut specs = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (index, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => {
                // The `;` in `positive;(negative)` belongs to the accounting code,
                // not to the separator between columns.
                let next = text[index + 1..].trim_start().chars().next();
                if next == Some('(') {
                    continue;
                }
                if !text[start..index].trim().is_empty() {
                    specs.push(&text[start..index]);
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    if !text[start..].trim().is_empty() {
        specs.push(&text[start..]);
    }
    specs
}

fn safe_sheet_file_name(name: &str) -> String {
    let mut out = String::new();
    for character in name.chars() {
        if character.is_control()
            || matches!(
                character,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
            )
        {
            out.push('_');
        } else {
            out.push(character);
        }
    }
    let trimmed = out.trim_matches([' ', '.']);
    if trimmed.is_empty() {
        "sheet".to_string()
    } else {
        trimmed.to_string()
    }
}

pub(crate) fn parse_highlight_rules(
    text: &str,
) -> Result<Vec<csvx_core::xlsx::ConditionalRule>, String> {
    let mut rules = Vec::new();
    for raw in text.split(';').filter(|raw| !raw.trim().is_empty()) {
        let (head, color) = raw.rsplit_once(':').ok_or_else(|| {
            format!("highlight rule {raw:?} needs a color (e.g. amount>1000:red)")
        })?;
        let color = color.trim().to_ascii_lowercase();
        if !matches!(
            color.as_str(),
            "red" | "amber" | "yellow" | "green" | "blue"
        ) {
            return Err(format!(
                "unsupported highlight color {color:?}; use red/amber/green/blue"
            ));
        }
        // Rank form: amount:top3:green / amount:bottom2:red.
        if let Some((column, rank)) = head.split_once(':') {
            let column = column.trim().to_string();
            let rank_spec = rank.trim().to_ascii_lowercase();
            let (operator, number) = if let Some(number) = rank_spec.strip_prefix("top") {
                ("top", number)
            } else if let Some(number) = rank_spec.strip_prefix("bottom") {
                ("bottom", number)
            } else {
                return Err(format!(
                    "highlight rule {raw:?}: expected topN/bottomN (e.g. amount:top3:green)"
                ));
            };
            let number: usize = number
                .trim()
                .parse()
                .map_err(|_| format!("highlight rule {raw:?}: rank must be a positive integer"))?;
            if number == 0 {
                return Err(format!(
                    "highlight rule {raw:?}: rank must be greater than 0"
                ));
            }
            if column.is_empty() {
                return Err(format!("highlight rule {raw:?} needs a column"));
            }
            rules.push(csvx_core::xlsx::ConditionalRule {
                column,
                operator: operator.to_string(),
                value: number.to_string(),
                color,
            });
            continue;
        }
        let (operator, (column, value)) = ["!=", ">=", "<=", ">", "<", "="]
            .iter()
            .find_map(|operator| {
                head.split_once(*operator)
                    .filter(|(_, value)| !value.trim().is_empty())
                    .map(|(column, value)| (*operator, (column, value)))
            })
            .ok_or_else(|| format!("highlight rule {head:?} needs an operator"))?;
        let column = column.trim().to_string();
        if column.is_empty() {
            return Err(format!("highlight rule {head:?} needs a column"));
        }
        rules.push(csvx_core::xlsx::ConditionalRule {
            column,
            operator: operator.to_string(),
            value: value.trim().to_string(),
            color,
        });
    }
    if rules.is_empty() {
        return Err("--highlight needs at least one rule".to_string());
    }
    Ok(rules)
}

pub(crate) fn cmd_convert(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut from = "auto".to_string();
    let mut to = "utf-8".to_string();
    let mut convert_format: Option<String> = None;
    let mut sheet: Option<String> = None;
    let mut all_sheets = false;
    let mut highlight: Option<String> = None;
    let mut number_format: Option<String> = None;
    let mut delim = "comma".to_string();
    let mut quiet = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--format" => match p.value("--format") {
                Ok(v) => convert_format = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--highlight" => match p.value("--highlight") {
                Ok(v) => highlight = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--all-sheets" => all_sheets = true,
            "--number-format" => match p.value("--number-format") {
                Ok(v) => number_format = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--sheet" => match p.value("--sheet") {
                Ok(v) => sheet = Some(v),
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
            "--from" => match p.value("--from") {
                Ok(v) => from = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--to" => match p.value("--to") {
                Ok(v) => to = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-q" | "--quiet" => quiet = true,
            "-h" | "--help" => {
                print_command_help("convert");
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
        eprintln!("error: convert needs an <input> path");
        return 2;
    };

    if let Some(target) = convert_format {
        let Some(out) = out else {
            eprintln!("error: --format needs --out <path>");
            return 2;
        };
        if input.is_dir() {
            if !target.eq_ignore_ascii_case("xlsx") {
                eprintln!("error: directory input is only supported with --format xlsx");
                return 2;
            }
            if sheet.is_some() {
                eprintln!("error: --sheet cannot be used with directory input");
                return 2;
            }
            if highlight.is_some() {
                eprintln!("error: --highlight needs a single xlsx output, not a directory");
                return 2;
            }
            if number_format.is_some() {
                eprintln!("error: --number-format needs a single xlsx output, not a directory");
                return 2;
            }
            if let Err(code) = check_output(Some(&input), &out) {
                return code;
            }
            let output_abs = out.canonicalize().unwrap_or_else(|_| out.clone());
            let entries = match std::fs::read_dir(&input) {
                Ok(entries) => entries,
                Err(e) => {
                    eprintln!("error: cannot read {}: {e}", input.display());
                    return 2;
                }
            };
            let mut files = Vec::new();
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"))
                {
                    let absolute = path.canonicalize().unwrap_or_else(|_| path.clone());
                    if absolute != output_abs {
                        files.push(path);
                    }
                }
            }
            if files.is_empty() {
                eprintln!("error: directory has no .csv files");
                return 2;
            }
            files.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
            let mut sheets = Vec::new();
            for file in &files {
                let delimiter = match parse_delimiter(&delim, file) {
                    Ok(delimiter) => delimiter,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                };
                match csvx_core::xlsx::read_csv_sheet(file, delimiter) {
                    Ok(sheet) => sheets.push(sheet),
                    Err(e) => {
                        eprintln!("error: {}: {e}", file.display());
                        return 1;
                    }
                }
            }
            return match csvx_core::xlsx::write_sheets(&out, &sheets) {
                Ok(()) => {
                    if !quiet {
                        println!(
                            "✓ converted {} sheets from {} → {}",
                            sheets.len(),
                            input.display(),
                            out.display()
                        );
                    }
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            };
        }
        let rules = match highlight.as_deref() {
            Some(_text) if !target.eq_ignore_ascii_case("xlsx") => {
                eprintln!("error: --highlight is only valid with --format xlsx");
                return 2;
            }
            Some(text) => match parse_highlight_rules(text) {
                Ok(rules) => rules,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            None => Vec::new(),
        };
        let column_formats = match number_format.as_deref() {
            Some(_) if !target.eq_ignore_ascii_case("xlsx") => {
                eprintln!("error: --number-format is only valid with --format xlsx");
                return 2;
            }
            Some(text) => match parse_column_formats(text) {
                Ok(formats) => formats,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            None => Vec::new(),
        };
        let input_name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if input_name.ends_with(".xls") || input_name.ends_with(".xlsb") {
            eprintln!(
                "error: legacy Excel format .xls/.xlsb is not supported; save it as .xlsx first"
            );
            return 2;
        }
        let is_xlsx = input_name.ends_with(".xlsx") || input_name.ends_with(".xlsm");
        if sheet.is_some() && (!is_xlsx || !target.eq_ignore_ascii_case("csv")) {
            eprintln!("error: --sheet is only valid for xlsx input converted to csv");
            return 2;
        }
        if all_sheets {
            if !is_xlsx {
                eprintln!("error: --all-sheets needs an .xlsx/.xlsm input");
                return 2;
            }
            if !target.eq_ignore_ascii_case("csv") {
                eprintln!("error: --all-sheets only supports --format csv");
                return 2;
            }
            if sheet.is_some() {
                eprintln!("error: --all-sheets cannot be combined with --sheet");
                return 2;
            }
            if highlight.is_some() {
                eprintln!("error: --all-sheets cannot be combined with --highlight");
                return 2;
            }
            if out.exists() && !out.is_dir() {
                eprintln!(
                    "error: --all-sheets needs an output directory: {}",
                    out.display()
                );
                return 2;
            }
            if let Err(error) = std::fs::create_dir_all(&out) {
                eprintln!("error: cannot create {}: {error}", out.display());
                return 2;
            }
            let names = match csvx_core::xlsx::xlsx_sheet_names(&input) {
                Ok(names) => names,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            };
            let mut used = std::collections::BTreeMap::<String, usize>::new();
            for name in names {
                let base = safe_sheet_file_name(&name);
                let count = used.entry(base.clone()).or_insert(0);
                *count += 1;
                let file_name = if *count == 1 {
                    format!("{base}.csv")
                } else {
                    format!("{base}-{count}.csv")
                };
                let target = out.join(file_name);
                if let Err(error) = csvx_core::xlsx::read_xlsx_to_csv(&input, &target, Some(&name))
                {
                    eprintln!("error: sheet {name:?}: {error}");
                    return 1;
                }
                if !quiet {
                    println!("✓ {} → {}", name, target.display());
                }
            }
            return 0;
        }
        if is_xlsx {
            if !target.eq_ignore_ascii_case("csv") {
                eprintln!("error: xlsx input can only be converted to csv; use --format csv");
                return 2;
            }
            if let Err(code) = check_output(Some(&input), &out) {
                return code;
            }
            return match csvx_core::xlsx::read_xlsx_to_csv(&input, &out, sheet.as_deref()) {
                Ok(()) => {
                    if !quiet {
                        println!(
                            "✓ converted {} → {} (xlsx → csv)",
                            input.display(),
                            out.display()
                        );
                    }
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            };
        }
        // A TSV/Parquet/NDJSON source has its own delimiter; validating a CSV header
        // with the CSV default would reject a perfectly good TSV before conversion.
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let delimiter = if name.ends_with(".tsv") {
            csvx_core::types::Delimiter::Tab
        } else if name.ends_with(".parquet")
            || name.ends_with(".ndjson")
            || name.ends_with(".jsonl")
            || name.ends_with(".json")
        {
            csvx_core::types::Delimiter::Comma
        } else {
            match parse_delimiter(&delim, &input) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        };
        if let Err(code) = check_output(Some(&input), &out) {
            return code;
        }
        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("convert", rx, true);
        let result = if !column_formats.is_empty() {
            csvx_core::xlsx::csv_to_xlsx_with_number_formats(
                &input,
                &out,
                delimiter,
                &tx,
                &rules,
                &column_formats,
            )
        } else if rules.is_empty() {
            csvx_core::engine::convert_format(&input, &out, &target, delimiter, &tx)
        } else {
            csvx_core::xlsx::csv_to_xlsx_with_rules(&input, &out, delimiter, &tx, &rules)
        };
        drop(tx);
        let _ = reporter.join();
        return match result {
            Ok(()) => {
                if !quiet {
                    println!(
                        "✓ converted {} → {} ({target})",
                        input.display(),
                        out.display()
                    );
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        };
    }

    let from_enc = match from.as_str() {
        "auto" | "" => None,
        other => match parse_name(other) {
            Some(e) => Some(e),
            None => {
                eprintln!("error: unknown encoding: {other}");
                return 2;
            }
        },
    };
    let to_enc: TextEncoding = match to.to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" => TextEncoding::Utf8 { bom: false },
        "utf-8-bom" | "utf8-bom" | "bom" => TextEncoding::Utf8 { bom: true },
        other => {
            eprintln!("error: --to only supports utf-8 or utf-8-bom (got {other})");
            return 2;
        }
    };

    let output = out.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "output".to_string());
        input.with_file_name(format!("{stem}.utf8.csv"))
    });

    if let Err(code) = check_output(Some(&input), &output) {
        return code;
    }
    let started = Instant::now();
    match encoding::transcode(&input, &output, from_enc, to_enc, 4 * 1024 * 1024) {
        Ok(bytes) => {
            if !quiet {
                println!(
                    "✓ {} bytes → {} ({}) in {:.2}s",
                    bytes,
                    output.display(),
                    if from_enc.is_some() {
                        "forced"
                    } else {
                        "detected"
                    },
                    started.elapsed().as_secs_f64()
                );
            }
            0
        }
        // `transcode` returns the error itself; nothing else printed it.
        Err(e) => report_failure("convert", e),
    }
}
