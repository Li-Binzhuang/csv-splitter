use crate::types::Delimiter;
use std::io::{self, BufRead, BufReader, Read};

/// Parse a single CSV line respecting quoted fields.
/// Properly handles multi-byte UTF-8 characters (e.g. Chinese, Japanese, etc.).
pub fn parse_csv_line(line: &str, delim: u8) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let trimmed = line.trim_end_matches(['\n', '\r']);
    let delim_char = delim as char;

    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(ch);
            }
        } else if ch == '"' {
            in_quotes = true;
        } else if ch == delim_char {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    fields.push(current);
    fields
}

/// Reconstruct a CSV line from fields.
pub fn join_csv_line(fields: &[&str], delim: u8) -> String {
    let d = delim as char;
    let mut out = String::new();
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push(d);
        }
        if f.contains(d) || f.contains('"') || f.contains('\n') {
            out.push('"');
            out.push_str(&f.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(f);
        }
    }
    out.push('\n');
    out
}

/// Read just the header row from a CSV file, returning column names.
pub fn read_headers(path: &std::path::Path, delim: Option<Delimiter>) -> io::Result<Vec<String>> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut first_line = String::new();
    reader.read_line(&mut first_line)?;
    let d = delim.unwrap_or(Delimiter::Comma).byte();
    Ok(parse_csv_line(&first_line, d))
}

/// Auto-detect delimiter by sampling first few lines.
pub fn detect_delimiter(reader: &mut impl Read) -> io::Result<(Delimiter, String)> {
    let mut buf_reader = BufReader::new(reader);
    let mut first_lines = String::new();
    for _ in 0..5 {
        let mut line = String::new();
        if buf_reader.read_line(&mut line)? == 0 {
            break;
        }
        first_lines.push_str(&line);
    }

    let first_line = first_lines.lines().next().unwrap_or("");
    let candidates: &[(u8, Delimiter)] = &[
        (b',', Delimiter::Comma),
        (b'\t', Delimiter::Tab),
        (b';', Delimiter::Semicolon),
        (b'|', Delimiter::Pipe),
    ];

    let mut best = Delimiter::Comma;
    let mut best_count = 0;
    for &(byte, delim) in candidates {
        let count = first_line.as_bytes().iter().filter(|&&b| b == byte).count();
        if count > best_count {
            best_count = count;
            best = delim;
        }
    }

    Ok((best, first_lines))
}
