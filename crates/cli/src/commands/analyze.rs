use crate::commands::shared::json_quote;
use crate::{
    parse_delimiter, parse_quotes, print_command_help, print_quality_markdown, print_quality_text,
    quality_json, spawn_reporter, warn_pending_edits, warn_quote_style, Parser,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_analyze(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut sample: Option<u64> = None;
    let mut input: Option<PathBuf> = None;
    let mut distinct_cap = csvx_core::profile::DEFAULT_DISTINCT_CAP;
    let mut json = false;
    let mut quality = false;
    let mut markdown = false;
    let mut keys: Vec<String> = Vec::new();
    let mut groups: Vec<String> = Vec::new();
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--quality" => quality = true,
            "--markdown" => markdown = true,
            "--key" => match p.value("--key") {
                Ok(v) => keys.extend(
                    v.split(',')
                        .map(|item| item.trim().to_string())
                        .filter(|item| !item.is_empty()),
                ),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--group" => match p.value("--group") {
                Ok(v) => groups.push(v.trim().to_string()),
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
            "--distinct-cap" => match p
                .value("--distinct-cap")
                .and_then(|v| v.parse().map_err(|_| "bad --distinct-cap".to_string()))
            {
                Ok(v) => distinct_cap = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },

            "--sample" => match p
                .value("--sample")
                .and_then(|v| v.parse().map_err(|_| "bad --sample".to_string()))
            {
                Ok(v) => sample = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("analyze");
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
        eprintln!("error: analyze needs <input>");
        return 2;
    };
    warn_quote_style(&input);
    warn_pending_edits(&input);
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("analyze", rx, true);
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let result =
        csvx_core::profile::profile_quotes(&input, delimiter, distinct_cap, sample, quotes, &tx);
    drop(tx);
    let _ = reporter.join();
    let profiles = match result {
        Ok(p) => p,
        Err(_) => {
            // The reporter already printed the reason.
            return 1;
        }
    };

    if quality {
        for key in &keys {
            if !profiles.iter().any(|profile| &profile.name == key) {
                eprintln!("error: unknown key column {key}");
                return 2;
            }
        }
        for group in &groups {
            if !profiles.iter().any(|profile| &profile.name == group) {
                eprintln!("error: unknown group column {group}");
                return 2;
            }
        }
        if markdown {
            print_quality_markdown(&input, &profiles, &keys, &groups);
            return 0;
        }
        if json {
            let cols: Vec<String> = profiles.iter().map(profile_json).collect();
            println!(
                r#"{{"columns":[{}],"quality":{}}}"#,
                cols.join(","),
                quality_json(&profiles, &keys, &groups)
            );
            return 0;
        }
        print_quality_text(&profiles, &keys, &groups);
        return 0;
    }
    if markdown {
        eprintln!("error: --markdown needs --quality");
        return 2;
    }
    if json {
        let cols: Vec<String> = profiles.iter().map(profile_json).collect();
        println!(r#"{{"columns":[{}]}}"#, cols.join(","));
        return 0;
    }

    if profiles.is_empty() {
        println!("(file has no columns: it is empty or contains only whitespace)");
        return 0;
    }
    // Ragged records are the first thing worth knowing about a foreign file.
    if let Some(first) = profiles.first() {
        if first.ragged > 0 {
            println!(
                "⚠ {} of {} rows have a field count different from the header — columns may be shifted on those rows",
                first.ragged, first.rows
            );
        }
    }
    if let Some(limit) = sample {
        // Say what the number is. A prefix sample is an estimate, and a user who
        // reads "1000 rows" as "this file has 1000 rows" will draw wrong conclusions.
        println!(
            "⚠ sampled the first {limit} rows (a prefix, not a random sample) — counts are estimates"
        );
    }
    println!(
        "{:<20} {:>9} {:>8} {:>9}  summary",
        "column", "rows", "empty", "distinct"
    );
    for c in &profiles {
        let distinct = if c.distinct_truncated {
            format!("≥{}", c.distinct)
        } else {
            c.distinct.to_string()
        };
        let summary = match &c.numeric {
            Some(n) => format!(
                "min {:.2}  max {:.2}  mean {:.2}  median {:.2}  sum {:.2}",
                n.min,
                n.max,
                n.mean,
                n.median.unwrap_or(f64::NAN),
                n.sum
            ),
            None => {
                let top = c
                    .top
                    .iter()
                    .take(3)
                    .map(|(v, n)| format!("{v} ({n})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                if c.mixed_types {
                    format!("text (mixed with numbers): {top}")
                } else {
                    format!("text: {top}")
                }
            }
        };
        println!(
            "{:<20} {:>9} {:>8} {:>9}  {}",
            c.name, c.rows, c.empty, distinct, summary
        );
    }
    0
}

fn profile_json(c: &csvx_core::profile::ColumnProfile) -> String {
    let numeric = match &c.numeric {
        Some(n) => format!(
            r#"{{"count":{},"min":{},"max":{},"sum":{},"mean":{},"median":{}}}"#,
            n.count,
            n.min,
            n.max,
            n.sum,
            n.mean,
            n.median
                .map(|m| m.to_string())
                .unwrap_or_else(|| "null".to_string())
        ),
        None => "null".to_string(),
    };
    let top: Vec<String> = c
        .top
        .iter()
        .map(|(v, n)| format!(r#"["{}",{}]"#, json_quote(v).trim_matches('"'), n))
        .collect();
    format!(
        r#"{{"name":"{}","rows":{},"ragged":{},"empty":{},"nonEmpty":{},"distinct":{},"distinctTruncated":{},"numeric":{},"mixedTypes":{},"top":[{}]}}"#,
        json_quote(&c.name).trim_matches('"'),
        c.rows,
        c.ragged,
        c.empty,
        c.non_empty,
        c.distinct,
        c.distinct_truncated,
        numeric,
        c.mixed_types,
        top.join(",")
    )
}
