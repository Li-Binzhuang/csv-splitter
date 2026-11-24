//! Minimal, dependency-free `.xlsx` writer.
//!
//! Writing a workbook is just a ZIP of XML parts. This module streams the CSV
//! into `xl/worksheets/sheet1.xml` so a large export does not build the whole
//! sheet in memory first. It deliberately does not write formulas, charts or
//! conditional formatting; values and a bold frozen header are enough for the
//! first Excel hand-off.

use crate::encoding::open_decoded;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Excel's hard limit for data rows in one worksheet (excluding the header).
pub const MAX_DATA_ROWS: u64 = 1_048_576;

/// One worksheet's input: headers plus rows already in memory.
pub struct Sheet {
    pub name: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// Export a CSV file as a single-sheet workbook, streaming rows.
pub fn csv_to_xlsx(
    input: &Path,
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
) -> std::io::Result<()> {
    let result = csv_to_xlsx_inner(input, output, delimiter, tx);
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
    write_part(&mut zip, "xl/styles.xml", styles_xml(), options)?;

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
    write!(zip, "</sheetData></worksheet>")?;
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
    for (index, sheet) in sheets.iter().enumerate() {
        let name = workbook_sheet_name(std::path::Path::new(&sheet.name));
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
        xml.push_str("<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>");
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
        "<cellXfs count=\"2\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/></cellXfs>",
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
