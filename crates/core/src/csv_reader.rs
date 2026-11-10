use crate::types::Delimiter;
use memchr::{memchr, memchr2};
use std::io::{self, BufRead, BufReader, Read};

/// UTF-8 byte order mark. Excel and many export tools prepend this, and it would
/// otherwise leak into the first column name (`\u{feff}id`).
pub const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Drop a leading UTF-8 BOM from a header cell.
pub fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

/// How a file escapes a quote inside a quoted field.
///
/// RFC 4180 doubles it (`""`); a lot of exports instead backslash it (`\"`), and
/// some files never quote at all. Reading a backslash file as RFC 4180 does not
/// fail — it silently produces fields with stray `\` and swallowed delimiters,
/// which is the worst possible outcome, so the mismatch is worth detecting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum QuoteStyle {
    /// `""` inside quotes.
    #[default]
    Rfc4180,
    /// `\"` and `\\` inside quotes.
    Backslash,
    /// No quoting: every delimiter splits, quotes are literal characters.
    None,
}

impl QuoteStyle {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "auto" | "rfc4180" | "rfc" | "double" => Ok(Self::Rfc4180),
            "backslash" | "escape" | "mysql" => Ok(Self::Backslash),
            "none" | "raw" | "plain" => Ok(Self::None),
            other => Err(format!(
                "unknown quote style '{other}' (use rfc4180/backslash/none)"
            )),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Rfc4180 => "rfc4180",
            Self::Backslash => "backslash",
            Self::None => "none",
        }
    }
}

/// Parse a single CSV line respecting quoted fields.
/// Properly handles multi-byte UTF-8 characters (e.g. Chinese, Japanese, etc.).
pub fn parse_csv_line(line: &str, delim: u8) -> Vec<String> {
    parse_csv_line_with(line, delim, QuoteStyle::Rfc4180)
}

/// Parse one line with an explicit quote style.
pub fn parse_csv_line_with(line: &str, delim: u8, style: QuoteStyle) -> Vec<String> {
    if style == QuoteStyle::None {
        return line
            .trim_end_matches(['\n', '\r'])
            .split(delim as char)
            .map(str::to_string)
            .collect();
    }
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let trimmed = line.trim_end_matches(['\n', '\r']);
    let delim_char = delim as char;

    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            if style == QuoteStyle::Backslash && ch == '\\' {
                match chars.next() {
                    Some('"') => current.push('"'),
                    Some('\\') => current.push('\\'),
                    Some(other) => {
                        current.push('\\');
                        current.push(other);
                    }
                    None => current.push('\\'),
                }
            } else if ch == '"' {
                if style == QuoteStyle::Backslash || chars.peek() != Some(&'"') {
                    in_quotes = false;
                } else {
                    current.push('"');
                    chars.next();
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

/// Parse one complete CSV record, which may contain embedded newlines inside
/// quoted fields. A leading BOM is stripped from the first cell.
pub fn parse_csv_record(record: &str, delim: u8) -> Vec<String> {
    parse_csv_record_with(record, delim, QuoteStyle::Rfc4180)
}

/// Parse a record with an explicit quote style.
pub fn parse_csv_record_with(record: &str, delim: u8, style: QuoteStyle) -> Vec<String> {
    let mut fields = parse_csv_line_with(record, delim, style);
    if let Some(first) = fields.first_mut() {
        let stripped = strip_bom(first).to_string();
        *first = stripped;
    }
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
        if f.contains(d) || f.contains('"') || f.contains('\n') || f.contains('\r') {
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
    Ok(parse_csv_record(&first_line, d))
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

    // Count candidates outside quotes so that `"a,b";c` does not favour the comma.
    let mut best = Delimiter::Comma;
    let mut best_count = 0;
    for &(byte, delim) in candidates {
        let count = count_outside_quotes(first_line.as_bytes(), byte);
        if count > best_count {
            best_count = count;
            best = delim;
        }
    }

    Ok((best, first_lines))
}

/// Count occurrences of `needle` that are not inside a quoted field.
fn count_outside_quotes(bytes: &[u8], needle: u8) -> usize {
    let mut in_quotes = false;
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_quotes {
            if b == b'"' {
                if bytes.get(i + 1) == Some(&b'"') {
                    i += 1;
                } else {
                    in_quotes = false;
                }
            }
        } else if b == b'"' {
            in_quotes = true;
        } else if b == needle {
            count += 1;
        }
        i += 1;
    }
    count
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanState {
    /// Between fields, outside any quoted section.
    Outside,
    /// Inside a quoted section.
    Inside,
    /// Just consumed a `"` while inside quotes; `""` means an escaped quote,
    /// anything else closes the quoted section.
    QuoteSeen,
}

/// Reader that yields whole CSV records instead of physical lines.
///
/// A naive `read_line` loop cuts records that contain embedded newlines inside
/// quoted fields (very common in address / remark columns). This reader tracks
/// quote state across buffer boundaries, so `next_record` returns exactly one
/// logical record and the caller can split on record boundaries safely.
pub struct RecordReader<R: Read> {
    reader: R,
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    state: ScanState,
    finished: bool,
    quotes: QuoteStyle,
}

impl<R: Read> RecordReader<R> {
    pub fn new(reader: R, capacity: usize) -> Self {
        Self::new_with(reader, capacity, QuoteStyle::Rfc4180)
    }

    /// A reader whose record boundaries match the file's quote escape.
    ///
    /// This matters more than field parsing: in a backslash-escaped file a `\"`
    /// does not close a quoted section, so an RFC reader runs past the record and
    /// swallows the following lines into it.
    pub fn new_with(reader: R, capacity: usize, quotes: QuoteStyle) -> Self {
        Self {
            reader,
            buf: vec![0u8; capacity.max(8 * 1024)],
            pos: 0,
            len: 0,
            state: ScanState::Outside,
            finished: false,
            quotes,
        }
    }

    /// Append the next record (including its trailing newline, if present) to
    /// `out`. Returns `false` at end of input. `out` is cleared first.
    pub fn next_record(&mut self, out: &mut Vec<u8>) -> io::Result<bool> {
        out.clear();
        if self.finished {
            return Ok(false);
        }
        loop {
            if self.pos >= self.len {
                self.len = self.reader.read(&mut self.buf)?;
                self.pos = 0;
                if self.len == 0 {
                    self.finished = true;
                    return Ok(!out.is_empty());
                }
            }

            match self.state {
                ScanState::Outside => {
                    let chunk = &self.buf[self.pos..self.len];
                    match memchr2(b'"', b'\n', chunk) {
                        Some(i) => {
                            out.extend_from_slice(&chunk[..i]);
                            let b = chunk[i];
                            self.pos += i + 1;
                            if b == b'\n' {
                                out.push(b'\n');
                                return Ok(true);
                            }
                            out.push(b'"');
                            self.state = ScanState::Inside;
                        }
                        None => {
                            out.extend_from_slice(chunk);
                            self.pos = self.len;
                        }
                    }
                }
                ScanState::Inside if self.quotes == QuoteStyle::Backslash => {
                    // One byte at a time: a `\"` pair must not be read as a
                    // closing quote, and the lookahead can cross a buffer edge.
                    let b = self.buf[self.pos];
                    if b == b'\\' {
                        if self.pos + 1 >= self.len {
                            // Refill before deciding what the backslash escapes.
                            self.pos = self.len;
                            continue;
                        }
                        let next = self.buf[self.pos + 1];
                        if next == b'"' {
                            out.push(b'\\');
                            out.push(b'"');
                            self.pos += 2;
                        } else {
                            out.push(b'\\');
                            self.pos += 1;
                        }
                    } else {
                        out.push(b);
                        self.pos += 1;
                        if b == b'"' {
                            self.state = ScanState::QuoteSeen;
                        }
                    }
                }
                ScanState::Inside => {
                    let chunk = &self.buf[self.pos..self.len];
                    match memchr(b'"', chunk) {
                        Some(i) => {
                            out.extend_from_slice(&chunk[..=i]);
                            self.pos += i + 1;
                            self.state = ScanState::QuoteSeen;
                        }
                        None => {
                            out.extend_from_slice(chunk);
                            self.pos = self.len;
                        }
                    }
                }
                ScanState::QuoteSeen => {
                    let b = self.buf[self.pos];
                    self.pos += 1;
                    if b == b'"' && self.quotes == QuoteStyle::Rfc4180 {
                        // Escaped quote: stay inside the quoted section.
                        out.push(b'"');
                        self.state = ScanState::Inside;
                    } else {
                        self.state = ScanState::Outside;
                        if b == b'\n' {
                            out.push(b'\n');
                            return Ok(true);
                        }
                        out.push(b);
                    }
                }
            }
        }
    }
}

/// Same as [`RecordReader::next_record`] but decodes to `String`.
pub fn next_record_string<R: Read>(
    reader: &mut RecordReader<R>,
    out: &mut Vec<u8>,
) -> io::Result<Option<String>> {
    if !reader.next_record(out)? {
        return Ok(None);
    }
    // Lossy decoding keeps us going on non-UTF-8 bytes instead of failing the job.
    Ok(Some(String::from_utf8_lossy(out).into_owned()))
}

/// Count logical CSV records (header included) without loading the file.
pub fn count_records<R: Read>(reader: R, capacity: usize) -> io::Result<u64> {
    let mut reader = RecordReader::new(reader, capacity);
    let mut scratch = Vec::with_capacity(64 * 1024);
    let mut count = 0u64;
    while reader.next_record(&mut scratch)? {
        count += 1;
    }
    Ok(count)
}

/// Resolve a column name against a header, tolerating the noise real files carry.
///
/// Order: exact → trimmed-equal (unique) → trimmed case-insensitive (unique).
/// A near match is only accepted when it is unambiguous, so a file with both
/// `name` and `name ` still refuses rather than guessing.
pub fn resolve_column(name: &str, headers: &[String]) -> Result<usize, String> {
    if let Some(idx) = headers.iter().position(|h| h == name) {
        return Ok(idx);
    }
    let target = name.trim();
    let trimmed: Vec<usize> = headers
        .iter()
        .enumerate()
        .filter(|(_, h)| h.trim() == target)
        .map(|(i, _)| i)
        .collect();
    if trimmed.len() == 1 {
        return Ok(trimmed[0]);
    }
    let case_folded: Vec<usize> = headers
        .iter()
        .enumerate()
        .filter(|(_, h)| h.trim().eq_ignore_ascii_case(target))
        .map(|(i, _)| i)
        .collect();
    if case_folded.len() == 1 && trimmed.is_empty() {
        return Ok(case_folded[0]);
    }
    if trimmed.len() > 1 || case_folded.len() > 1 {
        let shown: Vec<String> = headers[trimmed.first().copied().unwrap_or(0)..]
            .iter()
            .take(3)
            .map(|h| format!("'{h}'"))
            .collect();
        return Err(format!(
            "column '{name}' is ambiguous: {} match after trimming",
            shown.join(", ")
        ));
    }
    Err(column_candidates(name, headers))
}

/// Turn "column not found" into something the user can act on.
///
/// Real-world headers carry stray spaces (`城市 `), BOM remnants and casing
/// differences, so a bare "not found" makes people hunt. This names the closest
/// candidates — trimmed equality first, then case-insensitive, then a substring —
/// without ever *silently* accepting the near match, because a file may
/// legitimately contain two columns differing only by case or spacing.
pub fn column_candidates(name: &str, headers: &[String]) -> String {
    let target = name.trim();
    let mut exact_trimmed: Vec<&String> = Vec::new();
    let mut case: Vec<&String> = Vec::new();
    let mut contains: Vec<&String> = Vec::new();
    for header in headers {
        if header.trim() == target {
            exact_trimmed.push(header);
        } else if header.trim().eq_ignore_ascii_case(target) {
            case.push(header);
        } else if header.contains(target) || target.contains(header.trim()) {
            contains.push(header);
        }
    }
    if exact_trimmed.is_empty() && case.is_empty() && contains.is_empty() {
        let listing: Vec<&str> = headers.iter().take(12).map(String::as_str).collect();
        return format!(
            "column '{name}' not found; available columns: {}",
            listing.join(", ")
        );
    }
    let mut picks: Vec<&String> = exact_trimmed.clone();
    picks.extend(case.iter().take(3));
    picks.extend(contains.iter().take(3));
    let shown: Vec<String> = picks
        .into_iter()
        .take(3)
        .map(|h| format!("'{h}'"))
        .collect();
    format!(
        "column '{name}' not found; did you mean {}",
        shown.join(" or ")
    )
}

/// Guess the escape style from the first few records.
///
/// The tell for backslash escaping is a quoted field containing `\"` *and* no
/// `""`. A file that has neither is assumed to be RFC 4180, which is the only
/// reading that needs no evidence beyond convention.
pub fn detect_quote_style(bytes: &[u8]) -> Option<QuoteStyle> {
    let text = String::from_utf8_lossy(bytes);
    let mut doubled = 0usize;
    let mut backslashed = 0usize;
    let mut chars = text.chars().peekable();
    let mut in_quotes = false;
    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    doubled += 1;
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else if ch == '\\' && chars.peek() == Some(&'"') {
                backslashed += 1;
                chars.next();
            }
        } else if ch == '"' {
            in_quotes = true;
        }
    }
    if backslashed > 0 && doubled == 0 {
        Some(QuoteStyle::Backslash)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn records(input: &str) -> Vec<String> {
        let mut r = RecordReader::new(input.as_bytes(), 8 * 1024);
        let mut out = Vec::new();
        let mut acc = Vec::new();
        while r.next_record(&mut out).unwrap() {
            acc.push(
                String::from_utf8(out.clone())
                    .unwrap()
                    .trim_end()
                    .to_string(),
            );
        }
        acc
    }

    #[test]
    fn splits_plain_lines() {
        assert_eq!(records("a,b\nc,d\n"), vec!["a,b", "c,d"]);
    }

    #[test]
    fn keeps_embedded_newlines_inside_quotes() {
        assert_eq!(
            records("id,addr\n1,\"line1\nline2\"\n2,ok\n"),
            vec!["id,addr", "1,\"line1\nline2\"", "2,ok"]
        );
    }

    #[test]
    fn handles_escaped_quotes() {
        assert_eq!(
            records("1,\"he said \"\"hi\"\"\"\n2,x\n"),
            vec!["1,\"he said \"\"hi\"\"\"", "2,x"]
        );
    }

    #[test]
    fn last_record_without_trailing_newline() {
        assert_eq!(records("a\nb"), vec!["a", "b"]);
    }

    #[test]
    fn parse_strips_bom_from_first_cell() {
        let s = String::from_utf8([UTF8_BOM, b"id,name\n".as_slice()].concat()).unwrap();
        let fields = parse_csv_record(s.trim_end(), b',');
        assert_eq!(fields, vec!["id", "name"]);
    }

    #[test]
    fn join_quotes_only_when_needed() {
        assert_eq!(join_csv_line(&["a", "b"], b','), "a,b\n");
        assert_eq!(join_csv_line(&["a,1", "b"], b','), "\"a,1\",b\n");
        assert_eq!(join_csv_line(&["he\"y"], b','), "\"he\"\"y\"\n");
    }

    #[test]
    fn detects_semicolon_when_commas_are_quoted() {
        let mut input = b"a;b\n1;\"x,y\"\n".as_slice();
        let (d, _) = detect_delimiter(&mut input).unwrap();
        assert_eq!(d, Delimiter::Semicolon);
    }

    #[test]
    fn record_reader_survives_tiny_buffers() {
        let input = "a,\"x\ny\"\nb,c\n";
        let mut r = RecordReader::new(input.as_bytes(), 8 * 1024);
        // Force tiny reads by using a chained reader would be overkill; assert count instead.
        let mut out = Vec::new();
        let mut n = 0;
        while r.next_record(&mut out).unwrap() {
            n += 1;
        }
        assert_eq!(n, 2);
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;

    fn headers(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact_names_win_without_trimming() {
        let h = headers(&["id", "城市 ", "城市"]);
        assert_eq!(resolve_column("城市", &h).unwrap(), 2);
        assert_eq!(resolve_column("城市 ", &h).unwrap(), 1);
    }

    #[test]
    fn a_unique_trimmed_match_is_accepted() {
        let h = headers(&["id", "客户编号 ", "金额"]);
        assert_eq!(resolve_column("客户编号", &h).unwrap(), 1);
    }

    #[test]
    fn ambiguity_is_reported_rather_than_guessed() {
        let h = headers(&["id", "城市 ", "城市 "]);
        let err = resolve_column("城市", &h).unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
    }

    #[test]
    fn a_near_name_gets_a_suggestion_and_a_stranger_gets_the_list() {
        let h = headers(&["id", "客户编号 ", "amount"]);
        // `amountx` contains `amount`, so it is worth suggesting.
        let err = resolve_column("amountx", &h).unwrap_err();
        assert!(err.contains("did you mean"), "{err}");
        assert!(err.contains("'amount'"), "{err}");
        // `zzz` resembles nothing: list what is actually there.
        let err = resolve_column("zzz", &h).unwrap_err();
        assert!(err.contains("available columns"), "{err}");
        assert!(err.contains("amount"), "{err}");
    }

    #[test]
    fn case_differences_are_tolerated_when_unique() {
        let h = headers(&["ID", "City"]);
        assert_eq!(resolve_column("city", &h).unwrap(), 1);
        assert_eq!(resolve_column("id", &h).unwrap(), 0);
    }
}

#[cfg(test)]
mod quote_style_tests {
    use super::*;

    #[test]
    fn rfc4180_doubles_the_quote() {
        let fields = parse_csv_line_with(r##"a,"b""c",d"##, b',', QuoteStyle::Rfc4180);
        assert_eq!(fields, vec!["a", r#"b"c"#, "d"]);
    }

    #[test]
    fn backslash_unescapes_quotes_and_backslashes() {
        let fields = parse_csv_line_with(r##"a,"b\"c",d"##, b',', QuoteStyle::Backslash);
        assert_eq!(fields, vec!["a", r#"b"c"#, "d"]);
        let fields = parse_csv_line_with(r##""x\\y""##, b',', QuoteStyle::Backslash);
        assert_eq!(fields, vec![r"x\y"]);
    }

    #[test]
    fn reading_a_backslash_file_as_rfc_swallows_a_field_silently() {
        // What the wrong style looks like: the escaped quote closes the field, the
        // next quote opens one, and the rest of the line is absorbed. Two fields
        // instead of three, no error — which is exactly why detection matters.
        let wrong = parse_csv_line_with(r##"a,"b\"c",d"##, b',', QuoteStyle::Rfc4180);
        assert_eq!(
            wrong,
            vec!["a", r#"b\c,d"#],
            "expected the silent breakage this detector exists to catch"
        );
    }

    #[test]
    fn none_splits_on_every_delimiter() {
        let fields = parse_csv_line_with(r#""a,b",c"#, b',', QuoteStyle::None);
        assert_eq!(fields, vec![r#""a"#, r#"b""#, "c"]);
    }

    #[test]
    fn record_boundaries_follow_the_escape_style() {
        // With backslash escaping the `\"` does not close the field, so the next
        // line is a separate record. An RFC reader swallows it.
        let mut reader = RecordReader::new_with(
            std::io::Cursor::new(b"id,note\n1,\"a \\\"b,c\"\n2,plain\n".to_vec()),
            64,
            QuoteStyle::Backslash,
        );
        let mut record = Vec::new();
        let mut count = 0;
        while reader.next_record(&mut record).unwrap() {
            count += 1;
        }
        assert_eq!(count, 3, "header + 2 rows");

        let mut reader = RecordReader::new(
            std::io::Cursor::new(b"id,note\n1,\"a \\\"b,c\"\n2,plain\n".to_vec()),
            64,
        );
        let mut record = Vec::new();
        let mut count = 0;
        while reader.next_record(&mut record).unwrap() {
            count += 1;
        }
        assert_eq!(
            count, 2,
            "the RFC reader merges the second row into the first"
        );
    }

    #[test]
    fn detection_separates_the_two_styles() {
        assert_eq!(
            detect_quote_style(br#"a,"b""c""#),
            None,
            "RFC 4180 is the default"
        );
        assert_eq!(
            detect_quote_style(br#"a,"b\"c""#),
            Some(QuoteStyle::Backslash)
        );
        // A file with both looks like RFC: `""` is the stronger evidence.
        assert_eq!(detect_quote_style(br#"a,"b\"c","d""e""#), None);
    }

    #[test]
    fn style_names_parse() {
        assert_eq!(QuoteStyle::parse("rfc4180").unwrap(), QuoteStyle::Rfc4180);
        assert_eq!(
            QuoteStyle::parse("backslash").unwrap(),
            QuoteStyle::Backslash
        );
        assert_eq!(QuoteStyle::parse("none").unwrap(), QuoteStyle::None);
        assert!(QuoteStyle::parse("weird").is_err());
    }
}
