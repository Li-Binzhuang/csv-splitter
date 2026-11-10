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
        }
    }
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
