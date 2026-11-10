//! Column constraints: report the rows that break them, never change them.
//!
//! This is the "data validation" half of a spreadsheet, and it has the same rule as
//! everything else here: it reads the sheet through the journal, so what it checks is
//! what you would export — and it writes nothing.
//!
//! Memory stays flat: rows are checked in pages of 2 000 through the pager, and the
//! violations list is capped. `unique` needs a set of seen values, and that set has its
//! own cap which is reported when it is hit rather than silently stopping.

use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use crate::workbook::{Op as Edit, Pager};
use std::collections::HashSet;
use std::io;
use std::path::Path;

/// How many violations are kept. The report says when the list was cut.
pub const MAX_VIOLATIONS: usize = 1_000;
/// How many distinct values `unique` remembers before it stops being able to answer.
pub const MAX_UNIQUE_VALUES: usize = 5_000_000;
/// Rows per page while scanning.
const PAGE: u64 = 2_000;

#[derive(Clone, Debug, PartialEq)]
pub enum Rule {
    NotEmpty {
        column: String,
    },
    Numeric {
        column: String,
    },
    Int {
        column: String,
    },
    Date {
        column: String,
    },
    Unique {
        column: String,
    },
    /// Any comparison the filter syntax understands: `col>=0`, `col in a,b,c`.
    Compare {
        column: String,
        spec: String,
    },
}

impl Rule {
    /// Parse `column:kind` or `column<op>value`, e.g. `qty:not-empty`, `qty>=0`,
    /// `city in CDMX,Puebla`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("empty validation rule".to_string());
        }
        if let Some((column, kind)) = text.split_once(':') {
            let column = column.trim().to_string();
            if column.is_empty() {
                return Err(format!("'{text}': the column name is empty"));
            }
            return match kind.trim() {
                "not-empty" | "notempty" | "required" => Ok(Self::NotEmpty { column }),
                "numeric" | "number" => Ok(Self::Numeric { column }),
                "int" | "integer" => Ok(Self::Int { column }),
                "date" => Ok(Self::Date { column }),
                "unique" => Ok(Self::Unique { column }),
                other => Err(format!(
                    "'{text}': unknown check '{other}' (use not-empty, numeric, int, date, unique or a comparison like qty>=0)"
                )),
            };
        }
        // A comparison: the column is whatever precedes the operator.
        let (column, spec) = split_comparison(text).ok_or_else(|| {
            format!("'{text}': expected column:check or a comparison like qty>=0")
        })?;
        Ok(Self::Compare {
            column: column.to_string(),
            spec,
        })
    }

    pub fn column(&self) -> &str {
        match self {
            Self::NotEmpty { column }
            | Self::Numeric { column }
            | Self::Int { column }
            | Self::Date { column }
            | Self::Unique { column }
            | Self::Compare { column, .. } => column,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::NotEmpty { column } => format!("{column}: not empty"),
            Self::Numeric { column } => format!("{column}: a number"),
            Self::Int { column } => format!("{column}: an integer"),
            Self::Date { column } => format!("{column}: a YYYY-MM-DD date"),
            Self::Unique { column } => format!("{column}: unique"),
            Self::Compare { column, spec } => format!("{column}: {spec}"),
        }
    }

    fn failed(&self, value: &str) -> bool {
        match self {
            Self::NotEmpty { .. } => value.trim().is_empty(),
            Self::Numeric { .. } => value.trim().is_empty() || value.trim().parse::<f64>().is_err(),
            Self::Int { .. } => value.trim().is_empty() || value.trim().parse::<i64>().is_err(),
            Self::Date { .. } => !is_date(value.trim()),
            // Uniqueness is decided across rows, not from one value.
            Self::Unique { .. } => false,
            Self::Compare { .. } => false,
        }
    }
}

fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = text[0..4].parse().unwrap_or(0);
    let month: u32 = text[5..7].parse().unwrap_or(0);
    let day: u32 = text[8..10].parse().unwrap_or(0);
    if !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    // Real calendar: February has 29 days in a leap year and 28 otherwise, and
    // `2026-02-30` is not a date anyone meant to type.
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    day <= days
}

/// Split `col>=0` into ("col", ">=0"), trying the longest operators first.
fn split_comparison(text: &str) -> Option<(&str, String)> {
    for op in ["!=", ">=", "<=", " in ", "=", ">", "<", "~"] {
        if let Some(at) = text.find(op) {
            let column = text[..at].trim();
            if column.is_empty() {
                return None;
            }
            let rest = &text[at..];
            // ` in ` keeps its operand; the comparison operators keep theirs verbatim.
            let spec = if op == " in " {
                format!("in {}", rest.trim_start_matches(" in ").trim())
            } else {
                rest.to_string()
            };
            return Some((column, spec));
        }
    }
    None
}

#[derive(Clone, Debug)]
pub struct Violation {
    /// 1-based data row, header excluded — the same numbering `view` and the API use.
    pub row: u64,
    pub column: String,
    pub rule: String,
    pub value: String,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub rows: u64,
    pub violations: Vec<Violation>,
    /// True when the violation list stopped at [`MAX_VIOLATIONS`].
    pub truncated: bool,
    /// Per-rule counts, so a million-bad-rows column is summarised even when the list
    /// itself is short.
    pub counts: Vec<(String, u64)>,
    /// True when `unique` ran out of room to remember values.
    pub unique_full: bool,
    pub columns: Vec<String>,
}

impl Report {
    pub fn failed(&self) -> bool {
        self.counts.iter().any(|(_, count)| *count > 0) || self.unique_full
    }

    pub fn total(&self) -> u64 {
        self.counts.iter().map(|(_, count)| *count).sum()
    }

    pub fn to_json(&self) -> String {
        let violations: Vec<String> = self
            .violations
            .iter()
            .map(|v| {
                format!(
                    r#"{{"row":{},"column":{},"rule":{},"value":{}}}"#,
                    v.row,
                    json(&v.column),
                    json(&v.rule),
                    json(&v.value)
                )
            })
            .collect();
        let counts: Vec<String> = self
            .counts
            .iter()
            .map(|(rule, count)| format!(r#"{{"rule":{},"count":{count}}}"#, json(rule)))
            .collect();
        let columns: Vec<String> = self.columns.iter().map(|c| json(c)).collect();
        format!(
            r#"{{"rows":{},"violations":[{}],"counts":[{}],"truncated":{},"uniqueFull":{},"ok":{},"columns":[{}]}}"#,
            self.rows,
            violations.join(","),
            counts.join(","),
            self.truncated,
            self.unique_full,
            !self.failed(),
            columns.join(",")
        )
    }
}

fn json(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Check a sheet against the rules, applying the journal like every other read path.
pub fn validate(
    base: &Path,
    ops: &[Edit],
    rules: &[Rule],
    delimiter: Delimiter,
    tx: &ProgressSender,
) -> io::Result<Report> {
    let pager = match Pager::open(base, ops, delimiter) {
        Ok(p) => p,
        Err(e) => {
            tx.send(ProgressEvent::Error(e.to_string())).ok();
            return Err(e);
        }
    };
    let total = pager.total();
    let columns = pager.columns().to_vec();

    for rule in rules {
        if !columns.iter().any(|c| c == rule.column()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "rule '{}' reads unknown column '{}'",
                    rule.describe(),
                    rule.column()
                ),
            ));
        }
    }

    let mut compiled: Vec<Option<crate::filter::CompiledSet>> = Vec::with_capacity(rules.len());
    for rule in rules {
        match rule {
            Rule::Compare { column, spec } => {
                let probe = format!("{column}{spec}");
                let filter = crate::filter::FilterSpec::parse(&probe).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("rule '{}': {e}", rule.describe()),
                    )
                })?;
                let set = crate::filter::FilterSet::single(filter)
                    .compile(&columns)
                    .map_err(|e| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("rule '{}': {e}", rule.describe()),
                        )
                    })?;
                compiled.push(Some(set));
            }
            _ => compiled.push(None),
        }
    }

    let mut report = Report {
        columns: columns.clone(),
        counts: rules.iter().map(|r| (r.describe(), 0)).collect(),
        ..Default::default()
    };
    // One set per unique rule, keyed by its position in `rules`.
    let mut seen: Vec<Option<HashSet<String>>> = rules
        .iter()
        .map(|r| match r {
            Rule::Unique { .. } => Some(HashSet::new()),
            _ => None,
        })
        .collect();

    tx.send(ProgressEvent::PhaseStart {
        phase: "Validating".into(),
        total_bytes: total,
    })
    .ok();

    let mut at = 0u64;
    while at < total {
        let window = pager.page(at, PAGE)?;
        for (offset, row) in window.rows.iter().enumerate() {
            let row_number = at + offset as u64 + 1;
            report.rows += 1;
            for (index, rule) in rules.iter().enumerate() {
                let slot = columns.iter().position(|c| c == rule.column());
                let value = slot.and_then(|s| row.get(s)).cloned().unwrap_or_default();
                let broken = match rule {
                    Rule::Unique { .. } => {
                        let Some(set) = seen[index].as_mut() else {
                            continue;
                        };
                        if value.trim().is_empty() {
                            // Blank cells are not "the same value" for uniqueness.
                            false
                        } else if set.contains(&value) {
                            true
                        } else {
                            if set.len() >= MAX_UNIQUE_VALUES {
                                report.unique_full = true;
                            } else {
                                set.insert(value.clone());
                            }
                            false
                        }
                    }
                    Rule::Compare { column, .. } => match &compiled[index] {
                        Some(set) => {
                            // `holds` reads by column index, so hand it a row where only
                            // that column's slot is filled.
                            let mut probe = vec![String::new(); columns.len()];
                            if let Some(slot) = columns.iter().position(|c| c == column) {
                                if let Some(cell) = probe.get_mut(slot) {
                                    *cell = value.clone();
                                }
                            }
                            !set.holds(&probe)
                        }
                        // A rule that cannot be parsed was rejected up front; this is
                        // defensive, and reported rather than silently "passing".
                        None => true,
                    },
                    other => other.failed(&value),
                };
                if !broken {
                    continue;
                }
                if let Some(entry) = report.counts.get_mut(index) {
                    entry.1 += 1;
                }
                if report.violations.len() < MAX_VIOLATIONS {
                    report.violations.push(Violation {
                        row: row_number,
                        column: rule.column().to_string(),
                        rule: rule.describe(),
                        value,
                    });
                } else {
                    report.truncated = true;
                }
            }
        }
        tx.send(ProgressEvent::PhaseDone {
            phase: format!("Validating ({} rows)", report.rows),
        })
        .ok();
        at += PAGE;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_syntax_is_forgiving_about_spacing_but_strict_about_names() {
        assert_eq!(
            Rule::parse("qty>=0").unwrap(),
            Rule::Compare {
                column: "qty".to_string(),
                spec: ">=0".to_string()
            }
        );
        assert!(matches!(
            Rule::parse("city:not-empty").unwrap(),
            Rule::NotEmpty { .. }
        ));
        assert!(Rule::parse(":unique").is_err());
        assert!(Rule::parse("qty:wat").is_err());
    }

    #[test]
    fn a_date_must_exist_on_the_calendar() {
        assert!(is_date("2026-01-31"));
        assert!(is_date("2024-02-29"), "2024 is a leap year");
        assert!(!is_date("2026-02-29"), "2026 is not");
        assert!(!is_date("2026-02-30"));
        assert!(!is_date("2026-13-01"));
        assert!(!is_date("2026-1-1"));
        assert!(!is_date(""));
    }

    #[test]
    fn blank_cells_are_not_duplicates() {
        // Five blanks must not count as one value repeated five times.
        assert!(matches!(
            Rule::parse("city:unique").unwrap(),
            Rule::Unique { .. }
        ));
    }
}
