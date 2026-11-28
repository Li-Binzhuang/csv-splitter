//! Minimal, dependency-free `.xlsx` writer.
//!
//! Writing a workbook is just a ZIP of XML parts. This module streams the CSV
//! into `xl/worksheets/sheet1.xml` so a large export does not build the whole
//! sheet in memory first. It deliberately does not write formulas, charts or
//! conditional formatting; values and a bold frozen header are enough for the
//! first Excel hand-off.

use crate::encoding::open_decoded;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use regex::Regex;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

/// Excel's hard limit for data rows in one worksheet (excluding the header).
pub const MAX_DATA_ROWS: u64 = 1_048_575;

/// Excel's hard limit for rows in one worksheet, including the header row.
pub const MAX_SHEET_ROWS: u64 = MAX_DATA_ROWS + 1;

/// Excel's hard limit for columns in one worksheet.
pub const MAX_COLUMNS: usize = 16_384;

/// One worksheet's input: headers plus rows already in memory.
pub struct Sheet {
    pub name: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// One cell-highlighting rule, rendered as Excel conditional formatting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConditionalRule {
    pub column: String,
    pub operator: String,
    pub value: String,
    pub color: String,
}

/// Excel numeric validation kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumericKind {
    Whole,
    Decimal,
    Date,
    Time,
}

/// Bounds for a whole/decimal data validation; either bound may be omitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumericValidation {
    pub kind: NumericKind,
    pub min: Option<String>,
    pub max: Option<String>,
}

/// A list-style Excel data validation (a dropdown of allowed values).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataValidation {
    pub column: String,
    pub values: Vec<String>,
    /// Custom validation formula used instead of the list, e.g. `AND(A2>0,B2<100)`.
    pub formula: Option<String>,
    /// Optional input-message title shown when the cell is selected.
    pub prompt_title: Option<String>,
    /// Optional input-message body shown when the cell is selected.
    pub prompt_message: Option<String>,
    /// Optional error-dialog title shown for an invalid value.
    pub error_title: Option<String>,
    /// Optional error-dialog body shown for an invalid value.
    pub error_message: Option<String>,
    /// Whether an empty cell passes validation.
    pub allow_blank: bool,
    /// Whole/decimal range validation; replaces list/custom when present.
    pub numeric: Option<NumericValidation>,
    /// Error dialog style: stop (default), warning, or information.
    pub error_style: Option<String>,
    /// List source as an Excel range reference, e.g. `$A$2:$A$10`.
    pub list_range: Option<String>,
}

/// Read a worksheet's header names and their Excel number formats.
///
/// This is a metadata read: values are not returned. The header row is parsed
/// from the streamed worksheet so a million-row file does not build a table.
pub fn xlsx_column_formats(
    input: &Path,
    sheet: Option<&str>,
) -> std::io::Result<Vec<(String, Option<crate::layout::NumberFormat>)>> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target.clone()
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let styles = read_entry(&mut archive, "xl/styles.xml").unwrap_or_default();
    let style_formats = style_number_formats(&styles);
    let shared = read_entry(&mut archive, "xl/sharedStrings.xml").unwrap_or_default();
    let shared = shared_strings(&shared);
    let date_styles = date_style_map(&styles);
    let date1904 = workbook.contains("date1904=\"1\"") || workbook.contains("date1904=\"true\"");
    let mut header: Option<Vec<String>> = None;
    let mut captured_data = false;
    let mut resolved = false;
    let mut result = Vec::new();
    {
        let mut entry = archive
            .by_name(&normalized)
            .map_err(|error| invalid(&format!("xlsx missing {normalized}: {error}")))?;
        for_each_zip_row(&mut entry, |fragment| {
            let (_, cells) = row_from_fragment(fragment, &shared, &date_styles, date1904)?;
            if header.is_none() {
                header = Some(cells);
                return Ok(());
            }
            if resolved {
                return Ok(());
            }
            let styles = cell_style_indices(fragment);
            let row = column_formats_for(header.as_ref().unwrap(), &styles, &style_formats);
            if has_any_format(&row) {
                resolved = true;
            }
            if !captured_data || resolved {
                result = row;
                captured_data = true;
            }
            Ok(())
        })?;
    }
    let Some(header) = header else {
        return Err(invalid("xlsx worksheet has no header row"));
    };
    if result.is_empty() {
        for name in header {
            result.push((name, None));
        }
    }
    Ok(result)
}

fn cell_style_indices(fragment: &str) -> Vec<usize> {
    let mut styles = Vec::new();
    let mut cursor = 0;
    while let Some(start) = find_tag(fragment, cursor, "<c") {
        let Some(open_end) = fragment[start..].find('>').map(|index| start + index) else {
            break;
        };
        let attrs = &fragment[start + 2..open_end];
        styles.push(
            attr(attrs, "s")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
        );
        cursor = open_end + 1;
    }
    styles
}

/// Excel's built-in date/time number-format ids (14..22) with the closest
/// pattern this engine can display; unknown built-ins stay plain.
fn builtin_number_format(id: u32) -> Option<&'static str> {
    match id {
        14..=17 => Some("yyyy-mm-dd"),
        18 | 20 => Some("hh:mm"),
        19 | 21 => Some("hh:mm:ss"),
        22 => Some("yyyy-mm-dd hh:mm"),
        _ => None,
    }
}

fn style_number_formats(styles: &str) -> Vec<Option<crate::layout::NumberFormat>> {
    let mut custom = std::collections::HashMap::new();
    let num_fmt_re = Regex::new(r#"(?s)<numFmt\b([^>]*?)(?:/>|>)"#).unwrap();
    for captures in num_fmt_re.captures_iter(styles) {
        let attrs = &captures[1];
        let Some(id) = attr(attrs, "numFmtId").and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        if let Some(code) = attr(attrs, "formatCode") {
            custom.insert(id, code);
        }
    }
    let block_re = Regex::new(r#"(?s)<cellXfs\b[^>]*>(.*?)</cellXfs>"#).unwrap();
    let Some(block) = block_re.captures(styles) else {
        return Vec::new();
    };
    let xf_re = Regex::new(r#"(?s)<xf\b([^>]*?)(?:/>|>)"#).unwrap();
    xf_re
        .captures_iter(&block[1])
        .map(|captures| {
            let id = attr(&captures[1], "numFmtId").and_then(|value| value.parse::<u32>().ok())?;
            let code = custom
                .get(&id)
                .cloned()
                .or_else(|| builtin_number_format(id).map(str::to_string))?;
            crate::layout::NumberFormat::parse(&code)
                .or_else(|_| crate::layout::NumberFormat::parse_raw(&code))
                .ok()
        })
        .collect()
}

fn column_formats_for(
    header: &[String],
    styles: &[usize],
    style_formats: &[Option<crate::layout::NumberFormat>],
) -> Vec<(String, Option<crate::layout::NumberFormat>)> {
    header
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let style = styles.get(index).copied().unwrap_or(0);
            (name.clone(), style_formats.get(style).cloned().flatten())
        })
        .collect()
}

fn has_any_format(columns: &[(String, Option<crate::layout::NumberFormat>)]) -> bool {
    columns.iter().any(|(_, format)| format.is_some())
}

/// Export a CSV file as a single-sheet workbook, streaming rows.
pub fn csv_to_xlsx(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
) -> std::io::Result<()> {
    csv_to_xlsx_with_rules(input, output, delimiter, tx, &[])
}

/// Export a CSV and add Excel conditional-formatting rules for some columns.
pub fn csv_to_xlsx_with_rules(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
    rules: &[ConditionalRule],
) -> std::io::Result<()> {
    csv_to_xlsx_with_number_formats(input, output, delimiter, tx, rules, &[])
}

/// Export a CSV as one xlsx sheet with per-column Excel number formats.
pub fn csv_to_xlsx_with_number_formats(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
    rules: &[ConditionalRule],
    formats: &[(String, crate::layout::NumberFormat)],
) -> std::io::Result<()> {
    let result = csv_to_xlsx_inner(
        input,
        output,
        delimiter,
        tx,
        rules,
        formats,
        &[],
        &[],
        true,
        true,
        false,
        None,
        true,
        true,
    );
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

/// Export a CSV-as-xlsx sheet without the header auto-filter.
pub fn csv_to_xlsx_without_auto_filter(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
    rules: &[ConditionalRule],
    formats: &[(String, crate::layout::NumberFormat)],
) -> std::io::Result<()> {
    let result = csv_to_xlsx_inner(
        input,
        output,
        delimiter,
        tx,
        rules,
        formats,
        &[],
        &[],
        true,
        false,
        false,
        None,
        true,
        true,
    );
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

/// Export a CSV-as-xlsx sheet with explicit freeze and auto-filter choices.
#[allow(clippy::too_many_arguments)]
pub fn csv_to_xlsx_with_options(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
    rules: &[ConditionalRule],
    formats: &[(String, crate::layout::NumberFormat)],
    freeze_header: bool,
    auto_filter: bool,
) -> std::io::Result<()> {
    let result = csv_to_xlsx_inner(
        input,
        output,
        delimiter,
        tx,
        rules,
        formats,
        &[],
        &[],
        freeze_header,
        auto_filter,
        false,
        None,
        true,
        true,
    );
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

/// Export a CSV-as-xlsx sheet with all optional features at once.
#[allow(clippy::too_many_arguments)]
pub fn csv_to_xlsx_with_all(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
    rules: &[ConditionalRule],
    formats: &[(String, crate::layout::NumberFormat)],
    validations: &[DataValidation],
    widths: &[(String, f64)],
    freeze_header: bool,
    auto_filter: bool,
    protect: bool,
    protect_password: Option<&str>,
    protect_objects: bool,
    protect_scenarios: bool,
) -> std::io::Result<()> {
    let result = csv_to_xlsx_inner(
        input,
        output,
        delimiter,
        tx,
        rules,
        formats,
        validations,
        widths,
        freeze_header,
        auto_filter,
        protect,
        protect_password,
        protect_objects,
        protect_scenarios,
    );
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

/// Export a CSV as one xlsx sheet with explicit column widths.
pub fn csv_to_xlsx_with_column_widths(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
    widths: &[(String, f64)],
) -> std::io::Result<()> {
    let result = csv_to_xlsx_inner(
        input,
        output,
        delimiter,
        tx,
        &[],
        &[],
        &[],
        widths,
        true,
        true,
        false,
        None,
        true,
        true,
    );
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

/// Export a CSV as one xlsx sheet with worksheet protection enabled.
pub fn csv_to_xlsx_with_protection(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
) -> std::io::Result<()> {
    let result = csv_to_xlsx_inner(
        input,
        output,
        delimiter,
        tx,
        &[],
        &[],
        &[],
        &[],
        true,
        true,
        true,
        None,
        true,
        true,
    );
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

/// Export a CSV as one xlsx sheet with dropdown data validation.
pub fn csv_to_xlsx_with_validations(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
    validations: &[DataValidation],
) -> std::io::Result<()> {
    let result = csv_to_xlsx_inner(
        input,
        output,
        delimiter,
        tx,
        &[],
        &[],
        validations,
        &[],
        true,
        true,
        false,
        None,
        true,
        true,
    );
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

/// Export a CSV-as-xlsx sheet without freezing the header row.
pub fn csv_to_xlsx_without_freeze(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
    rules: &[ConditionalRule],
    formats: &[(String, crate::layout::NumberFormat)],
) -> std::io::Result<()> {
    let result = csv_to_xlsx_inner(
        input,
        output,
        delimiter,
        tx,
        rules,
        formats,
        &[],
        &[],
        false,
        true,
        false,
        None,
        true,
        true,
    );
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn csv_to_xlsx_inner(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
    rules: &[ConditionalRule],
    formats: &[(String, crate::layout::NumberFormat)],
    validations: &[DataValidation],
    widths: &[(String, f64)],
    freeze_header: bool,
    auto_filter: bool,
    protect: bool,
    protect_password: Option<&str>,
    protect_objects: bool,
    protect_scenarios: bool,
) -> std::io::Result<()> {
    let sheet_name = workbook_sheet_name(input);
    let total_bytes = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseStart {
        phase: "Writing xlsx".into(),
        total_bytes,
    })
    .ok();

    let reader = open_decoded(input)?;
    let mut records = crate::csv_reader::RecordReader::new(
        BufReader::with_capacity(4 * 1024 * 1024, reader),
        4 * 1024 * 1024,
    );
    let mut record = Vec::new();
    if !records.next_record(&mut record)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "xlsx export needs a header row",
        ));
    }
    let headers = crate::csv_reader::parse_csv_record(
        String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
        delimiter.byte(),
    );
    if headers.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "xlsx export needs a non-empty header",
        ));
    }

    let file = std::fs::File::create(output)?;
    let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(4 * 1024 * 1024, file));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);

    write_part(&mut zip, "[Content_Types].xml", &content_types(), options)?;
    write_part(&mut zip, "_rels/.rels", root_rels(), options)?;
    write_part(
        &mut zip,
        "xl/workbook.xml",
        &workbook_xml(&sheet_name),
        options,
    )?;
    write_part(
        &mut zip,
        "xl/_rels/workbook.xml.rels",
        workbook_rels(),
        options,
    )?;
    let (styles, style_map) = styles_xml_with_rules_and_formats(rules, formats);
    write_part(&mut zip, "xl/styles.xml", &styles, options)?;

    zip.start_file("xl/worksheets/sheet1.xml", options)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut sheet = String::new();
    sheet.push_str(XML_HEADER);
    sheet.push_str(
        "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    );
    // No dimension hint: the streamed row count is unknown until the sheet is
    // complete, and a false A1 hint is worse than letting Excel calculate it.
    if freeze_header {
        sheet.push_str("<sheetViews><sheetView workbookViewId=\"0\"><pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" state=\"frozen\"/></sheetView></sheetViews>");
    }
    if !widths.is_empty() {
        let columns = sheet_columns_xml(&headers, &[], widths);
        sheet.push_str(&columns);
    }
    sheet.push_str("<sheetFormatPr defaultRowHeight=\"15\"/><sheetData>");
    write!(zip, "{}", sheet)?;

    // Header row.
    write!(zip, "<row r=\"1\">")?;
    for (index, header) in headers.iter().enumerate() {
        write_cell(&mut zip, index, 1, header, true)?;
    }
    write!(zip, "</row>")?;

    let mut data_rows: u64 = 0;
    loop {
        if !records.next_record(&mut record)? {
            break;
        }
        let cells = crate::csv_reader::parse_csv_record(
            String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
            delimiter.byte(),
        );
        if cells.iter().all(|cell| cell.is_empty()) {
            continue;
        }
        data_rows += 1;
        if data_rows > MAX_DATA_ROWS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("xlsx supports at most {MAX_DATA_ROWS} data rows in one sheet"),
            ));
        }
        write!(zip, "<row r=\"{}\">", data_rows + 1)?;
        for (index, cell) in cells.iter().enumerate() {
            let format = formats
                .iter()
                .find(|(header, _)| header == &headers[index])
                .map(|(_, format)| format);
            match format {
                Some(format) => {
                    let code = format.name();
                    let style = style_map.get(&code).copied();
                    let mut xml = String::new();
                    write_formatted_cell(&mut xml, index, data_rows + 1, cell, format, style);
                    write!(zip, "{xml}")?;
                }
                None => write_cell(&mut zip, index, data_rows + 1, cell, false)?,
            }
        }
        write!(zip, "</row>")?;
        if data_rows.is_multiple_of(100_000) {
            tx.send(ProgressEvent::Info(format!("xlsx rows: {data_rows}")))
                .ok();
        }
    }
    write!(zip, "</sheetData>")?;
    if protect {
        match protect_password {
            Some(password) => write!(
                zip,
                "<sheetProtection sheet=\"1\" objects=\"{}\" scenarios=\"{}\" password=\"{}\" />",
                u8::from(protect_objects),
                u8::from(protect_scenarios),
                sheet_password_hash(password)
            )?,
            None => write!(
                zip,
                "<sheetProtection sheet=\"1\" objects=\"{}\" scenarios=\"{}\"/>",
                u8::from(protect_objects),
                u8::from(protect_scenarios)
            )?,
        }
    }
    if auto_filter && data_rows > 0 && !headers.is_empty() {
        let last_column = column_name(headers.len() - 1);
        write!(
            zip,
            "<autoFilter ref=\"A1:{last_column}{}\"/>",
            data_rows + 1
        )?;
    }
    if !rules.is_empty() && data_rows > 0 {
        let xml = conditional_formatting_xml(rules, &headers, data_rows + 1)?;
        write!(zip, "{}", xml)?;
    }
    if !validations.is_empty() && data_rows > 0 {
        let xml = data_validations_xml(validations, &headers, data_rows + 1)?;
        write!(zip, "{}", xml)?;
    }
    write!(zip, "</worksheet>")?;
    zip.finish()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    tx.send(ProgressEvent::PhaseDone {
        phase: "Writing xlsx".into(),
    })
    .ok();
    tx.send(ProgressEvent::Finished {
        output: output.to_path_buf(),
    })
    .ok();
    Ok(())
}

/// Write multiple in-memory sheets (used by report/agent exports later).
pub fn write_sheets(output: &Path, sheets: &[Sheet]) -> std::io::Result<()> {
    write_sheets_with_formats(output, sheets, &[])
}

/// Write sheets with per-column formats and explicit freeze/auto-filter choices.
pub fn write_sheets_with_options_and_formats(
    output: &Path,
    sheets: &[Sheet],
    formats: &[Vec<crate::layout::NumberFormat>],
    freeze_header: bool,
    auto_filter: bool,
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        formats,
        &[],
        freeze_header,
        auto_filter,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets with explicit freeze and auto-filter choices.
pub fn write_sheets_with_options(
    output: &Path,
    sheets: &[Sheet],
    freeze_header: bool,
    auto_filter: bool,
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        &[],
        freeze_header,
        auto_filter,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets without a header auto-filter.
pub fn write_sheets_without_auto_filter(output: &Path, sheets: &[Sheet]) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        &[],
        true,
        false,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets without freezing the header row on any sheet.
pub fn write_sheets_without_freeze(output: &Path, sheets: &[Sheet]) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        &[],
        false,
        true,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets and map each column's display format into an Excel cell style.
///
/// Formats are per sheet and per column: `formats[sheet][column]`. A missing
/// entry means plain text/number. The mapping is intentionally the same
/// `NumberFormat` used by the viewer, so `#,##0.00`, `0.0%`, `$#,##0.00`
/// and date/time codes do not get invented twice.
/// Write sheets with worksheet protection and a password hash on every sheet.
pub fn write_sheets_with_protection_password(
    output: &Path,
    sheets: &[Sheet],
    password: &str,
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        &[],
        true,
        true,
        true,
        Some(password),
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets with worksheet protection on every sheet.
pub fn write_sheets_with_protection(output: &Path, sheets: &[Sheet]) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        &[],
        true,
        true,
        true,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets with per-sheet formats, widths, freeze/filter and protection.
#[allow(clippy::too_many_arguments)]
pub fn write_sheets_with_formats_widths_and_protection(
    output: &Path,
    sheets: &[Sheet],
    formats: &[Vec<crate::layout::NumberFormat>],
    widths: &[Vec<(String, f64)>],
    freeze_header: bool,
    auto_filter: bool,
    protect: bool,
    protect_password: Option<&str>,
    protect_objects: bool,
    protect_scenarios: bool,
    rules: &[ConditionalRule],
    validations: &[DataValidation],
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        formats,
        widths,
        freeze_header,
        auto_filter,
        protect,
        protect_password,
        protect_objects,
        protect_scenarios,
        rules,
        validations,
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets with per-sheet visibility states (`visible`/`hidden`/`veryHidden`).
pub fn write_sheets_with_visibility(
    output: &Path,
    sheets: &[Sheet],
    sheet_states: &[String],
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        &[],
        true,
        true,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        sheet_states,
    )
}

/// Write sheets with document core properties (title/creator/dates).
pub fn write_sheets_with_properties(
    output: &Path,
    sheets: &[Sheet],
    properties: &CoreProperties,
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        &[],
        true,
        true,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        Some(properties),
        &[],
    )
}

/// Write sheets with merged-cell ranges (`A1:B1`) applied per sheet.
pub fn write_sheets_with_merges(
    output: &Path,
    sheets: &[Sheet],
    merges: &[Vec<String>],
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        &[],
        true,
        true,
        false,
        None,
        true,
        true,
        &[],
        &[],
        merges,
        &[],
        None,
        &[],
    )
}

/// Write sheets with explicit row heights (`(row_number, height)`).
pub fn write_sheets_with_heights(
    output: &Path,
    sheets: &[Sheet],
    row_heights: &[Vec<(u64, f64)>],
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        &[],
        true,
        true,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        row_heights,
        None,
        &[],
    )
}

/// Write sheets with formats, widths and freeze/filter choices.
pub fn write_sheets_with_formats_widths_and_options(
    output: &Path,
    sheets: &[Sheet],
    formats: &[Vec<crate::layout::NumberFormat>],
    widths: &[Vec<(String, f64)>],
    freeze_header: bool,
    auto_filter: bool,
    protect: bool,
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        formats,
        widths,
        freeze_header,
        auto_filter,
        protect,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets with formats plus freeze/filter choices.
pub fn write_sheets_with_formats_and_options(
    output: &Path,
    sheets: &[Sheet],
    formats: &[Vec<crate::layout::NumberFormat>],
    freeze_header: bool,
    auto_filter: bool,
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        formats,
        &[],
        freeze_header,
        auto_filter,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets with per-sheet formats and widths together.
pub fn write_sheets_with_formats_and_widths(
    output: &Path,
    sheets: &[Sheet],
    formats: &[Vec<crate::layout::NumberFormat>],
    widths: &[Vec<(String, f64)>],
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        formats,
        widths,
        true,
        true,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets with widths plus freeze/filter choices.
pub fn write_sheets_with_widths_and_options(
    output: &Path,
    sheets: &[Sheet],
    widths: &[Vec<(String, f64)>],
    freeze_header: bool,
    auto_filter: bool,
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        widths,
        freeze_header,
        auto_filter,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets with widths, freeze/filter choices and a protection password.
#[allow(clippy::too_many_arguments)]
pub fn write_sheets_with_options_and_password(
    output: &Path,
    sheets: &[Sheet],
    widths: &[Vec<(String, f64)>],
    freeze_header: bool,
    auto_filter: bool,
    password: Option<&str>,
    protect_objects: bool,
    protect_scenarios: bool,
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        widths,
        freeze_header,
        auto_filter,
        true,
        password,
        protect_objects,
        protect_scenarios,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

/// Write sheets with per-sheet column width overrides.
pub fn write_sheets_with_widths(
    output: &Path,
    sheets: &[Sheet],
    widths: &[Vec<(String, f64)>],
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        &[],
        widths,
        true,
        true,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

pub fn write_sheets_with_formats(
    output: &Path,
    sheets: &[Sheet],
    formats: &[Vec<crate::layout::NumberFormat>],
) -> std::io::Result<()> {
    write_sheets_inner(
        output,
        sheets,
        formats,
        &[],
        true,
        true,
        false,
        None,
        true,
        true,
        &[],
        &[],
        &[],
        &[],
        None,
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
fn write_sheets_inner(
    output: &Path,
    sheets: &[Sheet],
    formats: &[Vec<crate::layout::NumberFormat>],
    widths: &[Vec<(String, f64)>],
    freeze_header: bool,
    auto_filter: bool,
    protect: bool,
    protect_password: Option<&str>,
    protect_objects: bool,
    protect_scenarios: bool,
    rules: &[ConditionalRule],
    validations: &[DataValidation],
    merges: &[Vec<String>],
    row_heights: &[Vec<(u64, f64)>],
    properties: Option<&CoreProperties>,
    sheet_states: &[String],
) -> std::io::Result<()> {
    if sheets.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "xlsx needs at least one sheet",
        ));
    }
    if !widths.is_empty() && widths.len() != sheets.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "xlsx widths need one entry per sheet",
        ));
    }
    if !formats.is_empty() && formats.len() != sheets.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "xlsx formats need one entry per sheet",
        ));
    }
    if !merges.is_empty() && merges.len() != sheets.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "xlsx merges need one entry per sheet",
        ));
    }
    if !row_heights.is_empty() && row_heights.len() != sheets.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "xlsx row heights need one entry per sheet",
        ));
    }
    if !sheet_states.is_empty() && sheet_states.len() != sheets.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "xlsx sheet states need one entry per sheet",
        ));
    }
    for state in sheet_states {
        if !matches!(state.as_str(), "visible" | "hidden" | "veryHidden") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "xlsx sheet state must be visible, hidden, or veryHidden",
            ));
        }
    }
    let flat_formats: Vec<crate::layout::NumberFormat> = formats
        .iter()
        .flat_map(|columns| columns.iter().cloned())
        .collect();
    let (styles, style_map) = if rules.is_empty() {
        styles_xml_for(&flat_formats)
    } else {
        let pairs: Vec<(String, crate::layout::NumberFormat)> = flat_formats
            .iter()
            .cloned()
            .map(|format| (String::new(), format))
            .collect();
        styles_xml_with_rules_and_formats(rules, &pairs)
    };
    let file = std::fs::File::create(output)?;
    let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(4 * 1024 * 1024, file));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    write_part(
        &mut zip,
        "[Content_Types].xml",
        &content_types_for_with_properties(sheets.len(), properties.is_some()),
        options,
    )?;
    write_part(
        &mut zip,
        "_rels/.rels",
        &root_rels_with_properties(properties.is_some()),
        options,
    )?;
    write_part(&mut zip, "xl/styles.xml", &styles, options)?;
    if let Some(properties) = properties {
        write_part(
            &mut zip,
            "docProps/core.xml",
            &core_properties_xml(properties),
            options,
        )?;
    }

    let mut workbook = String::from(XML_HEADER);
    workbook.push_str("<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><bookViews><workbookView/></bookViews><sheets>");
    let mut rels = String::from(XML_HEADER);
    rels.push_str(
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    let mut used_names = std::collections::HashSet::new();
    for (index, sheet) in sheets.iter().enumerate() {
        let mut name = workbook_sheet_name(std::path::Path::new(&sheet.name));
        let base = name.clone();
        let mut suffix = 2;
        while !used_names.insert(name.clone()) {
            name = format!("{}_{}", base.chars().take(28).collect::<String>(), suffix);
            suffix += 1;
        }
        let id = index + 1;
        let state = sheet_states
            .get(index)
            .filter(|state| state.as_str() != "visible")
            .map(|state| format!(" state=\"{}\"", xml_attr(state)))
            .unwrap_or_default();
        workbook.push_str(&format!(
            "<sheet name=\"{}\" sheetId=\"{id}\"{state} r:id=\"rId{}\"/>",
            xml_attr(&name),
            id + 1
        ));
        rels.push_str(&format!(
            "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{id}.xml\"/>",
            id + 1
        ));
    }
    workbook.push_str("</sheets><calcPr calcId=\"0\"/></workbook>");
    rels.push_str("<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>");
    write_part(&mut zip, "xl/workbook.xml", &workbook, options)?;
    write_part(&mut zip, "xl/_rels/workbook.xml.rels", &rels, options)?;

    for (index, sheet) in sheets.iter().enumerate() {
        let id = index + 1;
        zip.start_file(format!("xl/worksheets/sheet{id}.xml"), options)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut xml = String::from(XML_HEADER);
        xml.push_str(
            "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
        );
        if freeze_header {
            xml.push_str("<sheetViews><sheetView workbookViewId=\"0\"><pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" state=\"frozen\"/></sheetView></sheetViews>");
        }
        let sheet_heights = row_heights.get(index);
        let height_attr = |row: u64| -> String {
            sheet_heights
                .and_then(|heights| heights.iter().find(|(number, _)| *number == row))
                .map(|(_, height)| format!(" ht=\"{height}\" customHeight=\"1\""))
                .unwrap_or_default()
        };
        let sheet_widths = widths.get(index).map(Vec::as_slice).unwrap_or(&[]);
        xml.push_str(&sheet_columns_xml(
            &sheet.headers,
            &sheet.rows,
            sheet_widths,
        ));
        xml.push_str("<sheetData>");
        xml.push_str(&format!("<row r=\"1\"{}>", height_attr(1)));
        for (column, header) in sheet.headers.iter().enumerate() {
            write_cell_string(&mut xml, column, 1, header, true);
        }
        xml.push_str("</row>");
        for (row, cells) in sheet.rows.iter().enumerate() {
            let number = row as u64 + 2;
            if number - 1 > MAX_DATA_ROWS {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("xlsx supports at most {MAX_DATA_ROWS} data rows in one sheet"),
                ));
            }
            xml.push_str(&format!("<row r=\"{number}\"{}>", height_attr(number)));
            let sheet_formats = formats.get(index).map(Vec::as_slice).unwrap_or(&[]);
            for (column, cell) in cells.iter().enumerate() {
                match sheet_formats.get(column) {
                    Some(format) => {
                        let code = format.name();
                        let style = style_map.get(&code).copied();
                        write_formatted_cell(&mut xml, column, number, cell, format, style);
                    }
                    None => write_cell_string(&mut xml, column, number, cell, false),
                }
            }
            xml.push_str("</row>");
        }
        xml.push_str("</sheetData>");
        if protect {
            match protect_password {
                Some(password) => xml.push_str(&format!(
                    "<sheetProtection sheet=\"1\" objects=\"{}\" scenarios=\"{}\" password=\"{}\" />",
                    u8::from(protect_objects),
                    u8::from(protect_scenarios),
                    sheet_password_hash(password)
                )),
                None => xml.push_str(&format!(
                    "<sheetProtection sheet=\"1\" objects=\"{}\" scenarios=\"{}\"/>",
                    u8::from(protect_objects),
                    u8::from(protect_scenarios)
                )),
            }
        }
        if let Some(ranges) = merges.get(index) {
            if !ranges.is_empty() {
                xml.push_str(&format!("<mergeCells count=\"{}\">", ranges.len()));
                for range in ranges {
                    xml.push_str(&format!(
                        "<mergeCell ref=\"{}\"/>",
                        xml_attribute(range.trim())
                    ));
                }
                xml.push_str("</mergeCells>");
            }
        }
        if !rules.is_empty() && !sheet.rows.is_empty() && !sheet.headers.is_empty() {
            let last_row = sheet.rows.len() as u64 + 1;
            xml.push_str(&conditional_formatting_xml(
                rules,
                &sheet.headers,
                last_row,
            )?);
        }
        if !validations.is_empty() && !sheet.rows.is_empty() && !sheet.headers.is_empty() {
            let last_row = sheet.rows.len() as u64 + 1;
            xml.push_str(&data_validations_xml(
                validations,
                &sheet.headers,
                last_row,
            )?);
        }
        if auto_filter && !sheet.rows.is_empty() && !sheet.headers.is_empty() {
            let last_column = column_name(sheet.headers.len() - 1);
            xml.push_str(&format!(
                "<autoFilter ref=\"A1:{last_column}{}\"/>",
                sheet.rows.len() + 1
            ));
        }
        xml.push_str("</worksheet>");
        write!(zip, "{xml}")?;
    }
    zip.finish()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

const XML_HEADER: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>";

fn write_part(
    zip: &mut zip::ZipWriter<BufWriter<std::fs::File>>,
    name: &str,
    body: &str,
    options: zip::write::SimpleFileOptions,
) -> std::io::Result<()> {
    zip.start_file(name, options)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    zip.write_all(body.as_bytes())
}

fn content_types() -> String {
    content_types_for(1)
}

fn content_types_for(sheets: usize) -> String {
    let mut xml = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
        "<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>",
        "<Default Extension=\"xml\" ContentType=\"application/xml\"/>",
        "<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>",
        "<Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>"
    ));
    for index in 1..=sheets {
        xml.push_str(&format!(
            "<Override PartName=\"/xl/worksheets/sheet{index}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
        ));
    }
    xml.push_str("</Types>");
    xml
}
fn content_types_for_with_properties(sheets: usize, doc_props: bool) -> String {
    let mut xml = content_types_for(sheets);
    if doc_props {
        xml = xml.replace(
            "</Types>",
            "<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/></Types>",
        );
    }
    xml
}

fn root_rels_with_properties(doc_props: bool) -> String {
    let mut xml = root_rels().to_string();
    if doc_props {
        xml = xml.replace(
            "</Relationships>",
            "<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>",
        );
    }
    xml
}

fn core_properties_xml(properties: &CoreProperties) -> String {
    let mut xml = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
        "<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">"
    ));
    if let Some(title) = properties.title.as_deref() {
        xml.push_str(&format!("<dc:title>{}</dc:title>", xml_text(title)));
    }
    if let Some(creator) = properties.creator.as_deref() {
        xml.push_str(&format!("<dc:creator>{}</dc:creator>", xml_text(creator)));
    }
    if let Some(created) = properties.created.as_deref() {
        xml.push_str(&format!(
            "<dcterms:created xsi:type=\"dcterms:W3CDTF\">{}</dcterms:created>",
            xml_text(created)
        ));
    }
    if let Some(modified) = properties.modified.as_deref() {
        xml.push_str(&format!(
            "<dcterms:modified xsi:type=\"dcterms:W3CDTF\">{}</dcterms:modified>",
            xml_text(modified)
        ));
    }
    xml.push_str("</cp:coreProperties>");
    xml
}

fn root_rels() -> &'static str {
    concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>",
        "</Relationships>"
    )
}

fn workbook_rels() -> &'static str {
    concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>",
        "<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/>",
        "</Relationships>"
    )
}

fn workbook_xml(sheet_name: &str) -> String {
    format!(
        "{}<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><bookViews><workbookView/></bookViews><sheets><sheet name=\"{}\" sheetId=\"1\" r:id=\"rId2\"/></sheets><calcPr calcId=\"0\"/></workbook>",
        XML_HEADER,
        xml_attr(sheet_name)
    )
}

fn styles_xml_for(
    formats: &[crate::layout::NumberFormat],
) -> (String, std::collections::HashMap<String, usize>) {
    let mut codes: Vec<String> = Vec::new();
    for format in formats {
        let code = format.name();
        if code != "plain" && code != "yyyy-mm-dd" && !codes.contains(&code) {
            codes.push(code);
        }
    }
    let mut map = std::collections::HashMap::new();
    for (index, code) in codes.iter().enumerate() {
        map.insert(code.clone(), 3 + index);
    }
    let mut num_fmts = String::from(
        "<numFmts count=\"1\"><numFmt numFmtId=\"14\" formatCode=\"yyyy-mm-dd\"/></numFmts>",
    );
    if !codes.is_empty() {
        num_fmts = format!("<numFmts count=\"{}\">", codes.len() + 1);
        num_fmts.push_str("<numFmt numFmtId=\"14\" formatCode=\"yyyy-mm-dd\"/>");
        for (index, code) in codes.iter().enumerate() {
            num_fmts.push_str(&format!(
                "<numFmt numFmtId=\"{}\" formatCode=\"{}\"/>",
                164 + index,
                xml_attr(code)
            ));
        }
        num_fmts.push_str("</numFmts>");
    }
    let mut cell_xfs = String::from(concat!(
        "<cellXfs count=\"3\">",
        "<xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>",
        "<xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/>",
        "<xf numFmtId=\"14\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/>"
    ));
    for (index, _) in codes.iter().enumerate() {
        cell_xfs.push_str(&format!(
            "<xf numFmtId=\"{}\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/>",
            164 + index
        ));
    }
    cell_xfs.push_str("</cellXfs>");
    let styles = format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
            "<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
            "<fonts count=\"2\"><font><sz val=\"11\"/><name val=\"Calibri\"/><family val=\"2\"/></font><font><b/><sz val=\"11\"/><name val=\"Calibri\"/><family val=\"2\"/></font></fonts>",
            "<fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills>",
            "<borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>",
            "<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>",
            "{}",
            "{}",
            "<cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles>",
            "</styleSheet>"
        ),
        num_fmts, cell_xfs
    );
    (styles, map)
}

fn write_formatted_cell(
    out: &mut String,
    column: usize,
    row: u64,
    value: &str,
    format: &crate::layout::NumberFormat,
    style: Option<usize>,
) {
    use crate::layout::NumberFormat;
    let reference = format!("{}{}", column_name(column), row);
    let trimmed = value.trim();
    match format {
        NumberFormat::Colored { inner, .. } => {
            write_formatted_cell(out, column, row, value, inner.as_ref(), style);
            return;
        }
        NumberFormat::Plain => {
            out.push_str(&cell_xml(column, row, value, false));
            return;
        }
        NumberFormat::Text => {
            if !value.is_empty() {
                out.push_str(&format!(
                    "<c r=\"{reference}\" s=\"{}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                    style.unwrap_or(0),
                    xml_text(value)
                ));
            }
            return;
        }
        NumberFormat::DateTime { pattern } => {
            let serial = if pattern.eq_ignore_ascii_case("[h]:mm:ss") {
                trimmed
                    .parse::<f64>()
                    .ok()
                    .or_else(|| elapsed_serial(trimmed))
            } else {
                None
            }
            .or_else(|| datetime_serial(trimmed))
            .or_else(|| date_serial(trimmed).map(|serial| serial as f64))
            .or_else(|| time_serial(trimmed));
            if let Some(serial) = serial {
                out.push_str(&format!(
                    "<c r=\"{reference}\" s=\"{}\"><v>{serial}</v></c>",
                    style.unwrap_or(2)
                ));
                return;
            }
        }
        NumberFormat::Custom { .. }
        | NumberFormat::Decimal { .. }
        | NumberFormat::Percent { .. }
        | NumberFormat::Currency { .. }
        | NumberFormat::Scientific { .. }
        | NumberFormat::Accounting { .. }
        | NumberFormat::Fraction { .. } => {
            let numeric = !trimmed.is_empty()
                && !(trimmed.starts_with('0') && !trimmed.starts_with("0.") && trimmed != "0")
                && trimmed.parse::<f64>().is_ok();
            if numeric {
                out.push_str(&format!(
                    "<c r=\"{reference}\" s=\"{}\"><v>{}</v></c>",
                    style.unwrap_or(0),
                    xml_text(trimmed)
                ));
                return;
            }
        }
    }
    out.push_str(&cell_xml(column, row, value, false));
}

fn cell_xml(column: usize, row: u64, value: &str, header: bool) -> String {
    let style = if header { " s=\"1\"" } else { "" };
    let reference = format!("{}{}", column_name(column), row);
    if value.is_empty() && !header {
        return String::new();
    }
    let trimmed = value.trim();
    if !header && !value.is_empty() {
        if let Some(serial) = date_serial(trimmed) {
            return format!("<c r=\"{reference}\" s=\"2\"><v>{serial}</v></c>");
        }
    }
    let numeric =
        !header && !value.is_empty() && !value.starts_with('0') && trimmed.parse::<f64>().is_ok();
    if numeric {
        return format!(
            "<c r=\"{reference}\"{style}><v>{}</v></c>",
            xml_text(trimmed)
        );
    }
    if !header && (trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("false")) {
        return format!(
            "<c r=\"{reference}\"{style} t=\"b\"><v>{}</v></c>",
            if trimmed.eq_ignore_ascii_case("true") {
                "1"
            } else {
                "0"
            }
        );
    }
    format!(
        "<c r=\"{reference}\"{style} t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
        xml_text(value)
    )
}

fn write_cell<W: Write>(
    out: &mut W,
    column: usize,
    row: u64,
    value: &str,
    header: bool,
) -> std::io::Result<()> {
    write!(out, "{}", cell_xml(column, row, value, header))
}

fn write_cell_string(out: &mut String, column: usize, row: u64, value: &str, header: bool) {
    out.push_str(&cell_xml(column, row, value, header));
}

fn dxf_color(name: &str) -> Option<(&'static str, &'static str)> {
    match name.to_ascii_lowercase().as_str() {
        "red" => Some(("FFFFC7CE", "FFFFC7CE")),
        "amber" | "yellow" => Some(("FFFFEB9C", "FFFFEB9C")),
        "green" => Some(("FFC6EFCE", "FFC6EFCE")),
        "blue" => Some(("FFBDD7EE", "FFBDD7EE")),
        _ => None,
    }
}

fn dxf_colors(rules: &[ConditionalRule]) -> Vec<(&'static str, &'static str)> {
    let mut colors = Vec::new();
    for rule in rules {
        let color = dxf_color(&rule.color).unwrap_or(("FFFFC7CE", "FFFFC7CE"));
        if !colors.contains(&color) {
            colors.push(color);
        }
    }
    colors
}

fn dxf_ids(rules: &[ConditionalRule]) -> Vec<usize> {
    let colors = dxf_colors(rules);
    rules
        .iter()
        .map(|rule| {
            let color = dxf_color(&rule.color).unwrap_or(("FFFFC7CE", "FFFFC7CE"));
            colors.iter().position(|item| *item == color).unwrap_or(0)
        })
        .collect()
}

fn styles_xml_with_rules_and_formats(
    rules: &[ConditionalRule],
    formats: &[(String, crate::layout::NumberFormat)],
) -> (String, std::collections::HashMap<String, usize>) {
    let flat: Vec<crate::layout::NumberFormat> =
        formats.iter().map(|(_, format)| format.clone()).collect();
    let (mut out, map) = styles_xml_for(&flat);
    if rules.is_empty() {
        return (out, map);
    }
    let colors = dxf_colors(rules);
    let mut dxfs = String::from("<dxfs count=\"");
    dxfs.push_str(&colors.len().to_string());
    dxfs.push_str("\">");
    for (fg, bg) in colors {
        dxfs.push_str(&format!(
            "<dxf><fill><patternFill patternType=\"solid\"><fgColor rgb=\"{fg}\"/><bgColor rgb=\"{bg}\"/></patternFill></fill></dxf>"
        ));
    }
    dxfs.push_str("</dxfs>");
    out = out.replace("</styleSheet>", &format!("{dxfs}</styleSheet>"));
    (out, map)
}

fn quote_formula(value: &str) -> String {
    if value.parse::<f64>().is_ok() {
        value.to_string()
    } else {
        formula_literal(value)
    }
}

/// A double-quoted Excel formula string. Inner quotes are doubled, and the
/// XML text is escaped so the formula parser receives the intended literal.
fn xml_attribute(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn validation_bound_serial(bound: &str, kind: NumericKind) -> std::io::Result<String> {
    if bound.parse::<f64>().is_ok() {
        return Ok(bound.to_string());
    }
    let serial = match kind {
        NumericKind::Date => date_serial(bound).map(|serial| serial as f64),
        NumericKind::Time => time_serial(bound),
        NumericKind::Whole | NumericKind::Decimal => None,
    };
    serial.map(|value| format!("{value}")).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("data validation bound {bound:?} is not a date/time or serial"),
        )
    })
}

fn data_validations_xml(
    validations: &[DataValidation],
    headers: &[String],
    last_row: u64,
) -> std::io::Result<String> {
    let mut out = format!("<dataValidations count=\"{}\">", validations.len());
    for validation in validations {
        let index = headers
            .iter()
            .position(|header| header == &validation.column)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("data validation column not found: {}", validation.column),
                )
            })?;
        if validation.formula.is_none()
            && validation.values.is_empty()
            && validation.numeric.is_none()
            && validation.list_range.is_none()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "data validation needs at least one value",
            ));
        }
        if validation.numeric.is_some()
            && (validation.formula.is_some() || !validation.values.is_empty())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "numeric data validation cannot combine with a list or custom formula",
            ));
        }
        if let Some(range) = validation.list_range.as_deref() {
            let range = range.trim();
            if range.is_empty() || range.chars().count() > 255 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "data validation list range must be 1-255 characters",
                ));
            }
            if validation.formula.is_some()
                || validation.numeric.is_some()
                || !validation.values.is_empty()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "data validation list range cannot combine with other sources",
                ));
            }
        }
        let mut error_style = String::new();
        if let Some(style) = validation.error_style.as_deref() {
            if !matches!(style, "stop" | "warning" | "information") {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "data validation error style must be stop, warning, or information",
                ));
            }
            error_style = format!(" errorStyle=\"{style}\"");
        }
        if validation.formula.is_some() && !validation.values.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "data validation cannot combine a list with a custom formula",
            ));
        }
        if validation.formula.is_none()
            && validation
                .values
                .iter()
                .any(|value| value.contains(',') || value.contains('"'))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "data validation values cannot contain commas or quotes",
            ));
        }
        let joined = validation.values.join(",");
        if validation.formula.is_none() && joined.chars().count() > 255 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "data validation list is longer than Excel's 255-character limit",
            ));
        }
        let column = column_name(index);
        let mut prompt = String::new();
        if validation.prompt_title.is_some() || validation.prompt_message.is_some() {
            let title = validation.prompt_title.as_deref().unwrap_or_default();
            let message = validation.prompt_message.as_deref().unwrap_or_default();
            if title.chars().count() > 32 || message.chars().count() > 255 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "data validation prompt title (32) or message (255) is too long",
                ));
            }
            prompt.push_str(&format!(
                " showInputMessage=\"1\" promptTitle=\"{}\" prompt=\"{}\"",
                xml_attribute(title),
                xml_attribute(message)
            ));
        }
        let mut error = String::new();
        if validation.error_title.is_some() || validation.error_message.is_some() {
            let title = validation.error_title.as_deref().unwrap_or_default();
            let message = validation.error_message.as_deref().unwrap_or_default();
            if title.chars().count() > 32 || message.chars().count() > 255 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "data validation error title (32) or message (255) is too long",
                ));
            }
            error.push_str(&format!(
                " errorTitle=\"{}\" error=\"{}\"",
                xml_attribute(title),
                xml_attribute(message)
            ));
        }
        if let Some(range) = validation.list_range.as_deref() {
            out.push_str(&format!(
                "<dataValidation type=\"list\" allowBlank=\"{}\" showErrorMessage=\"1\"{prompt}{error}{error_style} sqref=\"{column}2:{column}{last_row}\"><formula1>{}</formula1></dataValidation>",
                if validation.allow_blank { "1" } else { "0" },
                xml_text(range.trim())
            ));
        } else if let Some(numeric) = validation.numeric.as_ref() {
            let min = numeric
                .min
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty());
            let max = numeric
                .max
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty());
            if min.is_none() && max.is_none() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "numeric data validation needs a min or max bound",
                ));
            }
            for bound in [min, max].into_iter().flatten() {
                if bound.chars().count() > 255 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "data validation bound is longer than 255 characters",
                    ));
                }
            }
            let kind = match numeric.kind {
                NumericKind::Whole => "whole",
                NumericKind::Decimal => "decimal",
                NumericKind::Date => "date",
                NumericKind::Time => "time",
            };
            let (min, max) = if matches!(numeric.kind, NumericKind::Date | NumericKind::Time) {
                (
                    min.map(|value| validation_bound_serial(value, numeric.kind))
                        .transpose()?,
                    max.map(|value| validation_bound_serial(value, numeric.kind))
                        .transpose()?,
                )
            } else {
                (min.map(str::to_string), max.map(str::to_string))
            };
            let (operator, formula1, formula2) = match (min.as_deref(), max.as_deref()) {
                (Some(min), Some(max)) => ("between", min.to_string(), Some(max.to_string())),
                (Some(min), None) => ("greaterThanOrEqual", min.to_string(), None),
                (None, Some(max)) => ("lessThanOrEqual", max.to_string(), None),
                (None, None) => unreachable!(),
            };
            let second = formula2
                .map(|max| format!("<formula2>{}</formula2>", xml_text(&max)))
                .unwrap_or_default();
            out.push_str(&format!(
                "<dataValidation type=\"{kind}\" operator=\"{operator}\" allowBlank=\"{}\" showErrorMessage=\"1\"{prompt}{error}{error_style} sqref=\"{column}2:{column}{last_row}\"><formula1>{}</formula1>{second}</dataValidation>",
                if validation.allow_blank { "1" } else { "0" },
                xml_text(&formula1)
            ));
        } else if let Some(formula) = validation.formula.as_deref() {
            if formula.trim().is_empty() || formula.chars().count() > 255 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "data validation formula must be 1-255 characters",
                ));
            }
            out.push_str(&format!(
                "<dataValidation type=\"custom\" allowBlank=\"{}\" showErrorMessage=\"1\"{prompt}{error}{error_style} sqref=\"{column}2:{column}{last_row}\"><formula1>{}</formula1></dataValidation>",
                if validation.allow_blank { "1" } else { "0" },
                xml_text(formula)
            ));
        } else {
            out.push_str(&format!(
                "<dataValidation type=\"list\" allowBlank=\"{}\" showErrorMessage=\"1\"{prompt}{error}{error_style} sqref=\"{column}2:{column}{last_row}\"><formula1>{}</formula1></dataValidation>",
                if validation.allow_blank { "1" } else { "0" },
                formula_literal(&joined)
            ));
        }
    }
    out.push_str("</dataValidations>");
    Ok(out)
}

fn formula_literal(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\"\""),
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(ch),
        }
    }
    format!("\"{escaped}\"")
}

fn parse_hex_color(text: &str) -> Option<String> {
    let text = text.trim().trim_start_matches('#');
    if text.len() == 6 && text.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(format!("FF{}", text.to_ascii_uppercase()))
    } else {
        None
    }
}

fn conditional_formatting_xml(
    rules: &[ConditionalRule],
    headers: &[String],
    last_row: u64,
) -> std::io::Result<String> {
    let mut out = String::new();
    let dxf_ids = dxf_ids(rules);
    for (index, rule) in rules.iter().enumerate() {
        let dxf = dxf_ids[index];
        let column = headers
            .iter()
            .position(|header| header == &rule.column)
            .ok_or_else(|| {
                invalid(&format!(
                    "highlight column {:?} is not a header",
                    rule.column
                ))
            })?;
        let (operator, formula) = match rule.operator.as_str() {
            ">" => ("greaterThan", Some(rule.value.clone())),
            "<" => ("lessThan", Some(rule.value.clone())),
            ">=" => ("greaterThanOrEqual", Some(rule.value.clone())),
            "<=" => ("lessThanOrEqual", Some(rule.value.clone())),
            "=" => ("equal", Some(rule.value.clone())),
            "!=" => ("notEqual", Some(rule.value.clone())),
            // Excel's Top 10 / Bottom 10 family: the rank is N, not a comparison.
            "between" | "notBetween" => {
                let parts: Vec<&str> = rule.value.split(',').map(str::trim).collect();
                if parts.len() != 2 {
                    return Err(invalid(&format!(
                        "highlight {} needs two comma-separated bounds",
                        rule.operator
                    )));
                }
                let operator = if rule.operator == "between" {
                    "between"
                } else {
                    "notBetween"
                };
                let first = quote_formula(parts[0]);
                let second = quote_formula(parts[1]);
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"cellIs\" dxfId=\"{dxf}\" priority=\"{priority}\" operator=\"{operator}\"><formula>{first}</formula><formula>{second}</formula></cfRule></conditionalFormatting>",
                    col = column_name(column),
                    priority = index + 1,
                ));
                continue;
            }
            "blank" | "containsBlanks" | "nonblank" | "notContainsBlanks" => {
                let blank = rule.operator == "blank" || rule.operator == "containsBlanks";
                let (kind, formula) = if blank {
                    (
                        "containsBlanks",
                        format!("LEN(TRIM({col}2))=0", col = column_name(column)),
                    )
                } else {
                    (
                        "notContainsBlanks",
                        format!("LEN(TRIM({col}2))>0", col = column_name(column)),
                    )
                };
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"{kind}\" dxfId=\"{dxf}\" priority=\"{priority}\"><formula>{formula}</formula></cfRule></conditionalFormatting>",
                    col = column_name(column),
                    priority = index + 1,
                ));
                continue;
            }
            "beginsWith" | "startsWith" | "endsWith" => {
                if rule.value.trim().is_empty() {
                    return Err(invalid(&format!(
                        "highlight {} needs a non-empty text",
                        rule.operator
                    )));
                }
                let text = xml_attr(&rule.value);
                let literal = formula_literal(&rule.value);
                let (kind, formula) = if rule.operator == "endsWith" {
                    (
                        "endsWith",
                        format!(
                            "RIGHT({col}2,LEN({literal}))={literal}",
                            col = column_name(column)
                        ),
                    )
                } else {
                    (
                        "beginsWith",
                        format!(
                            "LEFT({col}2,LEN({literal}))={literal}",
                            col = column_name(column)
                        ),
                    )
                };
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"{kind}\" dxfId=\"{dxf}\" priority=\"{priority}\" operator=\"{kind}\" text=\"{text}\"><formula>{formula}</formula></cfRule></conditionalFormatting>",
                    col = column_name(column),
                    priority = index + 1,
                ));
                continue;
            }
            "dataBar" | "databar" => {
                let Some(value) = parse_hex_color(&rule.value) else {
                    return Err(invalid(&format!(
                        "highlight dataBar color {:?} is not #RRGGBB",
                        rule.value
                    )));
                };
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"dataBar\" priority=\"{priority}\"><dataBar><cfvo type=\"min\"/><cfvo type=\"max\"/><color rgb=\"{value}\"/></dataBar></cfRule></conditionalFormatting>",
                    col = column_name(column),
                    priority = index + 1,
                ));
                continue;
            }
            "today" | "yesterday" | "last7Days" | "last30Days" | "thisWeek" | "lastWeek"
            | "nextWeek" | "thisMonth" | "lastMonth" | "nextMonth" => {
                let col = column_name(column);
                let (period, formula) = match rule.operator.as_str() {
                    "today" => ("today", format!("FLOOR({col}2,1)=TODAY()")),
                    "yesterday" => ("yesterday", format!("FLOOR({col}2,1)=TODAY()-1")),
                    "last7Days" => (
                        "last7Days",
                        format!("AND(FLOOR({col}2,1)>TODAY()-7,FLOOR({col}2,1)<=TODAY())"),
                    ),
                    "last30Days" => (
                        "last30Days",
                        format!("AND(FLOOR({col}2,1)>TODAY()-30,FLOOR({col}2,1)<=TODAY())"),
                    ),
                    "thisWeek" => (
                        "thisWeek",
                        format!(
                            "AND(FLOOR({col}2,1)>=TODAY()-WEEKDAY(TODAY(),2)+1,FLOOR({col}2,1)<TODAY()-WEEKDAY(TODAY(),2)+8)"
                        ),
                    ),
                    "lastWeek" => (
                        "lastWeek",
                        format!(
                            "AND(FLOOR({col}2,1)>=TODAY()-WEEKDAY(TODAY(),2)-6,FLOOR({col}2,1)<TODAY()-WEEKDAY(TODAY(),2)+1)"
                        ),
                    ),
                    "nextMonth" | "lastMonth" | "thisMonth" => {
                        let start = match rule.operator.as_str() {
                            "thisMonth" => 0,
                            "lastMonth" => -1,
                            _ => 1,
                        };
                        (
                            rule.operator.as_str(),
                            format!(
                                "AND(FLOOR({col}2,1)>=DATE(YEAR(TODAY()),MONTH(TODAY()){start:+},1),FLOOR({col}2,1)<DATE(YEAR(TODAY()),MONTH(TODAY()){end:+},1))",
                                start = start,
                                end = start + 1,
                            ),
                        )
                    }
                    _ => (
                        "nextWeek",
                        format!(
                            "AND(FLOOR({col}2,1)>=TODAY()-WEEKDAY(TODAY(),2)+8,FLOOR({col}2,1)<TODAY()-WEEKDAY(TODAY(),2)+15)"
                        ),
                    ),
                };
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"timePeriod\" dxfId=\"{dxf}\" priority=\"{priority}\" timePeriod=\"{period}\"><formula>{formula}</formula></cfRule></conditionalFormatting>",
                    priority = index + 1,
                ));
                continue;
            }
            "iconSet" | "iconset" => {
                let preset = if rule.value.trim().is_empty() {
                    "3TrafficLights1"
                } else {
                    match rule.value.trim() {
                        "3TrafficLights1" | "3Symbols" | "3Arrows" | "3Arrows2" | "3Flags"
                        | "3Stars" => rule.value.trim(),
                        other => {
                            return Err(invalid(&format!(
                                "highlight iconSet preset {other:?} is not supported"
                            )))
                        }
                    }
                };
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"iconSet\" priority=\"{priority}\"><iconSet iconSet=\"{preset}\"><cfvo type=\"percent\" val=\"0\"/><cfvo type=\"percent\" val=\"33\"/><cfvo type=\"percent\" val=\"67\"/></iconSet></cfRule></conditionalFormatting>",
                    col = column_name(column),
                    priority = index + 1,
                ));
                continue;
            }
            "duplicate" | "duplicateValues" | "unique" | "uniqueValues" => {
                let kind = if rule.operator == "duplicate" || rule.operator == "duplicateValues" {
                    "duplicateValues"
                } else {
                    "uniqueValues"
                };
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"{kind}\" dxfId=\"{dxf}\" priority=\"{priority}\"/></conditionalFormatting>",
                    col = column_name(column),
                    priority = index + 1,
                ));
                continue;
            }
            "aboveAverage" | "belowAverage" => {
                let col = column_name(column);
                let formula = if rule.operator == "aboveAverage" {
                    format!("{col}2>AVERAGE({col}:{col})")
                } else {
                    format!("{col}2<AVERAGE({col}:{col})")
                };
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"{kind}\" dxfId=\"{dxf}\" priority=\"{priority}\"><formula>{formula}</formula></cfRule></conditionalFormatting>",
                    kind = rule.operator,
                    priority = index + 1,
                ));
                continue;
            }
            "notContains" | "notContainsText" => {
                if rule.value.trim().is_empty() {
                    return Err(invalid("highlight notContainsText needs a non-empty text"));
                }
                let text = xml_attr(&rule.value);
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"notContainsText\" dxfId=\"{dxf}\" priority=\"{priority}\" operator=\"notContainsText\" text=\"{text}\"><formula>ISERROR(SEARCH({literal},{col}2))</formula></cfRule></conditionalFormatting>",
                    literal = formula_literal(&rule.value),
                    col = column_name(column),
                    priority = index + 1,
                ));
                continue;
            }
            "contains" | "containsText" => {
                if rule.value.trim().is_empty() {
                    return Err(invalid("highlight containsText needs a non-empty text"));
                }
                let text = xml_attr(&rule.value);
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"containsText\" dxfId=\"{dxf}\" priority=\"{priority}\" operator=\"containsText\" text=\"{text}\"><formula>NOT(ISERROR(SEARCH({literal},{col}2)))</formula></cfRule></conditionalFormatting>",
                    literal = formula_literal(&rule.value),
                    col = column_name(column),
                    priority = index + 1,
                ));
                continue;
            }
            "scale2" | "scale3" => {
                let expected = if rule.operator == "scale2" { 2 } else { 3 };
                let colors: Vec<&str> = rule.value.split(',').map(str::trim).collect();
                if colors.len() != expected {
                    return Err(invalid(&format!(
                        "highlight {} needs {expected} comma-separated colors",
                        rule.operator
                    )));
                }
                let mut rgb = String::new();
                for color in &colors {
                    let Some(value) = parse_hex_color(color) else {
                        return Err(invalid(&format!(
                            "highlight color {color:?} is not #RRGGBB"
                        )));
                    };
                    rgb.push_str(&format!("<color rgb=\"{value}\"/>"));
                }
                let cfvo = if expected == 2 {
                    "<cfvo type=\"min\"/><cfvo type=\"max\"/>".to_string()
                } else {
                    "<cfvo type=\"min\"/><cfvo type=\"percentile\" val=\"50\"/><cfvo type=\"max\"/>"
                        .to_string()
                };
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"colorScale\" priority=\"{priority}\"><colorScale>{cfvo}{rgb}</colorScale></cfRule></conditionalFormatting>",
                    col = column_name(column),
                    priority = index + 1,
                ));
                continue;
            }
            "top" | "bottom" => {
                let (raw, percent) = match rule.value.strip_suffix('%') {
                    Some(raw) => (raw.trim(), true),
                    None => (rule.value.as_str(), false),
                };
                let rank = raw.parse::<u32>().map_err(|_| {
                    invalid(&format!(
                        "highlight {} rank {:?} is not a positive integer",
                        rule.operator, rule.value
                    ))
                })?;
                if rank == 0 {
                    return Err(invalid("highlight rank must be greater than 0"));
                }
                if percent && rank > 100 {
                    return Err(invalid("highlight percent rank must be 1–100"));
                }
                let kind = if rule.operator == "top" {
                    "top10"
                } else {
                    "bottom10"
                };
                let percent = if percent { " percent=\"1\"" } else { "" };
                out.push_str(&format!(
                    "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"{kind}\" dxfId=\"{dxf}\" priority=\"{priority}\" rank=\"{rank}\"{percent}/></conditionalFormatting>",
                    col = column_name(column),
                    priority = index + 1,
                ));
                continue;
            }
            other => {
                return Err(invalid(&format!(
                    "unsupported highlight operator {other:?}"
                )))
            }
        };
        let formula = formula.map(|value| {
            if value.parse::<f64>().is_ok() {
                value
            } else {
                format!("\"{}\"", xml_attr(&value))
            }
        });
        out.push_str(&format!(
            "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"cellIs\" dxfId=\"{dxf}\" priority=\"{priority}\" operator=\"{operator}\"><formula>{formula}</formula></cfRule></conditionalFormatting>",
            col = column_name(column),
            priority = index + 1,
            formula = formula.unwrap_or_default()
        ));
    }
    Ok(out)
}

/// Built-in Excel number formats that represent dates.
fn date_style_map(styles: &str) -> Vec<bool> {
    let mut date_ids: Vec<u32> = (14..=22).chain(45..=47).collect();
    let num_fmt_re = Regex::new(r#"(?s)<numFmt\b([^>]*?)(?:/>|>)"#).unwrap();
    for captures in num_fmt_re.captures_iter(styles) {
        let attrs = &captures[1];
        if let Some(id) = attr(attrs, "numFmtId").and_then(|value| value.parse::<u32>().ok()) {
            if attr(attrs, "formatCode").is_some_and(|code| is_date_format(&code)) {
                date_ids.push(id);
            }
        }
    }
    let block_re = Regex::new(r#"(?s)<cellXfs\b[^>]*>(.*?)</cellXfs>"#).unwrap();
    let Some(block) = block_re.captures(styles) else {
        return Vec::new();
    };
    let xf_re = Regex::new(r#"(?s)<xf\b([^>]*?)(?:/>|>)"#).unwrap();
    xf_re
        .captures_iter(&block[1])
        .map(|captures| {
            attr(&captures[1], "numFmtId")
                .and_then(|value| value.parse::<u32>().ok())
                .is_some_and(|id| date_ids.contains(&id))
        })
        .collect()
}

fn is_date_format(code: &str) -> bool {
    let lower = code.to_ascii_lowercase();
    if lower.contains("[h]") {
        return false;
    }
    lower.contains("yyyy")
        || lower.contains("yy")
        || lower.contains("mmm")
        || lower.contains("mm/dd")
        || lower.contains("dd/mm")
        || lower.contains("m/d")
        || lower.contains("d/m")
        || lower.contains("hh")
        || lower.contains("h:mm")
        || lower.contains("mm:ss")
}

#[cfg(test)]
fn serial_to_date(serial: f64) -> Option<String> {
    serial_to_date_with_system(serial, false)
}

fn serial_to_date_with_system(serial: f64, date1904: bool) -> Option<String> {
    if !serial.is_finite() || serial < 0.0 {
        return None;
    }
    let mut whole = serial.floor() as i64;
    let mut seconds = ((serial - whole as f64) * 86_400.0).round() as i64;
    if seconds >= 86_400 {
        whole += 1;
        seconds = 0;
    }
    if !date1904 && whole == 0 {
        return (seconds > 0).then(|| format_clock(seconds));
    }
    if !date1904 && whole == 60 {
        // Excel keeps Lotus 1-2-3's fake 1900-02-29 for serial 60.
        return Some(if seconds == 0 {
            "1900-02-29".to_string()
        } else {
            format!("1900-02-29T{}", format_clock(seconds))
        });
    }
    let epoch = if date1904 {
        days_from_civil(1904, 1, 1)
    } else {
        days_from_civil(1899, 12, 30)
    };
    let max_serial = if date1904 {
        2_958_465 - 1_462
    } else {
        2_958_465
    };
    if !(0..=max_serial).contains(&whole) {
        return None;
    }
    let days = whole + epoch;
    let (year, month, day) = civil_from_days(days);
    if !date1904 && year < 1900 {
        return None;
    }
    if seconds == 0 {
        Some(format!("{year:04}-{month:02}-{day:02}"))
    } else {
        Some(format!(
            "{year:04}-{month:02}-{day:02}T{}",
            format_clock(seconds)
        ))
    }
}

fn format_clock(seconds: i64) -> String {
    let hour = seconds / 3_600;
    let minute = (seconds % 3_600) / 60;
    let second = seconds % 60;
    format!("{hour:02}:{minute:02}:{second:02}")
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (year + i64::from(month <= 2), month, day)
}

/// ISO date-time (`YYYY-MM-DDTHH:MM:SS` or space-separated) as one serial.
fn datetime_serial(text: &str) -> Option<f64> {
    let (date, time) = text.split_once('T').or_else(|| text.split_once(' '))?;
    Some(date_serial(date)? as f64 + time_serial(time)?)
}

/// A time-only cell (`HH:MM[:SS]`) as an Excel serial fraction of a day.
fn time_serial(text: &str) -> Option<f64> {
    let mut pieces = text.trim().split(':');
    let hour: u32 = pieces.next()?.parse().ok()?;
    let minute: u32 = pieces.next()?.parse().ok()?;
    let second: u32 = pieces.next().unwrap_or("0").parse().ok()?;
    if pieces.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour * 3_600 + minute * 60 + second) as f64 / 86_400.0)
}

/// Elapsed clock (`H:MM:SS`, hours may exceed 23) as an Excel serial fraction.
fn elapsed_serial(text: &str) -> Option<f64> {
    let mut pieces = text.trim().split(':');
    let hour: u32 = pieces.next()?.parse().ok()?;
    let minute: u32 = pieces.next()?.parse().ok()?;
    let second: u32 = pieces.next()?.parse().ok()?;
    if pieces.next().is_some() || minute > 59 || second > 59 {
        return None;
    }
    Some((hour as f64 * 3_600.0 + minute as f64 * 60.0 + second as f64) / 86_400.0)
}

/// Strict ISO calendar date (`YYYY-MM-DD`) to an Excel 1900-date serial.
fn date_serial(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = value.get(0..4)?.parse().ok()?;
    let month: i64 = value.get(5..7)?.parse().ok()?;
    let day: i64 = value.get(8..10)?.parse().ok()?;
    if year < 1900 || !(1..=12).contains(&month) || day < 1 {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day > days_in_month {
        return None;
    }
    // 1899-12-30 is serial 0; the formula works for dates after 1900-03-01.
    Some(days_from_civil(year, month, day) - days_from_civil(1899, 12, 30))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let adjustment = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + adjustment + day - 1;
    era * 146_097 + doe - 719_468
}

/// Convert a zero-based column index to Excel letters, e.g. 1 -> "B".
pub fn excel_column_letter(index: usize) -> String {
    column_name(index)
}

fn column_name(mut index: usize) -> String {
    let mut out = String::new();
    index += 1;
    while index > 0 {
        let remainder = (index - 1) % 26;
        out.push((b'A' + remainder as u8) as char);
        index = (index - 1) / 26;
    }
    out.chars().rev().collect()
}

fn workbook_sheet_name(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Sheet1");
    let cleaned: String = stem
        .chars()
        .map(|c| match c {
            '[' | ']' | ':' | '*' | '?' | '/' | '\\' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let name = cleaned.trim_matches('\'').trim();
    let name = if name.is_empty() { "Sheet1" } else { name };
    name.chars().take(31).collect()
}

/// One row height read back from `<row ht>`.
#[derive(Clone, Debug, PartialEq)]
pub struct RowHeight {
    pub row: u64,
    pub height: f64,
    pub custom_height: bool,
}

/// Read explicit row heights declared on `<row ht>`.
pub fn xlsx_row_heights(input: &Path, sheet: Option<&str>) -> std::io::Result<Vec<RowHeight>> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    Ok(parse_row_heights(&sheet_xml))
}

fn parse_row_heights(sheet_xml: &str) -> Vec<RowHeight> {
    let mut out = Vec::new();
    let mut rest = sheet_xml;
    while let Some(start) = rest.find("<row ") {
        let tail = &rest[start..];
        let Some(open) = tail.find('>') else { break };
        let tag = &tail[..=open];
        let height = tag_attribute(tag, "ht").and_then(|value| value.parse::<f64>().ok());
        if let Some(height) = height {
            if let Some(row) = tag_attribute(tag, "r").and_then(|value| value.parse::<u64>().ok()) {
                out.push(RowHeight {
                    row,
                    height,
                    custom_height: tag_attribute(tag, "customHeight").as_deref() == Some("1"),
                });
            }
        }
        rest = &tail[open + 1..];
    }
    out
}

/// One column width read back from `<cols>`.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnWidth {
    pub column: String,
    pub width: f64,
    pub custom_width: bool,
}

/// Read column widths declared in a worksheet's `<cols>` block.
pub fn xlsx_column_widths(input: &Path, sheet: Option<&str>) -> std::io::Result<Vec<ColumnWidth>> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    Ok(parse_column_widths(&sheet_xml))
}

fn parse_column_widths(sheet_xml: &str) -> Vec<ColumnWidth> {
    let mut out = Vec::new();
    let mut rest = sheet_xml;
    while let Some(start) = rest.find("<col ") {
        let tail = &rest[start..];
        let Some(open) = tail.find('>') else { break };
        let tag = &tail[..=open];
        let width = tag_attribute(tag, "width").and_then(|value| value.parse::<f64>().ok());
        let min = tag_attribute(tag, "min").and_then(|value| value.parse::<usize>().ok());
        let max = tag_attribute(tag, "max").and_then(|value| value.parse::<usize>().ok());
        if let (Some(width), Some(min), Some(max)) = (width, min, max) {
            let custom_width = tag_attribute(tag, "customWidth").as_deref() == Some("1");
            for index in min..=max.min(min + 1024) {
                out.push(ColumnWidth {
                    column: column_name(index.saturating_sub(1)),
                    width,
                    custom_width,
                });
            }
        }
        rest = &tail[open + 1..];
    }
    out
}

/// Hidden rows and columns read back from a worksheet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HiddenLayout {
    /// 1-based row numbers with `hidden="1"`.
    pub rows: Vec<u64>,
    /// Excel column letters of `<col hidden="1">` entries (expanded across min..max).
    pub columns: Vec<String>,
}

/// Read hidden rows and columns for one worksheet.
pub fn xlsx_hidden_layout(input: &Path, sheet: Option<&str>) -> std::io::Result<HiddenLayout> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    Ok(parse_hidden_layout(&sheet_xml))
}

fn parse_hidden_layout(sheet_xml: &str) -> HiddenLayout {
    let mut layout = HiddenLayout::default();
    let mut rest = sheet_xml;
    while let Some(start) = rest.find("<row ") {
        let tail = &rest[start..];
        let Some(open) = tail.find('>') else { break };
        let tag = &tail[..=open];
        if tag_attribute(tag, "hidden").as_deref() == Some("1") {
            if let Some(row) = tag_attribute(tag, "r").and_then(|value| value.parse::<u64>().ok()) {
                layout.rows.push(row);
            }
        }
        rest = &tail[open + 1..];
    }
    let mut rest = sheet_xml;
    while let Some(start) = rest.find("<col ") {
        let tail = &rest[start..];
        let Some(open) = tail.find('>') else { break };
        let tag = &tail[..=open];
        if tag_attribute(tag, "hidden").as_deref() == Some("1") {
            let min = tag_attribute(tag, "min").and_then(|value| value.parse::<usize>().ok());
            let max = tag_attribute(tag, "max").and_then(|value| value.parse::<usize>().ok());
            if let (Some(min), Some(max)) = (min, max) {
                for index in min..=max.min(min + 1024) {
                    layout.columns.push(column_name(index.saturating_sub(1)));
                }
            }
        }
        rest = &tail[open + 1..];
    }
    layout
}

fn validate_merge_range(range: &str) -> std::io::Result<()> {
    let re = Regex::new(r"^[A-Z]{1,3}[0-9]+:[A-Z]{1,3}[0-9]+$").unwrap();
    if re.is_match(range.trim()) {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("merge range {range:?} must look like A1:B2"),
        ))
    }
}

/// Add or replace `<mergeCells>` on the first worksheet of an existing xlsx.
pub fn xlsx_set_merge_cells(path: &Path, ranges: &[String]) -> std::io::Result<()> {
    for range in ranges {
        validate_merge_range(range)?;
    }
    let source = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(source).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let temp = path.with_extension("merge.tmp");
    let target = std::fs::File::create(&temp)?;
    let mut writer = zip::ZipWriter::new(target);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    let mut found_sheet = false;
    let merge_re = Regex::new(r"(?s)<mergeCells\b.*?</mergeCells>|<mergeCells\b[^>]*/>").unwrap();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().to_string();
        let mut buffer = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buffer)?;
        if name == "xl/worksheets/sheet1.xml" {
            found_sheet = true;
            let mut xml = String::from_utf8_lossy(&buffer).to_string();
            xml = merge_re.replace_all(&xml, "").to_string();
            if !ranges.is_empty() {
                let mut block = format!("<mergeCells count=\"{}\">", ranges.len());
                for range in ranges {
                    block.push_str(&format!(
                        "<mergeCell ref=\"{}\"/>",
                        xml_attribute(range.trim())
                    ));
                }
                block.push_str("</mergeCells>");
                xml = xml.replace("</sheetData>", &format!("</sheetData>{block}"));
            }
            writer
                .start_file(&name, options)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            std::io::Write::write_all(&mut writer, xml.as_bytes())?;
        } else {
            writer
                .start_file(&name, options)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            std::io::Write::write_all(&mut writer, &buffer)?;
        }
    }
    writer
        .finish()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    if !found_sheet {
        let _ = std::fs::remove_file(&temp);
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "xlsx has no xl/worksheets/sheet1.xml",
        ));
    }
    std::fs::rename(&temp, path)?;
    Ok(())
}

/// Add or replace `docProps/core.xml` in an existing xlsx by rewriting its parts.
pub fn xlsx_set_core_properties(path: &Path, properties: &CoreProperties) -> std::io::Result<()> {
    let source = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(source).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let temp = path.with_extension("properties.tmp");
    let target = std::fs::File::create(&temp)?;
    let mut writer = zip::ZipWriter::new(target);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    let mut types = None;
    let mut rels = None;
    let mut has_core = false;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name().to_string();
        if name == "docProps/core.xml" {
            has_core = true;
            continue;
        }
        let mut buffer = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buffer)?;
        match name.as_str() {
            "[Content_Types].xml" => types = Some(buffer),
            "_rels/.rels" => rels = Some(buffer),
            _ => {
                writer
                    .start_file(&name, options)
                    .map_err(|error| std::io::Error::other(error.to_string()))?;
                std::io::Write::write_all(&mut writer, &buffer)?;
            }
        }
    }
    let mut types = String::from_utf8_lossy(&types.unwrap_or_default()).to_string();
    if !types.contains("/docProps/core.xml") {
        types = types.replace(
            "</Types>",
            "<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/></Types>",
        );
    }
    let mut rels = String::from_utf8_lossy(&rels.unwrap_or_default()).to_string();
    if !rels.contains("docProps/core.xml") {
        rels = rels.replace(
            "</Relationships>",
            "<Relationship Id=\"rIdProps\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>",
        );
    }
    for (name, body) in [("[Content_Types].xml", types), ("_rels/.rels", rels)] {
        writer
            .start_file(name, options)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        std::io::Write::write_all(&mut writer, body.as_bytes())?;
    }
    writer
        .start_file("docProps/core.xml", options)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    std::io::Write::write_all(&mut writer, core_properties_xml(properties).as_bytes())?;
    writer
        .finish()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let _ = has_core;
    std::fs::rename(&temp, path)?;
    Ok(())
}

/// Document core properties read back from `docProps/core.xml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoreProperties {
    pub title: Option<String>,
    pub creator: Option<String>,
    pub created: Option<String>,
    pub modified: Option<String>,
}

/// Read `docProps/core.xml` values; a missing part yields all `None`.
pub fn xlsx_core_properties(input: &Path) -> std::io::Result<CoreProperties> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let core = match read_entry(&mut archive, "docProps/core.xml") {
        Ok(core) => core,
        Err(_) => return Ok(CoreProperties::default()),
    };
    Ok(CoreProperties {
        title: inner_text(&core, "dc:title"),
        creator: inner_text(&core, "dc:creator"),
        created: inner_text(&core, "dcterms:created"),
        modified: inner_text(&core, "dcterms:modified"),
    })
}

/// One workbook sheet entry with its visibility state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetVisibilityEntry {
    pub name: String,
    /// `visible`, `hidden`, or `veryHidden`.
    pub state: String,
}

/// Read each sheet's `state` attribute from workbook.xml.
pub fn xlsx_sheet_visibility(input: &Path) -> std::io::Result<Vec<SheetVisibilityEntry>> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let mut out = Vec::new();
    let mut rest = workbook.as_str();
    while let Some(start) = rest.find("<sheet ") {
        let tail = &rest[start..];
        let Some(open) = tail.find('>') else { break };
        let tag = &tail[..=open];
        if let Some(name) = tag_attribute(tag, "name") {
            out.push(SheetVisibilityEntry {
                name: xml_unescape(&name),
                state: tag_attribute(tag, "state").unwrap_or_else(|| "visible".to_string()),
            });
        }
        rest = &tail[open + 1..];
    }
    Ok(out)
}

/// Merged-cell ranges read back from a worksheet, e.g. `A1:B2`.
pub fn xlsx_merged_ranges(input: &Path, sheet: Option<&str>) -> std::io::Result<Vec<String>> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    Ok(parse_merge_cells(&sheet_xml))
}

fn parse_merge_cells(sheet_xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = sheet_xml;
    while let Some(start) = rest.find("<mergeCell ") {
        let tail = &rest[start..];
        let Some(open) = tail.find('>') else { break };
        if let Some(range) = tag_attribute(&tail[..=open], "ref") {
            out.push(range);
        }
        rest = &tail[open + 1..];
    }
    out
}

/// First-row header cell values, paired with their Excel column letters.
pub fn xlsx_header_names(
    input: &Path,
    sheet: Option<&str>,
) -> std::io::Result<Vec<(String, String)>> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    let shared = read_entry(&mut archive, "xl/sharedStrings.xml").unwrap_or_default();
    Ok(parse_header_names(&sheet_xml, &shared))
}

fn inner_text(block: &str, name: &str) -> Option<String> {
    let start = block.find(&format!("<{name}"))?;
    let open = block[start..].find('>')? + start;
    if block[start..=open].ends_with("/>") {
        return None;
    }
    let end = block[open + 1..].find(&format!("</{name}>"))? + open + 1;
    Some(xml_unescape(&block[open + 1..end]))
}

fn parse_header_names(sheet_xml: &str, shared_xml: &str) -> Vec<(String, String)> {
    let shared: Vec<String> = shared_xml
        .match_indices("<si>")
        .filter_map(|(start, _)| {
            let tail = &shared_xml[start..];
            let end = tail.find("</si>")?;
            inner_text(&tail[..end], "t")
        })
        .collect();
    let mut out = Vec::new();
    let mut rest = sheet_xml;
    while let Some(start) = rest.find("<c ") {
        let tail = &rest[start..];
        let Some(end) = tail.find("</c>") else { break };
        let block = &tail[..end + "</c>".len()];
        let cell = tag_attribute(block, "r").unwrap_or_default();
        let row = cell
            .chars()
            .filter(|ch| ch.is_ascii_digit())
            .collect::<String>();
        if row != "1" {
            rest = &tail[end + "</c>".len()..];
            continue;
        }
        let letter: String = cell
            .chars()
            .take_while(|ch| ch.is_ascii_alphabetic())
            .collect();
        let name = if tag_attribute(block, "t").as_deref() == Some("s") {
            inner_text(block, "v")
                .and_then(|index| index.parse::<usize>().ok())
                .and_then(|index| shared.get(index).cloned())
                .unwrap_or_default()
        } else {
            inner_text(block, "t")
                .or_else(|| inner_text(block, "v"))
                .unwrap_or_default()
        };
        if !letter.is_empty() {
            out.push((letter, name));
        }
        rest = &tail[end + "</c>".len()..];
    }
    out
}

/// Frozen panes and auto-filter settings read back from a worksheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetViewInfo {
    pub frozen_rows: u64,
    pub frozen_columns: u64,
    pub auto_filter: Option<String>,
}

/// Read frozen panes and auto-filter settings for one worksheet.
pub fn xlsx_sheet_view(input: &Path, sheet: Option<&str>) -> std::io::Result<SheetViewInfo> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    Ok(parse_sheet_view(&sheet_xml))
}

fn parse_sheet_view(sheet_xml: &str) -> SheetViewInfo {
    let pane = sheet_xml.find("<pane ").map(|start| {
        let tail = &sheet_xml[start..];
        let end = tail.find('>').map(|index| index + 1).unwrap_or(tail.len());
        &tail[..end]
    });
    let frozen_rows = pane
        .and_then(|tag| tag_attribute(tag, "ySplit"))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let frozen_columns = pane
        .and_then(|tag| tag_attribute(tag, "xSplit"))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let auto_filter = sheet_xml.find("<autoFilter ").and_then(|start| {
        let tail = &sheet_xml[start..];
        let end = tail.find('>')?;
        tag_attribute(&tail[..=end], "ref")
    });
    SheetViewInfo {
        frozen_rows,
        frozen_columns,
        auto_filter,
    }
}

/// One conditional-formatting rule read back from an xlsx sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConditionalFormatInfo {
    /// The sqref range the rule applies to, e.g. `B2:B100`.
    pub range: String,
    /// Excel cfRule type such as `cellIs`, `top10`, `aboveAverage`.
    pub rule_type: String,
    pub operator: Option<String>,
    pub value: Option<String>,
    /// ARGB fill colour taken from the matching dxf, when present.
    pub color: Option<String>,
}

/// Read conditional-formatting rules for one worksheet.
pub fn xlsx_conditional_formats(
    input: &Path,
    sheet: Option<&str>,
) -> std::io::Result<Vec<ConditionalFormatInfo>> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    let styles = read_entry(&mut archive, "xl/styles.xml").unwrap_or_default();
    Ok(parse_conditional_formats(&sheet_xml, &styles))
}

fn parse_conditional_formats(sheet_xml: &str, styles_xml: &str) -> Vec<ConditionalFormatInfo> {
    let dxf_colors = dxf_colors_from_styles(styles_xml);
    let mut out = Vec::new();
    let mut rest = sheet_xml;
    while let Some(start) = rest.find("<conditionalFormatting ") {
        let tail = &rest[start..];
        let Some(open) = tail.find('>') else { break };
        let tag = &tail[..=open];
        let range = tag_attribute(tag, "sqref").unwrap_or_default();
        let body_end = tail[open + 1..]
            .find("</conditionalFormatting>")
            .map(|end| open + 1 + end)
            .unwrap_or(tail.len());
        let body = &tail[open + 1..body_end];
        let mut rules_rest = body;
        while let Some(rule_start) = rules_rest.find("<cfRule ") {
            let rule_tail = &rules_rest[rule_start..];
            let Some(rule_open) = rule_tail.find('>') else {
                break;
            };
            let rule_tag = &rule_tail[..=rule_open];
            let rule_body_end = rule_tail[rule_open + 1..]
                .find("</cfRule>")
                .map(|end| rule_open + 1 + end)
                .unwrap_or(rule_tail.len());
            let rule_body = &rule_tail[rule_open + 1..rule_body_end];
            let dxf_id =
                tag_attribute(rule_tag, "dxfId").and_then(|value| value.parse::<usize>().ok());
            out.push(ConditionalFormatInfo {
                range: range.clone(),
                rule_type: tag_attribute(rule_tag, "type").unwrap_or_else(|| "cellIs".into()),
                operator: tag_attribute(rule_tag, "operator"),
                value: element_text(rule_body, "formula"),
                color: dxf_id.and_then(|id| dxf_colors.get(id).cloned()),
            });
            rules_rest = &rule_tail[rule_body_end..];
        }
        rest = &tail[body_end..];
    }
    out
}

fn dxf_colors_from_styles(styles_xml: &str) -> Vec<String> {
    let mut colors = Vec::new();
    let mut rest = styles_xml;
    while let Some(start) = rest.find("<dxf>") {
        let tail = &rest[start..];
        let Some(end) = tail.find("</dxf>") else {
            break;
        };
        let block = &tail[..end + "</dxf>".len()];
        let rgb = ["fgColor", "bgColor"].iter().find_map(|name| {
            let needle = format!("{name} rgb=\"");
            let start = block.find(&needle)? + needle.len();
            let rest = &block[start..];
            let end = rest.find('"')?;
            Some(rest[..end].to_string())
        });
        if let Some(rgb) = rgb {
            colors.push(rgb);
        }
        rest = &tail[end + "</dxf>".len()..];
    }
    colors
}

/// One cell holding an Excel formula, with its cached value when present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormulaCell {
    pub cell: String,
    pub formula: String,
    pub cached_value: Option<String>,
}

/// Read formulas (not just cached values) from one worksheet.
pub fn xlsx_formulas(input: &Path, sheet: Option<&str>) -> std::io::Result<Vec<FormulaCell>> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    Ok(parse_formula_cells(&sheet_xml))
}

fn parse_formula_cells(sheet_xml: &str) -> Vec<FormulaCell> {
    let mut out = Vec::new();
    let mut rest = sheet_xml;
    while let Some(start) = rest.find("<c ") {
        let tail = &rest[start..];
        let Some(end) = tail.find("</c>") else {
            break;
        };
        let block = &tail[..end + "</c>".len()];
        let cell = tag_attribute(block, "r").unwrap_or_default();
        if let Some(formula) = formula_text(block) {
            out.push(FormulaCell {
                cell,
                formula,
                cached_value: element_text(block, "v"),
            });
        }
        rest = &tail[end + "</c>".len()..];
    }
    out
}

fn formula_text(block: &str) -> Option<String> {
    let start = block.find("<f")?;
    let open = block[start..].find('>')? + start;
    if block[start..=open].ends_with("/>") {
        return None;
    }
    let end = block[open + 1..].find("</f>")? + open + 1;
    Some(xml_unescape(&block[open + 1..end]))
}

/// One worksheet data validation read back from an xlsx file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataValidationInfo {
    pub sheet: String,
    /// Excel column letters taken from the sqref, e.g. "B".
    pub column: String,
    /// Full sqref range, e.g. "B2:B100".
    pub range: String,
    pub kind: String,
    pub operator: String,
    pub formula1: Option<String>,
    pub formula2: Option<String>,
    pub allow_blank: bool,
    pub prompt_title: Option<String>,
    pub prompt_message: Option<String>,
    pub error_title: Option<String>,
    pub error_message: Option<String>,
    pub error_style: Option<String>,
}

/// Read data validations for one worksheet, defaulting to the first sheet.
pub fn xlsx_data_validations(
    input: &Path,
    sheet: Option<&str>,
) -> std::io::Result<Vec<DataValidationInfo>> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    Ok(parse_data_validations(&sheet_xml, &selected.name))
}

fn parse_data_validations(sheet_xml: &str, sheet: &str) -> Vec<DataValidationInfo> {
    let mut out = Vec::new();
    let mut rest = sheet_xml;
    while let Some(start) = rest.find("<dataValidation ") {
        let tail = &rest[start..];
        let Some(tag_end) = tail.find('>') else {
            break;
        };
        let tag = &tail[..=tag_end];
        let body_start = tag_end + 1;
        let (body, next) = if tag.ends_with("/>") {
            ("", body_start)
        } else {
            match tail[body_start..].find("</dataValidation>") {
                Some(close) => (
                    &tail[body_start..body_start + close],
                    body_start + close + "</dataValidation>".len(),
                ),
                None => (&tail[body_start..], tail.len()),
            }
        };
        let sqref = tag_attribute(tag, "sqref").unwrap_or_default();
        let column: String = sqref
            .chars()
            .take_while(|ch| ch.is_ascii_alphabetic())
            .collect();
        out.push(DataValidationInfo {
            sheet: sheet.to_string(),
            column,
            range: sqref.clone(),
            kind: tag_attribute(tag, "type").unwrap_or_else(|| "none".to_string()),
            operator: tag_attribute(tag, "operator").unwrap_or_else(|| "between".to_string()),
            formula1: element_text(body, "formula1"),
            formula2: element_text(body, "formula2"),
            allow_blank: tag_attribute(tag, "allowBlank").as_deref() != Some("0"),
            prompt_title: tag_attribute(tag, "promptTitle").map(|v| xml_unescape(&v)),
            prompt_message: tag_attribute(tag, "prompt").map(|v| xml_unescape(&v)),
            error_title: tag_attribute(tag, "errorTitle").map(|v| xml_unescape(&v)),
            error_message: tag_attribute(tag, "error").map(|v| xml_unescape(&v)),
            error_style: tag_attribute(tag, "errorStyle"),
        });
        rest = &tail[next..];
    }
    out
}

fn tag_attribute(tag: &str, name: &str) -> Option<String> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn element_text(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(xml_unescape(&body[start..end]))
}

fn xml_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() && !matches!(c, '\t' | '\n' | '\r') => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn xml_attr(value: &str) -> String {
    xml_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn output_path_for(input: &Path) -> PathBuf {
    input.with_extension("xlsx")
}

/// Read one CSV file into an in-memory worksheet, for multi-sheet workbooks.
pub fn read_csv_sheet(path: &Path, delimiter: Delimiter) -> std::io::Result<Sheet> {
    let reader = open_decoded(path)?;
    let mut records = crate::csv_reader::RecordReader::new(
        BufReader::with_capacity(4 * 1024 * 1024, reader),
        4 * 1024 * 1024,
    );
    let mut record = Vec::new();
    if !records.next_record(&mut record)? {
        return Err(invalid("xlsx sheet source has no header"));
    }
    let headers = crate::csv_reader::parse_csv_record(
        String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
        delimiter.byte(),
    );
    if headers.is_empty() {
        return Err(invalid("xlsx sheet source has an empty header"));
    }
    let mut rows = Vec::new();
    while records.next_record(&mut record)? {
        let cells = crate::csv_reader::parse_csv_record(
            String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
            delimiter.byte(),
        );
        if cells.iter().all(|cell| cell.is_empty()) {
            continue;
        }
        rows.push(cells);
        if rows.len() as u64 > MAX_DATA_ROWS {
            return Err(invalid(&format!(
                "xlsx supports at most {MAX_DATA_ROWS} data rows in one sheet"
            )));
        }
    }
    Ok(Sheet {
        name: workbook_sheet_name(path),
        headers,
        rows,
    })
}

fn sheet_password_hash(password: &str) -> String {
    let mut hash: u16 = 0;
    for (index, byte) in password.bytes().enumerate() {
        let shift = (index + 1) as u32;
        let value = (byte as u32) << shift;
        let rotated = value >> 15;
        let value = value & 0x7fff;
        hash ^= (value | rotated) as u16;
    }
    hash ^= password.len() as u16;
    hash ^= 0xCE4B;
    format!("{hash:04X}")
}

fn sheet_columns_xml(headers: &[String], rows: &[Vec<String>], widths: &[(String, f64)]) -> String {
    let mut xml = String::from("<cols>");
    for (index, header) in headers.iter().enumerate() {
        let wider = rows
            .iter()
            .take(20)
            .filter_map(|row| row.get(index))
            .map(|cell| cell.chars().count())
            .max()
            .unwrap_or(0);
        let width = widths
            .iter()
            .find(|(name, _)| name == header)
            .map(|(_, width)| *width)
            .unwrap_or_else(|| header.chars().count().max(wider) as f64)
            .clamp(0.5, 255.0);
        xml.push_str(&format!(
            "<col min=\"{}\" max=\"{}\" width=\"{}\" customWidth=\"1\"/>",
            index + 1,
            index + 1,
            width
        ));
    }
    xml.push_str("</cols>");
    xml
}

fn select_sheet<'a>(sheets: &'a [SheetRef], sheet: Option<&str>) -> std::io::Result<&'a SheetRef> {
    if let Some(name) = sheet {
        return sheets
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| invalid(&format!("xlsx has no sheet named {name:?}")));
    }
    sheets
        .iter()
        .find(|entry| !entry.hidden)
        .or_else(|| sheets.first())
        .ok_or_else(|| invalid("xlsx has no worksheets"))
}

/// Read one worksheet from an `.xlsx`/`.xlsm` workbook into a CSV file.
///
/// The reader understands a small, common subset: shared strings, inline
fn write_atomically<F>(output: &std::path::Path, write: F) -> std::io::Result<()>
where
    F: FnOnce(&mut std::io::BufWriter<std::fs::File>) -> std::io::Result<()>,
{
    use std::io::Write as _;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("xlsx-output");
    let temp = output.with_file_name(format!(".{name}.{}.{}.tmp", std::process::id(), sequence));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let mut writer = std::io::BufWriter::new(file);
    let result = write(&mut writer).and_then(|()| writer.flush());
    drop(writer);
    if result.is_ok() {
        if let Err(error) = std::fs::rename(&temp, output) {
            std::fs::remove_file(&temp).ok();
            return Err(error);
        }
        Ok(())
    } else {
        std::fs::remove_file(&temp).ok();
        result
    }
}

fn table_from_rows(
    rows: Vec<(u64, Vec<String>)>,
    max_rows: u64,
) -> std::io::Result<Vec<Vec<String>>> {
    let mut table: Vec<Vec<String>> = Vec::new();
    for (row_number, cells) in rows {
        if row_number == 0 || cells.iter().all(|cell| cell.is_empty()) {
            continue;
        }
        table.push(cells);
        if table.len() as u64 > max_rows + 1 {
            return Err(invalid(&format!(
                "xlsx supports at most {max_rows} data rows in one sheet"
            )));
        }
    }
    if table.is_empty() {
        return Err(invalid("xlsx worksheet has no non-empty rows"));
    }
    Ok(table)
}

/// strings, cached formula values, booleans, numbers and date styles (built-in
/// IDs plus custom `numFmt` codes; 1904 date-system workbooks are honored;
/// dates are `YYYY-MM-DD`, date-times
/// `YYYY-MM-DDTHH:MM:SS`, time-only cells `HH:MM:SS`). Merged cells are not expanded: only
/// the stored top-left value is emitted. A formula cell with no cached value is
/// an error rather than an invented empty cell, and shared-string indices are
/// validated. Charts and comments are ignored.
pub fn xlsx_sheet_names(input: &Path) -> std::io::Result<Vec<String>> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    Ok(sheets.into_iter().map(|sheet| sheet.name).collect())
}

/// One sheet's name paired with its protection flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetProtectionEntry {
    pub name: String,
    pub protection: SheetProtection,
}

/// Worksheet protection flags read back from an xlsx sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SheetProtection {
    pub protected: bool,
    pub objects_locked: bool,
    pub scenarios_locked: bool,
    pub password_protected: bool,
}

/// Report the protection flags of the selected sheet.
pub fn xlsx_sheet_protection(
    input: &Path,
    sheet: Option<&str>,
) -> std::io::Result<SheetProtection> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    Ok(protection_from_xml(&sheet_xml))
}

fn protection_from_xml(sheet_xml: &str) -> SheetProtection {
    let Some(start) = sheet_xml.find("<sheetProtection") else {
        return SheetProtection {
            protected: false,
            objects_locked: false,
            scenarios_locked: false,
            password_protected: false,
        };
    };
    let tail = &sheet_xml[start..];
    let tag = tail.split("/>").next().unwrap_or(tail);
    SheetProtection {
        protected: true,
        objects_locked: !tag.contains("objects=\"0\""),
        scenarios_locked: !tag.contains("scenarios=\"0\""),
        password_protected: tag.contains("password=\""),
    }
}

/// Read protection flags for every worksheet in workbook order.
pub fn xlsx_protection_map(input: &Path) -> std::io::Result<Vec<SheetProtectionEntry>> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {error}"),
        )
    })?;
    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let mut out = Vec::with_capacity(sheets.len());
    for sheet in &sheets {
        let target = relationship_target(&rels, &sheet.rid)
            .or_else(|| (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string()))
            .ok_or_else(|| {
                invalid(&format!(
                    "xlsx sheet {:?} has no relationship target",
                    sheet.name
                ))
            })?;
        let normalized = if let Some(stripped) = target.strip_prefix('/') {
            stripped.to_string()
        } else if target.starts_with("xl/") {
            target
        } else {
            format!("xl/{}", target.trim_start_matches("./"))
        };
        let sheet_xml = read_entry(&mut archive, &normalized)?;
        out.push(SheetProtectionEntry {
            name: sheet.name.clone(),
            protection: protection_from_xml(&sheet_xml),
        });
    }
    Ok(out)
}

/// Report whether the selected sheet carries Excel worksheet protection.
pub fn xlsx_sheet_is_protected(input: &Path, sheet: Option<&str>) -> std::io::Result<bool> {
    Ok(xlsx_sheet_protection(input, sheet)?.protected)
}

pub fn read_xlsx_to_csv(input: &Path, output: &Path, sheet: Option<&str>) -> std::io::Result<()> {
    let file = std::fs::File::open(input)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("not a zip/xlsx: {e}"),
        )
    })?;

    let workbook = read_entry(&mut archive, "xl/workbook.xml")?;
    let rels = read_entry(&mut archive, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let sheets = workbook_sheets(&workbook);
    if sheets.is_empty() {
        return Err(invalid("xlsx has no worksheets"));
    }
    let selected = select_sheet(&sheets, sheet)?;
    let target = relationship_target(&rels, &selected.rid)
        .or_else(|| {
            // Some producers omit the relationship; one-sheet workbooks still work.
            (sheets.len() == 1).then(|| "xl/worksheets/sheet1.xml".to_string())
        })
        .ok_or_else(|| {
            invalid(&format!(
                "xlsx sheet {:?} has no relationship target",
                selected.name
            ))
        })?;
    let normalized = if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else if target.starts_with("xl/") {
        target.clone()
    } else {
        format!("xl/{}", target.trim_start_matches("./"))
    };
    let shared = read_entry(&mut archive, "xl/sharedStrings.xml").unwrap_or_default();
    let shared = shared_strings(&shared);
    let styles = read_entry(&mut archive, "xl/styles.xml").unwrap_or_default();
    let date_styles = date_style_map(&styles);

    let date1904 = workbook.contains("date1904=\"1\"") || workbook.contains("date1904=\"true\"");
    let (stream_width, ordered) =
        stream_zip_width(&mut archive, &normalized, &shared, &date_styles, date1904)?;
    if ordered && stream_width > 0 {
        return write_zip_stream(
            &mut archive,
            &normalized,
            output,
            &shared,
            &date_styles,
            date1904,
            stream_width,
        );
    }
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    let rows = sheet_rows_with_date_system(&sheet_xml, &shared, &date_styles, date1904)?;
    if rows.is_empty() {
        return Err(invalid("xlsx worksheet has no rows"));
    }
    let table = table_from_rows(rows, MAX_DATA_ROWS)?;

    let width = table.iter().map(|row| row.len()).max().unwrap_or(0);
    if width == 0 {
        return Err(invalid("xlsx worksheet is empty"));
    }
    write_atomically(output, |writer| {
        let mut padded: Vec<&str> = Vec::with_capacity(width);
        for row in &table {
            padded.clear();
            for index in 0..width {
                padded.push(row.get(index).map(String::as_str).unwrap_or(""));
            }
            writer.write_all(crate::csv_reader::join_csv_line(&padded, b',').as_bytes())?;
        }
        Ok(())
    })
}

struct SheetRef {
    name: String,
    rid: String,
    hidden: bool,
}

fn read_entry<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
) -> std::io::Result<String> {
    let mut entry = archive.by_name(name).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("xlsx missing {name}: {e}"),
        )
    })?;
    let mut text = String::new();
    entry.read_to_string(&mut text)?;
    Ok(text)
}

fn workbook_sheets(xml: &str) -> Vec<SheetRef> {
    let sheet_re = Regex::new(r#"<sheet\b([^>]*?)(?:/>|>)"#).unwrap();
    let mut out = Vec::new();
    for captures in sheet_re.captures_iter(xml) {
        let attrs = &captures[1];
        let Some(name) = attr(attrs, "name") else {
            continue;
        };
        let Some(rid) = attr(attrs, "r:id") else {
            continue;
        };
        let hidden = attr(attrs, "state")
            .map(|state| state == "hidden" || state == "veryHidden")
            .unwrap_or(false);
        out.push(SheetRef {
            name: xml_unescape(&name),
            rid: rid.to_string(),
            hidden,
        });
    }
    out
}

fn relationship_target(rels: &str, rid: &str) -> Option<String> {
    let rel_re = Regex::new(r#"<Relationship\b([^>]*?)/?>"#).unwrap();
    for captures in rel_re.captures_iter(rels) {
        let attrs = &captures[1];
        if attr(attrs, "Id").as_deref() != Some(rid) {
            continue;
        }
        if let Some(target) = attr(attrs, "Target") {
            return Some(target);
        }
    }
    None
}

fn shared_strings(xml: &str) -> Vec<String> {
    let si_re = Regex::new(r#"(?s)<si\b[^>]*>(.*?)</si>"#).unwrap();
    let t_re = Regex::new(r#"(?s)<t\b[^>]*>(.*?)</t>"#).unwrap();
    si_re
        .captures_iter(xml)
        .map(|captures| {
            let mut text = String::new();
            for run in t_re.captures_iter(&captures[1]) {
                text.push_str(&xml_unescape(&run[1]));
            }
            text
        })
        .collect()
}

fn find_tag(xml: &str, from: usize, tag: &str) -> Option<usize> {
    let mut cursor = from;
    while let Some(offset) = xml[cursor..].find(tag) {
        let start = cursor + offset;
        let after = start + tag.len();
        let next = xml[after..].chars().next();
        if next
            .map(|c| c == '>' || c == '/' || c.is_ascii_whitespace())
            .unwrap_or(false)
        {
            return Some(start);
        }
        cursor = after;
    }
    None
}

fn stream_zip_width<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    entry_name: &str,
    shared: &[String],
    date_styles: &[bool],
    date1904: bool,
) -> std::io::Result<(usize, bool)> {
    let mut entry = archive
        .by_name(entry_name)
        .map_err(|error| invalid(&format!("xlsx missing {entry_name}: {error}")))?;
    let mut width = 0usize;
    let mut previous = 0u64;
    let mut ordered = true;
    let mut rows = 0u64;
    for_each_zip_row(&mut entry, |fragment| {
        let (row, cells) = row_from_fragment(fragment, shared, date_styles, date1904)?;
        rows += 1;
        if rows > MAX_SHEET_ROWS {
            return Err(invalid(&format!(
                "xlsx supports at most {MAX_SHEET_ROWS} sheet rows including the header"
            )));
        }
        if row < previous {
            ordered = false;
        }
        previous = row;
        if row != 0 && !cells.iter().all(|cell| cell.is_empty()) {
            width = width.max(cells.len());
        }
        Ok(())
    })?;
    Ok((width, ordered))
}

fn write_zip_stream<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    entry_name: &str,
    output: &Path,
    shared: &[String],
    date_styles: &[bool],
    date1904: bool,
    width: usize,
) -> std::io::Result<()> {
    let mut entry = archive
        .by_name(entry_name)
        .map_err(|error| invalid(&format!("xlsx missing {entry_name}: {error}")))?;
    write_atomically(output, |writer| {
        let mut line = String::new();
        let mut wrote = false;
        let mut rows = 0u64;
        for_each_zip_row(&mut entry, |fragment| {
            let (row, cells) = row_from_fragment(fragment, shared, date_styles, date1904)?;
            rows += 1;
            if rows > MAX_SHEET_ROWS {
                return Err(invalid(&format!(
                    "xlsx supports at most {MAX_SHEET_ROWS} sheet rows including the header"
                )));
            }
            if row == 0 || cells.iter().all(|cell| cell.is_empty()) {
                return Ok(());
            }
            line.clear();
            for index in 0..width {
                if index > 0 {
                    line.push(',');
                }
                crate::csv_reader::push_csv_field(
                    &mut line,
                    cells.get(index).map(String::as_str).unwrap_or(""),
                    b',',
                );
            }
            line.push('\n');
            writer.write_all(line.as_bytes())?;
            wrote = true;
            Ok(())
        })?;
        if !wrote {
            return Err(invalid("xlsx worksheet has no non-empty rows"));
        }
        Ok(())
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

fn find_row_tag_start(buf: &[u8], from: usize) -> Option<usize> {
    let mut cursor = from;
    while cursor + 4 <= buf.len() {
        if &buf[cursor..cursor + 4] == b"<row" {
            let next = buf.get(cursor + 4).copied();
            if next
                .map(|byte| byte == b'>' || byte == b'/' || byte.is_ascii_whitespace())
                .unwrap_or(false)
            {
                return Some(cursor);
            }
        }
        cursor += 1;
    }
    None
}

fn for_each_zip_row<R: Read, F>(mut reader: R, mut callback: F) -> std::io::Result<()>
where
    F: FnMut(&str) -> std::io::Result<()>,
{
    let mut buffer: Vec<u8> = Vec::new();
    let mut search = 0usize;
    loop {
        if let Some(start) = find_row_tag_start(&buffer, search) {
            let Some(open_offset) = buffer[start..].iter().position(|byte| *byte == b'>') else {
                // Read more; the opening tag is split across a chunk.
                buffer.extend_from_slice(&read_chunk(&mut reader)?);
                continue;
            };
            let open_end = start + open_offset;
            let self_closing = buffer[start + 4..open_end]
                .iter()
                .rev()
                .find(|byte| !byte.is_ascii_whitespace())
                .is_some_and(|byte| *byte == b'/');
            let end = if self_closing {
                open_end + 1
            } else {
                let Some(close_offset) = find_bytes(&buffer, b"</row>", open_end + 1) else {
                    buffer.extend_from_slice(&read_chunk(&mut reader)?);
                    continue;
                };
                close_offset + 6
            };
            let fragment = std::str::from_utf8(&buffer[start..end])
                .map_err(|_| invalid("xlsx worksheet is not valid UTF-8"))?;
            callback(fragment)?;
            search = end;
            continue;
        }
        if search > 0 {
            buffer.drain(..search);
            search = 0;
        }
        let chunk = read_chunk(&mut reader)?;
        if chunk.is_empty() {
            break;
        }
        buffer.extend_from_slice(&chunk);
    }
    Ok(())
}

fn read_chunk<R: Read>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let mut chunk = vec![0u8; 64 * 1024];
    let read = reader.read(&mut chunk)?;
    chunk.truncate(read);
    Ok(chunk)
}

fn row_from_fragment(
    fragment: &str,
    shared: &[String],
    date_styles: &[bool],
    date1904: bool,
) -> std::io::Result<(u64, Vec<String>)> {
    let rows = sheet_rows_with_date_system(fragment, shared, date_styles, date1904)?;
    rows.into_iter()
        .next()
        .ok_or_else(|| invalid("xlsx worksheet row is empty"))
}

#[cfg(test)]
fn sheet_rows(
    xml: &str,
    shared: &[String],
    date_styles: &[bool],
) -> std::io::Result<Vec<(u64, Vec<String>)>> {
    sheet_rows_with_limit(xml, shared, date_styles, MAX_SHEET_ROWS)
}

fn sheet_rows_with_date_system(
    xml: &str,
    shared: &[String],
    date_styles: &[bool],
    date1904: bool,
) -> std::io::Result<Vec<(u64, Vec<String>)>> {
    sheet_rows_with_limit_system(xml, shared, date_styles, date1904, MAX_SHEET_ROWS)
}

#[cfg(test)]
fn sheet_rows_with_limit(
    xml: &str,
    shared: &[String],
    date_styles: &[bool],
    max_sheet_rows: u64,
) -> std::io::Result<Vec<(u64, Vec<String>)>> {
    sheet_rows_with_limit_system(xml, shared, date_styles, false, max_sheet_rows)
}

fn for_each_sheet_row<F>(
    xml: &str,
    shared: &[String],
    date_styles: &[bool],
    date1904: bool,
    max_sheet_rows: u64,
    mut callback: F,
) -> std::io::Result<()>
where
    F: FnMut(u64, &[String]) -> std::io::Result<()>,
{
    let mut cursor = 0;
    let mut rows = 0u64;
    let mut cells: Vec<String> = Vec::new();
    while let Some(row_start) = find_tag(xml, cursor, "<row") {
        let Some(open_end) = xml[row_start..].find('>').map(|i| row_start + i) else {
            break;
        };
        let attrs = &xml[row_start + 4..open_end];
        let self_closing = attrs.trim_end().ends_with('/');
        let row_attrs = attrs.trim_end_matches('/').trim_end();
        let body_start = open_end + 1;
        let (body, next) = if self_closing {
            ("", body_start)
        } else {
            match xml[body_start..].find("</row>") {
                Some(close) => (&xml[body_start..body_start + close], body_start + close + 6),
                None => (&xml[body_start..], xml.len()),
            }
        };
        let row: u64 = match attr_ref(row_attrs, "r") {
            Some(value) => value
                .parse()
                .map_err(|_| invalid(&format!("row reference {value:?} is invalid")))?,
            None => rows + 1,
        };
        if row == 0 {
            return Err(invalid("row reference 0 is invalid"));
        }
        if row > MAX_SHEET_ROWS {
            return Err(invalid(&format!(
                "row {row} is beyond Excel's {MAX_SHEET_ROWS} row limit"
            )));
        }
        for cell in &mut cells {
            cell.clear();
        }
        let mut row_width = 0usize;
        let mut cell_cursor = 0;
        while let Some(cell_start) = find_tag(body, cell_cursor, "<c") {
            let Some(cell_open_end) = body[cell_start..].find('>').map(|i| cell_start + i) else {
                break;
            };
            let attrs = &body[cell_start + 2..cell_open_end];
            let self_closing = attrs.trim_end().ends_with('/');
            let attrs = attrs.trim_end_matches('/').trim_end();
            let cell_body_start = cell_open_end + 1;
            let (cell_body, next) = if self_closing {
                ("", cell_body_start)
            } else {
                match body[cell_body_start..].find("</c>") {
                    Some(close) => (
                        &body[cell_body_start..cell_body_start + close],
                        cell_body_start + close + 4,
                    ),
                    None => (&body[cell_body_start..], body.len()),
                }
            };
            let reference = attr_ref(attrs, "r").unwrap_or_default();
            let (column, reference_row) = if reference.is_empty() {
                (row_width, row)
            } else {
                let (column, row_number) = cell_reference_parts(reference)
                    .ok_or_else(|| invalid(&format!("cell reference {reference:?} is invalid")))?;
                (column, row_number)
            };
            if reference_row != row {
                return Err(invalid(&format!(
                    "cell {reference} does not belong to row {row}"
                )));
            }
            if column >= MAX_COLUMNS {
                return Err(invalid(&format!(
                    "cell {reference} is beyond Excel's {MAX_COLUMNS} column limit"
                )));
            }
            while cells.len() <= column {
                cells.push(String::new());
            }
            cell_value_into(
                attrs,
                cell_body,
                shared,
                date_styles,
                date1904,
                &mut cells[column],
            )?;
            row_width = row_width.max(column + 1);
            cell_cursor = next;
        }
        callback(row, &cells[..row_width])?;
        rows += 1;
        if rows > max_sheet_rows {
            return Err(invalid(&format!(
                "xlsx supports at most {} sheet rows including the header",
                max_sheet_rows
            )));
        }
        cursor = next;
    }
    Ok(())
}

fn sheet_rows_with_limit_system(
    xml: &str,
    shared: &[String],
    date_styles: &[bool],
    date1904: bool,
    max_sheet_rows: u64,
) -> std::io::Result<Vec<(u64, Vec<String>)>> {
    let mut out = Vec::new();
    for_each_sheet_row(
        xml,
        shared,
        date_styles,
        date1904,
        max_sheet_rows,
        |row, cells| {
            out.push((row, cells.to_vec()));
            Ok(())
        },
    )?;
    out.sort_by_key(|(row, _)| *row);
    Ok(out)
}

fn cell_reference_parts(reference: &str) -> Option<(usize, u64)> {
    let mut column = 0usize;
    let mut letters = 0;
    let mut rest = reference;
    for (index, byte) in reference.bytes().enumerate() {
        if byte.is_ascii_alphabetic() {
            column = column * 26 + (byte.to_ascii_uppercase() - b'A' + 1) as usize;
            letters += 1;
        } else {
            rest = &reference[index..];
            break;
        }
    }
    if letters == 0 || rest.is_empty() || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let row = rest.parse::<u64>().ok()?;
    (row > 0).then_some((column.saturating_sub(1), row))
}

fn cell_value_into(
    attrs: &str,
    body: &str,
    shared: &[String],
    date_styles: &[bool],
    date1904: bool,
    out: &mut String,
) -> std::io::Result<()> {
    out.clear();
    let reference = attr_ref(attrs, "r").unwrap_or_default();
    if body.contains("<f") && !body.contains("<v") {
        return Err(invalid(&format!(
            "formula cell {reference} has no cached value; open it in Excel once or export values first"
        )));
    }
    let kind = attr_ref(attrs, "t").unwrap_or_default();
    match kind {
        "inlineStr" => {
            inline_string_into(body, out);
            Ok(())
        }
        "s" => {
            let index = v_text_ref(body)
                .and_then(|value| value.trim().parse::<usize>().ok())
                .ok_or_else(|| invalid(&format!("shared string cell {reference} has no index")))?;
            match shared.get(index) {
                Some(value) => {
                    out.push_str(value);
                    Ok(())
                }
                None => Err(invalid(&format!(
                    "shared string cell {reference} points at missing index {index}"
                ))),
            }
        }
        "str" => {
            if let Some(value) = v_text_ref(body) {
                xml_unescape_into(out, value.trim());
            }
            Ok(())
        }
        "b" => {
            out.push_str(if v_text_ref(body).map(|value| value.trim()) == Some("1") {
                "true"
            } else {
                "false"
            });
            Ok(())
        }
        _ => {
            if let Some(value) = v_text_ref(body) {
                xml_unescape_into(out, value.trim());
            }
            let style = attr_ref(attrs, "s")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if date_styles.get(style).copied().unwrap_or(false) {
                if let Ok(serial) = out.parse::<f64>() {
                    if let Some(date) = serial_to_date_with_system(serial, date1904) {
                        out.clear();
                        out.push_str(&date);
                    }
                }
            }
            Ok(())
        }
    }
}

fn attr_ref<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let mut search = 0;
    while let Some(offset) = attrs[search..].find(name) {
        let start = search + offset;
        let after_name = start + name.len();
        let before_ok = attrs[..start]
            .chars()
            .next_back()
            .map(|c| c.is_ascii_whitespace())
            .unwrap_or(true);
        let after_char = attrs[after_name..].chars().next();
        if !before_ok
            || !after_char
                .map(|c| c == '=' || c.is_ascii_whitespace())
                .unwrap_or(false)
        {
            search = after_name;
            continue;
        }
        let rest = attrs[after_name..].trim_start();
        let Some(value) = rest.strip_prefix('=') else {
            search = after_name;
            continue;
        };
        let value = value.trim_start();
        let quote = value.chars().next()?;
        if quote != '"' && quote != '\'' {
            search = after_name;
            continue;
        }
        let value = &value[1..];
        return value.find(quote).map(|end| &value[..end]);
    }
    None
}

fn attr(attrs: &str, name: &str) -> Option<String> {
    attr_ref(attrs, name).map(xml_unescape)
}

fn inline_string_into(body: &str, out: &mut String) {
    static IS_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static T_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let is_re = IS_RE.get_or_init(|| Regex::new(r#"(?s)<is\b[^>]*>(.*?)</is>"#).unwrap());
    let t_re = T_RE.get_or_init(|| Regex::new(r#"(?s)<t\b[^>]*>(.*?)</t>"#).unwrap());
    let Some(is) = is_re.captures(body) else {
        return;
    };
    for run in t_re.captures_iter(&is[1]) {
        xml_unescape_into(out, &run[1]);
    }
}

fn v_text_ref(body: &str) -> Option<&str> {
    let start = body.find("<v>")? + 3;
    let end = body[start..].find("</v>")?;
    Some(body[start..start + end].trim())
}

fn xml_unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    xml_unescape_into(&mut out, value);
    out
}

fn xml_unescape_into(out: &mut String, value: &str) {
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        let Some(semicolon) = tail.find(';') else {
            out.push_str(tail);
            return;
        };
        let entity = &tail[..semicolon + 1];
        match entity {
            "&amp;" => out.push('&'),
            "&lt;" => out.push('<'),
            "&gt;" => out.push('>'),
            "&quot;" => out.push('"'),
            "&apos;" => out.push('\''),
            other if other.starts_with("&#x") || other.starts_with("&#X") => {
                if let Some(character) =
                    char::from_u32(u32::from_str_radix(&other[3..other.len() - 1], 16).unwrap_or(0))
                {
                    out.push(character);
                }
            }
            other if other.starts_with("&#") => {
                if let Some(character) =
                    char::from_u32(other[2..other.len() - 1].parse().unwrap_or(0))
                {
                    out.push(character);
                }
            }
            _ => {}
        }
        rest = &tail[semicolon + 1..];
    }
    out.push_str(rest);
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "csvx_xlsx_{}_{}_{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn read_zip(path: &Path, name: &str) -> String {
        let file = std::fs::File::open(path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut entry = archive.by_name(name).unwrap();
        let mut text = String::new();
        entry.read_to_string(&mut text).unwrap();
        text
    }

    #[test]
    fn writes_a_streamed_single_sheet_workbook() {
        let dir = std::env::temp_dir().join(format!("csvx_xlsx_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("orders.csv");
        let output = dir.join("orders.xlsx");
        std::fs::write(
            &input,
            "id,amount,flag,note\n1,12.5,true,a&b\n2,007,false,\"<x>\"\n",
        )
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx(&input, &output, Delimiter::Comma, &tx).unwrap();
        assert!(output.exists());
        for name in [
            "[Content_Types].xml",
            "xl/workbook.xml",
            "xl/worksheets/sheet1.xml",
            "xl/styles.xml",
        ] {
            let text = read_zip(&output, name);
            assert!(!text.is_empty(), "{name}");
            assert!(text.starts_with("<?xml"), "{name}: {text}");
        }
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("xml:space=\"preserve\""), "{sheet}");
        assert!(sheet.contains("<v>12.5</v>"), "{sheet}");
        assert!(
            sheet.contains("<t xml:space=\"preserve\">007</t>"),
            "{sheet}"
        );
        assert!(sheet.contains("a&amp;b"), "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn iso_dates_are_written_as_excel_date_cells() {
        assert_eq!(date_serial("2024-01-01"), Some(45_292));
        assert_eq!(date_serial("2024-02-30"), None);
        assert_eq!(date_serial("not-a-date"), None);
        assert_eq!(time_serial("12:00"), Some(0.5));
        assert_eq!(time_serial("06:30:15"), Some(23_415.0 / 86_400.0));
        assert_eq!(time_serial("24:00"), None);
        assert_eq!(time_serial("12:60"), None);
        assert_eq!(datetime_serial("2024-01-01T12:00:00"), Some(45_292.5));
        assert_eq!(
            datetime_serial("2024-01-01 06:30:15"),
            Some(45_292.0 + 23_415.0 / 86_400.0)
        );
        assert_eq!(datetime_serial("2024-01-01T24:00:00"), None);
        assert_eq!(datetime_serial("2024-01-01"), None);
        let dir = std::env::temp_dir().join(format!("csvx_xlsx_date_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("dates.csv");
        let output = dir.join("dates.xlsx");
        std::fs::write(&input, "day,value\n2024-01-01,7\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx(&input, &output, Delimiter::Comma, &tx).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("s=\"2\""), "{sheet}");
        assert!(sheet.contains("<v>45292</v>"), "{sheet}");
        let back = dir.join("dates-back.csv");
        read_xlsx_to_csv(&output, &back, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&back).unwrap(),
            "day,value\n2024-01-01,7\n"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn conditional_rules_write_excel_cell_is_rules() {
        let dir = std::env::temp_dir().join(format!("csvx_xlsx_cf_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("cf.csv");
        let output = dir.join("cf.xlsx");
        std::fs::write(&input, "id,amount\n1,1200\n2,10\n").unwrap();
        let rules = vec![ConditionalRule {
            column: "amount".into(),
            operator: ">".into(),
            value: "1000".into(),
            color: "red".into(),
        }];
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &rules).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("<conditionalFormatting"), "{sheet}");
        assert!(sheet.contains("dxfId=\"0\""), "{sheet}");
        assert!(
            sheet.contains("amount") || sheet.contains("B2:B3"),
            "{sheet}"
        );
        let styles = read_zip(&output, "xl/styles.xml");
        assert!(styles.contains("<dxfs count=\"1\">"), "{styles}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn conditional_rules_support_top_and_bottom_ranks() {
        let dir = std::env::temp_dir().join(format!("csvx_xlsx_cf_rank_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("cf.csv");
        let output = dir.join("cf.xlsx");
        std::fs::write(
            &input,
            "id,amount
1,1200
2,10
3,900
",
        )
        .unwrap();
        let rules = vec![
            ConditionalRule {
                column: "amount".into(),
                operator: "top".into(),
                value: "2".into(),
                color: "green".into(),
            },
            ConditionalRule {
                column: "id".into(),
                operator: ">".into(),
                value: "0".into(),
                color: "green".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "bottom".into(),
                value: "1".into(),
                color: "red".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "top".into(),
                value: "50%".into(),
                color: "yellow".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "bottom".into(),
                value: "25%".into(),
                color: "azure".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "scale2".into(),
                value: "#F8696B,#63BE7B".into(),
                color: String::new(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "scale3".into(),
                value: "F8696B,FFEB84,63BE7B".into(),
                color: String::new(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "contains".into(),
                value: "00".into(),
                color: "blue".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "contains".into(),
                value: "a\"b".into(),
                color: "blue".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "contains".into(),
                value: "A&B<C".into(),
                color: "blue".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "contains".into(),
                value: "x>y".into(),
                color: "blue".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "dataBar".into(),
                value: "FF0000".into(),
                color: String::new(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "beginsWith".into(),
                value: "12".into(),
                color: "purple".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "beginsWith".into(),
                value: "a\"b".into(),
                color: "purple".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "beginsWith".into(),
                value: "A&B".into(),
                color: "purple".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "endsWith".into(),
                value: "00".into(),
                color: "orange".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "endsWith".into(),
                value: "B&C".into(),
                color: "orange".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "blank".into(),
                value: String::new(),
                color: "gray".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "nonblank".into(),
                value: String::new(),
                color: "cyan".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "between".into(),
                value: "500,1000".into(),
                color: "pink".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "notBetween".into(),
                value: "1,10".into(),
                color: "coral".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "notBetween".into(),
                value: "p&q,r".into(),
                color: "coral".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "between".into(),
                value: "apple,banana".into(),
                color: "pink".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "between".into(),
                value: "x&y,z".into(),
                color: "pink".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "notContains".into(),
                value: "zz".into(),
                color: "lime".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "notContains".into(),
                value: "x\"y".into(),
                color: "lime".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "notContains".into(),
                value: "P&Q".into(),
                color: "lime".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "aboveAverage".into(),
                value: String::new(),
                color: "teal".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "duplicate".into(),
                value: String::new(),
                color: "brown".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "unique".into(),
                value: String::new(),
                color: "navy".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "iconSet".into(),
                value: String::new(),
                color: String::new(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "iconSet".into(),
                value: "3Symbols".into(),
                color: String::new(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "today".into(),
                value: String::new(),
                color: "olive".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "last7Days".into(),
                value: String::new(),
                color: "maroon".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "last30Days".into(),
                value: String::new(),
                color: "silver".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "thisWeek".into(),
                value: String::new(),
                color: "gold".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "thisMonth".into(),
                value: String::new(),
                color: "magenta".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "lastMonth".into(),
                value: String::new(),
                color: "plum".into(),
            },
            ConditionalRule {
                column: "amount".into(),
                operator: "nextMonth".into(),
                value: String::new(),
                color: "violet".into(),
            },
        ];
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &rules).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("type=\"top10\""), "{sheet}");
        assert!(sheet.contains("rank=\"2\""), "{sheet}");
        assert!(sheet.contains("type=\"bottom10\""), "{sheet}");
        assert!(sheet.contains("rank=\"1\""), "{sheet}");
        assert!(sheet.contains("B2:B4"), "{sheet}");
        assert!(sheet.contains("rank=\"50\" percent=\"1\""), "{sheet}");
        assert!(sheet.contains("rank=\"25\" percent=\"1\""), "{sheet}");
        assert!(sheet.contains("type=\"colorScale\""), "{sheet}");
        assert!(
            sheet.contains("<cfvo type=\"percentile\" val=\"50\"/>"),
            "{sheet}"
        );
        assert!(sheet.contains("rgb=\"FFF8696B\""), "{sheet}");
        assert!(sheet.contains("rgb=\"FF63BE7B\""), "{sheet}");
        assert!(sheet.contains("type=\"containsText\""), "{sheet}");
        assert!(sheet.contains("text=\"a&quot;b\""), "{sheet}");
        assert!(sheet.contains("SEARCH(\"a\"\"b\",B2)"), "{sheet}");
        assert!(sheet.contains("text=\"A&amp;B&lt;C\""), "{sheet}");
        assert!(sheet.contains("SEARCH(\"A&amp;B&lt;C\",B2)"), "{sheet}");
        assert!(sheet.contains("SEARCH(\"x&gt;y\",B2)"), "{sheet}");
        assert!(sheet.contains("text=\"00\""), "{sheet}");
        assert!(sheet.contains("NOT(ISERROR(SEARCH(\"00\",B2)))"), "{sheet}");
        assert!(sheet.contains("type=\"dataBar\""), "{sheet}");
        assert!(sheet.contains("rgb=\"FFFF0000\""), "{sheet}");
        assert!(sheet.contains("type=\"beginsWith\""), "{sheet}");
        assert!(sheet.contains("LEFT(B2,LEN(\"12\"))=\"12\""), "{sheet}");
        assert!(sheet.contains("text=\"a&quot;b\""), "{sheet}");
        assert!(
            sheet.contains("LEFT(B2,LEN(\"a\"\"b\"))=\"a\"\"b\""),
            "{sheet}"
        );
        assert!(
            sheet.contains("LEFT(B2,LEN(\"A&amp;B\"))=\"A&amp;B\""),
            "{sheet}"
        );
        assert!(sheet.contains("type=\"endsWith\""), "{sheet}");
        assert!(sheet.contains("RIGHT(B2,LEN(\"00\"))=\"00\""), "{sheet}");
        assert!(
            sheet.contains("RIGHT(B2,LEN(\"B&amp;C\"))=\"B&amp;C\""),
            "{sheet}"
        );
        assert!(sheet.contains("type=\"containsBlanks\""), "{sheet}");
        assert!(sheet.contains("LEN(TRIM(B2))=0"), "{sheet}");
        assert!(sheet.contains("type=\"notContainsBlanks\""), "{sheet}");
        assert!(sheet.contains("LEN(TRIM(B2))>0"), "{sheet}");
        assert!(sheet.contains("operator=\"between\""), "{sheet}");
        assert!(
            sheet.contains("<formula>500</formula><formula>1000</formula>"),
            "{sheet}"
        );
        assert!(sheet.contains("operator=\"notBetween\""), "{sheet}");
        assert!(
            sheet.contains("<formula>\"apple\"</formula><formula>\"banana\"</formula>"),
            "{sheet}"
        );
        assert!(
            sheet.contains("<formula>\"x&amp;y\"</formula><formula>\"z\"</formula>"),
            "{sheet}"
        );
        assert!(
            sheet.contains("<formula>1</formula><formula>10</formula>"),
            "{sheet}"
        );
        assert!(
            sheet.contains("<formula>\"p&amp;q\"</formula><formula>\"r\"</formula>"),
            "{sheet}"
        );
        assert!(sheet.contains("type=\"notContainsText\""), "{sheet}");
        assert!(sheet.contains("ISERROR(SEARCH(\"zz\",B2))"), "{sheet}");
        assert!(sheet.contains("ISERROR(SEARCH(\"x\"\"y\",B2))"), "{sheet}");
        assert!(sheet.contains("ISERROR(SEARCH(\"P&amp;Q\",B2))"), "{sheet}");
        assert!(sheet.contains("type=\"aboveAverage\""), "{sheet}");
        assert!(sheet.contains("B2>AVERAGE(B:B)"), "{sheet}");
        assert!(sheet.contains("type=\"duplicateValues\""), "{sheet}");
        assert!(sheet.contains("type=\"uniqueValues\""), "{sheet}");
        assert!(sheet.contains("type=\"iconSet\""), "{sheet}");
        assert!(sheet.contains("iconSet=\"3TrafficLights1\""), "{sheet}");
        assert!(sheet.contains("iconSet=\"3Symbols\""), "{sheet}");
        assert!(sheet.contains("type=\"timePeriod\""), "{sheet}");
        assert!(sheet.contains("timePeriod=\"today\""), "{sheet}");
        assert!(sheet.contains("FLOOR(B2,1)=TODAY()"), "{sheet}");
        assert!(sheet.contains("timePeriod=\"last7Days\""), "{sheet}");
        assert!(
            sheet.contains("AND(FLOOR(B2,1)>TODAY()-7,FLOOR(B2,1)<=TODAY())"),
            "{sheet}"
        );
        assert!(sheet.contains("timePeriod=\"last30Days\""), "{sheet}");
        assert!(
            sheet.contains("AND(FLOOR(B2,1)>TODAY()-30,FLOOR(B2,1)<=TODAY())"),
            "{sheet}"
        );
        assert!(sheet.contains("timePeriod=\"thisWeek\""), "{sheet}");
        assert!(sheet.contains("WEEKDAY(TODAY(),2)"), "{sheet}");
        assert!(sheet.contains("timePeriod=\"thisMonth\""), "{sheet}");
        assert!(
            sheet.contains("DATE(YEAR(TODAY()),MONTH(TODAY())+0,1)"),
            "{sheet}"
        );
        assert!(sheet.contains("timePeriod=\"lastMonth\""), "{sheet}");
        assert!(
            sheet.contains("DATE(YEAR(TODAY()),MONTH(TODAY())-1,1)"),
            "{sheet}"
        );
        assert!(sheet.contains("timePeriod=\"nextMonth\""), "{sheet}");
        assert!(
            sheet.contains("DATE(YEAR(TODAY()),MONTH(TODAY())+1,1)"),
            "{sheet}"
        );
        let styles = read_zip(&output, "xl/styles.xml");
        assert!(styles.contains("<dxfs count=\"4\">"), "{styles}");
        assert!(sheet.contains("dxfId=\"3\""), "{sheet}");
        assert!(sheet.contains("<autoFilter ref=\"A1:B4\"/>"), "{sheet}");
        assert!(sheet.contains("sqref=\"A2:A4\""), "{sheet}");
        assert!(
            sheet.contains("<cfRule type=\"cellIs\" dxfId=\"0\" priority=\"2\" operator=\"greaterThan\"><formula>0</formula>"),
            "{sheet}"
        );

        let bad = vec![ConditionalRule {
            column: "amount".into(),
            operator: "top".into(),
            value: "0".into(),
            color: "green".into(),
        }];
        let err = csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &bad)
            .unwrap_err()
            .to_string();
        assert!(err.contains("greater than 0"), "{err}");

        for (operator, value) in [
            ("scale2", "#F8696B"),
            ("scale2", "#F8696B,#63BE7B,#FFEB84"),
            ("scale2", "#F8696B,red"),
            ("scale3", "#F8696B,#FFEB84,63BE7G"),
        ] {
            let bad = vec![ConditionalRule {
                column: "amount".into(),
                operator: operator.into(),
                value: value.into(),
                color: String::new(),
            }];
            let err = csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &bad)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("comma-separated colors") || err.contains("is not #RRGGBB"),
                "{value}: {err}"
            );
        }

        for preset in ["4Arrows", "3TrafficLights2"] {
            let bad = vec![ConditionalRule {
                column: "amount".into(),
                operator: "iconSet".into(),
                value: preset.into(),
                color: String::new(),
            }];
            let err = csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &bad)
                .unwrap_err()
                .to_string();
            assert!(err.contains("is not supported"), "{preset}: {err}");
        }

        for value in ["", "red"] {
            let bad = vec![ConditionalRule {
                column: "amount".into(),
                operator: "dataBar".into(),
                value: value.into(),
                color: String::new(),
            }];
            let err = csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &bad)
                .unwrap_err()
                .to_string();
            assert!(err.contains("is not #RRGGBB"), "{value}: {err}");
        }

        for operator in ["contains", "notContains", "beginsWith", "endsWith"] {
            let bad = vec![ConditionalRule {
                column: "amount".into(),
                operator: operator.into(),
                value: "  ".into(),
                color: "blue".into(),
            }];
            let err = csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &bad)
                .unwrap_err()
                .to_string();
            assert!(err.contains("non-empty text"), "{operator}: {err}");
        }

        for value in ["500", "500,1000,2000"] {
            let bad = vec![ConditionalRule {
                column: "amount".into(),
                operator: "between".into(),
                value: value.into(),
                color: "pink".into(),
            }];
            let err = csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &bad)
                .unwrap_err()
                .to_string();
            assert!(err.contains("two comma-separated bounds"), "{value}: {err}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reads_back_a_streamed_export() {
        let dir = std::env::temp_dir().join(format!("csvx_xlsx_read_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.csv");
        let workbook = dir.join("in.xlsx");
        let back = dir.join("back.csv");
        std::fs::write(
            &input,
            "id,amount,flag,note\n1,12.5,true,a&b\n2,007,false,<x>\n",
        )
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx(&input, &workbook, Delimiter::Comma, &tx).unwrap();
        read_xlsx_to_csv(&workbook, &back, None).unwrap();
        let text = std::fs::read_to_string(&back).unwrap();
        assert_eq!(
            text,
            "id,amount,flag,note\n1,12.5,true,a&b\n2,007,false,<x>\n"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn streamed_export_omits_the_dimension_hint() {
        let dir = std::env::temp_dir().join(format!("csvx_xlsx_dim_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.csv");
        let workbook = dir.join("in.xlsx");
        std::fs::write(&input, "id,amount\n1,2\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx(&input, &workbook, Delimiter::Comma, &tx).unwrap();
        let file = std::fs::File::open(&workbook).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut sheet = String::new();
        use std::io::Read as _;
        zip.by_name("xl/worksheets/sheet1.xml")
            .unwrap()
            .read_to_string(&mut sheet)
            .unwrap();
        assert!(!sheet.contains("<dimension"), "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reads_a_named_sheet() {
        let dir = std::env::temp_dir().join(format!("csvx_xlsx_sheet_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let workbook = dir.join("book.xlsx");
        let back = dir.join("data.csv");
        write_sheets(
            &workbook,
            &[
                Sheet {
                    name: "Summary".into(),
                    headers: vec!["city".into(), "n".into()],
                    rows: vec![vec!["A".into(), "2".into()]],
                },
                Sheet {
                    name: "Data".into(),
                    headers: vec!["id".into(), "name".into()],
                    rows: vec![vec!["7".into(), "x&y".into()]],
                },
            ],
        )
        .unwrap();
        read_xlsx_to_csv(&workbook, &back, Some("data")).unwrap();
        assert_eq!(std::fs::read_to_string(&back).unwrap(), "id,name\n7,x&y\n");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn writes_multiple_sheets() {
        let output = temp_path("multi.xlsx");
        write_sheets(
            &output,
            &[
                Sheet {
                    name: "Summary".into(),
                    headers: vec!["city".into(), "n".into()],
                    rows: vec![vec!["A".into(), "2".into()]],
                },
                Sheet {
                    name: "Data".into(),
                    headers: vec!["id".into()],
                    rows: vec![vec!["1".into()]],
                },
            ],
        )
        .unwrap();
        let workbook = read_zip(&output, "xl/workbook.xml");
        assert!(workbook.contains("Summary"), "{workbook}");
        assert!(workbook.contains("Data"), "{workbook}");
        std::fs::remove_file(output).ok();
    }

    #[test]
    fn reads_shared_strings_and_cached_formula_values() {
        let shared = vec!["name".to_string(), "Ada".to_string()];
        let xml = r#"<sheetData>
            <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1"><v>2</v></c></row>
        </sheetData>"#;
        let rows = sheet_rows(xml, &shared, &[]).unwrap();
        assert_eq!(rows[0].1, vec!["name", "Ada", "2"]);
    }

    #[test]
    fn formula_without_a_cached_value_is_an_error() {
        let xml = r#"<sheetData>
            <row r="1"><c r="A1"><f>SUM(B1:B2)</f></c></row>
        </sheetData>"#;
        let error = sheet_rows(xml, &[], &[]).unwrap_err().to_string();
        assert!(
            error.contains("formula cell A1 has no cached value"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_shared_string_index_is_an_error_not_an_empty_cell() {
        let xml = r#"<sheetData>
            <row r="1"><c r="A1" t="s"><v>99</v></c></row>
        </sheetData>"#;
        let error = sheet_rows(xml, &[], &[]).unwrap_err().to_string();
        assert!(error.contains("points at missing index 99"), "{error}");
    }

    #[test]
    fn merged_cells_keep_the_stored_top_left_value_without_inventing_others() {
        let xml = r#"<sheetData>
            <row r="1"><c r="A1" t="inlineStr"><is><t>merged</t></is></c></row>
            <row r="2"><c r="A2"><v>1</v></c></row>
        </sheetData>
        <mergeCells count="1"><mergeCell ref="A1:B1"/></mergeCells>"#;
        let rows = sheet_rows(xml, &[], &[]).unwrap();
        assert_eq!(rows[0].1, vec!["merged"], "only A1 has a stored value");
    }

    #[test]
    fn date_serials_carry_time_fractions_and_time_only_cells() {
        assert_eq!(serial_to_date(45292.0), Some("2024-01-01".into()));
        assert_eq!(serial_to_date(45292.5), Some("2024-01-01T12:00:00".into()));
        assert_eq!(serial_to_date(0.25), Some("06:00:00".into()));
        assert_eq!(serial_to_date(60.0), Some("1900-02-29".into()));
        assert_eq!(serial_to_date(-1.0), None);
        assert_eq!(
            serial_to_date_with_system(0.0, true),
            Some("1904-01-01".into())
        );
        assert_eq!(
            serial_to_date_with_system(1.0, true),
            Some("1904-01-02".into())
        );
        assert_eq!(
            serial_to_date_with_system(0.5, true),
            Some("1904-01-01T12:00:00".into())
        );
        let rows = sheet_rows(
            r#"<sheetData><row r="1"><c r="A1" s="1"><v>45292.5</v></c></row></sheetData>"#,
            &[],
            &[false, true],
        )
        .unwrap();
        assert_eq!(rows[0].1, vec!["2024-01-01T12:00:00"]);
    }

    #[test]
    fn sheet_rows_stops_before_an_unbounded_worksheet() {
        let xml = r#"<sheetData>
            <row r="1"><c r="A1"><v>1</v></c></row>
            <row r="2"><c r="A2"><v>2</v></c></row>
            <row r="3"><c r="A3"><v>3</v></c></row>
        </sheetData>"#;
        let error = sheet_rows_with_limit(xml, &[], &[], 2)
            .unwrap_err()
            .to_string();
        assert!(error.contains("at most 2 sheet rows"), "{error}");
    }

    #[test]
    fn table_from_rows_skips_empty_rows_and_enforces_the_limit() {
        let rows = vec![
            (1, vec!["id".to_string()]),
            (2, vec![String::new()]),
            (3, vec!["1".to_string()]),
            (4, vec!["2".to_string()]),
            (5, vec!["3".to_string()]),
        ];
        let error = table_from_rows(rows, 2).unwrap_err().to_string();
        assert!(error.contains("at most 2 data rows"), "{error}");
    }

    #[test]
    fn attributes_do_not_collide_on_prefixes_or_escapes() {
        let attrs = r#"r="A1" r:id="rId1" t="s" s="1" name="Sheet &amp; Data""#;
        assert_eq!(attr(attrs, "r").as_deref(), Some("A1"));
        assert_eq!(attr(attrs, "r:id").as_deref(), Some("rId1"));
        assert_eq!(attr(attrs, "t").as_deref(), Some("s"));
        assert_eq!(attr(attrs, "s").as_deref(), Some("1"));
        assert_eq!(attr(attrs, "name").as_deref(), Some("Sheet & Data"));
    }

    #[test]
    fn workbook_sheet_states_are_parsed() {
        let xml = r#"<workbook><sheets>
            <sheet name="Hidden" sheetId="1" state="hidden" r:id="rId1"/>
            <sheet name="Very" sheetId="2" state="veryHidden" r:id="rId2"/>
            <sheet name="Visible" sheetId="3" r:id="rId3"/>
        </sheets></workbook>"#;
        let sheets = workbook_sheets(xml);
        assert_eq!(sheets.len(), 3);
        assert!(sheets[0].hidden);
        assert!(sheets[1].hidden);
        assert!(!sheets[2].hidden);
    }

    #[test]
    fn hidden_sheets_are_not_selected_by_default() {
        let sheets = vec![
            SheetRef {
                name: "Hidden".into(),
                rid: "rId1".into(),
                hidden: true,
            },
            SheetRef {
                name: "Visible".into(),
                rid: "rId2".into(),
                hidden: false,
            },
        ];
        assert_eq!(select_sheet(&sheets, None).unwrap().name, "Visible");
        assert_eq!(
            select_sheet(&sheets, Some("hidden")).unwrap().name,
            "Hidden"
        );
    }

    #[test]
    fn rows_are_ordered_by_their_xml_reference() {
        let xml = r#"<sheetData>
            <row r="2"><c r="A2"><v>2</v></c></row>
            <row r="1"><c r="A1"><v>1</v></c></row>
        </sheetData>"#;
        let rows = sheet_rows(xml, &[], &[]).unwrap();
        assert_eq!(rows[0].0, 1);
        assert_eq!(rows[1].0, 2);
    }

    #[test]
    fn row_breaks_are_not_mistaken_for_rows() {
        let xml = r#"<sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><rowBreaks count="1"><break id="1"/></rowBreaks>"#;
        let rows = sheet_rows(xml, &[], &[]).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn a_row_beyond_excels_limit_is_an_error() {
        let xml = r#"<sheetData><row r="1048577"><c r="A1048577"><v>1</v></c></row></sheetData>"#;
        let error = sheet_rows(xml, &[], &[]).unwrap_err().to_string();
        assert!(error.contains("row limit"), "{error}");
    }

    #[test]
    fn a_cell_reference_must_match_its_row() {
        let xml = r#"<sheetData><row r="1"><c r="A2"><v>1</v></c></row></sheetData>"#;
        let error = sheet_rows(xml, &[], &[]).unwrap_err().to_string();
        assert!(error.contains("does not belong to row 1"), "{error}");
    }

    #[test]
    fn an_invalid_row_reference_is_an_error() {
        let xml = r#"<sheetData><row r="x"><c r="A1"><v>1</v></c></row></sheetData>"#;
        let error = sheet_rows(xml, &[], &[]).unwrap_err().to_string();
        assert!(error.contains("row reference"), "{error}");
    }

    #[test]
    fn an_invalid_cell_reference_is_an_error() {
        let xml = r#"<sheetData><row r="1"><c r="1A"><v>1</v></c></row></sheetData>"#;
        let error = sheet_rows(xml, &[], &[]).unwrap_err().to_string();
        assert!(error.contains("is invalid"), "{error}");
    }

    #[test]
    fn a_cell_beyond_excels_column_limit_is_an_error() {
        let xml = r#"<sheetData><row r="1"><c r="XFE1"><v>1</v></c></row></sheetData>"#;
        let error = sheet_rows(xml, &[], &[]).unwrap_err().to_string();
        assert!(error.contains("16384 column limit"), "{error}");
    }

    #[test]
    fn custom_time_formats_are_recognized_and_render_as_clock() {
        let styles = r#"<numFmts count="1"><numFmt numFmtId="166" formatCode="hh:mm:ss"/></numFmts><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="166"/></cellXfs>"#;
        assert_eq!(date_style_map(styles), vec![false, true]);
        let rows = sheet_rows(
            r#"<sheetData><row r="1"><c r="A1" s="1"><v>0.5</v></c></row></sheetData>"#,
            &[],
            &date_style_map(styles),
        )
        .unwrap();
        assert_eq!(rows[0].1, vec!["12:00:00"]);
    }

    #[test]
    fn cached_string_and_boolean_formula_values_are_read() {
        let xml = r#"<sheetData>
            <row r="1"><c r="A1" t="str"><f>CONCAT("a","b")</f><v>ab</v></c><c r="B1" t="b"><f>1=1</f><v>1</v></c></row>
        </sheetData>"#;
        let rows = sheet_rows(xml, &[], &[]).unwrap();
        assert_eq!(rows[0].1, vec!["ab", "true"]);
    }

    #[test]
    fn column_formats_fall_back_when_the_first_data_row_has_no_styles() {
        use crate::layout::NumberFormat;
        let header = vec!["a".to_string(), "b".to_string()];
        let style_formats = vec![None, Some(NumberFormat::parse("#,##0.00").unwrap())];
        let plain = column_formats_for(&header, &[0], &style_formats);
        assert!(!has_any_format(&plain), "{plain:?}");
        let styled = column_formats_for(&header, &[0, 1], &style_formats);
        assert!(has_any_format(&styled), "{styled:?}");
        assert_eq!(styled[1].1, Some(NumberFormat::parse("#,##0.00").unwrap()));
    }

    #[test]
    fn builtin_time_number_formats_are_not_collapsed_to_dates() {
        let styles = r#"<cellXfs count="5">
            <xf numFmtId="0"/><xf numFmtId="14"/><xf numFmtId="20"/>
            <xf numFmtId="21"/><xf numFmtId="22"/>
        </cellXfs>"#;
        let formats = style_number_formats(styles);
        assert!(formats[0].is_none());
        assert!(matches!(
            formats[1],
            Some(crate::layout::NumberFormat::DateTime { ref pattern }) if pattern == "yyyy-mm-dd"
        ));
        assert!(matches!(
            formats[2],
            Some(crate::layout::NumberFormat::DateTime { ref pattern }) if pattern == "hh:mm"
        ));
        assert!(matches!(
            formats[3],
            Some(crate::layout::NumberFormat::DateTime { ref pattern }) if pattern == "hh:mm:ss"
        ));
        assert!(matches!(
            formats[4],
            Some(crate::layout::NumberFormat::DateTime { ref pattern }) if pattern == "yyyy-mm-dd hh:mm"
        ));
    }

    #[test]
    fn custom_date_number_formats_are_recognized() {
        let styles = r#"<numFmts count="2">
            <numFmt numFmtId="164" formatCode="yyyy/mm/dd"/>
            <numFmt numFmtId="165" formatCode="0.00%"/>
        </numFmts>
        <cellXfs count="3"><xf numFmtId="0"/><xf numFmtId="164"/><xf numFmtId="165"/></cellXfs>"#;
        assert_eq!(date_style_map(styles), vec![false, true, false]);
        let rows = sheet_rows(
            r#"<sheetData><row r="1"><c r="A1" s="1"><v>45292</v></c><c r="B1" s="2"><v>0.5</v></c></row></sheetData>"#,
            &[],
            &date_style_map(styles),
        )
        .unwrap();
        assert_eq!(rows[0].1, vec!["2024-01-01", "0.5"]);
    }

    #[test]
    fn datetime_cells_are_written_with_a_time_fraction() {
        use crate::layout::NumberFormat;
        let format = NumberFormat::parse("yyyy-mm-dd hh:mm").unwrap();
        let mut out = String::new();
        write_formatted_cell(&mut out, 0, 2, "2024-01-02T03:30:00", &format, Some(4));
        assert!(out.contains("s=\"4\""), "{out}");
        assert!(out.contains("45293.145"), "{out}");
    }

    #[test]
    fn slash_date_formats_map_into_export_styles() {
        use crate::layout::NumberFormat;
        let path = temp_path("slash.xlsx");
        let sheet = Sheet {
            name: "slash".to_string(),
            headers: vec!["us".into(), "eu".into(), "stamp".into(), "ampm".into()],
            rows: vec![vec![
                "2024-01-02".into(),
                "2024-01-02".into(),
                "2024-01-02T09:05:07".into(),
                "2024-01-02T13:05:00".into(),
            ]],
        };
        write_sheets_with_formats(
            &path,
            &[sheet],
            &[vec![
                NumberFormat::parse("m/d/yyyy").unwrap(),
                NumberFormat::parse("mm/dd/yyyy").unwrap(),
                NumberFormat::parse("m/d/yyyy h:mm:ss").unwrap(),
                NumberFormat::parse("mm/dd/yyyy h:mm am/pm").unwrap(),
            ]],
        )
        .unwrap();
        let styles = read_zip(&path, "xl/styles.xml");
        assert!(styles.contains("formatCode=\"m/d/yyyy\""), "{styles}");
        assert!(styles.contains("formatCode=\"mm/dd/yyyy\""), "{styles}");
        assert!(
            styles.contains("formatCode=\"m/d/yyyy h:mm:ss\""),
            "{styles}"
        );
        assert!(
            styles.contains("formatCode=\"mm/dd/yyyy h:mm am/pm\""),
            "{styles}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn dotted_and_dashed_formats_map_into_export_styles() {
        use crate::layout::NumberFormat;
        let path = temp_path("dotted.xlsx");
        let sheet = Sheet {
            name: "dotted".to_string(),
            headers: vec!["when".into(), "stamp".into()],
            rows: vec![vec!["2024-01-02".into(), "2024-01-02T13:04:05".into()]],
        };
        write_sheets_with_formats(
            &path,
            &[sheet],
            &[vec![
                NumberFormat::parse("dd.mm.yyyy").unwrap(),
                NumberFormat::parse("dd-mm-yyyy h:mm:ss am/pm").unwrap(),
            ]],
        )
        .unwrap();
        let styles = read_zip(&path, "xl/styles.xml");
        assert!(styles.contains("formatCode=\"dd.mm.yyyy\""), "{styles}");
        assert!(
            styles.contains("formatCode=\"dd-mm-yyyy h:mm:ss am/pm\""),
            "{styles}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn coloured_formats_map_into_export_styles() {
        use crate::layout::NumberFormat;
        let path = temp_path("colored.xlsx");
        let sheet = Sheet {
            name: "colored".to_string(),
            headers: vec!["amount".into(), "rate".into()],
            rows: vec![vec!["1234.5".into(), "0.125".into()]],
        };
        write_sheets_with_formats(
            &path,
            &[sheet],
            &[vec![
                NumberFormat::parse("[Red]#,##0.00").unwrap(),
                NumberFormat::parse("[Blue]0.0%").unwrap(),
            ]],
        )
        .unwrap();
        let styles = read_zip(&path, "xl/styles.xml");
        assert!(styles.contains("formatCode=\"[Red]#,##0.00\""), "{styles}");
        assert!(styles.contains("formatCode=\"[Blue]0.0%\""), "{styles}");
        let sheet = read_zip(&path, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("<v>1234.5</v>"), "{sheet}");
        assert!(sheet.contains("<v>0.125</v>"), "{sheet}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn header_freeze_can_be_disabled() {
        use crate::layout::NumberFormat;
        let dir = temp_path("nofreeze");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("nofreeze.csv");
        let output = dir.join("nofreeze.xlsx");
        std::fs::write(&input, "id,amount\n1,10\n2,20\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let formats = vec![("amount".to_string(), NumberFormat::parse("0.00").unwrap())];
        csv_to_xlsx_without_freeze(&input, &output, Delimiter::Comma, &tx, &[], &formats).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(!sheet.contains("<pane "), "{sheet}");
        assert!(sheet.contains("<autoFilter"), "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multi_sheet_export_can_skip_header_freeze() {
        let dir = temp_path("multinofreeze");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multi.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["1".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["2".into()]],
            },
        ];
        write_sheets_without_freeze(&path, &sheets).unwrap();
        for part in ["xl/worksheets/sheet1.xml", "xl/worksheets/sheet2.xml"] {
            let xml = read_zip(&path, part);
            assert!(!xml.contains("<pane "), "{part}: {xml}");
            assert!(xml.contains("<autoFilter"), "{part}: {xml}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn auto_filter_can_be_disabled() {
        let dir = temp_path("nofilter");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("nofilter.csv");
        let output = dir.join("nofilter.xlsx");
        std::fs::write(&input, "id,amount\n1,10\n2,20\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx_without_auto_filter(&input, &output, Delimiter::Comma, &tx, &[], &[]).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(!sheet.contains("<autoFilter"), "{sheet}");
        assert!(sheet.contains("<pane "), "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn date_and_time_validations_convert_iso_bounds_to_serials() {
        let dir = temp_path("validation date time");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dates.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["day".into(), "hour".into()],
            rows: vec![vec!["2024-01-01".into(), "09:00".into()]],
        }];
        let validations = vec![
            DataValidation {
                column: "day".to_string(),
                values: Vec::new(),
                formula: None,
                prompt_title: None,
                prompt_message: None,
                error_title: None,
                error_message: None,
                allow_blank: true,
                numeric: Some(NumericValidation {
                    kind: NumericKind::Date,
                    min: Some("2024-01-01".to_string()),
                    max: Some("2024-12-31".to_string()),
                }),
                error_style: None,
                list_range: None,
            },
            DataValidation {
                column: "hour".to_string(),
                values: Vec::new(),
                formula: None,
                prompt_title: None,
                prompt_message: None,
                error_title: None,
                error_message: None,
                allow_blank: false,
                numeric: Some(NumericValidation {
                    kind: NumericKind::Time,
                    min: Some("09:00".to_string()),
                    max: Some("17:00".to_string()),
                }),
                error_style: None,
                list_range: None,
            },
        ];
        write_sheets_with_formats_widths_and_protection(
            &path,
            &sheets,
            &[Vec::new()],
            &[Vec::new()],
            true,
            false,
            false,
            None,
            true,
            true,
            &[],
            &validations,
        )
        .unwrap();
        let sheet = read_zip(&path, "xl/worksheets/sheet1.xml");
        assert!(
            sheet.contains("type=\"date\" operator=\"between\""),
            "{sheet}"
        );
        assert!(sheet.contains("<formula1>45292</formula1>"), "{sheet}");
        assert!(sheet.contains("<formula2>45657</formula2>"), "{sheet}");
        assert!(
            sheet.contains("type=\"time\" operator=\"between\""),
            "{sheet}"
        );
        assert!(sheet.contains("<formula1>0.375</formula1>"), "{sheet}");
        let error = xlsx_data_validations(&path, None).unwrap();
        assert_eq!(error.len(), 2);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn data_validation_list_ranges_use_a_formula_reference() {
        let dir = temp_path("validation list range");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("range.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["status".into()],
            rows: vec![vec!["good".into()]],
        }];
        let validations = vec![DataValidation {
            column: "status".to_string(),
            values: Vec::new(),
            formula: None,
            prompt_title: Some("Status".to_string()),
            prompt_message: None,
            error_title: None,
            error_message: None,
            allow_blank: false,
            numeric: None,
            error_style: None,
            list_range: Some("$A$2:$A$10".to_string()),
        }];
        write_sheets_with_formats_widths_and_protection(
            &path,
            &sheets,
            &[Vec::new()],
            &[Vec::new()],
            true,
            false,
            false,
            None,
            true,
            true,
            &[],
            &validations,
        )
        .unwrap();
        let sheet = read_zip(&path, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("type=\"list\""), "{sheet}");
        assert!(sheet.contains("<formula1>$A$2:$A$10</formula1>"), "{sheet}");
        assert!(!sheet.contains("$A$2:$A$10,"), "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn data_validations_read_back_lists_and_ranges() {
        let dir = temp_path("validation readback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("readback.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["status".into(), "amount".into()],
            rows: vec![vec!["good".into(), "10".into()]],
        }];
        let validations = vec![
            DataValidation {
                column: "status".to_string(),
                values: vec!["good".to_string(), "bad".to_string()],
                formula: None,
                prompt_title: Some("Status".to_string()),
                prompt_message: Some("Choose good or bad".to_string()),
                error_title: Some("Bad".to_string()),
                error_message: Some("Pick good or bad".to_string()),
                allow_blank: true,
                numeric: None,
                error_style: Some("warning".to_string()),
                list_range: None,
            },
            DataValidation {
                column: "amount".to_string(),
                values: Vec::new(),
                formula: None,
                prompt_title: None,
                prompt_message: None,
                error_title: None,
                error_message: None,
                allow_blank: false,
                numeric: Some(NumericValidation {
                    kind: NumericKind::Whole,
                    min: Some("0".to_string()),
                    max: Some("1200".to_string()),
                }),
                error_style: None,
                list_range: None,
            },
        ];
        write_sheets_with_formats_widths_and_protection(
            &path,
            &sheets,
            &[Vec::new()],
            &[Vec::new()],
            true,
            false,
            false,
            None,
            true,
            true,
            &[],
            &validations,
        )
        .unwrap();
        let sheet_xml = read_zip(&path, "xl/worksheets/sheet1.xml");
        assert!(sheet_xml.contains("errorStyle=\"warning\""), "{sheet_xml}");
        let read = xlsx_data_validations(&path, None).unwrap();
        assert_eq!(read.len(), 2, "{read:?}");
        let list = read
            .iter()
            .find(|validation| validation.column == "A")
            .unwrap();
        assert_eq!(list.kind, "list");
        assert_eq!(list.range, "A2:A2");
        assert_eq!(list.formula1.as_deref(), Some("\"good,bad\""));
        assert_eq!(list.prompt_title.as_deref(), Some("Status"));
        assert_eq!(list.prompt_message.as_deref(), Some("Choose good or bad"));
        assert_eq!(list.error_title.as_deref(), Some("Bad"));
        assert_eq!(list.error_style.as_deref(), Some("warning"));
        assert!(list.allow_blank);
        let range = read
            .iter()
            .find(|validation| validation.column == "B")
            .unwrap();
        assert_eq!(range.kind, "whole");
        assert_eq!(range.operator, "between");
        assert_eq!(range.formula1.as_deref(), Some("0"));
        assert_eq!(range.formula2.as_deref(), Some("1200"));
        assert!(!range.allow_blank);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn numeric_data_validations_write_whole_and_decimal_bounds() {
        let dir = temp_path("numeric validations");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("numeric.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["id".into(), "amount".into()],
            rows: vec![vec!["1".into(), "10".into()]],
        }];
        let validations = vec![
            DataValidation {
                column: "amount".to_string(),
                values: Vec::new(),
                formula: None,
                prompt_title: Some("Amount".to_string()),
                prompt_message: Some("Enter 0-1200".to_string()),
                error_title: None,
                error_message: None,
                allow_blank: true,
                numeric: Some(NumericValidation {
                    kind: NumericKind::Whole,
                    min: Some("0".to_string()),
                    max: Some("1200".to_string()),
                }),
                error_style: None,
                list_range: None,
            },
            DataValidation {
                column: "id".to_string(),
                values: Vec::new(),
                formula: None,
                prompt_title: None,
                prompt_message: None,
                error_title: None,
                error_message: None,
                allow_blank: false,
                numeric: Some(NumericValidation {
                    kind: NumericKind::Decimal,
                    min: Some("0.5".to_string()),
                    max: None,
                }),
                error_style: None,
                list_range: None,
            },
        ];
        write_sheets_with_formats_widths_and_protection(
            &path,
            &sheets,
            &[Vec::new()],
            &[Vec::new()],
            true,
            false,
            false,
            None,
            true,
            true,
            &[],
            &validations,
        )
        .unwrap();
        let sheet = read_zip(&path, "xl/worksheets/sheet1.xml");
        assert!(
            sheet.contains("type=\"whole\" operator=\"between\""),
            "{sheet}"
        );
        assert!(sheet.contains("<formula2>1200</formula2>"), "{sheet}");
        assert!(sheet.contains("promptTitle=\"Amount\""), "{sheet}");
        assert!(
            sheet.contains("type=\"decimal\" operator=\"greaterThanOrEqual\""),
            "{sheet}"
        );
        assert!(sheet.contains("<formula1>0.5</formula1>"), "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn options_can_disable_freeze_and_filter_with_formats() {
        use crate::layout::NumberFormat;
        let dir = temp_path("options");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("options.csv");
        let output = dir.join("options.xlsx");
        std::fs::write(&input, "id,amount\n1,10\n2,20\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let formats = vec![("amount".to_string(), NumberFormat::parse("0.00").unwrap())];
        csv_to_xlsx_with_options(
            &input,
            &output,
            Delimiter::Comma,
            &tx,
            &[],
            &formats,
            false,
            false,
        )
        .unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(!sheet.contains("<pane "), "{sheet}");
        assert!(!sheet.contains("<autoFilter"), "{sheet}");
        let styles = read_zip(&output, "xl/styles.xml");
        assert!(styles.contains("formatCode=\"0.00\""), "{styles}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multi_sheet_options_keep_formats_without_freeze_or_filter() {
        use crate::layout::NumberFormat;
        let dir = temp_path("multiformatoptions");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multi.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into(), "amount".into()],
                rows: vec![vec!["1".into(), "10".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["rate".into()],
                rows: vec![vec!["0.25".into()]],
            },
        ];
        let formats = vec![
            vec![
                NumberFormat::parse("0").unwrap(),
                NumberFormat::parse("0.00").unwrap(),
            ],
            vec![NumberFormat::parse("0.0%").unwrap()],
        ];
        write_sheets_with_options_and_formats(&path, &sheets, &formats, false, false).unwrap();
        let styles = read_zip(&path, "xl/styles.xml");
        assert!(styles.contains("formatCode=\"0.00\""), "{styles}");
        assert!(styles.contains("formatCode=\"0.0%\""), "{styles}");
        for part in ["xl/worksheets/sheet1.xml", "xl/worksheets/sheet2.xml"] {
            let xml = read_zip(&path, part);
            assert!(!xml.contains("<pane "), "{part}: {xml}");
            assert!(!xml.contains("<autoFilter"), "{part}: {xml}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn data_validation_lists_are_written_as_dropdowns() {
        let dir = temp_path("validation");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("validation.csv");
        let output = dir.join("validation.xlsx");
        std::fs::write(&input, "id,status\n1,good\n2,ok\n3,bad\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let validations = vec![
            DataValidation {
                column: "status".to_string(),
                values: vec!["good".into(), "ok".into(), "bad".into()],
                formula: None,
                prompt_title: None,
                prompt_message: None,
                error_title: None,
                error_message: None,
                allow_blank: false,
                numeric: None,
                error_style: None,
                list_range: None,
            },
            DataValidation {
                column: "id".to_string(),
                values: vec!["1".into(), "2".into(), "3".into()],
                formula: None,
                prompt_title: None,
                prompt_message: None,
                error_title: None,
                error_message: None,
                allow_blank: false,
                numeric: None,
                error_style: None,
                list_range: None,
            },
        ];
        csv_to_xlsx_with_validations(&input, &output, Delimiter::Comma, &tx, &validations).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("<dataValidations count=\"2\">"), "{sheet}");
        assert!(sheet.contains("type=\"list\""), "{sheet}");
        assert!(sheet.contains("sqref=\"B2:B4\""), "{sheet}");
        assert!(sheet.contains("sqref=\"A2:A4\""), "{sheet}");
        assert!(sheet.contains("<formula1>\"1,2,3\"</formula1>"), "{sheet}");
        assert!(
            sheet.contains("<formula1>\"good,ok,bad\"</formula1>"),
            "{sheet}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn data_validation_custom_formulas_are_written() {
        let dir = temp_path("validationformula");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("validation.csv");
        let output = dir.join("validation.xlsx");
        std::fs::write(&input, "id,amount\n1,10\n2,-5\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let validations = vec![DataValidation {
            column: "amount".to_string(),
            values: Vec::new(),
            formula: Some("AND(B2>0,B2<100)".to_string()),
            prompt_title: None,
            prompt_message: None,
            error_title: None,
            error_message: None,
            allow_blank: false,
            numeric: None,
            error_style: None,
            list_range: None,
        }];
        csv_to_xlsx_with_validations(&input, &output, Delimiter::Comma, &tx, &validations).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("type=\"custom\""), "{sheet}");
        assert!(
            sheet.contains("<formula1>AND(B2&gt;0,B2&lt;100)</formula1>"),
            "{sheet}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn data_validation_prompts_are_escaped_attributes() {
        let dir = temp_path("validationprompt");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("validation.csv");
        let output = dir.join("validation.xlsx");
        std::fs::write(&input, "id,status\n1,good\n2,ok\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let validations = vec![DataValidation {
            column: "status".to_string(),
            values: vec!["good".into(), "ok".into()],
            formula: None,
            prompt_title: Some("Status \"pick\"".to_string()),
            prompt_message: Some("Choose <one> & confirm".to_string()),
            error_title: None,
            error_message: None,
            allow_blank: false,
            numeric: None,
            error_style: None,
            list_range: None,
        }];
        csv_to_xlsx_with_validations(&input, &output, Delimiter::Comma, &tx, &validations).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("showInputMessage=\"1\""), "{sheet}");
        assert!(
            sheet.contains("promptTitle=\"Status &quot;pick&quot;\""),
            "{sheet}"
        );
        assert!(
            sheet.contains("prompt=\"Choose &lt;one&gt; &amp; confirm\""),
            "{sheet}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn data_validation_error_prompts_are_written() {
        let dir = temp_path("validationerror");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("validation.csv");
        let output = dir.join("validation.xlsx");
        std::fs::write(&input, "id,status\n1,good\n2,ok\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let validations = vec![DataValidation {
            column: "status".to_string(),
            values: vec!["good".into(), "ok".into()],
            formula: None,
            prompt_title: None,
            prompt_message: None,
            error_title: Some("Bad status".to_string()),
            error_message: Some("Pick \"good\" or ok".to_string()),
            allow_blank: false,
            numeric: None,
            error_style: None,
            list_range: None,
        }];
        csv_to_xlsx_with_validations(&input, &output, Delimiter::Comma, &tx, &validations).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("showErrorMessage=\"1\""), "{sheet}");
        assert!(sheet.contains("errorTitle=\"Bad status\""), "{sheet}");
        assert!(
            sheet.contains("error=\"Pick &quot;good&quot; or ok\""),
            "{sheet}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn data_validation_can_allow_blank_cells() {
        let dir = temp_path("validationblank");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("validation.csv");
        let output = dir.join("validation.xlsx");
        std::fs::write(&input, "id,status\n1,good\n2,ok\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let validations = vec![DataValidation {
            column: "status".to_string(),
            values: vec!["good".into(), "ok".into()],
            formula: None,
            prompt_title: None,
            prompt_message: None,
            error_title: None,
            error_message: None,
            allow_blank: true,
            numeric: None,
            error_style: None,
            list_range: None,
        }];
        csv_to_xlsx_with_validations(&input, &output, Delimiter::Comma, &tx, &validations).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("allowBlank=\"1\""), "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn worksheet_protection_is_written_before_filtering() {
        let dir = temp_path("protect");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("protect.csv");
        let output = dir.join("protect.xlsx");
        std::fs::write(&input, "id,amount\n1,10\n2,20\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx_with_protection(&input, &output, Delimiter::Comma, &tx).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(
            sheet.contains("<sheetProtection sheet=\"1\" objects=\"1\" scenarios=\"1\"/>"),
            "{sheet}"
        );
        assert!(sheet.contains("<autoFilter"), "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn explicit_column_widths_are_written() {
        let dir = temp_path("widths");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("widths.csv");
        let output = dir.join("widths.xlsx");
        std::fs::write(&input, "id,amount\n1,10\n2,20\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let widths = vec![("amount".to_string(), 20.0)];
        csv_to_xlsx_with_column_widths(&input, &output, Delimiter::Comma, &tx, &widths).unwrap();
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert!(
            sheet.contains("<col min=\"2\" max=\"2\" width=\"20\" customWidth=\"1\"/>"),
            "{sheet}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn single_sheet_protection_rejects_unknown_sheets() {
        let dir = temp_path("protection single unknown");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("map.xlsx");
        let sheets = vec![Sheet {
            name: "known".to_string(),
            headers: vec!["id".into()],
            rows: vec![vec!["1".into()]],
        }];
        write_sheets_with_protection_password(&path, &sheets, "test").unwrap();
        let error = xlsx_sheet_protection(&path, Some("missing")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("missing"), "{error}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn merged_ranges_are_written_and_read_back() {
        let dir = temp_path("merged writeback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("merged-out.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["title".into(), "value".into()],
            rows: vec![vec!["a".into(), "1".into()]],
        }];
        write_sheets_with_merges(&path, &sheets, &[vec!["A1:B1".to_string()]]).unwrap();
        let sheet = read_zip(&path, "xl/worksheets/sheet1.xml");
        assert!(
            sheet.contains("<mergeCells count=\"1\"><mergeCell ref=\"A1:B1\"/></mergeCells>"),
            "{sheet}"
        );
        let ranges = xlsx_merged_ranges(&path, None).unwrap();
        assert_eq!(ranges, vec!["A1:B1".to_string()]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn column_widths_are_written_and_read_back() {
        let dir = temp_path("widths readback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("widths.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["id".into(), "amount".into()],
            rows: vec![vec!["1".into(), "2".into()]],
        }];
        let widths = vec![vec![("id".to_string(), 9.0), ("amount".to_string(), 12.0)]];
        write_sheets_with_formats_and_widths(&path, &sheets, &[Vec::new()], &widths).unwrap();
        let found = xlsx_column_widths(&path, None).unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].column, "A");
        assert!((found[0].width - 9.0).abs() < 1e-9, "{found:?}");
        assert_eq!(found[1].column, "B");
        assert!((found[1].width - 12.0).abs() < 1e-9, "{found:?}");
        assert!(found[0].custom_width);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn row_heights_are_written_and_read_back() {
        let dir = temp_path("row height writeback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("heights-out.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["id".into()],
            rows: vec![vec!["1".into()]],
        }];
        write_sheets_with_heights(&path, &sheets, &[vec![(1, 30.0), (2, 18.5)]]).unwrap();
        let heights = xlsx_row_heights(&path, None).unwrap();
        assert_eq!(heights.len(), 2, "{heights:?}");
        assert_eq!(heights[0].height, 30.0);
        assert_eq!(heights[1].height, 18.5);
        assert!(heights[0].custom_height);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn row_heights_are_read_back() {
        let dir = temp_path("row height readback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rows.xlsx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        use std::io::Write as _;
        for (name, body) in [
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="one" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1" ht="30" customHeight="1"><c r="A1"><v>1</v></c></row><row r="2" ht="15"><c r="A2"><v>2</v></c></row></sheetData></worksheet>"#,
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let heights = xlsx_row_heights(&path, None).unwrap();
        assert_eq!(
            heights,
            vec![
                RowHeight {
                    row: 1,
                    height: 30.0,
                    custom_height: true
                },
                RowHeight {
                    row: 2,
                    height: 15.0,
                    custom_height: false
                }
            ]
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn hidden_rows_and_columns_are_read_back() {
        let dir = temp_path("hidden readback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hidden.xlsx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        use std::io::Write as _;
        for (name, body) in [
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="one" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols><col min="2" max="3" hidden="1" width="9"/></cols><sheetData><row r="1"><c r="A1"><v>1</v></c></row><row r="2" hidden="1"><c r="A2"><v>2</v></c></row></sheetData></worksheet>"#,
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let layout = xlsx_hidden_layout(&path, None).unwrap();
        assert_eq!(layout.rows, vec![2]);
        assert_eq!(layout.columns, vec!["B".to_string(), "C".to_string()]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn core_properties_are_written_and_read_back() {
        let dir = temp_path("core properties writeback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("props-out.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["id".into()],
            rows: vec![vec!["1".into()]],
        }];
        let properties = CoreProperties {
            title: Some("Quarterly".to_string()),
            creator: Some("csvx".to_string()),
            created: Some("2026-01-01T00:00:00Z".to_string()),
            modified: None,
        };
        write_sheets_with_properties(&path, &sheets, &properties).unwrap();
        let read_back = xlsx_core_properties(&path).unwrap();
        assert_eq!(read_back.title.as_deref(), Some("Quarterly"));
        assert_eq!(read_back.creator.as_deref(), Some("csvx"));
        assert_eq!(read_back.created.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(read_back.modified, None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn core_properties_can_be_set_after_export() {
        let dir = temp_path("props postprocess");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("props-set.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["id".into()],
            rows: vec![vec!["1".into()]],
        }];
        write_sheets_with_formats(&path, &sheets, &[Vec::new()]).unwrap();
        let properties = CoreProperties {
            title: Some("Set later".to_string()),
            creator: Some("csvx".to_string()),
            created: None,
            modified: None,
        };
        xlsx_set_core_properties(&path, &properties).unwrap();
        let read_back = xlsx_core_properties(&path).unwrap();
        assert_eq!(read_back.title.as_deref(), Some("Set later"));
        assert_eq!(read_back.creator.as_deref(), Some("csvx"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn core_properties_are_read_back() {
        let dir = temp_path("core properties");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("props.xlsx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        use std::io::Write as _;
        let core = r#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Report</dc:title><dc:creator>Lead</dc:creator><dcterms:created xsi:type="dcterms:W3CDTF">2026-01-02T03:04:05Z</dcterms:created><dcterms:modified xsi:type="dcterms:W3CDTF">2026-02-03T04:05:06Z</dcterms:modified></cp:coreProperties>"#;
        zip.start_file(
            "docProps/core.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(core.as_bytes()).unwrap();
        zip.finish().unwrap();
        let properties = xlsx_core_properties(&path).unwrap();
        assert_eq!(properties.title.as_deref(), Some("Report"));
        assert_eq!(properties.creator.as_deref(), Some("Lead"));
        assert_eq!(properties.created.as_deref(), Some("2026-01-02T03:04:05Z"));
        assert_eq!(properties.modified.as_deref(), Some("2026-02-03T04:05:06Z"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn sheet_visibility_states_are_written_and_read_back() {
        let dir = temp_path("sheet visibility writeback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("visibility-out.xlsx");
        let sheets = vec![
            Sheet {
                name: "shown".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["1".into()]],
            },
            Sheet {
                name: "secret".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["2".into()]],
            },
        ];
        write_sheets_with_visibility(
            &path,
            &sheets,
            &["visible".to_string(), "hidden".to_string()],
        )
        .unwrap();
        let states = xlsx_sheet_visibility(&path).unwrap();
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].state, "visible");
        assert_eq!(states[1].state, "hidden");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn sheet_visibility_states_are_read_back() {
        let dir = temp_path("sheet visibility");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("visibility.xlsx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        use std::io::Write as _;
        let workbook = r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="shown" sheetId="1" r:id="rId1"/><sheet name="secret" sheetId="2" state="hidden" r:id="rId2"/></sheets></workbook>"#;
        zip.start_file("xl/workbook.xml", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(workbook.as_bytes()).unwrap();
        zip.finish().unwrap();
        let sheets = xlsx_sheet_visibility(&path).unwrap();
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].name, "shown");
        assert_eq!(sheets[0].state, "visible");
        assert_eq!(sheets[1].name, "secret");
        assert_eq!(sheets[1].state, "hidden");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn merged_ranges_can_be_set_after_export() {
        let dir = temp_path("merged postprocess");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("merged-set.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["title".into(), "value".into()],
            rows: vec![vec!["a".into(), "1".into()]],
        }];
        write_sheets_with_formats(&path, &sheets, &[Vec::new()]).unwrap();
        xlsx_set_merge_cells(&path, &["A1:B1".to_string()]).unwrap();
        let ranges = xlsx_merged_ranges(&path, None).unwrap();
        assert_eq!(ranges, vec!["A1:B1".to_string()]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn merged_ranges_are_read_back() {
        let dir = temp_path("merged readback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("merged.xlsx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        use std::io::Write as _;
        for (name, body) in [
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="one" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>title</t></is></c></row></sheetData><mergeCells count="1"><mergeCell ref="A1:B1"/></mergeCells></worksheet>"#,
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let ranges = xlsx_merged_ranges(&path, None).unwrap();
        assert_eq!(ranges, vec!["A1:B1".to_string()]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn header_names_read_first_row_cells() {
        let dir = temp_path("header names");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("headers.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["id".into(), "amount".into()],
            rows: vec![vec!["1".into(), "2".into()]],
        }];
        write_sheets_with_formats(&path, &sheets, &[Vec::new()]).unwrap();
        let names = xlsx_header_names(&path, None).unwrap();
        assert_eq!(
            names,
            vec![
                ("A".to_string(), "id".to_string()),
                ("B".to_string(), "amount".to_string())
            ]
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn sheet_view_reads_frozen_panes_and_auto_filter() {
        let dir = temp_path("sheet view");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("view.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["id".into(), "amount".into()],
            rows: vec![vec!["1".into(), "2".into()]],
        }];
        write_sheets_with_formats_widths_and_protection(
            &path,
            &sheets,
            &[Vec::new()],
            &[Vec::new()],
            true,
            true,
            false,
            None,
            true,
            true,
            &[],
            &[],
        )
        .unwrap();
        let view = xlsx_sheet_view(&path, None).unwrap();
        assert_eq!(view.frozen_rows, 1);
        assert_eq!(view.frozen_columns, 0);
        assert_eq!(view.auto_filter.as_deref(), Some("A1:B2"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn conditional_formats_are_read_back_with_colors() {
        let dir = temp_path("cf readback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cf.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["id".into(), "amount".into()],
            rows: vec![vec!["1".into(), "1200".into()]],
        }];
        let rules = vec![ConditionalRule {
            column: "amount".to_string(),
            operator: ">".to_string(),
            value: "1000".to_string(),
            color: "red".to_string(),
        }];
        write_sheets_with_formats_widths_and_protection(
            &path,
            &sheets,
            &[Vec::new()],
            &[Vec::new()],
            true,
            true,
            false,
            None,
            true,
            true,
            &rules,
            &[],
        )
        .unwrap();
        let formats = xlsx_conditional_formats(&path, None).unwrap();
        assert_eq!(formats.len(), 1, "{formats:?}");
        assert_eq!(formats[0].range, "B2:B2");
        assert_eq!(formats[0].rule_type, "cellIs");
        assert_eq!(formats[0].operator.as_deref(), Some("greaterThan"));
        assert_eq!(formats[0].value.as_deref(), Some("1000"));
        assert_eq!(formats[0].color.as_deref(), Some("FFFFC7CE"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn formulas_are_read_back_with_cached_values() {
        let dir = temp_path("formula readback");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("formulas.xlsx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        use std::io::Write as _;
        for (name, body) in [
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="one" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><f>SUM(B2:C2)</f><v>3</v></c></row></sheetData></worksheet>"#,
            ),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        let formulas = xlsx_formulas(&path, None).unwrap();
        assert_eq!(
            formulas,
            vec![FormulaCell {
                cell: "A1".to_string(),
                formula: "SUM(B2:C2)".to_string(),
                cached_value: Some("3".to_string()),
            }]
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn protection_map_rejects_workbooks_without_sheets() {
        let dir = temp_path("protection map empty");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.xlsx");
        let file = std::fs::File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("xl/workbook.xml", zip::write::SimpleFileOptions::default())
            .unwrap();
        use std::io::Write as _;
        zip.write_all(
            br#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheets></sheets></workbook>"#,
        )
        .unwrap();
        zip.finish().unwrap();
        let error = xlsx_protection_map(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("no worksheets"), "{error}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn protection_map_rejects_non_xlsx_files() {
        let dir = temp_path("protection map invalid");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-a-workbook.xlsx");
        std::fs::write(&path, "id,amount\n1,10\n").unwrap();
        let error = xlsx_protection_map(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("not a zip/xlsx"), "{error}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn protection_map_matches_single_sheet_reads() {
        let dir = temp_path("protection map matches");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("map.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["1".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["2".into()]],
            },
        ];
        write_sheets_with_options_and_password(
            &path,
            &sheets,
            &[],
            true,
            true,
            Some("test"),
            true,
            false,
        )
        .unwrap();
        let map = xlsx_protection_map(&path).unwrap();
        for entry in &map {
            let single = xlsx_sheet_protection(&path, Some(&entry.name)).unwrap();
            assert_eq!(entry.protection, single, "{}", entry.name);
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn protection_map_reports_granularity() {
        let dir = temp_path("protection map granularity");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("map.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["1".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["2".into()]],
            },
        ];
        write_sheets_with_options_and_password(
            &path,
            &sheets,
            &[],
            true,
            true,
            Some("test"),
            false,
            true,
        )
        .unwrap();
        let map = xlsx_protection_map(&path).unwrap();
        assert_eq!(map.len(), 2);
        for entry in &map {
            assert!(entry.protection.protected);
            assert!(!entry.protection.objects_locked);
            assert!(entry.protection.scenarios_locked);
            assert!(entry.protection.password_protected);
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn protection_map_reports_every_sheet() {
        let dir = temp_path("protection map");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("map.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["1".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["2".into()]],
            },
        ];
        write_sheets_with_protection_password(&path, &sheets, "test").unwrap();
        let map = xlsx_protection_map(&path).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(map[0].name, "one");
        assert_eq!(map[1].name, "two");
        for entry in &map {
            assert!(entry.protection.protected);
            assert!(entry.protection.objects_locked);
            assert!(entry.protection.scenarios_locked);
            assert!(entry.protection.password_protected);
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn protection_password_and_granularity_round_trip() {
        let dir = temp_path("protection roundtrip");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.xlsx");
        let sheets = vec![Sheet {
            name: "one".to_string(),
            headers: vec!["id".into()],
            rows: vec![vec!["1".into()]],
        }];
        write_sheets_with_options_and_password(
            &path,
            &sheets,
            &[],
            true,
            true,
            Some("test"),
            true,
            false,
        )
        .unwrap();
        let detail = xlsx_sheet_protection(&path, None).unwrap();
        assert!(detail.protected);
        assert!(detail.objects_locked);
        assert!(!detail.scenarios_locked);
        assert!(detail.password_protected);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multi_sheet_protection_can_use_a_password() {
        let dir = temp_path("multi pass");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multi.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["1".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["2".into()]],
            },
        ];
        write_sheets_with_protection_password(&path, &sheets, "test").unwrap();
        for name in ["xl/worksheets/sheet1.xml", "xl/worksheets/sheet2.xml"] {
            let xml = read_zip(&path, name);
            assert!(xml.contains("password=\"CBEB\""), "{name}: {xml}");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multi_sheet_formats_widths_and_protection_coexist() {
        use crate::layout::NumberFormat;
        let dir = temp_path("multi fmt widths protect");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multi.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into(), "amount".into()],
                rows: vec![vec!["1".into(), "1234.5".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["rate".into()],
                rows: vec![vec!["0.125".into()]],
            },
        ];
        let formats = vec![
            vec![NumberFormat::Plain, NumberFormat::parse("0.00").unwrap()],
            vec![NumberFormat::parse("0.0%").unwrap()],
        ];
        let widths = vec![
            vec![("amount".to_string(), 20.0)],
            vec![("rate".to_string(), 25.0)],
        ];
        write_sheets_with_formats_widths_and_options(
            &path, &sheets, &formats, &widths, true, true, true,
        )
        .unwrap();
        let styles = read_zip(&path, "xl/styles.xml");
        assert!(styles.contains("formatCode=\"0.00\""), "{styles}");
        assert!(styles.contains("formatCode=\"0.0%\""), "{styles}");
        for (name, width) in [
            ("xl/worksheets/sheet1.xml", "20"),
            ("xl/worksheets/sheet2.xml", "25"),
        ] {
            let xml = read_zip(&path, name);
            assert!(xml.contains(&format!("width=\"{width}\"")), "{name}: {xml}");
            assert!(
                xml.contains("<sheetProtection sheet=\"1\" objects=\"1\" scenarios=\"1\"/>"),
                "{name}: {xml}"
            );
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multi_sheet_protection_is_written_on_every_sheet() {
        let dir = temp_path("multi protect");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multi.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["1".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["id".into()],
                rows: vec![vec!["2".into()]],
            },
        ];
        write_sheets_with_protection(&path, &sheets).unwrap();
        for name in ["xl/worksheets/sheet1.xml", "xl/worksheets/sheet2.xml"] {
            let xml = read_zip(&path, name);
            assert!(
                xml.contains("<sheetProtection sheet=\"1\" objects=\"1\" scenarios=\"1\"/>"),
                "{name}: {xml}"
            );
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multi_sheet_formats_widths_and_no_freeze_coexist() {
        use crate::layout::NumberFormat;
        let dir = temp_path("multiallnofreeze");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multi.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into(), "amount".into()],
                rows: vec![vec!["1".into(), "1234.5".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["rate".into()],
                rows: vec![vec!["0.125".into()]],
            },
        ];
        let formats = vec![
            vec![NumberFormat::Plain, NumberFormat::parse("0.00").unwrap()],
            vec![NumberFormat::parse("0.0%").unwrap()],
        ];
        let widths = vec![
            vec![("amount".to_string(), 20.0)],
            vec![("rate".to_string(), 25.0)],
        ];
        write_sheets_with_formats_widths_and_options(
            &path, &sheets, &formats, &widths, false, true, false,
        )
        .unwrap();
        let styles = read_zip(&path, "xl/styles.xml");
        assert!(styles.contains("formatCode=\"0.00\""), "{styles}");
        assert!(styles.contains("formatCode=\"0.0%\""), "{styles}");
        let one = read_zip(&path, "xl/worksheets/sheet1.xml");
        assert!(one.contains("width=\"20\""), "{one}");
        assert!(!one.contains("<pane "), "{one}");
        let two = read_zip(&path, "xl/worksheets/sheet2.xml");
        assert!(two.contains("width=\"25\""), "{two}");
        assert!(!two.contains("<pane "), "{two}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multi_sheet_formats_survive_without_freeze() {
        use crate::layout::NumberFormat;
        let dir = temp_path("multifmtsnofreeze");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multi.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into(), "amount".into()],
                rows: vec![vec!["1".into(), "1234.5".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["rate".into()],
                rows: vec![vec!["0.125".into()]],
            },
        ];
        let formats = vec![
            vec![NumberFormat::Plain, NumberFormat::parse("0.00").unwrap()],
            vec![NumberFormat::parse("0.0%").unwrap()],
        ];
        write_sheets_with_formats_and_options(&path, &sheets, &formats, false, true).unwrap();
        let styles = read_zip(&path, "xl/styles.xml");
        assert!(styles.contains("formatCode=\"0.00\""), "{styles}");
        assert!(styles.contains("formatCode=\"0.0%\""), "{styles}");
        let one = read_zip(&path, "xl/worksheets/sheet1.xml");
        assert!(!one.contains("<pane "), "{one}");
        let two = read_zip(&path, "xl/worksheets/sheet2.xml");
        assert!(!two.contains("<pane "), "{two}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multi_sheet_formats_and_widths_coexist() {
        use crate::layout::NumberFormat;
        let dir = temp_path("multifmtswidths");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multi.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into(), "amount".into()],
                rows: vec![vec!["1".into(), "1234.5".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["rate".into()],
                rows: vec![vec!["0.125".into()]],
            },
        ];
        let formats = vec![
            vec![
                NumberFormat::Plain,
                NumberFormat::parse("[Red]#,##0.00").unwrap(),
            ],
            vec![NumberFormat::parse("0.0%").unwrap()],
        ];
        let widths = vec![
            vec![("amount".to_string(), 20.0)],
            vec![("rate".to_string(), 25.0)],
        ];
        write_sheets_with_formats_and_widths(&path, &sheets, &formats, &widths).unwrap();
        let styles = read_zip(&path, "xl/styles.xml");
        assert!(styles.contains("formatCode=\"[Red]#,##0.00\""), "{styles}");
        assert!(styles.contains("formatCode=\"0.0%\""), "{styles}");
        let one = read_zip(&path, "xl/worksheets/sheet1.xml");
        assert!(one.contains("width=\"20\""), "{one}");
        let two = read_zip(&path, "xl/worksheets/sheet2.xml");
        assert!(two.contains("width=\"25\""), "{two}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn multi_sheet_widths_apply_per_sheet() {
        let dir = temp_path("multiwidths");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multi.xlsx");
        let sheets = vec![
            Sheet {
                name: "one".to_string(),
                headers: vec!["id".into(), "amount".into()],
                rows: vec![vec!["1".into(), "10".into()]],
            },
            Sheet {
                name: "two".to_string(),
                headers: vec!["rate".into()],
                rows: vec![vec!["0.25".into()]],
            },
        ];
        let widths = vec![
            vec![("amount".to_string(), 18.0)],
            vec![("rate".to_string(), 25.0)],
        ];
        write_sheets_with_widths(&path, &sheets, &widths).unwrap();
        let one = read_zip(&path, "xl/worksheets/sheet1.xml");
        assert!(one.contains("width=\"18\""), "{one}");
        let two = read_zip(&path, "xl/worksheets/sheet2.xml");
        assert!(two.contains("width=\"25\""), "{two}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn protection_state_is_read_back() {
        let dir = temp_path("readprotection");
        std::fs::create_dir_all(&dir).unwrap();
        let intact = dir.join("intact.csv");
        let protected = dir.join("protected.xlsx");
        let plain = dir.join("plain.xlsx");
        std::fs::write(&intact, "id,amount\n1,10\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx_with_protection(&intact, &protected, Delimiter::Comma, &tx).unwrap();
        write_sheets(
            &plain,
            &[Sheet {
                name: "plain".into(),
                headers: vec!["id".into()],
                rows: vec![vec!["1".into()]],
            }],
        )
        .unwrap();
        assert!(xlsx_sheet_is_protected(&protected, None).unwrap());
        assert!(!xlsx_sheet_is_protected(&plain, None).unwrap());
        let default = xlsx_sheet_protection(&protected, None).unwrap();
        assert!(default.protected);
        assert!(default.objects_locked);
        assert!(default.scenarios_locked);
        assert!(!default.password_protected);

        let opts = dir.join("opts.xlsx");
        write_sheets_with_options_and_password(
            &opts,
            &[Sheet {
                name: "opts".into(),
                headers: vec!["id".into()],
                rows: vec![vec!["1".into()]],
            }],
            &[],
            true,
            true,
            Some("test"),
            false,
            true,
        )
        .unwrap();
        let detail = xlsx_sheet_protection(&opts, None).unwrap();
        assert!(detail.protected);
        assert!(!detail.objects_locked);
        assert!(detail.scenarios_locked);
        assert!(detail.password_protected);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn same_colour_rules_share_one_dxf() {
        let dir = temp_path("dxfreuse");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("rules.csv");
        let output = dir.join("rules.xlsx");
        std::fs::write(&input, "id,amount\n1,500\n2,1500\n").unwrap();
        let rules = vec![
            ConditionalRule {
                column: "id".to_string(),
                operator: ">".to_string(),
                value: "0".to_string(),
                color: "red".to_string(),
            },
            ConditionalRule {
                column: "amount".to_string(),
                operator: ">".to_string(),
                value: "1000".to_string(),
                color: "red".to_string(),
            },
        ];
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &rules).unwrap();
        let styles = read_zip(&output, "xl/styles.xml");
        assert!(styles.contains("<dxfs count=\"1\">"), "{styles}");
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert_eq!(sheet.matches("dxfId=\"0\"").count(), 2, "{sheet}");
        assert!(!sheet.contains("dxfId=\"1\""), "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn blue_rules_share_one_dxf() {
        let dir = temp_path("dxfblue");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("rules.csv");
        let output = dir.join("rules.xlsx");
        std::fs::write(&input, "id,amount\n1,500\n2,1500\n").unwrap();
        let rules = vec![
            ConditionalRule {
                column: "id".to_string(),
                operator: ">".to_string(),
                value: "0".to_string(),
                color: "blue".to_string(),
            },
            ConditionalRule {
                column: "amount".to_string(),
                operator: ">".to_string(),
                value: "1000".to_string(),
                color: "blue".to_string(),
            },
        ];
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &rules).unwrap();
        let styles = read_zip(&output, "xl/styles.xml");
        assert!(styles.contains("<dxfs count=\"1\">"), "{styles}");
        assert!(styles.contains("FFBDD7EE"), "{styles}");
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert_eq!(sheet.matches("dxfId=\"0\"").count(), 2, "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn three_colours_create_three_dxfs() {
        let dir = temp_path("dxfthree");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("rules.csv");
        let output = dir.join("rules.xlsx");
        std::fs::write(&input, "id,amount,status\n1,500,ok\n2,1500,bad\n").unwrap();
        let rules = vec![
            ConditionalRule {
                column: "id".to_string(),
                operator: ">".to_string(),
                value: "0".to_string(),
                color: "red".to_string(),
            },
            ConditionalRule {
                column: "amount".to_string(),
                operator: ">".to_string(),
                value: "1000".to_string(),
                color: "blue".to_string(),
            },
            ConditionalRule {
                column: "status".to_string(),
                operator: "=".to_string(),
                value: "bad".to_string(),
                color: "green".to_string(),
            },
        ];
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &rules).unwrap();
        let styles = read_zip(&output, "xl/styles.xml");
        assert!(styles.contains("<dxfs count=\"3\">"), "{styles}");
        assert!(styles.contains("FFFFC7CE"), "{styles}");
        assert!(styles.contains("FFBDD7EE"), "{styles}");
        assert!(styles.contains("FFC6EFCE"), "{styles}");
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        for id in ["0", "1", "2"] {
            assert_eq!(
                sheet.matches(&format!("dxfId=\"{id}\"")).count(),
                1,
                "dxf {id}: {sheet}"
            );
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn unknown_rule_colours_fall_back_to_red() {
        let dir = temp_path("dxfunknown");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("rules.csv");
        let output = dir.join("rules.xlsx");
        std::fs::write(&input, "id,amount\n1,500\n2,1500\n").unwrap();
        let rules = vec![ConditionalRule {
            column: "amount".to_string(),
            operator: ">".to_string(),
            value: "1000".to_string(),
            color: "purple".to_string(),
        }];
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &rules).unwrap();
        let styles = read_zip(&output, "xl/styles.xml");
        assert!(styles.contains("<dxfs count=\"1\">"), "{styles}");
        assert!(styles.contains("FFFFC7CE"), "{styles}");
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert_eq!(sheet.matches("dxfId=\"0\"").count(), 1, "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn amber_rules_use_the_yellow_dxf() {
        let dir = temp_path("dxfamber");
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("rules.csv");
        let output = dir.join("rules.xlsx");
        std::fs::write(&input, "id,amount\n1,500\n2,1500\n").unwrap();
        let rules = vec![ConditionalRule {
            column: "amount".to_string(),
            operator: ">".to_string(),
            value: "1000".to_string(),
            color: "amber".to_string(),
        }];
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &rules).unwrap();
        let styles = read_zip(&output, "xl/styles.xml");
        assert!(styles.contains("<dxfs count=\"1\">"), "{styles}");
        assert!(styles.contains("FFFFEB9C"), "{styles}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn dxf_colour_table_maps_every_alias() {
        let dir = temp_path("dxftable");
        std::fs::create_dir_all(&dir).unwrap();
        for (color, argb) in [
            ("red", "FFFFC7CE"),
            ("blue", "FFBDD7EE"),
            ("green", "FFC6EFCE"),
            ("amber", "FFFFEB9C"),
            ("yellow", "FFFFEB9C"),
            ("purple", "FFFFC7CE"),
        ] {
            let input = dir.join(format!("{color}.csv"));
            let output = dir.join(format!("{color}.xlsx"));
            std::fs::write(&input, "id,amount\n1,1500\n").unwrap();
            let rules = vec![ConditionalRule {
                column: "amount".to_string(),
                operator: ">".to_string(),
                value: "1000".to_string(),
                color: color.to_string(),
            }];
            let (tx, _rx) = std::sync::mpsc::channel();
            csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &rules).unwrap();
            let styles = read_zip(&output, "xl/styles.xml");
            assert!(styles.contains("<dxfs count=\"1\">"), "{color}: {styles}");
            assert!(styles.contains(argb), "{color}: {styles}");
            let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
            assert_eq!(sheet.matches("dxfId=\"0\"").count(), 1, "{color}: {sheet}");
        }
        let input = dir.join("alias.csv");
        let output = dir.join("alias.xlsx");
        std::fs::write(&input, "id,amount\n1,1500\n").unwrap();
        let rules = vec![
            ConditionalRule {
                column: "id".to_string(),
                operator: ">".to_string(),
                value: "0".to_string(),
                color: "amber".to_string(),
            },
            ConditionalRule {
                column: "amount".to_string(),
                operator: ">".to_string(),
                value: "1000".to_string(),
                color: "yellow".to_string(),
            },
        ];
        let (tx, _rx) = std::sync::mpsc::channel();
        csv_to_xlsx_with_rules(&input, &output, Delimiter::Comma, &tx, &rules).unwrap();
        let styles = read_zip(&output, "xl/styles.xml");
        assert!(styles.contains("<dxfs count=\"1\">"), "{styles}");
        let sheet = read_zip(&output, "xl/worksheets/sheet1.xml");
        assert_eq!(sheet.matches("dxfId=\"0\"").count(), 2, "{sheet}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn sheet_password_hashes_match_excel() {
        assert_eq!(sheet_password_hash("test"), "CBEB");
        assert_eq!(sheet_password_hash("password"), "83AF");
        assert_eq!(sheet_password_hash(""), "CE4B");
    }

    #[test]
    fn formats_are_mapped_into_export_styles_and_round_trip() {
        use crate::layout::NumberFormat;
        let path = temp_path("formatted.xlsx");
        let output = temp_path("formatted.csv");
        let sheet = Sheet {
            name: "formatted".to_string(),
            headers: vec![
                "amount".into(),
                "rate".into(),
                "when".into(),
                "label".into(),
                "clock".into(),
                "code".into(),
                "frac".into(),
                "pctg".into(),
                "pct0".into(),
                "ampm".into(),
                "ampms".into(),
                "ampmh".into(),
                "shortdate".into(),
                "shortdt".into(),
                "shortdts".into(),
                "dashdate".into(),
                "dashdt".into(),
                "dashdts".into(),
                "shortampm".into(),
                "shortampms".into(),
                "dashampm".into(),
                "dashampms".into(),
                "isoampm".into(),
                "isoampms".into(),
                "daydate".into(),
                "padshort".into(),
            ],
            rows: vec![vec![
                "1234.5".into(),
                "0.123".into(),
                "2024-01-02".into(),
                "001".into(),
                "06:30:15".into(),
                "123".into(),
                "1.25".into(),
                "12345.6".into(),
                "12345.6".into(),
                "13:04:05".into(),
                "13:04:05".into(),
                "13:04:05".into(),
                "2024-01-02".into(),
                "2024-01-02 13:04:05".into(),
                "2024-01-02 13:04:05".into(),
                "2024-01-02".into(),
                "2024-01-02 13:04:05".into(),
                "2024-01-02 13:04:05".into(),
                "2024-01-02 13:04:05".into(),
                "2024-01-02 13:04:05".into(),
                "2024-01-02 13:04:05".into(),
                "2024-01-02 13:04:05".into(),
                "2024-01-02 13:04:05".into(),
                "2024-01-02 13:04:05".into(),
                "2024-01-02".into(),
                "2024-01-02".into(),
            ]],
        };
        write_sheets_with_formats(
            &path,
            &[sheet],
            &[vec![
                NumberFormat::parse("#,##0.00").unwrap(),
                NumberFormat::parse("0.0%").unwrap(),
                NumberFormat::parse("yyyy-mm-dd").unwrap(),
                NumberFormat::parse("@").unwrap(),
                NumberFormat::parse("hh:mm:ss").unwrap(),
                NumberFormat::parse("@").unwrap(),
                NumberFormat::parse("# ?/?").unwrap(),
                NumberFormat::parse("#,##0.0%").unwrap(),
                NumberFormat::parse("#,##0%").unwrap(),
                NumberFormat::parse("h:mm AM/PM").unwrap(),
                NumberFormat::parse("h:mm:ss AM/PM").unwrap(),
                NumberFormat::parse("hh AM/PM").unwrap(),
                NumberFormat::parse("m/d/yy").unwrap(),
                NumberFormat::parse("m/d/yy h:mm").unwrap(),
                NumberFormat::parse("m/d/yy h:mm:ss").unwrap(),
                NumberFormat::parse("yy-mm-dd").unwrap(),
                NumberFormat::parse("yy-mm-dd h:mm").unwrap(),
                NumberFormat::parse("yy-mm-dd h:mm:ss").unwrap(),
                NumberFormat::parse("m/d/yy h:mm AM/PM").unwrap(),
                NumberFormat::parse("m/d/yy h:mm:ss AM/PM").unwrap(),
                NumberFormat::parse("yy-mm-dd h:mm AM/PM").unwrap(),
                NumberFormat::parse("yy-mm-dd h:mm:ss AM/PM").unwrap(),
                NumberFormat::parse("yyyy-mm-dd hh:mm AM/PM").unwrap(),
                NumberFormat::parse("yyyy-mm-dd hh:mm:ss AM/PM").unwrap(),
                NumberFormat::parse("d/m/yyyy").unwrap(),
                NumberFormat::parse("mm/dd/yy").unwrap(),
            ]],
        )
        .unwrap();
        let styles = read_zip(&path, "xl/styles.xml");
        assert!(styles.contains("formatCode=\"#,##0.00\""), "{styles}");
        assert!(styles.contains("formatCode=\"0.0%\""), "{styles}");
        assert!(styles.contains("formatCode=\"yyyy-mm-dd\""), "{styles}");
        assert!(styles.contains("formatCode=\"hh:mm:ss\""), "{styles}");
        assert!(styles.contains("formatCode=\"@\""), "{styles}");
        assert!(styles.contains("formatCode=\"# ?/?\""), "{styles}");
        assert!(styles.contains("formatCode=\"#,##0.0%\""), "{styles}");
        assert!(styles.contains("formatCode=\"#,##0%\""), "{styles}");
        assert!(styles.contains("formatCode=\"h:mm AM/PM\""), "{styles}");
        assert!(styles.contains("formatCode=\"h:mm:ss AM/PM\""), "{styles}");
        assert!(styles.contains("formatCode=\"hh AM/PM\""), "{styles}");
        assert!(styles.contains("formatCode=\"m/d/yy\""), "{styles}");
        assert!(styles.contains("formatCode=\"m/d/yy h:mm\""), "{styles}");
        assert!(styles.contains("formatCode=\"m/d/yy h:mm:ss\""), "{styles}");
        assert!(styles.contains("formatCode=\"yy-mm-dd\""), "{styles}");
        assert!(styles.contains("formatCode=\"yy-mm-dd h:mm\""), "{styles}");
        assert!(
            styles.contains("formatCode=\"yy-mm-dd h:mm:ss\""),
            "{styles}"
        );
        read_xlsx_to_csv(&path, &output, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&output).unwrap(),
            "amount,rate,when,label,clock,code,frac,pctg,pct0,ampm,ampms,ampmh,shortdate,shortdt,shortdts,dashdate,dashdt,dashdts,shortampm,shortampms,dashampm,dashampms,isoampm,isoampms,daydate,padshort\n1234.5,0.123,2024-01-02,001,06:30:15,123,1.25,12345.6,12345.6,13:04:05,13:04:05,13:04:05,2024-01-02,2024-01-02T13:04:05,2024-01-02T13:04:05,2024-01-02,2024-01-02T13:04:05,2024-01-02T13:04:05,2024-01-02T13:04:05,2024-01-02T13:04:05,2024-01-02T13:04:05,2024-01-02T13:04:05,2024-01-02T13:04:05,2024-01-02T13:04:05,2024-01-02,2024-01-02\n"
        );
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(output);
    }

    #[test]
    fn builtin_date_num_fmt_14_reads_as_an_iso_date() {
        use crate::layout::NumberFormat;
        let styles = r#"<styleSheet><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14"/></cellXfs></styleSheet>"#;
        let formats = style_number_formats(styles);
        assert_eq!(formats.len(), 2);
        assert_eq!(formats[0], None);
        assert_eq!(
            formats[1],
            Some(NumberFormat::DateTime {
                pattern: "yyyy-mm-dd".into()
            })
        );
    }

    #[test]
    fn elapsed_formats_export_as_serial_and_round_trip_raw() {
        use crate::layout::NumberFormat;
        let path = temp_path("elapsed.xlsx");
        let output = temp_path("elapsed.csv");
        let sheet = Sheet {
            name: "elapsed".to_string(),
            headers: vec!["a".into(), "b".into()],
            rows: vec![vec!["1.5".into(), "36:00:00".into()]],
        };
        let format = NumberFormat::parse("[h]:mm:ss").unwrap();
        write_sheets_with_formats(&path, &[sheet], &[vec![format.clone(), format]]).unwrap();
        let styles = read_zip(&path, "xl/styles.xml");
        assert!(styles.contains("formatCode=\"[h]:mm:ss\""), "{styles}");
        let sheet_xml = read_zip(&path, "xl/worksheets/sheet1.xml");
        assert_eq!(sheet_xml.matches("<v>1.5</v>").count(), 2, "{sheet_xml}");
        read_xlsx_to_csv(&path, &output, None).unwrap();
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "a,b\n1.5,1.5\n");
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(output);
    }

    #[test]
    fn repeated_column_formats_are_deduplicated_across_rows() {
        use crate::layout::NumberFormat;
        let path = temp_path("dedup-formats.xlsx");
        let sheet = Sheet {
            name: "dedup".to_string(),
            headers: vec!["a".into(), "b".into()],
            rows: vec![
                vec!["1.5".into(), "2.5".into()],
                vec!["3.5".into(), "4.5".into()],
            ],
        };
        let format = NumberFormat::parse("#,##0.00").unwrap();
        write_sheets_with_formats(&path, &[sheet], &[vec![format.clone(), format]]).unwrap();
        let styles = read_zip(&path, "xl/styles.xml");
        assert_eq!(
            styles.matches("formatCode=\"#,##0.00\"").count(),
            1,
            "{styles}"
        );
        let columns = xlsx_column_formats(&path, None).unwrap();
        assert_eq!(columns.len(), 2, "{columns:?}");
        assert_eq!(columns[0].0, "a");
        assert_eq!(columns[1].0, "b");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn custom_format_codes_survive_import_readback() {
        use crate::layout::NumberFormat;
        let path = temp_path("custom-format-import.xlsx");
        let sheet = Sheet {
            name: "custom".to_string(),
            headers: vec!["amount".into()],
            rows: vec![vec!["1234.5".into()]],
        };
        let custom = NumberFormat::parse_raw("[$-409]m/d/yyyy").unwrap();
        write_sheets_with_formats(&path, &[sheet], &[vec![custom.clone()]]).unwrap();
        let columns = xlsx_column_formats(&path, None).unwrap();
        assert_eq!(columns.len(), 1, "{columns:?}");
        assert_eq!(columns[0].0, "amount");
        assert_eq!(columns[0].1, Some(custom));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn column_formats_are_read_back_from_export_styles() {
        use crate::layout::NumberFormat;
        let path = temp_path("column-formats.xlsx");
        let sheet = Sheet {
            name: "formats".to_string(),
            headers: vec![
                "amount".into(),
                "rate".into(),
                "when".into(),
                "label".into(),
                "sci".into(),
                "money".into(),
                "frac".into(),
                "pctg".into(),
                "pct0".into(),
                "ampm".into(),
                "ampms".into(),
                "ampmh".into(),
                "shortdate".into(),
                "shortdt".into(),
                "shortdts".into(),
                "dashdate".into(),
                "dashdt".into(),
                "dashdts".into(),
                "shortampm".into(),
                "shortampms".into(),
                "dashampm".into(),
                "dashampms".into(),
                "isoampm".into(),
                "isoampms".into(),
                "daydate".into(),
                "padshort".into(),
            ],
            rows: vec![
                vec![
                    "1234.5".into(),
                    "0.123".into(),
                    "2024-01-02".into(),
                    "x".into(),
                    "1234.5".into(),
                    "-1234.5".into(),
                    "1.25".into(),
                    "12345.6".into(),
                    "12345.6".into(),
                    "13:04:05".into(),
                    "13:04:05".into(),
                    "13:04:05".into(),
                    "2024-01-03".into(),
                    "2024-01-03 13:04:05".into(),
                    "2024-01-03 13:04:05".into(),
                    "2024-01-03".into(),
                    "2024-01-03 13:04:05".into(),
                    "2024-01-03 13:04:05".into(),
                    "2024-01-03 13:04:05".into(),
                    "2024-01-03 13:04:05".into(),
                    "2024-01-03 13:04:05".into(),
                    "2024-01-03 13:04:05".into(),
                    "2024-01-03 13:04:05".into(),
                    "2024-01-03 13:04:05".into(),
                    "2024-01-03".into(),
                    "2024-01-03".into(),
                ],
                vec![
                    "99.5".into(),
                    "0.5".into(),
                    "2024-01-03".into(),
                    "y".into(),
                    "9.5".into(),
                    "9.5".into(),
                    "1.25".into(),
                ],
            ],
        };
        write_sheets_with_formats(
            &path,
            &[sheet],
            &[vec![
                NumberFormat::parse("#,##0.00").unwrap(),
                NumberFormat::parse("0.0%").unwrap(),
                NumberFormat::parse("yyyy-mm-dd").unwrap(),
                NumberFormat::parse("plain").unwrap(),
                NumberFormat::parse("0.00E+00").unwrap(),
                NumberFormat::parse("$#,##0.00;($#,##0.00)").unwrap(),
                NumberFormat::parse("# ?/?").unwrap(),
                NumberFormat::parse("#,##0.0%").unwrap(),
                NumberFormat::parse("#,##0%").unwrap(),
                NumberFormat::parse("h:mm AM/PM").unwrap(),
                NumberFormat::parse("h:mm:ss AM/PM").unwrap(),
                NumberFormat::parse("hh AM/PM").unwrap(),
                NumberFormat::parse("m/d/yy").unwrap(),
                NumberFormat::parse("m/d/yy h:mm").unwrap(),
                NumberFormat::parse("m/d/yy h:mm:ss").unwrap(),
                NumberFormat::parse("yy-mm-dd").unwrap(),
                NumberFormat::parse("yy-mm-dd h:mm").unwrap(),
                NumberFormat::parse("yy-mm-dd h:mm:ss").unwrap(),
                NumberFormat::parse("m/d/yy h:mm AM/PM").unwrap(),
                NumberFormat::parse("m/d/yy h:mm:ss AM/PM").unwrap(),
                NumberFormat::parse("yy-mm-dd h:mm AM/PM").unwrap(),
                NumberFormat::parse("yy-mm-dd h:mm:ss AM/PM").unwrap(),
                NumberFormat::parse("yyyy-mm-dd hh:mm AM/PM").unwrap(),
                NumberFormat::parse("yyyy-mm-dd hh:mm:ss AM/PM").unwrap(),
                NumberFormat::parse("d/m/yyyy").unwrap(),
                NumberFormat::parse("mm/dd/yy").unwrap(),
            ]],
        )
        .unwrap();
        let columns = xlsx_column_formats(&path, None).unwrap();
        assert_eq!(columns.len(), 26, "{columns:?}");
        assert_eq!(columns[0].0, "amount");
        assert_eq!(columns[0].1, Some(NumberFormat::parse("#,##0.00").unwrap()));
        assert_eq!(columns[1].1, Some(NumberFormat::parse("0.0%").unwrap()));
        assert_eq!(
            columns[2].1,
            Some(NumberFormat::parse("yyyy-mm-dd").unwrap())
        );
        assert_eq!(columns[6].1, Some(NumberFormat::parse("# ?/?").unwrap()));
        assert_eq!(columns[3].1, None);
        assert_eq!(columns[4].1, Some(NumberFormat::parse("0.00E+00").unwrap()));
        assert_eq!(
            columns[5].1,
            Some(NumberFormat::parse("$#,##0.00;($#,##0.00)").unwrap())
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sanitises_sheet_names() {
        assert_eq!(workbook_sheet_name(Path::new("a:b*.csv")), "a_b_");
        assert_eq!(workbook_sheet_name(Path::new("")), "Sheet1");
    }
}
