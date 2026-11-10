//! Chart data: categories, series, numbers — nothing the client has to compute.
//!
//! A chart is an aggregation whose output shape is "one point per category". That
//! means it inherits the property that matters here: **cost follows the number of
//! categories, not the number of rows**. A million transactions charted by month is
//! twelve points, and the pass over the file is streaming.
//!
//! The output is deliberately boring JSON — `categories` plus one array per `series`
//! — because every charting library already accepts that shape, and because it keeps
//! the server out of the drawing business. `null` marks a category where a series
//! has no value, which line charts render as a gap instead of a fake zero.

use crate::aggregate::{Accumulator, AggSpec};
use crate::csv_reader::{parse_csv_record_with, resolve_column, QuoteStyle, RecordReader};
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, BufReader};
use std::path::Path;

/// Ceiling on categories (points per series).
pub const DEFAULT_MAX_POINTS: usize = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartKind {
    Line,
    Bar,
    Pie,
    Area,
}

impl ChartKind {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "line" => Ok(Self::Line),
            "bar" | "column" => Ok(Self::Bar),
            "pie" | "donut" => Ok(Self::Pie),
            "area" => Ok(Self::Area),
            other => Err(format!(
                "unknown chart type '{other}' (use line/bar/pie/area)"
            )),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Bar => "bar",
            Self::Pie => "pie",
            Self::Area => "area",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChartOptions {
    /// Column whose values become the x axis.
    pub x: String,
    /// Column whose values become separate series. `None` produces one series.
    pub series: Option<String>,
    /// The measure for the y axis.
    pub y: AggSpec,
    pub kind: ChartKind,
    /// Sort categories by value instead of by label (pie charts usually want this).
    pub sort_by_value: bool,
    pub descending: bool,
    pub max_points: usize,
}

impl Default for ChartOptions {
    fn default() -> Self {
        Self {
            x: String::new(),
            series: None,
            y: AggSpec {
                column: String::new(),
                op: crate::aggregate::AggOp::Sum,
            },
            kind: ChartKind::Line,
            sort_by_value: false,
            descending: false,
            max_points: DEFAULT_MAX_POINTS,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Series {
    pub name: String,
    /// One value per category; `None` = no data for that point.
    pub points: Vec<Option<f64>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChartData {
    pub kind: &'static str,
    pub categories: Vec<String>,
    pub series: Vec<Series>,
    pub rows: u64,
}

impl ChartData {
    /// JSON for the whole chart.
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(256 + self.categories.len() * 32);
        let _ = write!(
            out,
            r#"{{"type":"{}","rows":{},"categories":["#,
            self.kind, self.rows
        );
        for (index, category) in self.categories.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(out, "{}", quote(category));
        }
        out.push_str("],\"series\":[");
        for (index, series) in self.series.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let _ = write!(out, "{{\"name\":{},\"points\":[", quote(&series.name));
            for (point_index, point) in series.points.iter().enumerate() {
                if point_index > 0 {
                    out.push(',');
                }
                match point {
                    Some(v) => {
                        let _ = write!(out, "{}", crate::aggregate::format_number(*v));
                    }
                    None => out.push_str("null"),
                }
            }
            out.push_str("]}");
        }
        out.push_str("]}");
        out
    }
}

fn quote(text: &str) -> String {
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

/// Build chart data in one streaming pass.
pub fn chart(
    path: &Path,
    delimiter: Delimiter,
    options: &ChartOptions,
    tx: &ProgressSender,
) -> io::Result<ChartData> {
    chart_quotes(path, delimiter, options, QuoteStyle::Rfc4180, tx)
}

/// Same as [`chart`], with an explicit quote style.
pub fn chart_quotes(
    path: &Path,
    delimiter: Delimiter,
    options: &ChartOptions,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<ChartData> {
    if options.x.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a chart needs an --x column",
        ));
    }
    if options.y.column.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a chart needs a --y measure (column:op)",
        ));
    }
    let delim = delimiter.byte();
    let mut records = RecordReader::new_with(
        BufReader::with_capacity(4 * 1024 * 1024, encoding::open_decoded(path)?),
        4 * 1024 * 1024,
        quotes,
    );
    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the file has no header",
        ));
    }
    let header = parse_csv_record_with(
        String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
        delim,
        quotes,
    );

    let x_idx = resolve_column(&options.x, &header)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let series_idx = match &options.series {
        Some(name) => Some(
            resolve_column(name, &header)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
        ),
        None => None,
    };
    let y_idx = resolve_column(&options.y.column, &header)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    tx.send(ProgressEvent::PhaseStart {
        phase: "Charting".into(),
        total_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    })
    .ok();

    // category → (series name → accumulator). Insertion order is preserved for
    // categories so a date-like x axis comes out in the order it appears.
    let mut categories: Vec<String> = Vec::new();
    let mut category_slot: HashMap<String, usize> = HashMap::new();
    let mut series_names: Vec<String> = Vec::new();
    let mut series_slot: HashMap<String, usize> = HashMap::new();
    let mut cells: Vec<Vec<Accumulator>> = Vec::new();
    let mut rows = 0u64;
    let mut scanned = 0u64;

    while records.next_record(&mut record)? {
        let fields = parse_csv_record_with(
            String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
            delim,
            quotes,
        );
        rows += 1;
        scanned += record.len() as u64;
        if rows.is_multiple_of(500_000) {
            tx.send(ProgressEvent::Progress { bytes: scanned }).ok();
        }

        let category = fields.get(x_idx).cloned().unwrap_or_default();
        let category_index = match category_slot.get(&category) {
            Some(i) => *i,
            None => {
                if categories.len() >= options.max_points {
                    return Err(io::Error::other(format!(
                        "more than {} categories in '{}'; raise --max-points or chart a lower-cardinality column",
                        options.max_points, options.x
                    )));
                }
                categories.push(category.clone());
                category_slot.insert(category.clone(), categories.len() - 1);
                cells.push(Vec::new());
                categories.len() - 1
            }
        };

        let name = match series_idx {
            Some(i) => fields.get(i).cloned().unwrap_or_default(),
            None => String::new(),
        };
        let series_index = match series_slot.get(&name) {
            Some(i) => *i,
            None => {
                series_names.push(name.clone());
                series_slot.insert(name.clone(), series_names.len() - 1);
                series_names.len() - 1
            }
        };

        let row_cells = &mut cells[category_index];
        while row_cells.len() <= series_index {
            row_cells.push(Accumulator::new());
        }
        row_cells[series_index].observe(fields.get(y_idx).map(String::as_str).unwrap_or(""));
    }

    // Multi-series charts need every row to carry every series, using null where a
    // combination never occurred; that difference is what line charts draw as a gap.
    let values: Vec<Vec<Option<f64>>> = cells
        .iter()
        .map(|row| {
            let mut out = Vec::with_capacity(series_names.len());
            for index in 0..series_names.len() {
                match row.get(index) {
                    Some(acc) if acc.count() > 0 => out.push(Some(acc.value(options.y.op))),
                    _ => out.push(None),
                }
            }
            out
        })
        .collect();

    let order: Vec<usize> = if options.sort_by_value {
        let mut indexes: Vec<usize> = (0..categories.len()).collect();
        indexes.sort_by(|a, b| {
            let left = total(&values[*a]);
            let right = total(&values[*b]);
            left.partial_cmp(&right)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if options.descending {
            indexes.reverse();
        }
        indexes
    } else if options.descending {
        (0..categories.len()).rev().collect()
    } else {
        (0..categories.len()).collect()
    };

    let category_order: Vec<String> = order.iter().map(|i| categories[*i].clone()).collect();
    let series: Vec<Series> = series_names
        .iter()
        .enumerate()
        .map(|(slot, name)| Series {
            name: if name.is_empty() {
                options.y.output_name()
            } else {
                name.clone()
            },
            points: order.iter().map(|i| values[*i][slot]).collect(),
        })
        .collect();

    tx.send(ProgressEvent::PhaseDone {
        phase: "Charting".into(),
    })
    .ok();

    Ok(ChartData {
        kind: options.kind.name(),
        categories: category_order,
        series,
        rows,
    })
}

fn total(values: &[Option<f64>]) -> f64 {
    values.iter().flatten().sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::AggOp;
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
                "csvs_chart_{tag}_{}_{}",
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

    const SALES: &str = "\
month,city,amount
Jan,CDMX,10
Feb,CDMX,20
Jan,Puebla,30
Feb,Puebla,40
Jan,CDMX,5
";

    fn options(x: &str, series: Option<&str>, y: &str) -> ChartOptions {
        ChartOptions {
            x: x.into(),
            series: series.map(String::from),
            y: AggSpec {
                column: y.into(),
                op: AggOp::Sum,
            },
            ..Default::default()
        }
    }

    fn run(csv: &str, options: ChartOptions) -> ChartData {
        let dir = Dir::new("run");
        let path = dir.join("in.csv");
        std::fs::write(&path, csv).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let data = chart(&path, Delimiter::Comma, &options, &tx).unwrap();
        // The directory is dropped here; the data is what the caller needs.
        data
    }

    #[test]
    fn a_single_series_needs_no_series_column() {
        let data = run(SALES, options("month", None, "amount"));
        assert_eq!(data.categories, vec!["Jan", "Feb"]);
        assert_eq!(data.series.len(), 1);
        assert_eq!(data.series[0].points, vec![Some(45.0), Some(60.0)]);
        assert_eq!(data.rows, 5);
    }

    #[test]
    fn a_series_column_splits_into_multiple_lines() {
        let data = run(SALES, options("month", Some("city"), "amount"));
        assert_eq!(data.categories, vec!["Jan", "Feb"]);
        assert_eq!(data.series.len(), 2);
        assert_eq!(data.series[0].name, "CDMX");
        assert_eq!(data.series[1].name, "Puebla");
        assert_eq!(data.series[0].points, vec![Some(15.0), Some(20.0)]);
        assert_eq!(data.series[1].points, vec![Some(30.0), Some(40.0)]);
    }

    #[test]
    fn a_missing_combination_is_null_not_zero() {
        let csv = "month,city,amount\nJan,CDMX,10\nFeb,Puebla,20\n";
        let data = run(csv, options("month", Some("city"), "amount"));
        // feb/CDMX never happens: null keeps the line chart honest.
        assert_eq!(data.series[0].points, vec![Some(10.0), None]);
        assert_eq!(data.series[1].points, vec![None, Some(20.0)]);
    }

    #[test]
    fn sorting_by_value_orders_categories_by_their_total() {
        let mut options = options("month", None, "amount");
        options.sort_by_value = true;
        options.descending = true;
        let data = run(SALES, options);
        // Feb totals 60, Jan totals 45.
        assert_eq!(data.categories, vec!["Feb", "Jan"]);
    }

    #[test]
    fn too_many_categories_is_refused_with_the_parameter_to_raise() {
        let mut csv = String::from("id,amount\n");
        for i in 0..200 {
            csv.push_str(&format!("{i},1\n"));
        }
        let mut options = options("id", None, "amount");
        options.max_points = 100;
        let dir = Dir::new("wide");
        let path = dir.join("in.csv");
        std::fs::write(&path, &csv).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = chart(&path, Delimiter::Comma, &options, &tx).unwrap_err();
        assert!(err.to_string().contains("max-points"), "{err}");
    }

    #[test]
    fn json_shape_is_what_charting_libraries_expect() {
        let data = run(SALES, options("month", Some("city"), "amount"));
        let json = data.to_json();
        assert!(
            json.starts_with(r#"{"type":"line","rows":5,"categories":["Jan","Feb"]"#),
            "{json}"
        );
        assert!(json.contains(r#""name":"CDMX","points":[15,20]"#), "{json}");
        // Nulls stay nulls.
        let data = run(
            "month,city,amount\nJan,A,1\nFeb,B,2\n",
            options("month", Some("city"), "amount"),
        );
        assert!(data.to_json().contains("null"), "{}", data.to_json());
    }

    #[test]
    fn unknown_columns_are_reported() {
        let dir = Dir::new("bad");
        let path = dir.join("in.csv");
        std::fs::write(&path, SALES).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = chart(
            &path,
            Delimiter::Comma,
            &options("nope", None, "amount"),
            &tx,
        )
        .unwrap_err();
        assert!(err.to_string().contains("nope"), "{err}");
    }
}
