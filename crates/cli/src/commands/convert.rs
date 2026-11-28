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

pub(crate) fn parse_column_formats(
    text: &str,
    raw: bool,
) -> Result<Vec<(String, csvx_core::layout::NumberFormat)>, String> {
    let mut formats = Vec::new();
    for spec in split_format_specs(text) {
        let (column, code) = spec
            .split_once(':')
            .ok_or_else(|| format!("number format {spec:?} needs column:format-code"))?;
        let column = column.trim().to_string();
        if column.is_empty() {
            return Err(format!("number format {spec:?} needs a column"));
        }
        if formats.iter().any(|(name, _)| name == &column) {
            return Err(format!("number format column {column:?} is repeated"));
        }
        let format = if raw {
            csvx_core::layout::NumberFormat::parse_raw(code)
        } else {
            csvx_core::layout::NumberFormat::parse(code.trim())
        }
        .map_err(|error| format!("number format {column:?}: {error}"))?;
        formats.push((column, format));
    }
    if formats.is_empty() {
        return Err("--number-format needs at least one column:format-code".to_string());
    }
    Ok(formats)
}

fn parse_data_validations(text: &str) -> Result<Vec<csvx_core::xlsx::DataValidation>, String> {
    let mut validations = Vec::new();
    for raw in split_format_specs(text) {
        let (column, values) = raw
            .split_once(':')
            .ok_or_else(|| format!("data validation {raw:?} needs column:value,value"))?;
        let column = column.trim().to_string();
        if column.is_empty() {
            return Err(format!("data validation {raw:?} needs a column"));
        }
        if validations
            .iter()
            .any(|validation: &csvx_core::xlsx::DataValidation| validation.column == column)
        {
            return Err(format!("data validation column {column:?} is repeated"));
        }
        let values: Vec<String> = values
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect();
        if values.is_empty() {
            return Err(format!(
                "data validation {column:?} needs at least one value"
            ));
        }
        validations.push(csvx_core::xlsx::DataValidation {
            column,
            values,
            formula: None,
            prompt_title: None,
            prompt_message: None,
            error_title: None,
            error_message: None,
            allow_blank: false,
            numeric: None,
            error_style: None,
            list_range: None,
        });
    }
    if validations.is_empty() {
        return Err("--data-validation needs at least one column:values".to_string());
    }
    Ok(validations)
}

fn parse_validation_formulas(text: &str) -> Result<Vec<csvx_core::xlsx::DataValidation>, String> {
    let mut validations = Vec::new();
    for raw in split_format_specs(text) {
        let (column, formula) = raw
            .split_once(':')
            .ok_or_else(|| format!("data validation formula {raw:?} needs column:formula"))?;
        let column = column.trim().to_string();
        let formula = formula.trim().to_string();
        if column.is_empty() || formula.is_empty() {
            return Err(format!(
                "data validation formula {raw:?} needs column:formula"
            ));
        }
        if validations
            .iter()
            .any(|validation: &csvx_core::xlsx::DataValidation| validation.column == column)
        {
            return Err(format!("data validation column {column:?} is repeated"));
        }
        validations.push(csvx_core::xlsx::DataValidation {
            column,
            values: Vec::new(),
            formula: Some(formula),
            prompt_title: None,
            prompt_message: None,
            error_title: None,
            error_message: None,
            allow_blank: false,
            numeric: None,
            error_style: None,
            list_range: None,
        });
    }
    Ok(validations)
}

fn parse_numeric_validations(
    text: &str,
    kind: csvx_core::xlsx::NumericKind,
    flag: &str,
) -> Result<Vec<csvx_core::xlsx::DataValidation>, String> {
    let mut validations = Vec::new();
    for raw in split_format_specs(text) {
        let (column, bounds) = raw
            .split_once(':')
            .ok_or_else(|| format!("{flag} {raw:?} needs column:min,max"))?;
        let column = column.trim().to_string();
        if column.is_empty() {
            return Err(format!("{flag} {raw:?} needs a column"));
        }
        if validations
            .iter()
            .any(|validation: &csvx_core::xlsx::DataValidation| validation.column == column)
        {
            return Err(format!("data validation column {column:?} is repeated"));
        }
        let mut parts = bounds.split(',');
        let min = parts.next().map(str::trim).filter(|part| !part.is_empty());
        let max = match (parts.next(), parts.next()) {
            (Some(max), None) => max.trim().to_string(),
            (Some(_), Some(_)) => {
                return Err(format!("{flag} {column:?} needs exactly min,max"));
            }
            (None, _) => String::new(),
        };
        if min.is_none() && max.is_empty() {
            return Err(format!("{flag} {column:?} needs a min or max bound"));
        }
        validations.push(csvx_core::xlsx::DataValidation {
            column,
            values: Vec::new(),
            formula: None,
            prompt_title: None,
            prompt_message: None,
            error_title: None,
            error_message: None,
            allow_blank: false,
            numeric: Some(csvx_core::xlsx::NumericValidation {
                kind,
                min: min.map(str::to_string),
                max: (!max.is_empty()).then_some(max),
            }),
            error_style: None,
            list_range: None,
        });
    }
    if validations.is_empty() {
        return Err(format!("{flag} needs at least one column:min,max"));
    }
    Ok(validations)
}

fn merge_numeric_validations(
    validations: &mut Vec<csvx_core::xlsx::DataValidation>,
    text: &str,
    kind: csvx_core::xlsx::NumericKind,
    flag: &str,
) -> Result<(), String> {
    for numeric in parse_numeric_validations(text, kind, flag)? {
        if validations.iter().any(|v| v.column == numeric.column) {
            return Err(format!(
                "data validation column {:?} is repeated",
                numeric.column
            ));
        }
        validations.push(numeric);
    }
    Ok(())
}

fn merge_validation_ranges(
    validations: &mut Vec<csvx_core::xlsx::DataValidation>,
    text: &str,
) -> Result<(), String> {
    for validation in parse_validation_ranges(text)? {
        if validations.iter().any(|v| v.column == validation.column) {
            return Err(format!(
                "data validation column {:?} is repeated",
                validation.column
            ));
        }
        validations.push(validation);
    }
    Ok(())
}

fn parse_validation_ranges(text: &str) -> Result<Vec<csvx_core::xlsx::DataValidation>, String> {
    let mut validations = Vec::new();
    for raw in split_format_specs(text) {
        let (column, range) = raw
            .split_once(':')
            .ok_or_else(|| format!("data validation range {raw:?} needs column:range"))?;
        let column = column.trim().to_string();
        let range = range.trim().to_string();
        if column.is_empty() || range.is_empty() {
            return Err(format!("data validation range {raw:?} needs column:range"));
        }
        if validations
            .iter()
            .any(|validation: &csvx_core::xlsx::DataValidation| validation.column == column)
        {
            return Err(format!("data validation column {column:?} is repeated"));
        }
        validations.push(csvx_core::xlsx::DataValidation {
            column,
            values: Vec::new(),
            formula: None,
            prompt_title: None,
            prompt_message: None,
            error_title: None,
            error_message: None,
            allow_blank: false,
            numeric: None,
            error_style: None,
            list_range: Some(range),
        });
    }
    if validations.is_empty() {
        return Err("--data-validation-range needs at least one column:range".to_string());
    }
    Ok(validations)
}

fn parse_validation_error_styles(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut styles = Vec::new();
    for raw in split_format_specs(text) {
        let (column, style) = raw
            .split_once(':')
            .ok_or_else(|| format!("data validation error style {raw:?} needs column:style"))?;
        let column = column.trim().to_string();
        let style = style.trim().to_ascii_lowercase();
        if column.is_empty() {
            return Err(format!(
                "data validation error style {raw:?} needs a column"
            ));
        }
        if !matches!(style.as_str(), "stop" | "warning" | "information") {
            return Err(format!(
                "data validation error style for {column:?} must be stop, warning, or information"
            ));
        }
        if styles.iter().any(|(name, _)| name == &column) {
            return Err(format!(
                "data validation error style column {column:?} is repeated"
            ));
        }
        styles.push((column, style));
    }
    Ok(styles)
}

fn parse_column_widths(text: &str) -> Result<Vec<(String, f64)>, String> {
    let mut widths = Vec::new();
    for raw in split_format_specs(text) {
        let (column, width) = raw
            .split_once(':')
            .ok_or_else(|| format!("column width {raw:?} needs column:width"))?;
        let column = column.trim().to_string();
        if column.is_empty() {
            return Err(format!("column width {raw:?} needs a column"));
        }
        if widths.iter().any(|(name, _)| name == &column) {
            return Err(format!("column width {column:?} is repeated"));
        }
        let width: f64 = width
            .trim()
            .parse()
            .map_err(|_| format!("column width for {column:?} is not a number"))?;
        if !(0.5..=255.0).contains(&width) {
            return Err(format!("column width for {column:?} must be 0.5..=255"));
        }
        widths.push((column, width));
    }
    if widths.is_empty() {
        return Err("--col-width needs at least one column:width".to_string());
    }
    Ok(widths)
}

fn parse_validation_prompts(text: &str) -> Result<Vec<(String, String, String)>, String> {
    let mut prompts = Vec::new();
    for raw in split_format_specs(text) {
        let (column, body) = raw
            .split_once(':')
            .ok_or_else(|| format!("validation prompt {raw:?} needs column:title|message"))?;
        let column = column.trim().to_string();
        let (title, message) = body
            .split_once('|')
            .ok_or_else(|| format!("validation prompt {raw:?} needs title|message"))?;
        if column.is_empty() {
            return Err(format!("validation prompt {raw:?} needs a column"));
        }
        if prompts.iter().any(|(name, _, _)| name == &column) {
            return Err(format!("validation prompt column {column:?} is repeated"));
        }
        prompts.push((column, title.trim().to_string(), message.trim().to_string()));
    }
    if prompts.is_empty() {
        return Err("--data-validation-prompt needs column:title|message".to_string());
    }
    Ok(prompts)
}

/// Split `a:fmt;b:fmt` on semicolons that are not inside an accounting `(...)`.
fn projected_validation_bound(kind: &str, value: &Option<String>) -> Option<String> {
    let value = value.as_deref()?;
    let pattern = match kind {
        "date" => "yyyy-mm-dd",
        "time" => "hh:mm:ss",
        _ => return Some(value.to_string()),
    };
    Some(
        csvx_core::layout::NumberFormat::DateTime {
            pattern: pattern.to_string(),
        }
        .render(value),
    )
}

fn json_optional(value: &Option<String>) -> String {
    match value {
        Some(value) => json_string(value),
        None => "null".to_string(),
    }
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control < ' ' => out.push_str(&format!("\\u{:04x}", control as u32)),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

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
    let mut number_format_raw: Option<String> = None;
    let mut no_freeze = false;
    let mut no_auto_filter = false;
    let mut data_validation: Option<String> = None;
    let mut data_validation_whole: Option<String> = None;
    let mut data_validation_decimal: Option<String> = None;
    let mut data_validation_error_style: Option<String> = None;
    let mut data_validation_range: Option<String> = None;
    let mut data_validation_date: Option<String> = None;
    let mut data_validation_time: Option<String> = None;
    let mut data_validation_blank = false;
    let mut protect_sheet = false;
    let mut protect_password: Option<String> = None;
    let mut data_validation_formula: Option<String> = None;
    let mut protect_objects = true;
    let mut protect_scenarios = true;
    let mut column_width: Option<String> = None;
    let mut data_validation_prompt: Option<String> = None;
    let mut data_validation_error: Option<String> = None;
    let mut show_formats = false;
    let mut formats_json = false;
    let mut protection = false;
    let mut validations = false;
    let mut formulas = false;
    let mut conditional_formats = false;
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
            "--no-freeze" => no_freeze = true,
            "--no-auto-filter" => no_auto_filter = true,
            "--data-validation-error" => match p.value("--data-validation-error") {
                Ok(v) => data_validation_error = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--data-validation-blank" => data_validation_blank = true,
            "--protect-sheet" => protect_sheet = true,
            "--protect-sheet-allow-objects" => protect_objects = false,
            "--protect-sheet-allow-scenarios" => protect_scenarios = false,
            "--protect-sheet-password" => match p.value("--protect-sheet-password") {
                Ok(v) => protect_password = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--col-width" => match p.value("--col-width") {
                Ok(v) => column_width = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--data-validation-prompt" => match p.value("--data-validation-prompt") {
                Ok(v) => data_validation_prompt = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--data-validation" => match p.value("--data-validation") {
                Ok(v) => data_validation = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--data-validation-formula" => match p.value("--data-validation-formula") {
                Ok(v) => data_validation_formula = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--data-validation-whole" => match p.value("--data-validation-whole") {
                Ok(v) => data_validation_whole = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--data-validation-decimal" => match p.value("--data-validation-decimal") {
                Ok(v) => data_validation_decimal = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--data-validation-error-style" => match p.value("--data-validation-error-style") {
                Ok(v) => data_validation_error_style = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--data-validation-range" => match p.value("--data-validation-range") {
                Ok(v) => data_validation_range = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--data-validation-date" => match p.value("--data-validation-date") {
                Ok(v) => data_validation_date = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--data-validation-time" => match p.value("--data-validation-time") {
                Ok(v) => data_validation_time = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--formats" => show_formats = true,
            "--protection" => protection = true,
            "--validations" => validations = true,
            "--formulas" => formulas = true,
            "--conditional-formats" => conditional_formats = true,
            "--json" => formats_json = true,
            "--number-format-raw" => match p.value("--number-format-raw") {
                Ok(v) => number_format_raw = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
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

    if formats_json
        && !show_formats
        && !protection
        && !validations
        && !formulas
        && !conditional_formats
    {
        eprintln!("error: --json needs --formats or --protection");
        return 2;
    }

    if conditional_formats {
        if protection || validations || formulas || show_formats {
            eprintln!(
                "error: --conditional-formats cannot be combined with other inspection modes"
            );
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --conditional-formats needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --conditional-formats cannot be combined with output flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --conditional-formats --all-sheets cannot be combined with --sheet");
            return 2;
        }
        let names = if all_sheets {
            match csvx_core::xlsx::xlsx_sheet_names(&input) {
                Ok(names) => names,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        } else {
            vec![sheet.clone().unwrap_or_default()]
        };
        let mut entries = Vec::new();
        for name in &names {
            let sheet_arg = (!name.is_empty()).then_some(name.as_str());
            match csvx_core::xlsx::xlsx_conditional_formats(&input, sheet_arg) {
                Ok(found) => {
                    let sheet_name = sheet_arg.unwrap_or("");
                    for rule in found {
                        entries.push((sheet_name.to_string(), rule));
                    }
                }
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        }
        if formats_json {
            print!("[");
            for (index, (sheet_name, rule)) in entries.iter().enumerate() {
                if index > 0 {
                    print!(",");
                }
                print!(
                    "{{\"sheet\":{},\"range\":{},\"type\":{},\"operator\":{},\"value\":{},\"color\":{}}}",
                    json_string(sheet_name),
                    json_string(&rule.range),
                    json_string(&rule.rule_type),
                    json_optional(&rule.operator),
                    json_optional(&rule.value),
                    json_optional(&rule.color),
                );
            }
            println!("]");
        } else if entries.is_empty() {
            println!("no conditional formats");
        } else {
            for (sheet_name, rule) in &entries {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                println!(
                    "range={} type={} operator={} value={} color={}",
                    rule.range,
                    rule.rule_type,
                    rule.operator.as_deref().unwrap_or(""),
                    rule.value.as_deref().unwrap_or(""),
                    rule.color.as_deref().unwrap_or("")
                );
            }
        }
        return 0;
    }

    if formulas {
        if protection || validations || show_formats {
            eprintln!("error: --formulas cannot be combined with other inspection modes");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --formulas needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --formulas cannot be combined with output/conversion flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --formulas --all-sheets cannot be combined with --sheet");
            return 2;
        }
        let names = if all_sheets {
            match csvx_core::xlsx::xlsx_sheet_names(&input) {
                Ok(names) => names,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        } else {
            vec![sheet.clone().unwrap_or_default()]
        };
        let mut entries = Vec::new();
        for name in &names {
            let sheet_arg = (!name.is_empty()).then_some(name.as_str());
            match csvx_core::xlsx::xlsx_formulas(&input, sheet_arg) {
                Ok(found) => {
                    let sheet_name = sheet_arg.unwrap_or("");
                    for cell in found {
                        entries.push((sheet_name.to_string(), cell));
                    }
                }
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        }
        if formats_json {
            print!("[");
            for (index, (sheet_name, cell)) in entries.iter().enumerate() {
                if index > 0 {
                    print!(",");
                }
                print!(
                    "{{\"sheet\":{},\"cell\":{},\"formula\":{},\"cached_value\":{}}}",
                    json_string(sheet_name),
                    json_string(&cell.cell),
                    json_string(&cell.formula),
                    json_optional(&cell.cached_value),
                );
            }
            println!("]");
        } else if entries.is_empty() {
            println!("no formulas");
        } else {
            for (sheet_name, cell) in &entries {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                println!(
                    "cell={} formula={} cached_value={}",
                    cell.cell,
                    cell.formula,
                    cell.cached_value.as_deref().unwrap_or("")
                );
            }
        }
        return 0;
    }

    if protection || validations {
        if validations {
            if protection || show_formats {
                eprintln!("error: --validations cannot be combined with --protection or --formats");
                return 2;
            }
            let name = input
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
                eprintln!("error: --validations needs an .xlsx/.xlsm input");
                return 2;
            }
            if convert_format.is_some() || out.is_some() {
                eprintln!("error: --validations cannot be combined with output/conversion flags");
                return 2;
            }
            if sheet.is_some() && all_sheets {
                eprintln!("error: --validations --all-sheets cannot be combined with --sheet");
                return 2;
            }
            let names = if all_sheets {
                match csvx_core::xlsx::xlsx_sheet_names(&input) {
                    Ok(names) => names,
                    Err(error) => {
                        eprintln!("error: {error}");
                        return 1;
                    }
                }
            } else {
                vec![sheet.clone().unwrap_or_default()]
            };
            let mut entries = Vec::new();
            for name in &names {
                let sheet_arg = (!name.is_empty()).then_some(name.as_str());
                match csvx_core::xlsx::xlsx_data_validations(&input, sheet_arg) {
                    Ok(mut found) => entries.append(&mut found),
                    Err(error) => {
                        eprintln!("error: {error}");
                        return 1;
                    }
                }
            }
            if formats_json {
                print!("[");
                for (index, entry) in entries.iter().enumerate() {
                    if index > 0 {
                        print!(",");
                    }
                    print!(
                    "{{\"sheet\":{},\"column\":{},\"range\":{},\"type\":{},\"operator\":{},\"formula1\":{},\"formula2\":{},\"allow_blank\":{},\"prompt_title\":{},\"prompt_message\":{},\"error_title\":{},\"error_message\":{},\"error_style\":{}}}",
                    json_string(&entry.sheet),
                    json_string(&entry.column),
                    json_string(&entry.range),
                    json_string(&entry.kind),
                    json_string(&entry.operator),
                    json_optional(&projected_validation_bound(&entry.kind, &entry.formula1)),
                    json_optional(&projected_validation_bound(&entry.kind, &entry.formula2)),
                    entry.allow_blank,
                    json_optional(&entry.prompt_title),
                    json_optional(&entry.prompt_message),
                    json_optional(&entry.error_title),
                    json_optional(&entry.error_message),
                    json_optional(&entry.error_style),
                );
                }
                println!("]");
            } else if entries.is_empty() {
                println!("no data validations");
            } else {
                for entry in &entries {
                    if all_sheets {
                        println!("# {}", entry.sheet);
                    }
                    println!(
                    "column={} range={} type={} operator={} formula1={} formula2={} allow_blank={} prompt_title={} error_style={}",
                    entry.column,
                    entry.range,
                    entry.kind,
                    entry.operator,
                    projected_validation_bound(&entry.kind, &entry.formula1)
                        .as_deref()
                        .unwrap_or(""),
                    projected_validation_bound(&entry.kind, &entry.formula2)
                        .as_deref()
                        .unwrap_or(""),
                    entry.allow_blank,
                    entry.prompt_title.as_deref().unwrap_or(""),
                    entry.error_style.as_deref().unwrap_or("")
                );
                }
            }
            return 0;
        }

        if show_formats {
            eprintln!("error: --protection cannot be combined with --formats");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --protection needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --protection cannot be combined with output/conversion flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --protection --all-sheets cannot be combined with --sheet");
            return 2;
        }
        let names = if all_sheets {
            match csvx_core::xlsx::xlsx_sheet_names(&input) {
                Ok(names) => names,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        } else {
            vec![sheet.clone().unwrap_or_default()]
        };
        if formats_json {
            print!("[");
        }
        for (index, name) in names.iter().enumerate() {
            let sheet_arg = (!name.is_empty()).then_some(name.as_str());
            let detail = match csvx_core::xlsx::xlsx_sheet_protection(&input, sheet_arg) {
                Ok(detail) => detail,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            };
            let sheet_name = sheet_arg.unwrap_or("");
            if formats_json {
                if index > 0 {
                    print!(",");
                }
                print!(
                    "{{\"sheet\":{},\"protected\":{},\"objects_locked\":{},\"scenarios_locked\":{},\"password_protected\":{}}}",
                    json_string(sheet_name),
                    detail.protected,
                    detail.objects_locked,
                    detail.scenarios_locked,
                    detail.password_protected
                );
            } else {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                println!(
                    "protected={} objects_locked={} scenarios_locked={} password_protected={}",
                    detail.protected,
                    detail.objects_locked,
                    detail.scenarios_locked,
                    detail.password_protected
                );
            }
        }
        if formats_json {
            println!("]");
        }
        return 0;
    }

    if show_formats {
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --formats needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some()
            || out.is_some()
            || highlight.is_some()
            || number_format.is_some()
        {
            eprintln!("error: --formats cannot be combined with output/conversion flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --formats --all-sheets cannot be combined with --sheet");
            return 2;
        }
        let names = if all_sheets {
            match csvx_core::xlsx::xlsx_sheet_names(&input) {
                Ok(names) => names,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        } else {
            vec![sheet.clone().unwrap_or_default()]
        };
        if formats_json {
            print!("[");
        }
        for (sheet_index, name) in names.iter().enumerate() {
            let sheet_arg = (!name.is_empty()).then_some(name.as_str());
            let columns = match csvx_core::xlsx::xlsx_column_formats(&input, sheet_arg) {
                Ok(columns) => columns,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            };
            let sheet_name = sheet_arg.unwrap_or("");
            if formats_json {
                if sheet_index > 0 {
                    print!(",");
                }
                print!("{{\"sheet\":{},\"columns\":[", json_string(sheet_name));
                for (index, (column, format)) in columns.iter().enumerate() {
                    if index > 0 {
                        print!(", ");
                    }
                    let code = format
                        .as_ref()
                        .map(|format| format.name())
                        .unwrap_or_else(|| "plain".into());
                    print!(
                        "{{\"name\":{},\"format\":{},\"column_index\":{}}}",
                        json_string(column),
                        json_string(&code),
                        index
                    );
                }
                print!("]}}");
            } else {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                for (column, format) in columns {
                    let code = format
                        .map(|format| format.name())
                        .unwrap_or_else(|| "plain".into());
                    println!("{column}\t{code}");
                }
            }
        }
        if formats_json {
            println!("]");
        }
        return 0;
    }

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
            let widths = match column_width.as_deref() {
                Some(text) => match parse_column_widths(text) {
                    Ok(widths) => widths,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                },
                None => Vec::new(),
            };
            let raw_format_text = number_format_raw.as_deref().or(number_format.as_deref());
            let raw_format = number_format_raw.is_some();
            let column_formats = match raw_format_text {
                Some(text) => match parse_column_formats(text, raw_format) {
                    Ok(formats) => formats,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                },
                None => Vec::new(),
            };
            let rules = match highlight.as_deref() {
                Some(text) => match parse_highlight_rules(text) {
                    Ok(rules) => rules,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                },
                None => Vec::new(),
            };
            let mut validations = match data_validation.as_deref() {
                Some(text) => match parse_data_validations(text) {
                    Ok(validations) => validations,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                },
                None => Vec::new(),
            };
            if let Some(text) = data_validation_formula.as_deref() {
                match parse_validation_formulas(text) {
                    Ok(formulas) => {
                        for formula in formulas {
                            if validations.iter().any(|v| v.column == formula.column) {
                                eprintln!(
                                    "error: data validation column {:?} is repeated",
                                    formula.column
                                );
                                return 2;
                            }
                            validations.push(formula);
                        }
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                }
            }
            if let Some(text) = data_validation_whole.as_deref() {
                match merge_numeric_validations(
                    &mut validations,
                    text,
                    csvx_core::xlsx::NumericKind::Whole,
                    "--data-validation-whole",
                ) {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                }
            }
            if let Some(text) = data_validation_decimal.as_deref() {
                match merge_numeric_validations(
                    &mut validations,
                    text,
                    csvx_core::xlsx::NumericKind::Decimal,
                    "--data-validation-decimal",
                ) {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                }
            }
            if let Some(text) = data_validation_range.as_deref() {
                match merge_validation_ranges(&mut validations, text) {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                }
            }
            if let Some(text) = data_validation_date.as_deref() {
                match merge_numeric_validations(
                    &mut validations,
                    text,
                    csvx_core::xlsx::NumericKind::Date,
                    "--data-validation-date",
                ) {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                }
            }
            if let Some(text) = data_validation_time.as_deref() {
                match merge_numeric_validations(
                    &mut validations,
                    text,
                    csvx_core::xlsx::NumericKind::Time,
                    "--data-validation-time",
                ) {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                }
            }
            let validation_prompts = match data_validation_prompt.as_deref() {
                Some(text) => match parse_validation_prompts(text) {
                    Ok(prompts) => prompts,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                },
                None => Vec::new(),
            };
            for (column, title, message) in validation_prompts {
                let Some(validation) = validations
                    .iter_mut()
                    .find(|validation| validation.column == column)
                else {
                    eprintln!("error: data validation prompt column {column:?} has no validation");
                    return 2;
                };
                validation.prompt_title = (!title.is_empty()).then_some(title);
                validation.prompt_message = (!message.is_empty()).then_some(message);
            }
            let validation_errors = match data_validation_error.as_deref() {
                Some(text) => match parse_validation_prompts(text) {
                    Ok(errors) => errors,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                },
                None => Vec::new(),
            };
            for (column, title, message) in validation_errors {
                let Some(validation) = validations
                    .iter_mut()
                    .find(|validation| validation.column == column)
                else {
                    eprintln!("error: data validation error column {column:?} has no validation");
                    return 2;
                };
                validation.error_title = (!title.is_empty()).then_some(title);
                validation.error_message = (!message.is_empty()).then_some(message);
            }
            if let Some(text) = data_validation_error_style.as_deref() {
                match parse_validation_error_styles(text) {
                    Ok(styles) => {
                        for (column, style) in styles {
                            let Some(validation) =
                                validations.iter_mut().find(|v| v.column == column)
                            else {
                                eprintln!(
                                    "error: data validation error style column {column:?} has no validation"
                                );
                                return 2;
                            };
                            validation.error_style = Some(style);
                        }
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                }
            }
            if data_validation_blank {
                for validation in &mut validations {
                    validation.allow_blank = true;
                }
            }
            let per_sheet = if widths.is_empty() {
                Vec::new()
            } else {
                vec![widths; sheets.len()]
            };
            let per_sheet_formats: Vec<Vec<csvx_core::layout::NumberFormat>> =
                if column_formats.is_empty() {
                    Vec::new()
                } else {
                    let mut mapped = Vec::new();
                    for sheet in &sheets {
                        let mut row = Vec::new();
                        for header in &sheet.headers {
                            row.push(
                                column_formats
                                    .iter()
                                    .find(|(column, _)| column == header)
                                    .map(|(_, format)| format.clone())
                                    .unwrap_or(csvx_core::layout::NumberFormat::Plain),
                            );
                        }
                        mapped.push(row);
                    }
                    mapped
                };
            let result =
                if !per_sheet_formats.is_empty() || !rules.is_empty() || !validations.is_empty() {
                    csvx_core::xlsx::write_sheets_with_formats_widths_and_protection(
                        &out,
                        &sheets,
                        &per_sheet_formats,
                        &per_sheet,
                        !no_freeze,
                        !no_auto_filter,
                        protect_sheet
                            || protect_password.is_some()
                            || !protect_objects
                            || !protect_scenarios,
                        protect_password.as_deref(),
                        protect_objects,
                        protect_scenarios,
                        &rules,
                        &validations,
                    )
                } else if protect_password.is_some() || !protect_objects || !protect_scenarios {
                    csvx_core::xlsx::write_sheets_with_options_and_password(
                        &out,
                        &sheets,
                        &per_sheet,
                        !no_freeze,
                        !no_auto_filter,
                        protect_password.as_deref(),
                        protect_objects,
                        protect_scenarios,
                    )
                } else if !per_sheet.is_empty() {
                    csvx_core::xlsx::write_sheets_with_widths_and_options(
                        &out,
                        &sheets,
                        &per_sheet,
                        !no_freeze,
                        !no_auto_filter,
                    )
                } else if no_freeze || no_auto_filter {
                    csvx_core::xlsx::write_sheets_with_options(
                        &out,
                        &sheets,
                        !no_freeze,
                        !no_auto_filter,
                    )
                } else {
                    csvx_core::xlsx::write_sheets(&out, &sheets)
                };
            return match result {
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
        let raw_format_text = number_format_raw.as_deref().or(number_format.as_deref());
        let raw_format = number_format_raw.is_some();
        let column_formats = match raw_format_text {
            Some(_) if !target.eq_ignore_ascii_case("xlsx") => {
                eprintln!("error: --number-format is only valid with --format xlsx");
                return 2;
            }
            Some(text) => match parse_column_formats(text, raw_format) {
                Ok(formats) => formats,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            None => Vec::new(),
        };
        let mut data_validations = match data_validation.as_deref() {
            Some(_) if !target.eq_ignore_ascii_case("xlsx") => {
                eprintln!("error: --data-validation is only valid with --format xlsx");
                return 2;
            }
            Some(text) => match parse_data_validations(text) {
                Ok(validations) => validations,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            None => Vec::new(),
        };
        if let Some(password) = protect_password.as_deref() {
            if !target.eq_ignore_ascii_case("xlsx") {
                eprintln!("error: --protect-sheet-password is only valid with --format xlsx");
                return 2;
            }
            if password.is_empty()
                || password.len() > 15
                || !password.is_ascii()
                || password
                    .chars()
                    .any(|character| !character.is_ascii_graphic())
            {
                eprintln!("error: sheet password must be 1-15 ASCII letters or symbols");
                return 2;
            }
        }
        let protect_sheet =
            protect_sheet || protect_password.is_some() || !protect_objects || !protect_scenarios;
        let column_widths = match column_width.as_deref() {
            Some(_) if !target.eq_ignore_ascii_case("xlsx") => {
                eprintln!("error: --col-width is only valid with --format xlsx");
                return 2;
            }
            Some(text) => match parse_column_widths(text) {
                Ok(widths) => widths,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            None => Vec::new(),
        };
        if let Some(text) = data_validation_formula.as_deref() {
            if !target.eq_ignore_ascii_case("xlsx") {
                eprintln!("error: --data-validation-formula is only valid with --format xlsx");
                return 2;
            }
            match parse_validation_formulas(text) {
                Ok(formulas) => {
                    for formula in formulas {
                        if data_validations.iter().any(|v| v.column == formula.column) {
                            eprintln!(
                                "error: data validation column {:?} is repeated",
                                formula.column
                            );
                            return 2;
                        }
                        data_validations.push(formula);
                    }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        }
        if let Some(text) = data_validation_whole.as_deref() {
            if !target.eq_ignore_ascii_case("xlsx") {
                eprintln!("error: --data-validation-whole is only valid with --format xlsx");
                return 2;
            }
            match merge_numeric_validations(
                &mut data_validations,
                text,
                csvx_core::xlsx::NumericKind::Whole,
                "--data-validation-whole",
            ) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        }
        if let Some(text) = data_validation_decimal.as_deref() {
            if !target.eq_ignore_ascii_case("xlsx") {
                eprintln!("error: --data-validation-decimal is only valid with --format xlsx");
                return 2;
            }
            match merge_numeric_validations(
                &mut data_validations,
                text,
                csvx_core::xlsx::NumericKind::Decimal,
                "--data-validation-decimal",
            ) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        }
        if let Some(text) = data_validation_range.as_deref() {
            if !target.eq_ignore_ascii_case("xlsx") {
                eprintln!("error: --data-validation-range is only valid with --format xlsx");
                return 2;
            }
            match merge_validation_ranges(&mut data_validations, text) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        }
        if let Some(text) = data_validation_date.as_deref() {
            if !target.eq_ignore_ascii_case("xlsx") {
                eprintln!("error: --data-validation-date is only valid with --format xlsx");
                return 2;
            }
            match merge_numeric_validations(
                &mut data_validations,
                text,
                csvx_core::xlsx::NumericKind::Date,
                "--data-validation-date",
            ) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        }
        if let Some(text) = data_validation_time.as_deref() {
            if !target.eq_ignore_ascii_case("xlsx") {
                eprintln!("error: --data-validation-time is only valid with --format xlsx");
                return 2;
            }
            match merge_numeric_validations(
                &mut data_validations,
                text,
                csvx_core::xlsx::NumericKind::Time,
                "--data-validation-time",
            ) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        }
        let validation_prompts = match data_validation_prompt.as_deref() {
            Some(_) if !target.eq_ignore_ascii_case("xlsx") => {
                eprintln!("error: --data-validation-prompt is only valid with --format xlsx");
                return 2;
            }
            Some(text) => match parse_validation_prompts(text) {
                Ok(prompts) => prompts,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            None => Vec::new(),
        };
        for (column, title, message) in validation_prompts {
            let Some(validation) = data_validations
                .iter_mut()
                .find(|validation| validation.column == column)
            else {
                eprintln!("error: data validation prompt column {column:?} has no validation");
                return 2;
            };
            validation.prompt_title = (!title.is_empty()).then_some(title);
            validation.prompt_message = (!message.is_empty()).then_some(message);
        }
        let validation_errors = match data_validation_error.as_deref() {
            Some(_) if !target.eq_ignore_ascii_case("xlsx") => {
                eprintln!("error: --data-validation-error is only valid with --format xlsx");
                return 2;
            }
            Some(text) => match parse_validation_prompts(text) {
                Ok(errors) => errors,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            None => Vec::new(),
        };
        if let Some(text) = data_validation_error_style.as_deref() {
            if !target.eq_ignore_ascii_case("xlsx") {
                eprintln!("error: --data-validation-error-style is only valid with --format xlsx");
                return 2;
            }
            match parse_validation_error_styles(text) {
                Ok(styles) => {
                    for (column, style) in styles {
                        let Some(validation) =
                            data_validations.iter_mut().find(|v| v.column == column)
                        else {
                            eprintln!(
                                "error: data validation error style column {column:?} has no validation"
                            );
                            return 2;
                        };
                        validation.error_style = Some(style);
                    }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        }
        if data_validation_blank
            && !data_validations.is_empty()
            && !target.eq_ignore_ascii_case("xlsx")
        {
            eprintln!("error: --data-validation-blank is only valid with --format xlsx");
            return 2;
        }
        if data_validation_blank {
            for validation in &mut data_validations {
                validation.allow_blank = true;
            }
        }
        for (column, title, message) in validation_errors {
            let Some(validation) = data_validations
                .iter_mut()
                .find(|validation| validation.column == column)
            else {
                eprintln!("error: data validation error column {column:?} has no validation");
                return 2;
            };
            validation.error_title = (!title.is_empty()).then_some(title);
            validation.error_message = (!message.is_empty()).then_some(message);
        }
        if protect_sheet && !target.eq_ignore_ascii_case("xlsx") {
            eprintln!("error: --protect-sheet is only valid with --format xlsx");
            return 2;
        }
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
        let xlsx_options = no_freeze
            || no_auto_filter
            || !column_formats.is_empty()
            || !rules.is_empty()
            || !data_validations.is_empty()
            || protect_sheet
            || protect_password.is_some()
            || !column_widths.is_empty();
        let result = if xlsx_options {
            csvx_core::xlsx::csv_to_xlsx_with_all(
                &input,
                &out,
                delimiter,
                &tx,
                &rules,
                &column_formats,
                &data_validations,
                &column_widths,
                !no_freeze,
                !no_auto_filter,
                protect_sheet,
                protect_password.as_deref(),
                protect_objects,
                protect_scenarios,
            )
        } else {
            csvx_core::engine::convert_format(&input, &out, &target, delimiter, &tx)
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
