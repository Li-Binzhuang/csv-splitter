//! Display metadata: what a column is called, how wide it is, how numbers print.
//!
//! None of this changes a cell's value, so it lives in its own sidecar
//! (`<file>.view`) rather than in the journal. That separation matters:
//!
//! * an edit changes what a cell *is*, and belongs in the audit trail;
//! * a width or a decimal count changes only how it *looks*, and should be cheap to
//!   discard.
//!
//! Merging them would make "reset column widths" an edit operation with an undo
//! step, which is absurd.
//!
//! The file is CSV (one column per line) for the same reason the journal is: it can
//! be read, diffed and hand-edited.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// How a number is printed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NumberFormat {
    /// Whatever the value already says.
    #[default]
    Plain,
    /// `1,234.56`: fixed decimals with thousands separators.
    Decimal { places: usize, grouping: bool },
    /// `12.3%`: the value is a fraction (0.123 → 12.3%).
    Percent { places: usize },
    /// Money: `$ 1,234.50`.
    Currency { symbol: String, places: usize },
    /// Date/time display, e.g. `yyyy-mm-dd` or `yyyy-mm-dd hh:mm`.
    DateTime { pattern: String },
    /// Scientific notation, e.g. `0.00E+00`.
    Scientific { places: usize, upper: bool },
    /// Negative values in parentheses, e.g. `#,##0.00;(#,##0.00)`.
    Accounting {
        symbol: Option<String>,
        places: usize,
    },
    /// `@`: text is shown as entered.
    Text,
}

impl NumberFormat {
    /// Parse `#,##0.00`, `#,##0`, `0.00`, `0`, `0%`, `0.0%`, `$#,##0.00`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        if spec.is_empty() || spec.eq_ignore_ascii_case("plain") {
            return Ok(Self::Plain);
        }
        if let Some(percent) = spec.strip_suffix('%') {
            return Ok(Self::Percent {
                places: decimals(percent),
            });
        }
        if spec == "@" {
            return Ok(Self::Text);
        }
        if let Some((positive, negative)) = spec.split_once(';') {
            let positive = positive.trim();
            let negative = negative.trim();
            if negative.starts_with('(') && negative.ends_with(')') {
                let symbol = positive
                    .chars()
                    .next()
                    .filter(|character| matches!(character, '$' | '¥' | '€'))
                    .map(|character| character.to_string());
                let rest = symbol.as_ref().map(|_| &positive[1..]).unwrap_or(positive);
                if rest
                    .chars()
                    .all(|character| matches!(character, '#' | '0' | ',' | '.'))
                {
                    return Ok(Self::Accounting {
                        symbol,
                        places: decimals(rest),
                    });
                }
            }
        }
        if let Some((mantissa, exponent)) = spec.split_once(['E', 'e']) {
            let upper = spec.contains('E');
            let mantissa_ok = mantissa
                .chars()
                .all(|character| matches!(character, '0' | '.'))
                && mantissa.contains('.');
            let exponent_ok = exponent
                .strip_prefix(['+', '-'])
                .unwrap_or(exponent)
                .chars()
                .all(|character| character == '0');
            if mantissa_ok && exponent_ok && !exponent.is_empty() {
                return Ok(Self::Scientific {
                    places: decimals(mantissa),
                    upper,
                });
            }
        }
        if is_date_time_pattern(spec) {
            return Ok(Self::DateTime {
                pattern: spec.to_string(),
            });
        }
        if spec.starts_with('$') || spec.starts_with('¥') || spec.starts_with('€') {
            let symbol = spec.chars().next().unwrap().to_string();
            let rest = &spec[1..];
            return Ok(Self::Currency {
                symbol,
                places: decimals(rest),
            });
        }
        // Must look like a number pattern, not a random word.
        if !spec.chars().all(|c| matches!(c, '#' | '0' | ',' | '.')) {
            return Err(format!(
                "'{spec}' is not a number format (use #,##0.00, 0.00, 0%, $#,##0.00)"
            ));
        }
        Ok(Self::Decimal {
            places: decimals(spec),
            grouping: spec.contains(','),
        })
    }

    pub fn name(&self) -> String {
        match self {
            Self::Plain => "plain".to_string(),
            Self::Decimal { places, grouping } => {
                let base = if *places == 0 {
                    "0".to_string()
                } else {
                    format!("0.{}", "0".repeat(*places))
                };
                if *grouping {
                    format!("#,##{base}")
                } else {
                    base
                }
            }
            Self::Percent { places } => {
                if *places == 0 {
                    "0%".to_string()
                } else {
                    format!("0.{}%", "0".repeat(*places))
                }
            }
            Self::Currency { symbol, places } => {
                if *places == 0 {
                    format!("{symbol}#,##0")
                } else {
                    format!("{symbol}#,##0.{}", "0".repeat(*places))
                }
            }
            Self::DateTime { pattern } => pattern.clone(),
            Self::Scientific { places, upper } => {
                let exponent = if *upper { "E+00" } else { "e+00" };
                if *places == 0 {
                    format!("0{exponent}")
                } else {
                    format!("0.{} {exponent}", "0".repeat(*places)).replace(' ', "")
                }
            }
            Self::Accounting { symbol, places } => {
                let symbol = symbol.as_deref().unwrap_or("");
                let body = if *places == 0 {
                    "#,##0".to_string()
                } else {
                    format!("#,##0.{}", "0".repeat(*places))
                };
                format!("{symbol}{body};({symbol}{body})")
            }
            Self::Text => "@".to_string(),
        }
    }

    /// Render a cell value.
    ///
    /// A cell that is not a number is returned unchanged: a format is a promise
    /// about numbers, and inventing something for `N/A` would be worse than
    /// showing what is actually there.
    pub fn render(&self, value: &str) -> String {
        if matches!(self, Self::Plain) {
            return value.to_string();
        }
        let Ok(number) = value.trim().parse::<f64>() else {
            if let Self::DateTime { pattern } = self {
                return render_date_time(pattern, value).unwrap_or_else(|| value.to_string());
            }
            return value.to_string();
        };
        match self {
            Self::Plain => value.to_string(),
            Self::Decimal { places, grouping } => {
                let rendered = format!("{number:.places$}");
                if *grouping {
                    group(&rendered)
                } else {
                    rendered
                }
            }
            Self::Percent { places } => {
                let rendered = format!("{:.places$}", number * 100.0);
                format!("{rendered}%")
            }
            Self::Currency { symbol, places } => {
                format!("{symbol}{}", group(&format!("{number:.places$}")))
            }
            Self::DateTime { pattern } => {
                render_date_time(pattern, value).unwrap_or_else(|| value.to_string())
            }
            Self::Scientific { places, upper } => render_scientific(number, *places, *upper),
            Self::Accounting { symbol, places } => {
                let (negative, rendered) = if number < 0.0 {
                    (true, format!("{:.places$}", number.abs()))
                } else {
                    (false, format!("{number:.places$}"))
                };
                let rendered = group(&rendered);
                let symbol = symbol.as_deref().unwrap_or("");
                if negative {
                    format!("({symbol}{rendered})")
                } else {
                    format!("{symbol}{rendered}")
                }
            }
            Self::Text => value.to_string(),
        }
    }
}

fn render_scientific(number: f64, places: usize, upper: bool) -> String {
    let rendered = format!("{number:.places$E}");
    let (mantissa, exponent) = rendered.split_once('E').unwrap_or((&rendered, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let marker = if upper { 'E' } else { 'e' };
    let sign = if exponent < 0 { '-' } else { '+' };
    format!("{mantissa}{marker}{sign}{:02}", exponent.abs())
}

fn is_date_time_pattern(spec: &str) -> bool {
    let lower = spec.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "yyyy-mm-dd"
            | "yyyy/mm/dd"
            | "dd/mm/yyyy"
            | "mm/dd/yyyy"
            | "m/d/yyyy"
            | "yyyy/m/d"
            | "yyyy-mm-dd hh:mm"
            | "yyyy-mm-dd hh:mm:ss"
            | "hh:mm"
            | "hh:mm:ss"
    )
}

fn render_date_time(pattern: &str, value: &str) -> Option<String> {
    let lower = pattern.to_ascii_lowercase();
    let text = value.trim();
    let normalized = text.replace('T', " ");
    let (date, time) = match normalized.split_once(' ') {
        Some((date, time)) => (Some(date), Some(time)),
        None if normalized.contains(':') => (None, Some(normalized.as_str())),
        None => (Some(normalized.as_str()), None),
    };
    let date_parts = match date {
        Some(value) => Some(parse_date_parts(value)?),
        None => None,
    };
    let time_parts = match time {
        Some(value) => Some(parse_time_parts(value)?),
        None => None,
    };
    match lower.as_str() {
        "yyyy-mm-dd" => {
            let (year, month, day) = date_parts?;
            Some(format!("{year:04}-{month:02}-{day:02}"))
        }
        "yyyy/mm/dd" => {
            let (year, month, day) = date_parts?;
            Some(format!("{year:04}/{month:02}/{day:02}"))
        }
        "dd/mm/yyyy" => {
            let (year, month, day) = date_parts?;
            Some(format!("{day:02}/{month:02}/{year:04}"))
        }
        "mm/dd/yyyy" => {
            let (year, month, day) = date_parts?;
            Some(format!("{month:02}/{day:02}/{year:04}"))
        }
        "m/d/yyyy" => {
            let (year, month, day) = date_parts?;
            Some(format!("{month}/{day}/{year:04}"))
        }
        "yyyy/m/d" => {
            let (year, month, day) = date_parts?;
            Some(format!("{year:04}/{month}/{day}"))
        }
        "yyyy-mm-dd hh:mm" => {
            let (year, month, day) = date_parts?;
            let (hour, minute, _) = time_parts?;
            Some(format!(
                "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}"
            ))
        }
        "yyyy-mm-dd hh:mm:ss" => {
            let (year, month, day) = date_parts?;
            let (hour, minute, second) = time_parts?;
            Some(format!(
                "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
            ))
        }
        "hh:mm" => {
            let (hour, minute, _) = time_parts?;
            Some(format!("{hour:02}:{minute:02}"))
        }
        "hh:mm:ss" => {
            let (hour, minute, second) = time_parts?;
            Some(format!("{hour:02}:{minute:02}:{second:02}"))
        }
        _ => None,
    }
}

fn parse_date_parts(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split(['-', '/']);
    let first = parts.next()?;
    let second = parts.next()?;
    let third = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let first: u32 = first.parse().ok()?;
    let second: u32 = second.parse().ok()?;
    let third: u32 = third.parse().ok()?;
    let (year, month, day) = if first > 31 {
        (first, second, third)
    } else {
        (third, second, first)
    };
    ((1..=12).contains(&month) && (1..=31).contains(&day)).then_some((year, month, day))
}

fn parse_time_parts(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second))
}

fn decimals(pattern: &str) -> usize {
    match pattern.split_once('.') {
        Some((_, tail)) => tail.chars().filter(|c| *c == '0' || *c == '#').count(),
        None => 0,
    }
}

/// Insert thousands separators into a plain decimal string.
fn group(text: &str) -> String {
    let negative = text.starts_with('-');
    let body = text.trim_start_matches('-');
    let (integer, fraction) = match body.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (body, None),
    };
    let mut grouped = String::with_capacity(integer.len() + integer.len() / 3);
    for (index, ch) in integer.chars().enumerate() {
        if index > 0 && (integer.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(&grouped);
    if let Some(fraction) = fraction {
        out.push('.');
        out.push_str(fraction);
    }
    out
}

/// Everything the client needs to lay a column out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnMeta {
    pub column: String,
    /// Header text shown to the user; defaults to the column name.
    pub display_name: Option<String>,
    pub width: Option<u32>,
    pub frozen: bool,
    pub format: NumberFormat,
}

impl ColumnMeta {
    pub fn to_json(&self, original: &str) -> String {
        format!(
            r#"{{"column":{},"displayName":{},"width":{},"frozen":{},"format":{}}}"#,
            crate::format_json(original),
            match &self.display_name {
                Some(name) => crate::format_json(name),
                None => "null".to_string(),
            },
            match self.width {
                Some(w) => w.to_string(),
                None => "null".to_string(),
            },
            self.frozen,
            crate::format_json(&self.format.name())
        )
    }
}

/// Column metadata for one file, ordered as the sheet presents its columns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    entries: HashMap<String, ColumnMeta>,
}

impl Layout {
    /// Sidecar path: `orders.csv` → `orders.csv.view`.
    pub fn sidecar(base: &Path) -> PathBuf {
        let mut name = base.as_os_str().to_string_lossy().to_string();
        name.push_str(".view");
        PathBuf::from(name)
    }

    pub fn load(base: &Path) -> io::Result<Self> {
        let path = Self::sidecar(base);
        if !path.exists() {
            return Ok(Self::default());
        }
        let reader =
            std::io::BufRead::lines(BufReader::with_capacity(64 * 1024, File::open(&path)?));
        let mut entries = HashMap::new();
        for line in reader {
            let line = line?;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split('\u{1f}').collect();
            if fields.len() < 5 {
                continue;
            }
            let format = NumberFormat::parse(fields[4]).unwrap_or_default();
            entries.insert(
                fields[0].to_string(),
                ColumnMeta {
                    column: fields[0].to_string(),
                    display_name: (!fields[1].is_empty()).then(|| fields[1].to_string()),
                    width: fields[2].parse().ok(),
                    frozen: fields[3] == "1",
                    format,
                },
            );
        }
        Ok(Self { entries })
    }

    pub fn save(&self, base: &Path) -> io::Result<()> {
        let path = Self::sidecar(base);
        if self.entries.is_empty() {
            // Nothing to remember: leave no file behind rather than an empty one.
            std::fs::remove_file(&path).ok();
            return Ok(());
        }
        let tmp = path.with_extension("view.tmp");
        {
            let mut out = BufWriter::with_capacity(64 * 1024, File::create(&tmp)?);
            for meta in self.entries.values() {
                writeln!(
                    out,
                    "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
                    meta.column,
                    meta.display_name.clone().unwrap_or_default(),
                    meta.width.map(|w| w.to_string()).unwrap_or_default(),
                    if meta.frozen { "1" } else { "0" },
                    meta.format.name()
                )?;
            }
            out.flush()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Metadata for the given columns, in order, filling defaults where unset.
    pub fn resolve(&self, columns: &[String]) -> Vec<ColumnMeta> {
        columns
            .iter()
            .map(|name| match self.entries.get(name) {
                Some(meta) => meta.clone(),
                None => ColumnMeta {
                    column: name.clone(),
                    ..Default::default()
                },
            })
            .collect()
    }

    /// Set one property of one column.
    pub fn set(&mut self, column: &str, key: &str, value: &str) -> Result<(), String> {
        let entry = self
            .entries
            .entry(column.to_string())
            .or_insert_with(|| ColumnMeta {
                column: column.to_string(),
                ..Default::default()
            });
        match key.trim().to_ascii_lowercase().as_str() {
            "name" | "displayname" => {
                entry.display_name = (!value.trim().is_empty()).then(|| value.trim().to_string());
            }
            "width" => {
                let parsed: u32 = value
                    .trim()
                    .parse()
                    .map_err(|_| format!("'{value}' is not a width"))?;
                if !(24..=2000).contains(&parsed) {
                    return Err(format!("width {parsed} is outside 24..2000"));
                }
                entry.width = Some(parsed);
            }
            "frozen" | "freeze" => {
                entry.frozen = matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                );
            }
            "format" => entry.format = NumberFormat::parse(value)?,
            other => {
                return Err(format!(
                    "unknown property '{other}' (use name, width, frozen, format)"
                ))
            }
        }
        Ok(())
    }

    /// How many leading columns are frozen.
    ///
    /// Frozen columns are a *prefix*, like a spreadsheet's panes: freezing the third
    /// column while the first two scroll would look broken, so the prefix is what
    /// counts and a gap simply stops it.
    pub fn frozen_prefix(&self, columns: &[String]) -> usize {
        columns
            .iter()
            .take_while(|name| self.entries.get(*name).is_some_and(|meta| meta.frozen))
            .count()
    }

    /// Freeze the first `count` columns, and unfreeze everything else.
    pub fn set_frozen_prefix(&mut self, columns: &[String], count: usize) -> Result<(), String> {
        if count > columns.len() {
            return Err(format!(
                "cannot freeze {count} columns; the sheet has {}",
                columns.len()
            ));
        }
        for (index, name) in columns.iter().enumerate() {
            self.set(name, "frozen", if index < count { "true" } else { "false" })?;
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_layout_{tag}_{}_{}",
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

    #[test]
    fn number_formats_render_the_way_a_spreadsheet_does() {
        let fmt = NumberFormat::parse("#,##0.00").unwrap();
        assert_eq!(fmt.render("1234567.891"), "1,234,567.89");
        assert_eq!(fmt.render("12"), "12.00");
        // Non-numbers are left alone: the format is a promise about numbers.
        assert_eq!(fmt.render("N/A"), "N/A");
        assert_eq!(fmt.render(""), "");

        let pct = NumberFormat::parse("0.0%").unwrap();
        assert_eq!(pct.render("0.1234"), "12.3%");
        assert_eq!(pct.render("1"), "100.0%");

        let money = NumberFormat::parse("$#,##0.00").unwrap();
        assert_eq!(money.render("1234.5"), "$1,234.50");

        let plain = NumberFormat::parse("0").unwrap();
        assert_eq!(plain.render("3.7"), "4");

        assert!(NumberFormat::parse("banana").is_err());
    }

    #[test]
    fn date_time_formats_render_iso_values() {
        let date = NumberFormat::parse("yyyy-mm-dd").unwrap();
        assert_eq!(date.render("2024-01-02"), "2024-01-02");
        assert_eq!(date.name(), "yyyy-mm-dd");
        let slash = NumberFormat::parse("m/d/yyyy").unwrap();
        assert_eq!(slash.render("2024-01-02"), "1/2/2024");
        let short = NumberFormat::parse("yyyy/m/d").unwrap();
        assert_eq!(short.render("2024-01-02"), "2024/1/2");
        let day_first = NumberFormat::parse("dd/mm/yyyy").unwrap();
        assert_eq!(day_first.render("2024-01-02"), "02/01/2024");
        let month_first = NumberFormat::parse("mm/dd/yyyy").unwrap();
        assert_eq!(month_first.render("2024-01-02"), "01/02/2024");
        let minute = NumberFormat::parse("yyyy-mm-dd hh:mm").unwrap();
        assert_eq!(minute.render("2024-01-02 13:04:05"), "2024-01-02 13:04");
        assert_eq!(minute.render("2024-01-02T13:04:05"), "2024-01-02 13:04");
        let seconds = NumberFormat::parse("hh:mm:ss").unwrap();
        assert_eq!(seconds.render("13:04:05"), "13:04:05");
        // A format never invents a date from a non-date value.
        assert_eq!(date.render("N/A"), "N/A");
        assert_eq!(date.render("123"), "123");
        assert!(NumberFormat::parse("yyyy-mm").is_err());
    }

    #[test]
    fn scientific_accounting_and_text_formats_work() {
        let sci = NumberFormat::parse("0.00E+00").unwrap();
        assert_eq!(sci.name(), "0.00E+00");
        assert_eq!(sci.render("1234.5"), "1.23E+03");
        let lower = NumberFormat::parse("0.0e+00").unwrap();
        assert_eq!(lower.render("1234.5"), "1.2e+03");

        let money = NumberFormat::parse("$#,##0.00;($#,##0.00)").unwrap();
        assert_eq!(money.render("1234.5"), "$1,234.50");
        assert_eq!(money.render("-1234.5"), "($1,234.50)");

        let text = NumberFormat::parse("@").unwrap();
        assert_eq!(text.render("007"), "007");
        assert_eq!(text.render("-"), "-");
        assert!(NumberFormat::parse("0.00E").is_err());
    }

    #[test]
    fn grouping_keeps_the_sign_and_the_decimals() {
        assert_eq!(group("-1234567.891"), "-1,234,567.891");
        assert_eq!(group("999"), "999");
        assert_eq!(group("1000"), "1,000");
    }

    #[test]
    fn layout_round_trips_through_its_sidecar() {
        let dir = Dir::new("roundtrip");
        let base = dir.join("t.csv");
        std::fs::write(&base, "city,amount\nCDMX,10\n").unwrap();

        let mut layout = Layout::default();
        layout.set("city", "width", "160").unwrap();
        layout.set("city", "name", "城市").unwrap();
        layout.set("amount", "format", "#,##0.00").unwrap();
        layout.set("city", "freeze", "true").unwrap();
        layout.save(&base).unwrap();

        let loaded = Layout::load(&base).unwrap();
        assert_eq!(loaded, layout, "layout must survive a save/load cycle");
        assert!(Layout::sidecar(&base).exists());
    }

    #[test]
    fn defaults_are_filled_for_columns_without_metadata() {
        let dir = Dir::new("defaults");
        let base = dir.join("t.csv");
        std::fs::write(&base, "a,b,c\n1,2,3\n").unwrap();
        let mut layout = Layout::default();
        layout.set("b", "width", "80").unwrap();

        let resolved = layout.resolve(&["a".into(), "b".into(), "c".into()]);
        assert_eq!(
            resolved[0].display_name, None,
            "unset column keeps defaults"
        );
        assert_eq!(resolved[1].width, Some(80));
        assert_eq!(resolved[2].format, NumberFormat::Plain);
    }

    #[test]
    fn frozen_columns_are_a_prefix() {
        let columns = vec!["id".to_string(), "city".to_string(), "amount".to_string()];
        let mut layout = Layout::default();
        layout.set_frozen_prefix(&columns, 2).unwrap();
        assert_eq!(layout.frozen_prefix(&columns), 2);

        // A gap stops the prefix: freezing only the third column pins nothing.
        layout.set("id", "frozen", "false").unwrap();
        layout.set("city", "frozen", "false").unwrap();
        layout.set("amount", "frozen", "true").unwrap();
        assert_eq!(layout.frozen_prefix(&columns), 0);

        assert!(layout.set_frozen_prefix(&columns, 4).is_err());
    }

    #[test]
    fn invalid_settings_are_refused() {
        let mut layout = Layout::default();
        assert!(layout.set("a", "width", "abc").is_err());
        assert!(layout.set("a", "width", "5").is_err(), "too narrow");
        assert!(layout.set("a", "format", "purple").is_err());
        assert!(
            layout.set("a", "colour", "red").is_err(),
            "unknown property"
        );
    }

    #[test]
    fn numbers_keep_full_precision_before_formatting() {
        // The formatter must not round through the aggregate formatter: a currency
        // column of 215 billion would lose its cents otherwise.
        let fmt = NumberFormat::parse("$#,##0.00").unwrap();
        assert_eq!(fmt.render("215064677256.2"), "$215,064,677,256.20");
    }
}
