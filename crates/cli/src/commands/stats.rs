use crate::{
    parse_delimiter, parse_quotes, print_command_help, report_failure, spawn_reporter,
    warn_pending_edits, Parser,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_stats(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut sample = 50_000usize;
    let mut top = 5usize;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },

            "--sample" => match p
                .value("--sample")
                .and_then(|v| v.parse().map_err(|_| "bad --sample".to_string()))
            {
                Ok(v) => sample = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--top" => match p
                .value("--top")
                .and_then(|v| v.parse().map_err(|_| "bad --top".to_string()))
            {
                Ok(v) => top = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("stats");
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
        eprintln!("error: stats needs an <input> path");
        return 2;
    };
    warn_pending_edits(&input);
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("stats", rx, true);
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let result = csvx_core::preview::compute_column_stats_with(
        &input, delimiter, sample, 100_000, quotes, &tx,
    );
    drop(tx);
    reporter.join().ok();

    match result {
        Ok(stats) => {
            println!(
                "{} column(s), sampled {} rows",
                stats.len(),
                stats.first().map(|s| s.total_sampled).unwrap_or(0)
            );
            for col in stats {
                println!(
                    "\n▸ {}  (unique≈{}, empty={})",
                    col.name, col.unique_count, col.empty_count
                );
                for (value, count) in col.top_values.iter().take(top) {
                    println!("    {count:>8}  {value}");
                }
            }
            0
        }
        Err(e) => report_failure("stats", e), // engine returned the error itself
    }
}
