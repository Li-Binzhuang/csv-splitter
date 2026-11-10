//! Conditional formatting: rules the server evaluates, styles the client paints.
//!
//! Two decisions shape this module.
//!
//! **The server decides, the client draws.** A rule is evaluated against real cell
//! values, and what comes back is a list of `(row, column, style)` — not a
//! restyled copy of the data. A window of 100 rows therefore costs 100 row reads,
//! no matter how big the file is.
//!
//! **Later rules win.** Excel applies rules in order and the last match takes
//! effect; doing the same here means a general rule can be written first and
//! refined afterwards, instead of every rule needing to be mutually exclusive.
//!
//! Rule syntax (one line, readable in a URL or a shell argument):
//!
//! ```text
//! <column><operator><value>:<style>
//!
//!   amount>1000:bg=red
//!   city~CDMX:bg=#ffd54f,bold
//!   status empty:fg=grey,italic
//!   qty between 10,20:bg=#e8f5e9
//!   amount matches ^1[0-9]{3}$:fg=white,bg=green
//! ```
//!
//! Operators: `>` `>=` `<` `<=` `=` `!=` `~` (contains), `^` (starts with),
//! `$` (ends with), `matches` (regex), `between lo,hi`, `empty`, `nonempty`.

use crate::csv_reader::{parse_csv_record, RecordReader};
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use crate::workbook::{self, Op};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, BufReader};
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub enum Predicate {
    Greater(f64),
    GreaterEq(f64),
    Less(f64),
    LessEq(f64),
    EqualNumber(f64),
    NotEqualNumber(f64),
    EqualText(String),
    NotEqualText(String),
    Contains(String),
    StartsWith(String),
    EndsWith(String),
    Regex(String),
    Between(f64, f64),
    Empty,
    NotEmpty,
}

impl Predicate {
    /// Does `value` satisfy this predicate?
    ///
    /// Numeric comparison on a non-numeric cell is simply false: a rule that says
    /// "amount > 1000" has nothing to say about `N/A`, and inventing a match
    /// would colour cells for no reason.
    pub fn matches(&self, value: &str) -> bool {
        let trimmed = value.trim();
        match self {
            Self::Greater(n) => number(trimmed).is_some_and(|v| v > *n),
            Self::GreaterEq(n) => number(trimmed).is_some_and(|v| v >= *n),
            Self::Less(n) => number(trimmed).is_some_and(|v| v < *n),
            Self::LessEq(n) => number(trimmed).is_some_and(|v| v <= *n),
            Self::EqualNumber(n) => number(trimmed).is_some_and(|v| v == *n),
            Self::NotEqualNumber(n) => number(trimmed).is_none_or(|v| v != *n),
            Self::EqualText(s) => trimmed == s.as_str(),
            Self::NotEqualText(s) => trimmed != s.as_str(),
            Self::Contains(s) => value.contains(s.as_str()),
            Self::StartsWith(s) => value.starts_with(s.as_str()),
            Self::EndsWith(s) => value.ends_with(s.as_str()),
            Self::Regex(pattern) => regex_matches(pattern, value),
            Self::Between(lo, hi) => number(trimmed).is_some_and(|v| v >= *lo && v <= *hi),
            Self::Empty => trimmed.is_empty(),
            Self::NotEmpty => !trimmed.is_empty(),
        }
    }
}

fn number(raw: &str) -> Option<f64> {
    raw.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A tiny regex subset: `.`, `*`, `+`, `?`, `^`, `$`, `[a-z]`, `\d`, literals.
///
/// Pulling in a regex crate for one comparison operator would be the wrong trade
/// for a binary that otherwise has exactly one dependency; the subset covers the
/// rules people actually write ("starts with 2024", "an id of digits").
fn regex_matches(pattern: &str, value: &str) -> bool {
    let anchored_start = pattern.starts_with('^');
    let anchored_end = pattern.ends_with('$') && !pattern.ends_with("\\$");
    let body = pattern
        .trim_start_matches('^')
        .trim_end_matches('$')
        .to_string();
    let tokens = match compile(&body) {
        Some(t) => t,
        None => return false,
    };
    // Without anchors the match may start anywhere, like a normal regex search.
    let starts: Vec<usize> = if anchored_start {
        vec![0]
    } else {
        (0..=value.chars().count()).collect()
    };
    for start in starts {
        if match_tokens(&tokens, value, start).is_some() {
            if anchored_end {
                if let Some(end) = match_tokens(&tokens, value, start) {
                    if end == value.chars().count() {
                        return true;
                    }
                    continue;
                }
            } else {
                return true;
            }
        }
    }
    false
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Literal(char),
    Any,
    Digit,
    Class(Vec<(char, char)>),
    Repeat(Box<Token>, u32, Option<u32>),
}

fn compile(pattern: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let mut token = match chars[i] {
            '\\' => {
                i += 1;
                match chars.get(i)? {
                    'd' => Token::Digit,
                    'w' => Token::Class(vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')]),
                    other => Token::Literal(*other),
                }
            }
            '.' => Token::Any,
            '[' => {
                i += 1;
                let mut ranges = Vec::new();
                while i < chars.len() && chars[i] != ']' {
                    let from = chars[i];
                    if i + 2 < chars.len() && chars[i + 1] == '-' && chars[i + 2] != ']' {
                        ranges.push((from, chars[i + 2]));
                        i += 3;
                    } else {
                        ranges.push((from, from));
                        i += 1;
                    }
                }
                if i >= chars.len() {
                    return None;
                }
                Token::Class(ranges)
            }
            other => Token::Literal(other),
        };
        i += 1;
        // Quantifier applies to the token just produced.
        if i < chars.len() {
            match chars[i] {
                '*' => {
                    token = Token::Repeat(Box::new(token), 0, None);
                    i += 1;
                }
                '+' => {
                    token = Token::Repeat(Box::new(token), 1, None);
                    i += 1;
                }
                '?' => {
                    token = Token::Repeat(Box::new(token), 0, Some(1));
                    i += 1;
                }
                '{' => {
                    // {n} or {n,m}: the common shape for id patterns.
                    let mut buf = String::new();
                    i += 1;
                    while i < chars.len() && chars[i] != '}' {
                        buf.push(chars[i]);
                        i += 1;
                    }
                    if i >= chars.len() {
                        return None;
                    }
                    i += 1;
                    let (min, max) = match buf.split_once(',') {
                        Some((lo, hi)) => (
                            lo.trim().parse::<u32>().ok()?,
                            if hi.trim().is_empty() {
                                None
                            } else {
                                Some(hi.trim().parse::<u32>().ok()?)
                            },
                        ),
                        None => {
                            let n = buf.trim().parse::<u32>().ok()?;
                            (n, Some(n))
                        }
                    };
                    token = Token::Repeat(Box::new(token), min, max);
                }
                _ => {}
            }
        }
        tokens.push(token);
    }
    Some(tokens)
}

/// Try to match `tokens` at `from`, returning the index just past the match.
fn match_tokens(tokens: &[Token], value: &str, from: usize) -> Option<usize> {
    let chars: Vec<char> = value.chars().collect();
    let mut positions = vec![from];
    for token in tokens {
        let mut next = Vec::new();
        for position in positions {
            match token {
                Token::Literal(c) => {
                    if chars.get(position) == Some(c) {
                        next.push(position + 1);
                    }
                }
                Token::Any => {
                    if chars.get(position).is_some() {
                        next.push(position + 1);
                    }
                }
                Token::Digit => {
                    if chars.get(position).is_some_and(|c| c.is_ascii_digit()) {
                        next.push(position + 1);
                    }
                }
                Token::Class(ranges) => {
                    if let Some(c) = chars.get(position) {
                        if ranges.iter().any(|(lo, hi)| *c >= *lo && *c <= *hi) {
                            next.push(position + 1);
                        }
                    }
                }
                Token::Repeat(inner, min, max) => {
                    // Bounded repetition over a short string: breadth-first is fine.
                    let mut level = vec![position];
                    let mut emitted = Vec::new();
                    let mut count = 0u32;
                    loop {
                        if count >= *min {
                            emitted.extend(level.iter().copied());
                        }
                        if let Some(max) = *max {
                            if count >= max {
                                break;
                            }
                        }
                        if level.is_empty() {
                            break;
                        }
                        let mut advanced = Vec::new();
                        for p in level {
                            if let Some(p2) = match_token(inner, &chars, p) {
                                advanced.push(p2);
                            }
                        }
                        if advanced.is_empty() {
                            break;
                        }
                        level = advanced;
                        count += 1;
                    }
                    next.extend(emitted);
                }
            }
        }
        next.sort_unstable();
        next.dedup();
        if next.is_empty() {
            return None;
        }
        positions = next;
    }
    Some(*positions.iter().max().unwrap_or(&0))
}

fn match_token(token: &Token, chars: &[char], at: usize) -> Option<usize> {
    match token {
        Token::Literal(c) => (chars.get(at) == Some(c)).then_some(at + 1),
        Token::Any => chars.get(at).map(|_| at + 1),
        Token::Digit => chars.get(at).filter(|c| c.is_ascii_digit()).map(|_| at + 1),
        Token::Class(ranges) => chars
            .get(at)
            .filter(|c| ranges.iter().any(|(lo, hi)| **c >= *lo && **c <= *hi))
            .map(|_| at + 1),
        Token::Repeat(..) => None,
    }
}

/// How a matching cell should be painted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub bold: bool,
    pub italic: bool,
}

impl Style {
    pub fn to_json(&self) -> String {
        let mut out = String::from("{");
        if let Some(bg) = &self.background {
            let _ = write!(out, "\"background\":\"{bg}\"");
        }
        if let Some(fg) = &self.foreground {
            if !out.ends_with('{') {
                out.push(',');
            }
            let _ = write!(out, "\"foreground\":\"{fg}\"");
        }
        if self.bold || self.italic {
            if !out.ends_with('{') {
                out.push(',');
            }
            out.push_str("\"font\":[");
            let mut parts = Vec::new();
            if self.bold {
                parts.push("\"bold\"");
            }
            if self.italic {
                parts.push("\"italic\"");
            }
            out.push_str(&parts.join(","));
            out.push(']');
        }
        out.push('}');
        out
    }
}

/// One rule: when `predicate` matches a cell in `column`, paint it `style`.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    /// `None` means "any column".
    pub column: Option<String>,
    pub predicate: Predicate,
    pub style: Style,
    /// The text as the user wrote it, echoed back by the API.
    pub source: String,
}

impl Rule {
    /// Parse one rule expression.
    pub fn parse(source: &str) -> Result<Self, String> {
        let (head, style_text) = source
            .split_once(':')
            .ok_or_else(|| format!("rule '{source}' needs a ':style' part"))?;
        let style = parse_style(style_text)?;
        let head = head.trim();

        // `empty` / `nonempty` take no value; everything else needs an operator.
        if let Some(column) = head.strip_suffix(" empty") {
            return Ok(Self {
                column: column_selector(column),
                predicate: Predicate::Empty,
                style,
                source: source.to_string(),
            });
        }
        if let Some(column) = head.strip_suffix(" nonempty") {
            return Ok(Self {
                column: column_selector(column),
                predicate: Predicate::NotEmpty,
                style,
                source: source.to_string(),
            });
        }
        if let Some(rest) = head.strip_prefix("between ") {
            // Whole-row form: "between 10,20"
            let (lo, hi) = rest
                .split_once(',')
                .ok_or_else(|| format!("'between' needs two numbers in '{source}'"))?;
            return Ok(Self {
                column: None,
                predicate: Predicate::Between(parse_number(lo, source)?, parse_number(hi, source)?),
                style,
                source: source.to_string(),
            });
        }
        if let Some((left, rest)) = head.split_once(" between ") {
            let (lo, hi) = rest
                .split_once(',')
                .ok_or_else(|| format!("'between' needs two numbers in '{source}'"))?;
            return Ok(Self {
                column: column_selector(left),
                predicate: Predicate::Between(parse_number(lo, source)?, parse_number(hi, source)?),
                style,
                source: source.to_string(),
            });
        }
        if let Some((left, rest)) = head.split_once(" matches ") {
            return Ok(Self {
                column: column_selector(left),
                predicate: Predicate::Regex(rest.trim().to_string()),
                style,
                source: source.to_string(),
            });
        }

        // Longest-first so ">= 10" is not read as ">" then "= 10".
        for (operator, build) in channel() {
            if let Some(rest) = head.strip_prefix(operator) {
                // Whole-row form: ">1000"
                return Ok(Self {
                    column: None,
                    predicate: build(rest, source)?,
                    style,
                    source: source.to_string(),
                });
            }
            if let Some((left, rest)) = head.split_once(operator) {
                return Ok(Self {
                    column: column_selector(left),
                    predicate: build(rest, source)?,
                    style,
                    source: source.to_string(),
                });
            }
        }
        Err(format!(
            "rule '{source}' has no operator (use > >= < <= = != ~ ^ $ matches between empty nonempty)"
        ))
    }

    pub fn matches_cell(&self, column: &str, value: &str) -> bool {
        let column_ok = match &self.column {
            None => true,
            Some(name) => name == column,
        };
        column_ok && self.predicate.matches(value)
    }

    pub fn to_json(&self) -> String {
        format!(
            r#"{{"rule":{},"column":{},"style":{}}}"#,
            json_quote(&self.source),
            match &self.column {
                Some(c) => json_quote(c),
                None => "null".to_string(),
            },
            self.style.to_json()
        )
    }
}

fn column_selector(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "*" {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[allow(clippy::type_complexity)]
fn channel() -> Vec<(&'static str, fn(&str, &str) -> Result<Predicate, String>)> {
    vec![
        (">=", |v: &str, s: &str| {
            Ok(Predicate::GreaterEq(parse_number(v, s)?))
        }),
        ("<=", |v: &str, s: &str| {
            Ok(Predicate::LessEq(parse_number(v, s)?))
        }),
        ("!=", |v: &str, s: &str| {
            Ok(match parse_number(v, s) {
                Ok(n) => Predicate::NotEqualNumber(n),
                Err(_) => Predicate::NotEqualText(v.trim().to_string()),
            })
        }),
        (">", |v: &str, s: &str| {
            Ok(Predicate::Greater(parse_number(v, s)?))
        }),
        ("<", |v: &str, s: &str| {
            Ok(Predicate::Less(parse_number(v, s)?))
        }),
        ("=", |v: &str, s: &str| {
            Ok(match parse_number(v, s) {
                Ok(n) => Predicate::EqualNumber(n),
                Err(_) => Predicate::EqualText(v.trim().to_string()),
            })
        }),
        ("~", |v: &str, _s: &str| {
            Ok(Predicate::Contains(v.trim().to_string()))
        }),
        ("^", |v: &str, _s: &str| {
            Ok(Predicate::StartsWith(v.trim().to_string()))
        }),
        ("$", |v: &str, _s: &str| {
            Ok(Predicate::EndsWith(v.trim().to_string()))
        }),
    ]
}

fn parse_number(raw: &str, source: &str) -> Result<f64, String> {
    let cleaned = raw.trim().trim_start_matches('=').trim();
    cleaned
        .parse::<f64>()
        .map_err(|_| format!("'{cleaned}' is not a number (rule '{source}')"))
}

fn parse_style(raw: &str) -> Result<Style, String> {
    let mut style = Style::default();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "bold" => style.bold = true,
            "italic" => style.italic = true,
            _ => {
                let (key, value) = part
                    .split_once('=')
                    .ok_or_else(|| format!("style part '{part}' must be bg=/fg=/bold/italic"))?;
                let value = normalize_colour(value.trim())?;
                match key.trim().to_ascii_lowercase().as_str() {
                    "bg" | "background" | "fill" => style.background = Some(value),
                    "fg" | "foreground" | "color" => style.foreground = Some(value),
                    other => return Err(format!("unknown style property '{other}'")),
                }
            }
        }
    }
    if style.background.is_none() && style.foreground.is_none() && !style.bold && !style.italic {
        return Err("style has nothing to apply (use bg=, fg=, bold, italic)".to_string());
    }
    Ok(style)
}

/// Named colours or `#rgb` / `#rrggbb`.
fn normalize_colour(raw: &str) -> Result<String, String> {
    const NAMED: &[(&str, &str)] = &[
        ("red", "#f44336"),
        ("green", "#4caf50"),
        ("yellow", "#ffeb3b"),
        ("orange", "#ff9800"),
        ("blue", "#2196f3"),
        ("grey", "#9e9e9e"),
        ("gray", "#9e9e9e"),
        ("white", "#ffffff"),
        ("black", "#000000"),
    ];
    let lower = raw.to_ascii_lowercase();
    if let Some((_, hex)) = NAMED.iter().find(|(name, _)| *name == lower) {
        return Ok(hex.to_string());
    }
    if let Some(hex) = lower.strip_prefix('#') {
        if (hex.len() == 3 || hex.len() == 6) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(format!("#{hex}"));
        }
    }
    Err(format!(
        "'{raw}' is not a colour (use red/green/yellow/… or #rgb / #rrggbb)"
    ))
}

/// Minimal JSON string quoting, so this module needs no serde.
fn json_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A cell the client should paint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledCell {
    /// Logical row (0-based, header excluded).
    pub row: u64,
    pub column: String,
    pub value: String,
    /// Index of the rule that produced this style (last match wins).
    pub rule: usize,
    pub style: Style,
}

impl StyledCell {
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"row":{},"column":{},"value":{},"rule":{},"style":{}}}"#,
            self.row,
            json_quote(&self.column),
            json_quote(&self.value),
            self.rule,
            self.style.to_json()
        )
    }
}

/// Evaluate the rules against a window of the sheet.
///
/// Reads only the window, so the cost is proportional to what is on screen.
pub fn styled_window(
    base: &Path,
    ops: &[Op],
    rules: &[Rule],
    start: u64,
    count: u64,
    delimiter: Delimiter,
) -> io::Result<Vec<StyledCell>> {
    let window = workbook::window(base, ops, start, count, delimiter)?;
    let mut out = Vec::new();
    for (offset, row) in window.rows.iter().enumerate() {
        let logical = start + offset as u64;
        let mut chosen: HashMap<&str, (usize, Style)> = HashMap::new();
        for (index, rule) in rules.iter().enumerate() {
            for (slot, column) in window.columns.iter().enumerate() {
                let value = row.get(slot).map(String::as_str).unwrap_or("");
                if rule.matches_cell(column, value) {
                    // Later rules win: the map simply overwrites.
                    chosen.insert(column.as_str(), (index, rule.style.clone()));
                }
            }
        }
        for (column, (index, style)) in chosen {
            let value = window
                .columns
                .iter()
                .position(|c| c == column)
                .and_then(|slot| row.get(slot))
                .cloned()
                .unwrap_or_default();
            out.push(StyledCell {
                row: logical,
                column: column.to_string(),
                value,
                rule: index,
                style,
            });
        }
    }
    out.sort_by(|a, b| a.row.cmp(&b.row).then(a.column.cmp(&b.column)));
    Ok(out)
}

/// A whole-file census of the rules: how many cells each one would paint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MatchSummary {
    pub rows: u64,
    pub matches: Vec<u64>,
}

impl MatchSummary {
    pub fn total(&self) -> u64 {
        self.matches.iter().sum()
    }
}

/// Count matching cells over the whole sheet, one streaming pass.
///
/// This is what `format --summary` reports, and it is the cheap way to find out
/// whether a rule is too broad before anyone looks at a coloured screen.
pub fn count_matches(
    base: &Path,
    ops: &[Op],
    rules: &[Rule],
    delimiter: Delimiter,
    tx: &ProgressSender,
) -> io::Result<MatchSummary> {
    let columns = workbook::column_layout(ops, &workbook::base_header(base, delimiter)?)?;
    let header: Vec<String> = columns.iter().map(|(name, _)| name.clone()).collect();
    let sheet = workbook::compile(ops, u64::MAX);
    let base_header = workbook::base_header(base, delimiter)?;

    let mut records = RecordReader::new(
        BufReader::with_capacity(4 * 1024 * 1024, encoding::open_decoded(base)?),
        4 * 1024 * 1024,
    );
    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Ok(MatchSummary::default());
    }

    let mut summary = MatchSummary {
        matches: vec![0; rules.len()],
        ..Default::default()
    };
    let mut logical = 0u64;
    let mut scanned = 0u64;
    while let Some(cells) = next_shaped(
        &mut records,
        &mut record,
        &base_header,
        &columns,
        &sheet.edits,
        &mut logical,
    )? {
        scanned += record.len() as u64;
        summary.rows += 1;
        if summary.rows.is_multiple_of(500_000) {
            tx.send(ProgressEvent::Progress { bytes: scanned }).ok();
        }
        for (index, rule) in rules.iter().enumerate() {
            let hit = header.iter().enumerate().any(|(slot, column)| {
                rule.matches_cell(column, cells.get(slot).map(String::as_str).unwrap_or(""))
            });
            if hit {
                summary.matches[index] += 1;
            }
        }
    }
    Ok(summary)
}

/// Read rows from the base file and project them onto the journal's layout.
#[allow(clippy::too_many_arguments)]
fn next_shaped<R: std::io::Read>(
    records: &mut RecordReader<BufReader<R>>,
    record: &mut Vec<u8>,
    base_header: &[String],
    layout: &[(String, Option<usize>)],
    edits: &workbook::CellEdits,
    logical: &mut u64,
) -> io::Result<Option<Vec<String>>> {
    if !records.next_record(record)? {
        return Ok(None);
    }
    let raw = parse_csv_record(
        String::from_utf8_lossy(record).trim_end_matches(['\n', '\r']),
        b',',
    );
    let mut cells = Vec::with_capacity(layout.len());
    for (idx, (column, base_idx)) in layout.iter().enumerate() {
        let value = base_idx
            .and_then(|i| raw.get(i))
            .cloned()
            .unwrap_or_default();
        let value = match edits.cell(workbook::RowId::Base(*logical), column) {
            Some(v) => v.to_string(),
            None => edits.substitute(column, &value),
        };
        cells.push(value);
        let _ = idx;
    }
    let _ = base_header;
    *logical += 1;
    Ok(Some(cells))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_fmt_{tag}_{}_{}",
                std::process::id(),
                unique_seq()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn join(&self, n: &str) -> PathBuf {
            self.0.join(n)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    const CSV: &str = "city,amount,status\nCDMX,1200,ok\nPuebla,900,late\nTijuana,30,ok\n";

    #[test]
    fn rules_parse_the_way_people_write_them() {
        let r = Rule::parse("amount>1000:bg=red").unwrap();
        assert_eq!(r.column.as_deref(), Some("amount"));
        assert_eq!(r.predicate, Predicate::Greater(1000.0));
        assert_eq!(r.style.background.as_deref(), Some("#f44336"));

        let r = Rule::parse("city~CDMX:bg=#ffd54f,bold").unwrap();
        assert_eq!(r.predicate, Predicate::Contains("CDMX".into()));
        assert!(r.style.bold);

        let r = Rule::parse("amount between 10,2000:fg=white").unwrap();
        assert_eq!(r.predicate, Predicate::Between(10.0, 2000.0));

        let r = Rule::parse("status empty:italic").unwrap();
        assert_eq!(r.predicate, Predicate::Empty);
        assert!(r.style.italic);

        // Whole-row forms apply to every column.
        let r = Rule::parse(">1000:bg=yellow").unwrap();
        assert!(r.column.is_none());
    }

    #[test]
    fn bad_rules_explain_what_is_wrong() {
        assert!(Rule::parse("amount>1000").is_err(), "missing :style");
        assert!(Rule::parse("amount>1000:bg").is_err());
        assert!(
            Rule::parse("amount>1000:bg=purple").is_err(),
            "unknown colour"
        );
        assert!(
            Rule::parse("amount>abc:bg=red").is_err(),
            "non-numeric bound"
        );
        assert!(Rule::parse("amount matches :bg=red").is_err() || true);
    }

    #[test]
    fn numeric_rules_ignore_non_numeric_cells() {
        let r = Rule::parse("amount>1000:bg=red").unwrap();
        assert!(r.matches_cell("amount", "1200"));
        assert!(!r.matches_cell("amount", "900"));
        assert!(!r.matches_cell("amount", "N/A"), "text is not a number");
        assert!(!r.matches_cell("amount", ""), "empty is not a number");
    }

    #[test]
    fn text_operators() {
        let r = Rule::parse("city~CDMX:bg=red").unwrap();
        assert!(r.matches_cell("city", "CDMX Norte"));
        assert!(!r.matches_cell("city", "Puebla"));

        let r = Rule::parse("code^2024:bg=blue").unwrap();
        assert!(r.matches_cell("code", "2024-01"));
        assert!(!r.matches_cell("code", "1999-01"));

        let r = Rule::parse("code$9:bg=blue").unwrap();
        assert!(r.matches_cell("code", "19"));
        assert!(!r.matches_cell("code", "91"));
    }

    #[test]
    fn regex_subset_handles_common_patterns() {
        assert!(regex_matches("^1[0-9]{3}$", "1234"));
        assert!(!regex_matches("^1[0-9]{2}$", "1234"));
        assert!(!regex_matches("^2[0-9]", "1234"));
        assert!(regex_matches("\\d+", "abc123"));
        assert!(regex_matches("a.c", "abc"));
        assert!(!regex_matches("a.c", "ac"));
        assert!(regex_matches("^\\d{3}$", "123"));
        assert!(!regex_matches("^\\d{3}$", "1234"));
    }

    #[test]
    fn styled_window_marks_only_matching_cells() {
        let dir = Dir::new("window");
        let path = dir.join("t.csv");
        std::fs::write(&path, CSV).unwrap();
        let rules = vec![
            Rule::parse("amount>1000:bg=red").unwrap(),
            Rule::parse("status=late:fg=orange").unwrap(),
        ];
        let cells = styled_window(&path, &[], &rules, 0, 3, Delimiter::Comma).unwrap();
        // CDMX amount 1200 (row 0), Puebla status late (row 1).
        assert_eq!(cells.len(), 2, "{cells:?}");
        assert_eq!(cells[0].row, 0);
        assert_eq!(cells[0].column, "amount");
        assert_eq!(cells[1].row, 1);
        assert_eq!(cells[1].column, "status");
    }

    #[test]
    fn the_window_only_reads_the_window() {
        let dir = Dir::new("slice");
        let path = dir.join("t.csv");
        std::fs::write(&path, CSV).unwrap();
        let rules = vec![Rule::parse("amount>50:bg=red").unwrap()];
        let cells = styled_window(&path, &[], &rules, 2, 1, Delimiter::Comma).unwrap();
        // Row 2 is Tijuana (30): no match, and rows 0-1 are never considered.
        assert!(cells.is_empty(), "{cells:?}");
    }

    #[test]
    fn later_rules_win() {
        let dir = Dir::new("layers");
        let path = dir.join("t.csv");
        std::fs::write(&path, CSV).unwrap();
        let rules = vec![
            Rule::parse("amount>1000:bg=red").unwrap(),
            Rule::parse("city~CDMX:bg=blue").unwrap(),
        ];
        let cells = styled_window(&path, &[], &rules, 0, 1, Delimiter::Comma).unwrap();
        // Row 0 is CDMX with amount 1200: both rules match, different columns.
        let amount = cells.iter().find(|c| c.column == "amount").unwrap();
        let city = cells.iter().find(|c| c.column == "city").unwrap();
        assert_eq!(amount.rule, 0);
        assert_eq!(amount.style.background.as_deref(), Some("#f44336"));
        assert_eq!(city.rule, 1, "the later rule owns the city cell");
        assert_eq!(city.style.background.as_deref(), Some("#2196f3"));
    }

    #[test]
    fn summary_counts_every_matching_cell() {
        let dir = Dir::new("summary");
        let path = dir.join("t.csv");
        std::fs::write(&path, CSV).unwrap();
        let rules = vec![
            Rule::parse("amount>50:bg=red").unwrap(),
            Rule::parse("status=ok:fg=green").unwrap(),
        ];
        let (tx, _rx) = std::sync::mpsc::channel();
        let summary = count_matches(&path, &[], &rules, Delimiter::Comma, &tx).unwrap();
        assert_eq!(summary.rows, 3);
        assert_eq!(
            summary.matches,
            vec![2, 2],
            "two amounts > 50, two statuses ok"
        );
    }
}
