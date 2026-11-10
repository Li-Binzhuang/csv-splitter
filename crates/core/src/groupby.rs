//! Group-by aggregation, streaming and bounded.
//!
//! `sql "SELECT city, sum(amount) GROUP BY 1"` already answers this, but it pays
//! DuckDB's startup and has to hold the whole table in its own format. For the
//! shapes people actually ask of a CSV — a few thousand keys, one pass, output
//! written straight to a file — a native accumulator is simpler and keeps memory
//! proportional to the number of *groups* rather than the number of rows.
//!
//! The bounded part matters: a map keyed by group value can explode if someone
//! groups by an id column. Past `max_groups` the run fails and says so, instead of
//! swapping until the machine dies.
//!
//! Aggregates settle on the first value seen per column: a column that turns out
//! to hold `N/A` half-way through does not retroactively become an average of
//! the numeric rows only — it is reported as non-numeric.

use crate::csv_reader::{join_csv_line, parse_csv_record_with, QuoteStyle, RecordReader};
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;

/// Default ceiling on retained groups (roughly 100 bytes each).
pub const DEFAULT_MAX_GROUPS: usize = 1_000_000;

pub use crate::aggregate::{format_number, Accumulator, AggOp, AggSpec};

#[derive(Clone, Debug)]
struct Group {
    values: Vec<String>,
    accs: Vec<Accumulator>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupSummary {
    pub rows: u64,
    pub groups: u64,
    pub bytes: u64,
    pub key_columns: Vec<String>,
    pub aggregates: Vec<String>,
}

/// How the grouped result is produced and ordered.
#[derive(Clone, Debug)]
pub struct GroupOptions {
    pub keys: Vec<String>,
    pub aggregates: Vec<AggSpec>,
    /// Output column to sort by; `None` sorts by the key columns.
    pub order_by: Option<String>,
    pub descending: bool,
    pub max_groups: usize,
}

impl Default for GroupOptions {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            aggregates: Vec::new(),
            order_by: None,
            descending: false,
            max_groups: DEFAULT_MAX_GROUPS,
        }
    }
}

/// Group by `options.keys` and aggregate into `output`.
pub fn groupby(
    path: &Path,
    delimiter: Delimiter,
    output: &Path,
    options: &GroupOptions,
    tx: &ProgressSender,
) -> io::Result<GroupSummary> {
    groupby_quotes(path, delimiter, output, options, QuoteStyle::Rfc4180, tx)
}

/// Same as [`groupby`], with an explicit quote style.
pub fn groupby_quotes(
    path: &Path,
    delimiter: Delimiter,
    output: &Path,
    options: &GroupOptions,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<GroupSummary> {
    let keys = &options.keys;
    let aggs = &options.aggregates;
    let order_by = options.order_by.as_deref();
    let descending = options.descending;
    let max_groups = options.max_groups;
    if keys.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no key columns",
        ));
    }
    if aggs.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no aggregates"));
    }
    let total_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseStart {
        phase: "Grouping".into(),
        total_bytes,
    })
    .ok();

    let reader = encoding::open_decoded(path)?;
    let mut records = RecordReader::new_with(
        BufReader::with_capacity(8 * 1024 * 1024, reader),
        8 * 1024 * 1024,
        quotes,
    );
    let delim = delimiter.byte();
    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
    }
    let headers = parse_csv_record_with(&String::from_utf8_lossy(&record), delim, quotes);

    let key_indices = resolve(&headers, keys)?;
    let mut agg_indices = Vec::with_capacity(aggs.len());
    for agg in aggs {
        agg_indices.push(resolve(&headers, std::slice::from_ref(&agg.column))?[0]);
    }

    let mut groups: HashMap<String, Group> = HashMap::new();
    let max_groups = max_groups.max(1);
    let mut rows: u64 = 0;

    while records.next_record(&mut record)? {
        rows += 1;
        let text = String::from_utf8_lossy(&record);
        let fields = parse_csv_record_with(text.trim_end_matches(['\n', '\r']), delim, quotes);
        let key_values: Vec<String> = key_indices
            .iter()
            .map(|&i| fields.get(i).cloned().unwrap_or_default())
            .collect();
        // A separator that cannot appear in a CSV field: the unit separator.
        let map_key = key_values.join("\u{1f}");

        if !groups.contains_key(&map_key) && groups.len() >= max_groups {
            return Err(io::Error::other(format!(
                "more than {max_groups} groups; raise --max-groups or group by a lower-cardinality column"
            )));
        }

        let group = groups.entry(map_key).or_insert_with(|| Group {
            values: key_values.clone(),
            accs: vec![Accumulator::new(); aggs.len()],
        });

        for (slot, idx) in agg_indices.iter().enumerate() {
            let value = fields.get(*idx).map(String::as_str).unwrap_or("");
            let acc = &mut group.accs[slot];
            acc.observe(value);
        }
        if rows.is_multiple_of(500_000) {
            tx.send(ProgressEvent::Progress { bytes: rows }).ok();
        }
    }

    let mut output_columns: Vec<String> = keys.to_vec();
    output_columns.extend(aggs.iter().map(AggSpec::output_name));
    let summary = GroupSummary {
        rows,
        groups: groups.len() as u64,
        key_columns: keys.to_vec(),
        aggregates: aggs.iter().map(AggSpec::output_name).collect(),
        ..Default::default()
    };

    // Order the result: by a key column, or by one of the aggregate outputs.
    let mut rendered: Vec<(Vec<String>, Group)> = groups
        .into_values()
        .map(|group| {
            let cells: Vec<String> = group
                .values
                .iter()
                .chain(
                    group
                        .accs
                        .iter()
                        .enumerate()
                        .map(|(i, acc)| acc.render(aggs[i].op).as_str().to_owned())
                        .collect::<Vec<String>>()
                        .iter(),
                )
                .cloned()
                .collect();
            (cells, group)
        })
        .collect();

    let order_idx = match order_by {
        Some(name) => {
            let pos = output_columns
                .iter()
                .position(|c| c == name)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("cannot order by '{name}': not an output column"),
                    )
                })?;
            Some(pos)
        }
        None => None,
    };
    match order_idx {
        Some(pos) => rendered.sort_by(|a, b| {
            let ord = smart_cmp(&a.0[pos], &b.0[pos]);
            if descending {
                ord.reverse()
            } else {
                ord
            }
        }),
        None => {
            rendered.sort_by(|a, b| a.0.cmp(&b.0));
            if descending {
                rendered.reverse();
            }
        }
    }

    let mut out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(output)?);
    let header_refs: Vec<&str> = output_columns.iter().map(String::as_str).collect();
    out.write_all(join_csv_line(&header_refs, delim).as_bytes())?;
    for (cells, _) in &rendered {
        let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
        out.write_all(join_csv_line(&refs, delim).as_bytes())?;
    }
    out.flush()?;

    Ok(GroupSummary {
        bytes: std::fs::metadata(output).map(|m| m.len()).unwrap_or(0),
        ..summary
    })
}

fn resolve(headers: &[String], names: &[String]) -> io::Result<Vec<usize>> {
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let idx = crate::csv_reader::resolve_column(name, headers)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        out.push(idx);
    }
    Ok(out)
}

fn smart_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(fa), Ok(fb)) => fa.partial_cmp(&fb).unwrap_or(std::cmp::Ordering::Equal),
        _ => a.cmp(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_group_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn join(&self, n: &str) -> std::path::PathBuf {
            self.0.join(n)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn run(
        csv: &str,
        keys: &[&str],
        aggs: &str,
        order: Option<&str>,
        tag: &str,
    ) -> (GroupSummary, String) {
        let d = Dir::new(tag);
        let input = d.join("in.csv");
        let output = d.join("out.csv");
        std::fs::write(&input, csv).unwrap();
        let aggs = AggSpec::parse_list(aggs).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let summary = groupby(
            &input,
            Delimiter::Comma,
            &output,
            &GroupOptions {
                keys: keys.iter().map(|k| k.to_string()).collect(),
                aggregates: aggs,
                order_by: order.map(|o| o.to_string()),
                descending: true,
                max_groups: DEFAULT_MAX_GROUPS,
            },
            &tx,
        )
        .unwrap();
        (summary, std::fs::read_to_string(&output).unwrap())
    }

    #[test]
    fn sums_and_counts_per_group() {
        let csv = "city,amount\nCDMX,10\nPuebla,5\nCDMX,20\nCDMX,30\n";
        let (s, text) = run(csv, &["city"], "amount:sum,amount:count", None, "sum");
        assert_eq!(s.groups, 2);
        assert_eq!(s.rows, 4);
        let mut lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "city,sum_amount,count_amount");
        lines.sort();
        assert!(lines.contains(&"CDMX,60,3"), "{text}");
        assert!(lines.contains(&"Puebla,5,1"), "{text}");
    }

    #[test]
    fn integer_formatting_does_not_show_float_noise() {
        let (_s, text) = run("k,v\na,0.1\na,0.2\n", &["k"], "v:sum", None, "float");
        assert_eq!(text, "k,sum_v\na,0.3\n", "{text}");
    }

    #[test]
    fn mean_and_min_max_work() {
        let (_s, text) = run(
            "k,v\na,10\na,20\nb,5\n",
            &["k"],
            "v:mean,v:min,v:max",
            None,
            "mean",
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "k,mean_v,min_v,max_v");
        assert!(text.contains("a,15,10,20"), "{text}");
        assert!(text.contains("b,5,5,5"), "{text}");
    }

    #[test]
    fn a_text_column_aggregates_lexically_and_reports_mixed_sums() {
        let (_s, text) = run("k,v\na,x\na,y\n", &["k"], "v:min,v:max,v:sum", None, "text");
        assert!(text.contains("a,x,y,<mixed>"), "{text}");
    }

    #[test]
    fn orders_by_an_aggregate_output() {
        let csv = "city,amount\nCDMX,10\nPuebla,50\nTijuana,30\n";
        let (_s, text) = run(csv, &["city"], "amount:sum", Some("sum_amount"), "order");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "Puebla,50");
        assert_eq!(lines[3], "CDMX,10");
    }

    #[test]
    fn multi_key_grouping_and_cap() {
        let csv = "city,kind,v\nCDMX,a,1\nCDMX,b,2\nCDMX,a,3\n";
        let (_s, text) = run(csv, &["city", "kind"], "v:sum", None, "multi");
        assert!(text.contains("CDMX,a,4"), "{text}");
        assert!(text.contains("CDMX,b,2"), "{text}");
    }

    #[test]
    fn group_cap_fails_loudly() {
        let d = Dir::new("cap");
        let input = d.join("in.csv");
        let mut csv = String::from("k,v\n");
        for i in 0..20 {
            csv.push_str(&format!("{i},1\n"));
        }
        std::fs::write(&input, csv).unwrap();
        let aggs = AggSpec::parse_list("v:sum").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = groupby(
            &input,
            Delimiter::Comma,
            &d.join("o.csv"),
            &GroupOptions {
                keys: vec!["k".to_string()],
                aggregates: aggs,
                order_by: None,
                descending: true,
                max_groups: 5,
            },
            &tx,
        )
        .unwrap_err();
        assert!(err.to_string().contains("more than 5 groups"), "{err}");
    }

    #[test]
    fn unknown_column_and_bad_aggregate_are_client_errors() {
        assert!(AggSpec::parse_list("v:nope").is_err());
        assert!(AggSpec::parse_list("v").is_err());
        let d = Dir::new("bad");
        let input = d.join("in.csv");
        std::fs::write(&input, "a\n1\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = groupby(
            &input,
            Delimiter::Comma,
            &d.join("o.csv"),
            &GroupOptions {
                keys: vec!["nope".to_string()],
                aggregates: AggSpec::parse_list("a:sum").unwrap(),
                ..Default::default()
            },
            &tx,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

#[cfg(test)]
mod formatting_tests {
    use super::*;

    #[test]
    fn large_sums_do_not_show_float_noise() {
        // A total of 2e11 cannot resolve micro-units in f64; printing them
        // would be pretending to a precision that is not there.
        // Simulate accumulated drift on a 2e11 total: build the value from
        // parts so no oversized float literal is needed.
        let drift = 215_064_677_256.0 + 0.200012;
        assert_eq!(format_number(drift), "215064677256.2");
        let below = 213_244_423_807.0 + 0.299988;
        assert_eq!(format_number(below), "213244423807.3");
    }

    #[test]
    fn ordinary_magnitudes_keep_their_cents() {
        assert_eq!(format_number(1234.5678), "1234.5678");
        assert_eq!(format_number(0.3), "0.3");
        assert_eq!(format_number(60.0), "60");
        assert_eq!(format_number(-0.0000001), "0");
    }
}
