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
pub const MAX_DATA_ROWS: u64 = 1_048_576;

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
    let result = csv_to_xlsx_inner(input, output, delimiter, tx, rules);
    if result.is_err() {
        let _ = std::fs::remove_file(output);
    }
    result
}

fn csv_to_xlsx_inner(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
    rules: &[ConditionalRule],
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
    write_part(
        &mut zip,
        "xl/styles.xml",
        &styles_xml_with_rules(rules),
        options,
    )?;

    zip.start_file("xl/worksheets/sheet1.xml", options)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut sheet = String::new();
    sheet.push_str(XML_HEADER);
    sheet.push_str(
        "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    );
    sheet.push_str("<dimension ref=\"A1\"/>");
    sheet.push_str("<sheetViews><sheetView workbookViewId=\"0\"><pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" state=\"frozen\"/></sheetView></sheetViews>");
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
            write_cell(&mut zip, index, data_rows + 1, cell, false)?;
        }
        write!(zip, "</row>")?;
        if data_rows.is_multiple_of(100_000) {
            tx.send(ProgressEvent::Info(format!("xlsx rows: {data_rows}")))
                .ok();
        }
    }
    write!(zip, "</sheetData>")?;
    if !rules.is_empty() && data_rows > 0 {
        let xml = conditional_formatting_xml(rules, &headers, data_rows + 1)?;
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
    if sheets.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "xlsx needs at least one sheet",
        ));
    }
    let file = std::fs::File::create(output)?;
    let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(4 * 1024 * 1024, file));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    write_part(
        &mut zip,
        "[Content_Types].xml",
        &content_types_for(sheets.len()),
        options,
    )?;
    write_part(&mut zip, "_rels/.rels", root_rels(), options)?;
    write_part(&mut zip, "xl/styles.xml", styles_xml(), options)?;

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
        workbook.push_str(&format!(
            "<sheet name=\"{}\" sheetId=\"{id}\" r:id=\"rId{}\"/>",
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
        xml.push_str("<sheetViews><sheetView workbookViewId=\"0\"><pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" state=\"frozen\"/></sheetView></sheetViews>");
        xml.push_str(&sheet_columns_xml(&sheet.headers, &sheet.rows));
        xml.push_str("<sheetData>");
        xml.push_str("<row r=\"1\">");
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
            xml.push_str(&format!("<row r=\"{number}\">"));
            for (column, cell) in cells.iter().enumerate() {
                write_cell_string(&mut xml, column, number, cell, false);
            }
            xml.push_str("</row>");
        }
        xml.push_str("</sheetData></worksheet>");
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

fn styles_xml() -> &'static str {
    concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
        "<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
        "<fonts count=\"2\"><font><sz val=\"11\"/><name val=\"Calibri\"/><family val=\"2\"/></font><font><b/><sz val=\"11\"/><name val=\"Calibri\"/><family val=\"2\"/></font></fonts>",
        "<fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills>",
        "<borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>",
        "<cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>",
        "<numFmts count=\"1\"><numFmt numFmtId=\"14\" formatCode=\"yyyy-mm-dd\"/></numFmts>",
        "<cellXfs count=\"3\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/><xf numFmtId=\"14\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/></cellXfs>",
        "<cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles>",
        "</styleSheet>"
    )
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

fn styles_xml_with_rules(rules: &[ConditionalRule]) -> String {
    let mut out = styles_xml().to_string();
    if rules.is_empty() {
        return out;
    }
    let mut dxfs = String::from("<dxfs count=\"");
    dxfs.push_str(&rules.len().to_string());
    dxfs.push_str("\">");
    for rule in rules {
        let (fg, bg) = dxf_color(&rule.color).unwrap_or(("FFFFC7CE", "FFFFC7CE"));
        dxfs.push_str(&format!(
            "<dxf><fill><patternFill patternType=\"solid\"><fgColor rgb=\"{fg}\"/><bgColor rgb=\"{bg}\"/></patternFill></fill></dxf>"
        ));
    }
    dxfs.push_str("</dxfs>");
    out = out.replace("</styleSheet>", &format!("{dxfs}</styleSheet>"));
    out
}

fn conditional_formatting_xml(
    rules: &[ConditionalRule],
    headers: &[String],
    last_row: u64,
) -> std::io::Result<String> {
    let mut out = String::new();
    for (index, rule) in rules.iter().enumerate() {
        let column = headers
            .iter()
            .position(|header| header == &rule.column)
            .ok_or_else(|| {
                invalid(&format!(
                    "highlight column {:?} is not a header",
                    rule.column
                ))
            })?;
        let operator = match rule.operator.as_str() {
            ">" => "greaterThan",
            "<" => "lessThan",
            ">=" => "greaterThanOrEqual",
            "<=" => "lessThanOrEqual",
            "=" => "equal",
            "!=" => "notEqual",
            other => {
                return Err(invalid(&format!(
                    "unsupported highlight operator {other:?}"
                )))
            }
        };
        let formula = if rule.value.parse::<f64>().is_ok() {
            rule.value.clone()
        } else {
            format!("\"{}\"", xml_attr(&rule.value))
        };
        out.push_str(&format!(
            "<conditionalFormatting sqref=\"{col}2:{col}{last_row}\"><cfRule type=\"cellIs\" dxfId=\"{index}\" priority=\"{priority}\" operator=\"{operator}\"><formula>{formula}</formula></cfRule></conditionalFormatting>",
            col = column_name(column),
            priority = index + 1,
        ));
    }
    Ok(out)
}

/// Built-in Excel number formats that represent dates.
fn date_style_map(styles: &str) -> Vec<bool> {
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
                .is_some_and(|id| matches!(id, 14..=22 | 45..=47))
        })
        .collect()
}

fn serial_to_date(serial: f64) -> Option<String> {
    if !serial.is_finite() || serial.fract() != 0.0 {
        return None;
    }
    let serial = serial as i64;
    if !(1..=2_958_465).contains(&serial) {
        return None;
    }
    let days = serial + days_from_civil(1899, 12, 30);
    let (year, month, day) = civil_from_days(days);
    if year < 1900 {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
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

fn sheet_columns_xml(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut xml = String::from("<cols>");
    for (index, header) in headers.iter().enumerate() {
        let wider = rows
            .iter()
            .take(20)
            .filter_map(|row| row.get(index))
            .map(|cell| cell.chars().count())
            .max()
            .unwrap_or(0);
        let width = header.chars().count().max(wider).clamp(8, 60);
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

/// Read one worksheet from an `.xlsx`/`.xlsm` workbook into a CSV file.
///
/// The reader understands a small, common subset: shared strings, inline
/// strings, cached formula values, booleans, numbers and built-in date styles
/// (serials are rendered as `YYYY-MM-DD`). Merged cells, charts and comments
/// are ignored.
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
    let selected = match sheet {
        Some(name) => sheets
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| invalid(&format!("xlsx has no sheet named {name:?}")))?,
        None => &sheets[0],
    };
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
    let sheet_xml = read_entry(&mut archive, &normalized)?;
    let shared = read_entry(&mut archive, "xl/sharedStrings.xml").unwrap_or_default();
    let shared = shared_strings(&shared);
    let styles = read_entry(&mut archive, "xl/styles.xml").unwrap_or_default();
    let date_styles = date_style_map(&styles);

    let rows = sheet_rows(&sheet_xml, &shared, &date_styles)?;
    if rows.is_empty() {
        return Err(invalid("xlsx worksheet has no rows"));
    }
    let mut table: Vec<Vec<String>> = Vec::new();
    for (row_number, cells) in rows {
        if row_number == 0 {
            continue;
        }
        if cells.iter().all(|cell| cell.is_empty()) {
            continue;
        }
        table.push(cells);
    }
    if table.is_empty() {
        return Err(invalid("xlsx worksheet has no non-empty rows"));
    }

    let width = table.iter().map(|row| row.len()).max().unwrap_or(0);
    if width == 0 {
        return Err(invalid("xlsx worksheet is empty"));
    }
    let mut text = String::new();
    for row in &table {
        let mut padded: Vec<&str> = Vec::with_capacity(width);
        for index in 0..width {
            padded.push(row.get(index).map(String::as_str).unwrap_or(""));
        }
        text.push_str(&crate::csv_reader::join_csv_line(&padded, b','));
    }
    std::fs::write(output, text)
}

struct SheetRef {
    name: String,
    rid: String,
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
        out.push(SheetRef {
            name: xml_unescape(&name),
            rid: rid.to_string(),
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

fn sheet_rows(
    xml: &str,
    shared: &[String],
    date_styles: &[bool],
) -> std::io::Result<Vec<(u64, Vec<String>)>> {
    let row_re = Regex::new(r#"(?s)<row\b([^>]*?)(?:/>|>(.*?)</row>)"#).unwrap();
    let cell_re = Regex::new(r#"(?s)<c\b([^>]*?)(?:/>|>(.*?)</c>)"#).unwrap();
    let mut out = Vec::new();
    for captures in row_re.captures_iter(xml) {
        let row_attrs = &captures[1];
        let row: u64 = attr(row_attrs, "r")
            .and_then(|value| value.parse().ok())
            .unwrap_or(out.len() as u64 + 1);
        let body = captures.get(2).map(|m| m.as_str()).unwrap_or("");
        let mut cells = Vec::new();
        for cell in cell_re.captures_iter(body) {
            let attrs = &cell[1];
            let body = cell.get(2).map(|m| m.as_str()).unwrap_or("");
            let reference = attr(attrs, "r").unwrap_or_default();
            let column = column_from_reference(&reference).unwrap_or(cells.len());
            while cells.len() <= column {
                cells.push(String::new());
            }
            cells[column] = cell_value(attrs, body, shared, date_styles)?;
        }
        out.push((row, cells));
    }
    Ok(out)
}

fn column_from_reference(reference: &str) -> Option<usize> {
    let mut column = 0usize;
    let mut saw = false;
    for byte in reference.bytes() {
        if byte.is_ascii_alphabetic() {
            column = column * 26 + (byte.to_ascii_uppercase() - b'A' + 1) as usize;
            saw = true;
        } else if byte.is_ascii_digit() {
            break;
        } else {
            return None;
        }
    }
    saw.then_some(column.saturating_sub(1))
}

fn cell_value(
    attrs: &str,
    body: &str,
    shared: &[String],
    date_styles: &[bool],
) -> std::io::Result<String> {
    let kind = attr(attrs, "t").unwrap_or_default();
    match kind.as_str() {
        "inlineStr" => {
            let is_re = Regex::new(r#"(?s)<is\b[^>]*>(.*?)</is>"#).unwrap();
            let t_re = Regex::new(r#"(?s)<t\b[^>]*>(.*?)</t>"#).unwrap();
            let Some(is) = is_re.captures(body) else {
                return Ok(String::new());
            };
            let mut text = String::new();
            for run in t_re.captures_iter(&is[1]) {
                text.push_str(&xml_unescape(&run[1]));
            }
            Ok(text)
        }
        "s" => {
            let v_re = Regex::new(r#"(?s)<v>(.*?)</v>"#).unwrap();
            let index = v_re
                .captures(body)
                .and_then(|m| m[1].trim().parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            Ok(shared.get(index).cloned().unwrap_or_default())
        }
        "str" => {
            let v_re = Regex::new(r#"(?s)<v>(.*?)</v>"#).unwrap();
            Ok(v_re
                .captures(body)
                .map(|m| xml_unescape(m[1].trim()))
                .unwrap_or_default())
        }
        "b" => {
            let v_re = Regex::new(r#"(?s)<v>(.*?)</v>"#).unwrap();
            Ok(v_re
                .captures(body)
                .map(|m| if m[1].trim() == "1" { "true" } else { "false" })
                .unwrap_or_default()
                .to_string())
        }
        _ => {
            let v_re = Regex::new(r#"(?s)<v>(.*?)</v>"#).unwrap();
            let raw = v_re
                .captures(body)
                .map(|m| xml_unescape(m[1].trim()))
                .unwrap_or_default();
            let style = attr(attrs, "s")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            if date_styles.get(style).copied().unwrap_or(false) {
                if let Ok(serial) = raw.parse::<f64>() {
                    if let Some(date) = serial_to_date(serial) {
                        return Ok(date);
                    }
                }
            }
            Ok(raw)
        }
    }
}

fn attr(attrs: &str, name: &str) -> Option<String> {
    let pattern = format!(r#"{name}="([^"]*)""#);
    Regex::new(&pattern)
        .ok()?
        .captures(attrs)
        .map(|captures| xml_unescape(&captures[1]))
}

fn xml_unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        let Some(semicolon) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[..semicolon + 1];
        let replacement: String = match entity {
            "&amp;" => "&".into(),
            "&lt;" => "<".into(),
            "&gt;" => ">".into(),
            "&quot;" => "\"".into(),
            "&apos;" => "'".into(),
            other if other.starts_with("&#x") || other.starts_with("&#X") => {
                char::from_u32(u32::from_str_radix(&other[3..other.len() - 1], 16).unwrap_or(0))
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            }
            other if other.starts_with("&#") => {
                char::from_u32(other[2..other.len() - 1].parse().unwrap_or(0))
                    .map(|c| c.to_string())
                    .unwrap_or_default()
            }
            _ => String::new(),
        };
        out.push_str(&replacement);
        rest = &tail[semicolon + 1..];
    }
    out.push_str(rest);
    out
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
        read_xlsx_to_csv(&workbook, &back, Some("Data")).unwrap();
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
    fn sanitises_sheet_names() {
        assert_eq!(workbook_sheet_name(Path::new("a:b*.csv")), "a_b_");
        assert_eq!(workbook_sheet_name(Path::new("")), "Sheet1");
    }
}
