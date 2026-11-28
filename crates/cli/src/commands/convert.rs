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

fn color_name(argb: &str) -> Option<&'static str> {
    match argb {
        "FFFFC7CE" => Some("red"),
        "FFFFEB9C" => Some("amber"),
        "FFC6EFCE" => Some("green"),
        "FFBDD7EE" => Some("blue"),
        _ => None,
    }
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
    let mut sheet_view = false;
    let mut validation_checks = false;
    let mut highlight_text = false;
    let mut show_headers = false;
    let mut merged_ranges = false;
    let mut column_widths = false;
    let mut row_heights = false;
    let mut sheet_visibility = false;
    let mut properties = false;
    let mut page_setup = false;
    let mut print_layout = false;
    let mut header_footer = false;
    let mut print_area: Option<String> = None;
    let mut orientation: Option<String> = None;
    let mut paper_size: Option<u32> = None;
    let mut fit_to_width: Option<u32> = None;
    let mut fit_to_height: Option<u32> = None;
    let mut scale: Option<u32> = None;
    let mut margin_left: Option<f64> = None;
    let mut margin_right: Option<f64> = None;
    let mut margin_top: Option<f64> = None;
    let mut margin_bottom: Option<f64> = None;
    let mut margin_header: Option<f64> = None;
    let mut margin_footer: Option<f64> = None;
    let mut header_text: Option<String> = None;
    let mut footer_text: Option<String> = None;
    let mut title: Option<String> = None;
    let mut author: Option<String> = None;
    let mut merges: Vec<String> = Vec::new();
    let mut hide_sheets: Vec<String> = Vec::new();
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
            "--sheet-view" => sheet_view = true,
            "--checks" => validation_checks = true,
            "--highlight-text" => highlight_text = true,
            "--headers" => show_headers = true,
            "--merged" => merged_ranges = true,
            "--widths" => column_widths = true,
            "--heights" => row_heights = true,
            "--visibility" => sheet_visibility = true,
            "--properties" => properties = true,
            "--page-setup" => page_setup = true,
            "--print-layout" => print_layout = true,
            "--header-footer" => header_footer = true,
            "--header" => match p.value("--header") {
                Ok(v) if v.chars().count() <= 255 => header_text = Some(v),
                Ok(_) => {
                    eprintln!("error: --header must be 255 characters or fewer");
                    return 2;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--footer" => match p.value("--footer") {
                Ok(v) if v.chars().count() <= 255 => footer_text = Some(v),
                Ok(_) => {
                    eprintln!("error: --footer must be 255 characters or fewer");
                    return 2;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--margin-left" => match p.value("--margin-left").and_then(|v| {
                v.parse::<f64>()
                    .map_err(|_| "bad --margin-left".to_string())
            }) {
                Ok(v) if (0.0..=10.0).contains(&v) => margin_left = Some(v),
                _ => {
                    eprintln!("error: --margin-left must be 0-10");
                    return 2;
                }
            },
            "--margin-right" => match p.value("--margin-right").and_then(|v| {
                v.parse::<f64>()
                    .map_err(|_| "bad --margin-right".to_string())
            }) {
                Ok(v) if (0.0..=10.0).contains(&v) => margin_right = Some(v),
                _ => {
                    eprintln!("error: --margin-right must be 0-10");
                    return 2;
                }
            },
            "--margin-top" => match p
                .value("--margin-top")
                .and_then(|v| v.parse::<f64>().map_err(|_| "bad --margin-top".to_string()))
            {
                Ok(v) if (0.0..=10.0).contains(&v) => margin_top = Some(v),
                _ => {
                    eprintln!("error: --margin-top must be 0-10");
                    return 2;
                }
            },
            "--margin-bottom" => match p.value("--margin-bottom").and_then(|v| {
                v.parse::<f64>()
                    .map_err(|_| "bad --margin-bottom".to_string())
            }) {
                Ok(v) if (0.0..=10.0).contains(&v) => margin_bottom = Some(v),
                _ => {
                    eprintln!("error: --margin-bottom must be 0-10");
                    return 2;
                }
            },
            "--margin-header" => match p.value("--margin-header").and_then(|v| {
                v.parse::<f64>()
                    .map_err(|_| "bad --margin-header".to_string())
            }) {
                Ok(v) if (0.0..=10.0).contains(&v) => margin_header = Some(v),
                _ => {
                    eprintln!("error: --margin-header must be 0-10");
                    return 2;
                }
            },
            "--margin-footer" => match p.value("--margin-footer").and_then(|v| {
                v.parse::<f64>()
                    .map_err(|_| "bad --margin-footer".to_string())
            }) {
                Ok(v) if (0.0..=10.0).contains(&v) => margin_footer = Some(v),
                _ => {
                    eprintln!("error: --margin-footer must be 0-10");
                    return 2;
                }
            },
            "--orientation" => match p.value("--orientation") {
                Ok(v) if v == "portrait" || v == "landscape" => orientation = Some(v),
                Ok(_) => {
                    eprintln!("error: --orientation must be portrait or landscape");
                    return 2;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--paper-size" => match p
                .value("--paper-size")
                .and_then(|v| v.parse::<u32>().map_err(|_| "bad --paper-size".to_string()))
            {
                Ok(v) if v > 0 && v <= 118 => paper_size = Some(v),
                Ok(_) => {
                    eprintln!("error: --paper-size must be 1-118");
                    return 2;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--fit-to-width" => match p.value("--fit-to-width").and_then(|v| {
                v.parse::<u32>()
                    .map_err(|_| "bad --fit-to-width".to_string())
            }) {
                Ok(v) if v > 0 && v <= 32767 => fit_to_width = Some(v),
                _ => {
                    eprintln!("error: --fit-to-width must be 1-32767");
                    return 2;
                }
            },
            "--fit-to-height" => match p.value("--fit-to-height").and_then(|v| {
                v.parse::<u32>()
                    .map_err(|_| "bad --fit-to-height".to_string())
            }) {
                Ok(v) if v > 0 && v <= 32767 => fit_to_height = Some(v),
                _ => {
                    eprintln!("error: --fit-to-height must be 1-32767");
                    return 2;
                }
            },
            "--scale" => match p
                .value("--scale")
                .and_then(|v| v.parse::<u32>().map_err(|_| "bad --scale".to_string()))
            {
                Ok(v) if (10..=400).contains(&v) => scale = Some(v),
                _ => {
                    eprintln!("error: --scale must be 10-400");
                    return 2;
                }
            },
            "--print-area" => match p.value("--print-area") {
                Ok(v) => print_area = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--title" => match p.value("--title") {
                Ok(v) => title = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--merge" => match p.value("--merge") {
                Ok(v) => merges.extend(
                    v.split(',')
                        .map(|range| range.trim().to_string())
                        .filter(|range| !range.is_empty()),
                ),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--hide-sheet" => match p.value("--hide-sheet") {
                Ok(v) => hide_sheets.push(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--author" => match p.value("--author") {
                Ok(v) => author = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
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
        && !sheet_view
        && !show_headers
        && !merged_ranges
        && !column_widths
        && !row_heights
        && !sheet_visibility
        && !properties
        && !page_setup
        && !print_layout
        && !header_footer
    {
        eprintln!("error: --json needs --formats or --protection");
        return 2;
    }

    if header_footer {
        if protection
            || validations
            || formulas
            || conditional_formats
            || sheet_view
            || show_formats
            || show_headers
            || merged_ranges
            || column_widths
            || row_heights
            || sheet_visibility
            || properties
            || page_setup
            || print_layout
        {
            eprintln!("error: --header-footer cannot be combined with other inspection modes");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --header-footer needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --header-footer cannot be combined with output flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --header-footer --all-sheets cannot be combined with --sheet");
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
            match csvx_core::xlsx::xlsx_header_footer(&input, sheet_arg) {
                Ok(info) => entries.push((sheet_arg.unwrap_or("").to_string(), info)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        }
        if formats_json {
            print!("[");
            for (index, (sheet_name, info)) in entries.iter().enumerate() {
                if index > 0 {
                    print!(",");
                }
                print!(
                    "{{\"sheet\":{},\"odd_header\":{},\"odd_footer\":{},\"even_header\":{},\"even_footer\":{},\"first_header\":{},\"first_footer\":{}}}",
                    json_string(sheet_name),
                    json_optional(&info.odd_header),
                    json_optional(&info.odd_footer),
                    json_optional(&info.even_header),
                    json_optional(&info.even_footer),
                    json_optional(&info.first_header),
                    json_optional(&info.first_footer)
                );
            }
            println!("]");
        } else {
            for (sheet_name, info) in &entries {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                println!("odd_header={}", info.odd_header.as_deref().unwrap_or(""));
                println!("odd_footer={}", info.odd_footer.as_deref().unwrap_or(""));
                println!("even_header={}", info.even_header.as_deref().unwrap_or(""));
                println!(
                    "first_header={}",
                    info.first_header.as_deref().unwrap_or("")
                );
            }
        }
        return 0;
    }

    if print_layout {
        if protection
            || validations
            || formulas
            || conditional_formats
            || sheet_view
            || show_formats
            || show_headers
            || merged_ranges
            || column_widths
            || row_heights
            || sheet_visibility
            || properties
            || page_setup
        {
            eprintln!("error: --print-layout cannot be combined with other inspection modes");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --print-layout needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --print-layout cannot be combined with output flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --print-layout --all-sheets cannot be combined with --sheet");
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
            match csvx_core::xlsx::xlsx_print_layout(&input, sheet_arg) {
                Ok(layout) => entries.push((sheet_arg.unwrap_or("").to_string(), layout)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        }
        if formats_json {
            print!("[");
            for (index, (sheet_name, layout)) in entries.iter().enumerate() {
                if index > 0 {
                    print!(",");
                }
                print!(
                    "{{\"sheet\":{},\"print_area\":{},\"margins\":{}",
                    json_string(sheet_name),
                    json_optional(&layout.print_area),
                    match layout.margins.as_ref() {
                        Some(margins) => format!(
                            "{{\"left\":{},\"right\":{},\"top\":{},\"bottom\":{},\"header\":{},\"footer\":{}}}",
                            margins.left, margins.right, margins.top, margins.bottom, margins.header, margins.footer
                        ),
                        None => "null".to_string(),
                    }
                );
                print!("}}");
            }
            println!("]");
        } else {
            for (sheet_name, layout) in &entries {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                println!(
                    "print_area={} margins={}",
                    layout.print_area.as_deref().unwrap_or(""),
                    layout
                        .margins
                        .as_ref()
                        .map(|m| format!(
                            "left={} right={} top={} bottom={} header={} footer={}",
                            m.left, m.right, m.top, m.bottom, m.header, m.footer
                        ))
                        .unwrap_or_default()
                );
            }
        }
        return 0;
    }

    if page_setup {
        if protection
            || validations
            || formulas
            || conditional_formats
            || sheet_view
            || show_formats
            || show_headers
            || merged_ranges
            || column_widths
            || row_heights
            || sheet_visibility
            || properties
        {
            eprintln!("error: --page-setup cannot be combined with other inspection modes");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --page-setup needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --page-setup cannot be combined with output flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --page-setup --all-sheets cannot be combined with --sheet");
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
            match csvx_core::xlsx::xlsx_page_setup(&input, sheet_arg) {
                Ok(setup) => entries.push((sheet_arg.unwrap_or("").to_string(), setup)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        }
        if formats_json {
            print!("[");
            for (index, (sheet_name, setup)) in entries.iter().enumerate() {
                if index > 0 {
                    print!(",");
                }
                let orientation = match setup.orientation.as_deref() {
                    Some(value) => json_string(value),
                    None => "null".to_string(),
                };
                let field = |value: Option<u32>| match value {
                    Some(value) => value.to_string(),
                    None => "null".to_string(),
                };
                print!(
                    "{{\"sheet\":{},\"orientation\":{},\"paper_size\":{},\"fit_to_width\":{},\"fit_to_height\":{},\"scale\":{}}}",
                    json_string(sheet_name),
                    orientation,
                    field(setup.paper_size),
                    field(setup.fit_to_width),
                    field(setup.fit_to_height),
                    field(setup.scale)
                );
            }
            println!("]");
        } else {
            for (sheet_name, setup) in &entries {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                println!(
                    "orientation={} paper_size={} fit_to_width={} fit_to_height={} scale={}",
                    setup.orientation.as_deref().unwrap_or(""),
                    setup.paper_size.map(|v| v.to_string()).unwrap_or_default(),
                    setup
                        .fit_to_width
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    setup
                        .fit_to_height
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    setup.scale.map(|v| v.to_string()).unwrap_or_default()
                );
            }
        }
        return 0;
    }

    if properties {
        if protection
            || validations
            || formulas
            || conditional_formats
            || sheet_view
            || show_formats
            || show_headers
            || merged_ranges
            || column_widths
            || row_heights
            || sheet_visibility
        {
            eprintln!("error: --properties cannot be combined with other inspection modes");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --properties needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --properties cannot be combined with output flags");
            return 2;
        }
        let core = match csvx_core::xlsx::xlsx_core_properties(&input) {
            Ok(core) => core,
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        };
        if formats_json {
            println!(
                "{{\"title\":{},\"creator\":{},\"created\":{},\"modified\":{}}}",
                json_optional(&core.title),
                json_optional(&core.creator),
                json_optional(&core.created),
                json_optional(&core.modified)
            );
        } else {
            println!("title={}", core.title.as_deref().unwrap_or(""));
            println!("creator={}", core.creator.as_deref().unwrap_or(""));
            println!("created={}", core.created.as_deref().unwrap_or(""));
            println!("modified={}", core.modified.as_deref().unwrap_or(""));
        }
        return 0;
    }

    if sheet_visibility {
        if protection
            || validations
            || formulas
            || conditional_formats
            || sheet_view
            || show_formats
            || show_headers
            || merged_ranges
            || column_widths
            || row_heights
        {
            eprintln!("error: --visibility cannot be combined with other inspection modes");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --visibility needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --visibility cannot be combined with output flags");
            return 2;
        }
        let entries = match csvx_core::xlsx::xlsx_sheet_visibility(&input) {
            Ok(entries) => entries,
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        };
        if formats_json {
            print!("[");
            for (index, entry) in entries.iter().enumerate() {
                if index > 0 {
                    print!(",");
                }
                print!(
                    "{{\"sheet\":{},\"state\":{}}}",
                    json_string(&entry.name),
                    json_string(&entry.state)
                );
            }
            println!("]");
        } else {
            for entry in &entries {
                println!("{}\t{}", entry.name, entry.state);
            }
        }
        return 0;
    }

    if row_heights {
        if protection
            || validations
            || formulas
            || conditional_formats
            || sheet_view
            || show_formats
            || show_headers
            || merged_ranges
            || column_widths
        {
            eprintln!("error: --heights cannot be combined with other inspection modes");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --heights needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --heights cannot be combined with output flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --heights --all-sheets cannot be combined with --sheet");
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
            match csvx_core::xlsx::xlsx_row_heights(&input, sheet_arg) {
                Ok(heights) => entries.push((sheet_arg.unwrap_or("").to_string(), heights)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        }
        if formats_json {
            print!("[");
            for (index, (sheet_name, heights)) in entries.iter().enumerate() {
                if index > 0 {
                    print!(",");
                }
                print!("{{\"sheet\":{},\"rows\":[", json_string(sheet_name));
                for (height_index, height) in heights.iter().enumerate() {
                    if height_index > 0 {
                        print!(",");
                    }
                    print!(
                        "{{\"row\":{},\"height\":{},\"custom_height\":{}}}",
                        height.row, height.height, height.custom_height
                    );
                }
                print!("]}}");
            }
            println!("]");
        } else if entries.iter().all(|(_, heights)| heights.is_empty()) {
            println!("no row heights");
        } else {
            for (sheet_name, heights) in &entries {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                for height in heights {
                    println!("{}\t{}", height.row, height.height);
                }
            }
        }
        return 0;
    }

    if column_widths {
        if protection
            || validations
            || formulas
            || conditional_formats
            || sheet_view
            || show_formats
            || show_headers
            || merged_ranges
        {
            eprintln!("error: --widths cannot be combined with other inspection modes");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --widths needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --widths cannot be combined with output flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --widths --all-sheets cannot be combined with --sheet");
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
            match csvx_core::xlsx::xlsx_column_widths(&input, sheet_arg) {
                Ok(widths) => entries.push((sheet_arg.unwrap_or("").to_string(), widths)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        }
        if formats_json {
            print!("[");
            for (index, (sheet_name, widths)) in entries.iter().enumerate() {
                if index > 0 {
                    print!(",");
                }
                print!("{{\"sheet\":{},\"columns\":[", json_string(sheet_name));
                for (width_index, width) in widths.iter().enumerate() {
                    if width_index > 0 {
                        print!(",");
                    }
                    print!(
                        "{{\"column\":{},\"width\":{},\"custom_width\":{}}}",
                        json_string(&width.column),
                        width.width,
                        width.custom_width
                    );
                }
                print!("]}}");
            }
            println!("]");
        } else if entries.iter().all(|(_, widths)| widths.is_empty()) {
            println!("no column widths");
        } else {
            for (sheet_name, widths) in &entries {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                for width in widths {
                    println!("{}\t{}", width.column, width.width);
                }
            }
        }
        return 0;
    }

    if merged_ranges {
        if protection
            || validations
            || formulas
            || conditional_formats
            || sheet_view
            || show_formats
            || show_headers
        {
            eprintln!("error: --merged cannot be combined with other inspection modes");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --merged needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --merged cannot be combined with output flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --merged --all-sheets cannot be combined with --sheet");
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
            match csvx_core::xlsx::xlsx_merged_ranges(&input, sheet_arg) {
                Ok(ranges) => entries.push((sheet_arg.unwrap_or("").to_string(), ranges)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        }
        if formats_json {
            print!("[");
            for (index, (sheet_name, ranges)) in entries.iter().enumerate() {
                if index > 0 {
                    print!(",");
                }
                print!("{{\"sheet\":{},\"ranges\":[", json_string(sheet_name));
                for (range_index, range) in ranges.iter().enumerate() {
                    if range_index > 0 {
                        print!(",");
                    }
                    print!("{}", json_string(range));
                }
                print!("]}}");
            }
            println!("]");
        } else if entries.iter().all(|(_, ranges)| ranges.is_empty()) {
            println!("no merged cells");
        } else {
            for (sheet_name, ranges) in &entries {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                for range in ranges {
                    println!("{range}");
                }
            }
        }
        return 0;
    }

    if show_headers {
        if protection
            || validations
            || formulas
            || conditional_formats
            || sheet_view
            || show_formats
        {
            eprintln!("error: --headers cannot be combined with other inspection modes");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --headers needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --headers cannot be combined with output flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --headers --all-sheets cannot be combined with --sheet");
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
            match csvx_core::xlsx::xlsx_header_names(&input, sheet_arg) {
                Ok(headers) => entries.push((sheet_arg.unwrap_or("").to_string(), headers)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        }
        if formats_json {
            print!("[");
            for (index, (sheet_name, headers)) in entries.iter().enumerate() {
                if index > 0 {
                    print!(",");
                }
                print!("{{\"sheet\":{},\"headers\":[", json_string(sheet_name));
                for (cell_index, (column, header)) in headers.iter().enumerate() {
                    if cell_index > 0 {
                        print!(",");
                    }
                    print!(
                        "{{\"column\":{},\"name\":{}}}",
                        json_string(column),
                        json_string(header)
                    );
                }
                print!("]}}");
            }
            println!("]");
        } else if entries.iter().all(|(_, headers)| headers.is_empty()) {
            println!("no headers");
        } else {
            for (sheet_name, headers) in &entries {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                for (column, header) in headers {
                    println!("{column}\t{header}");
                }
            }
        }
        return 0;
    }

    if sheet_view {
        if protection || validations || formulas || conditional_formats || show_formats {
            eprintln!("error: --sheet-view cannot be combined with other inspection modes");
            return 2;
        }
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(name.ends_with(".xlsx") || name.ends_with(".xlsm")) {
            eprintln!("error: --sheet-view needs an .xlsx/.xlsm input");
            return 2;
        }
        if convert_format.is_some() || out.is_some() {
            eprintln!("error: --sheet-view cannot be combined with output flags");
            return 2;
        }
        if sheet.is_some() && all_sheets {
            eprintln!("error: --sheet-view --all-sheets cannot be combined with --sheet");
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
            match csvx_core::xlsx::xlsx_sheet_view(&input, sheet_arg) {
                Ok(view) => entries.push((sheet_arg.unwrap_or("").to_string(), view)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            }
        }
        if formats_json {
            print!("[");
            for (index, (sheet_name, view)) in entries.iter().enumerate() {
                if index > 0 {
                    print!(",");
                }
                print!(
                    "{{\"sheet\":{},\"frozen_rows\":{},\"frozen_columns\":{},\"auto_filter\":{}}}",
                    json_string(sheet_name),
                    view.frozen_rows,
                    view.frozen_columns,
                    json_optional(&view.auto_filter),
                );
            }
            println!("]");
        } else {
            for (sheet_name, view) in &entries {
                if all_sheets {
                    println!("# {sheet_name}");
                }
                println!(
                    "frozen_rows={} frozen_columns={} auto_filter={}",
                    view.frozen_rows,
                    view.frozen_columns,
                    view.auto_filter.as_deref().unwrap_or("")
                );
            }
        }
        return 0;
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
        if highlight_text {
            let headers = if all_sheets {
                Vec::new()
            } else {
                match csvx_core::xlsx::xlsx_header_names(&input, sheet.as_deref()) {
                    Ok(headers) => headers,
                    Err(error) => {
                        eprintln!("error: {error}");
                        return 1;
                    }
                }
            };
            let mut highlights: Vec<String> = Vec::new();
            for (_, rule) in &entries {
                let operator = match rule.operator.as_deref() {
                    Some("greaterThan") => ">",
                    Some("greaterThanOrEqual") => ">=",
                    Some("lessThan") => "<",
                    Some("lessThanOrEqual") => "<=",
                    Some("equal") => "=",
                    Some("notEqual") => "!=",
                    _ => continue,
                };
                let Some(value) = rule.value.as_deref() else {
                    continue;
                };
                let Some(color) = rule.color.as_deref().and_then(color_name) else {
                    continue;
                };
                let letter = rule
                    .range
                    .split(':')
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take_while(|ch| ch.is_ascii_alphabetic())
                    .collect::<String>();
                let column = headers
                    .iter()
                    .find(|(cell, _)| cell == &letter)
                    .map(|(_, name)| name.as_str())
                    .unwrap_or(&letter);
                if column.is_empty() {
                    continue;
                }
                highlights.push(format!("{column}{operator}{value}:{color}"));
            }
            if formats_json {
                print!("[");
                for (index, item) in highlights.iter().enumerate() {
                    if index > 0 {
                        print!(",");
                    }
                    print!("{}", json_string(item));
                }
                println!("]");
            } else {
                for item in highlights {
                    println!("{item}");
                }
            }
            return 0;
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

    if highlight_text && !conditional_formats {
        eprintln!("error: --highlight-text is only valid together with --conditional-formats");
        return 2;
    }

    if validation_checks && !validations {
        eprintln!("error: --checks is only valid together with --validations");
        return 2;
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
            if validation_checks {
                let mut rules: Vec<String> = Vec::new();
                for sheet_name in &names {
                    let sheet_arg = (!sheet_name.is_empty()).then_some(sheet_name.as_str());
                    let headers = match csvx_core::xlsx::xlsx_header_names(&input, sheet_arg) {
                        Ok(headers) => headers,
                        Err(error) => {
                            eprintln!("error: {error}");
                            return 1;
                        }
                    };
                    for entry in entries
                        .iter()
                        .filter(|entry| !all_sheets || entry.sheet == sheet_name.as_str())
                    {
                        let header = headers
                            .iter()
                            .find(|(letter, _)| letter == &entry.column)
                            .map(|(_, name)| name.as_str())
                            .unwrap_or(&entry.column);
                        match entry.kind.as_str() {
                            "list" => {
                                if let Some(formula) = entry.formula1.as_deref() {
                                    if let Some(inner) = formula
                                        .trim()
                                        .strip_prefix('"')
                                        .and_then(|text| text.strip_suffix('"'))
                                    {
                                        if !inner.trim().is_empty() {
                                            rules.push(format!("{header} in {inner}"));
                                        }
                                    }
                                }
                            }
                            "whole" | "decimal" => {
                                if let Some(min) = entry.formula1.as_deref() {
                                    rules.push(format!("{header}>={min}"));
                                }
                                if let Some(max) = entry.formula2.as_deref() {
                                    rules.push(format!("{header}<={max}"));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if formats_json {
                    print!("[");
                    for (index, rule) in rules.iter().enumerate() {
                        if index > 0 {
                            print!(",");
                        }
                        print!("{}", json_string(rule));
                    }
                    println!("]");
                } else if rules.is_empty() {
                    println!("no enforceable data validation checks");
                } else {
                    for rule in rules {
                        println!("{rule}");
                    }
                }
                return 0;
            }

            let headers_by_sheet: Vec<(String, Vec<(String, String)>)> = names
                .iter()
                .filter_map(|sheet_name| {
                    let sheet_arg = (!sheet_name.is_empty()).then_some(sheet_name.as_str());
                    csvx_core::xlsx::xlsx_header_names(&input, sheet_arg)
                        .ok()
                        .map(|headers| (sheet_name.clone(), headers))
                })
                .collect();
            let header_for = |sheet: &str, letter: &str| -> Option<String> {
                headers_by_sheet
                    .iter()
                    .find(|(name, _)| name == sheet || name.is_empty())
                    .and_then(|(_, headers)| {
                        headers
                            .iter()
                            .find(|(cell, _)| cell == letter)
                            .map(|(_, header)| header.clone())
                    })
            };
            if formats_json {
                print!("[");
                for (index, entry) in entries.iter().enumerate() {
                    if index > 0 {
                        print!(",");
                    }
                    print!(
                    "{{\"sheet\":{},\"column\":{},\"header\":{},\"range\":{},\"type\":{},\"operator\":{},\"formula1\":{},\"formula2\":{},\"allow_blank\":{},\"prompt_title\":{},\"prompt_message\":{},\"error_title\":{},\"error_message\":{},\"error_style\":{}}}",
                    json_string(&entry.sheet),
                    json_string(&entry.column),
                    json_optional(&header_for(&entry.sheet, &entry.column)),
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
                    header_for(&entry.sheet, &entry.column).unwrap_or_else(|| entry.column.clone()),
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
                    let custom =
                        matches!(format, Some(csvx_core::layout::NumberFormat::Custom { .. }));
                    let code = format
                        .as_ref()
                        .map(|format| format.name())
                        .unwrap_or_else(|| "plain".into());
                    print!(
                        "{{\"name\":{},\"format\":{},\"custom\":{},\"column_index\":{}}}",
                        json_string(column),
                        json_string(&code),
                        custom,
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
        let doc_properties = if title.is_some() || author.is_some() {
            if target != "xlsx" {
                eprintln!("error: --title/--author need an xlsx output");
                return 2;
            }
            if input.is_dir() {
                eprintln!("error: --title/--author apply to single-file xlsx exports");
                return 2;
            }
            Some(csvx_core::xlsx::CoreProperties {
                title: title.clone(),
                creator: author.clone(),
                created: None,
                modified: None,
            })
        } else {
            None
        };

        let merge_ranges = if merges.is_empty() {
            None
        } else {
            if target != "xlsx" {
                eprintln!("error: --merge needs an xlsx output");
                return 2;
            }
            if input.is_dir() {
                eprintln!("error: --merge applies to single-file xlsx exports");
                return 2;
            }
            Some(merges.clone())
        };

        let hidden_sheets = if hide_sheets.is_empty() {
            None
        } else {
            if target != "xlsx" {
                eprintln!("error: --hide-sheet needs an xlsx output");
                return 2;
            }
            if input.is_dir() {
                eprintln!("error: --hide-sheet applies to single-file xlsx exports");
                return 2;
            }
            Some(hide_sheets.clone())
        };

        let print_area_value = if let Some(area) = print_area.as_ref() {
            if target != "xlsx" {
                eprintln!("error: --print-area needs an xlsx output");
                return 2;
            }
            if input.is_dir() {
                eprintln!("error: --print-area applies to single-file xlsx exports");
                return 2;
            }
            Some(area.clone())
        } else {
            None
        };

        let page_setup_value = if orientation.is_some()
            || paper_size.is_some()
            || fit_to_width.is_some()
            || fit_to_height.is_some()
            || scale.is_some()
        {
            if target != "xlsx" {
                eprintln!(
                    "error: --orientation/--paper-size/--fit-to-*/--scale need an xlsx output"
                );
                return 2;
            }
            if input.is_dir() {
                eprintln!("error: page setup flags apply to single-file xlsx exports");
                return 2;
            }
            Some(csvx_core::xlsx::PageSetupInfo {
                orientation: orientation.clone(),
                paper_size,
                fit_to_width,
                fit_to_height,
                scale,
            })
        } else {
            None
        };

        let margins_value = if margin_left.is_some()
            || margin_right.is_some()
            || margin_top.is_some()
            || margin_bottom.is_some()
            || margin_header.is_some()
            || margin_footer.is_some()
        {
            if target != "xlsx" {
                eprintln!("error: --margin-* flags need an xlsx output");
                return 2;
            }
            if input.is_dir() {
                eprintln!("error: --margin-* flags apply to single-file xlsx exports");
                return 2;
            }
            Some(csvx_core::xlsx::PageMargins {
                left: margin_left.unwrap_or(0.7),
                right: margin_right.unwrap_or(0.7),
                top: margin_top.unwrap_or(0.75),
                bottom: margin_bottom.unwrap_or(0.75),
                header: margin_header.unwrap_or(0.3),
                footer: margin_footer.unwrap_or(0.3),
            })
        } else {
            None
        };

        let header_footer_value = if header_text.is_some() || footer_text.is_some() {
            if target != "xlsx" {
                eprintln!("error: --header/--footer need an xlsx output");
                return 2;
            }
            if input.is_dir() {
                eprintln!("error: --header/--footer apply to single-file xlsx exports");
                return 2;
            }
            Some(csvx_core::xlsx::HeaderFooterInfo {
                odd_header: header_text.clone(),
                odd_footer: footer_text.clone(),
                even_header: None,
                even_footer: None,
                first_header: None,
                first_footer: None,
            })
        } else {
            None
        };

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
                if let Some(properties) = doc_properties.as_ref() {
                    if let Err(error) = csvx_core::xlsx::xlsx_set_core_properties(&out, properties)
                    {
                        eprintln!("error: {error}");
                        return 1;
                    }
                }
                if let Some(ranges) = merge_ranges.as_ref() {
                    if let Err(error) = csvx_core::xlsx::xlsx_set_merge_cells(&out, ranges) {
                        eprintln!("error: {error}");
                        return 1;
                    }
                }
                if let Some(names) = hidden_sheets.as_ref() {
                    if let Err(error) = csvx_core::xlsx::xlsx_hide_sheets(&out, names) {
                        eprintln!("error: {error}");
                        return 1;
                    }
                }
                if let Some(area) = print_area_value.as_ref() {
                    if let Err(error) = csvx_core::xlsx::xlsx_set_print_area(&out, area) {
                        eprintln!("error: {error}");
                        return 1;
                    }
                }
                if let Some(setup) = page_setup_value.as_ref() {
                    if let Err(error) = csvx_core::xlsx::xlsx_set_page_setup(&out, setup) {
                        eprintln!("error: {error}");
                        return 1;
                    }
                }
                if let Some(margins) = margins_value.as_ref() {
                    if let Err(error) = csvx_core::xlsx::xlsx_set_page_margins(&out, margins) {
                        eprintln!("error: {error}");
                        return 1;
                    }
                }
                if let Some(info) = header_footer_value.as_ref() {
                    if let Err(error) = csvx_core::xlsx::xlsx_set_header_footer(&out, info) {
                        eprintln!("error: {error}");
                        return 1;
                    }
                }
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
