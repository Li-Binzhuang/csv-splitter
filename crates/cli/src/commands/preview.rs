use crate::commands::shared::print_table;
use crate::{
    parse_delimiter, print_command_help, project_columns, report_failure, warn_pending_edits,
    warn_quote_style, Parser,
};
use csvx_core::csv_reader::{detect_delimiter, parse_csv_record};
use csvx_core::index::{SparseIndex, DEFAULT_INTERVAL};
use csvx_core::types::Delimiter;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) fn cmd_preview(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut rows = 10usize;
    let mut delim = "auto".to_string();
    let mut start_row: u64 = 0;
    let mut shown_columns: Vec<String> = Vec::new();
    let mut index_path: Option<PathBuf> = None;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--columns" => match p.value("--columns") {
                Ok(v) => {
                    shown_columns = v
                        .split(',')
                        .map(|c| c.trim().to_string())
                        .filter(|c| !c.is_empty())
                        .collect()
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--rows" => match p
                .value("--rows")
                .and_then(|v| v.parse().map_err(|_| "bad --rows".to_string()))
            {
                Ok(v) => rows = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--start-row" => match p
                .value("--start-row")
                .and_then(|v| v.parse().map_err(|_| "bad --start-row".to_string()))
            {
                Ok(v) => start_row = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--index" => match p.value("--index") {
                Ok(v) => index_path = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
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
            "-h" | "--help" => {
                print_command_help("preview");
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
        eprintln!("error: preview needs an <input> path");
        return 2;
    };
    warn_quote_style(&input);
    warn_pending_edits(&input);

    let override_delim = if delim == "auto" {
        None
    } else {
        match parse_delimiter(&delim, &input) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        }
    };

    if start_row > 0 {
        return preview_from_index(&input, rows, start_row, override_delim, index_path);
    }

    let quotes = match csvx_core::csv_reader::QuoteStyle::parse(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    match csvx_core::preview::load_preview_with(&input, rows, override_delim, quotes) {
        Ok(preview) => {
            println!(
                "{} — {:.1} MB, ~{} rows, delimiter {:?}",
                input.display(),
                preview.file_size as f64 / (1024.0 * 1024.0),
                preview.total_rows_estimate,
                preview.detected_delimiter
            );
            let mut headers = preview.headers.clone();
            let mut shown = preview.rows.clone();
            if let Err(code) = project_columns(&mut headers, &mut shown, &shown_columns) {
                return code;
            }
            print_table(&headers, &shown);
            0
        }
        Err(e) => report_failure("preview", e),
    }
}

/// Print a window of rows by seeking into the file through the sparse index.
fn preview_from_index(
    input: &Path,
    rows: usize,
    start_row: u64,
    delim: Option<Delimiter>,
    index_path: Option<PathBuf>,
) -> i32 {
    let sidecar = index_path.unwrap_or_else(|| SparseIndex::sidecar_path(input));
    let started = Instant::now();
    let idx = match SparseIndex::load(&sidecar) {
        Ok(i) if i.matches(input) => i,
        _ => match SparseIndex::build(input, DEFAULT_INTERVAL) {
            Ok(i) => {
                i.save(&sidecar).ok();
                i
            }
            Err(e) => {
                let _ = report_failure("index", format!("cannot build the index: {e}"));
                return 1;
            }
        },
    };
    let delim_byte = match delim {
        Some(d) => d.byte(),
        None => detect_delimiter(&mut std::fs::File::open(input).unwrap())
            .map(|(d, _)| d.byte())
            .unwrap_or(b','),
    };

    let header = idx.read_header(input).unwrap_or_default();
    println!(
        "{} — {} data rows, index ready in {:?} (interval {}), from row {}",
        input.display(),
        idx.data_rows(),
        started.elapsed(),
        idx.interval(),
        start_row
    );
    println!("{}", String::from_utf8_lossy(&header).trim_end());
    println!("{}", "-".repeat(40));

    match idx.read_rows(input, start_row, rows as u64) {
        Ok(found) => {
            for (i, raw) in found.iter().enumerate() {
                let text = String::from_utf8_lossy(raw);
                let fields = parse_csv_record(text.trim_end(), delim_byte);
                println!("{:>9}  {}", start_row + i as u64 + 1, fields.join(" | "));
            }
            0
        }
        Err(e) => {
            let _ = report_failure("preview", format!("reading rows: {e}"));
            1
        }
    }
}
