use crate::{
    note_index_build, parse_delimiter, print_command_help, report_failure, spawn_reporter, Parser,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_format(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut rules_text: Vec<String> = Vec::new();
    let mut start: u64 = 0;
    let mut count: u64 = 100;
    let mut summary = false;
    let mut json = false;
    let mut quiet = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--rule" => match p.value("--rule") {
                Ok(v) => rules_text.push(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--start" => match p
                .value("--start")
                .and_then(|v| v.parse().map_err(|_| "bad --start".to_string()))
            {
                Ok(v) => start = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--count" => match p
                .value("--count")
                .and_then(|v| v.parse().map_err(|_| "bad --count".to_string()))
            {
                Ok(v) => count = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--summary" => summary = true,
            "--json" => json = true,
            "--quiet" | "-q" => quiet = true,
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("format");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => {
                if input.is_none() {
                    input = Some(PathBuf::from(other));
                } else {
                    eprintln!("error: unexpected argument: {other}");
                    return 2;
                }
            }
        }
    }

    let Some(input) = input else {
        eprintln!("error: format needs an <input> file");
        return 2;
    };
    if rules_text.is_empty() {
        eprintln!("error: format needs at least one --rule \"column>value:bg=...\"");
        return 2;
    }
    let mut rules = Vec::new();
    for text in &rules_text {
        match csvx_core::format::Rule::parse(text) {
            Ok(rule) => rules.push(rule),
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        }
    }
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let sidecar = csvx_core::workbook::Journal::sidecar(&input);
    let journal = match csvx_core::workbook::Journal::load(&sidecar) {
        Ok(j) => j,
        Err(e) => {
            return report_failure("format", format!("cannot read {}: {e}", sidecar.display()))
        }
    };

    if summary {
        // Whole-file census: which rule is too broad, before anyone sees colours.
        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("format", rx, quiet || json);
        let result =
            csvx_core::format::count_matches(&input, journal.ops(), &rules, delimiter, &tx);
        drop(tx);
        let _ = reporter.join();
        let summary = match result {
            Ok(s) => s,
            Err(_) => return 1,
        };
        if json {
            let counts: Vec<String> = summary.matches.iter().map(|n| n.to_string()).collect();
            println!(
                r#"{{"rows":{},"rules":[{}],"matches":[{}]}}"#,
                summary.rows,
                rules
                    .iter()
                    .map(|r| r.to_json())
                    .collect::<Vec<_>>()
                    .join(","),
                counts.join(",")
            );
        } else {
            println!("{} rows scanned", summary.rows);
            for (rule, hits) in rules.iter().zip(&summary.matches) {
                let pct = if summary.rows == 0 {
                    0.0
                } else {
                    *hits as f64 / summary.rows as f64 * 100.0
                };
                println!("  {:>6} rows ({pct:.1}%)  {}", hits, rule.source);
            }
        }
        return 0;
    }

    note_index_build(&input);
    let cells = match csvx_core::format::styled_window(
        &input,
        journal.ops(),
        &rules,
        start,
        count,
        delimiter,
    ) {
        Ok(c) => c,
        Err(e) => return report_failure("format", e),
    };

    if json {
        let items: Vec<String> = cells.iter().map(|c| c.to_json()).collect();
        println!(
            r#"{{"start":{start},"count":{},"cells":[{}]}}"#,
            cells.len(),
            items.join(",")
        );
    } else {
        println!(
            "{} cell(s) match in rows {start}..{}",
            cells.len(),
            start + count
        );
        for cell in cells.iter().take(40) {
            println!(
                "  row {:>6}  {:<12} = {:<20} → {}",
                cell.row + 1,
                cell.column,
                cell.value,
                cell.style.to_json()
            );
        }
        if cells.len() > 40 {
            println!("  … and {} more", cells.len() - 40);
        }
    }
    0
}
