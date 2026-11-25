use crate::commands::report::truncate_for_display;
use crate::{
    parse_delimiter, parse_quotes, print_command_help, report_failure, spawn_reporter, Parser,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_validate(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut limit: Option<u64> = None;
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut rules_text: Vec<String> = Vec::new();
    let mut json = false;
    let mut quiet = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--check" | "--rule" => match p.value("--check") {
                Ok(v) => rules_text.push(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--checks" => match p.value("--checks") {
                Ok(v) => rules_text.extend(v.split(';').map(|r| r.trim().to_string())),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--limit" => match p
                .value("--limit")
                .and_then(|v| v.parse::<u64>().map_err(|_| "bad --limit".to_string()))
            {
                Ok(v) if v > 0 => limit = Some(v),
                Ok(_) => {
                    eprintln!("error: --limit is a positive row count");
                    return 2;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "-q" | "--quiet" => quiet = true,
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
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
                print_command_help("validate");
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
        eprintln!("error: validate needs a file");
        return 2;
    };
    if rules_text.is_empty() {
        eprintln!("error: validate needs at least one --check (e.g. --check \"qty>=0\")");
        return 2;
    }
    let rules: Vec<csvx_core::validate::Rule> = {
        let mut parsed = Vec::new();
        for text in &rules_text {
            match csvx_core::validate::Rule::parse(text) {
                Ok(rule) => parsed.push(rule),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        }
        parsed
    };
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let ops =
        match csvx_core::workbook::Journal::load(&csvx_core::workbook::Journal::sidecar(&input)) {
            Ok(journal) => journal.ops().to_vec(),
            Err(e) => return report_failure("validate", e.to_string()),
        };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("validate", rx, quiet || json);
    let result = csvx_core::validate::validate_limited(&input, &ops, &rules, delimiter, limit, &tx);
    drop(tx);
    let _ = reporter.join();
    let _ = quotes;
    let report = match result {
        Ok(r) => r,
        Err(e) => return report_failure("validate", e),
    };

    if json {
        println!("{}", report.to_json());
        return if report.failed() { 1 } else { 0 };
    }

    let failed = report.failed();
    for (rule, count) in &report.counts {
        let mark = if *count == 0 { "✓" } else { "✗" };
        println!("{mark} {rule}: {count} violation(s)");
    }
    if report.partial {
        println!(
            "! checked {} of {} row(s) (--limit); the counts describe that prefix only",
            report.checked, report.rows
        );
    }
    if report.unique_full {
        println!(
            "✗ unique ran out of room after {} distinct values; raise the cap or check a narrower range",
            csvx_core::validate::MAX_UNIQUE_VALUES
        );
    }
    if !report.violations.is_empty() {
        println!();
        for violation in report.violations.iter().take(20) {
            println!(
                "  row {}  {}  ({})  value: {}",
                violation.row,
                violation.column,
                violation.rule,
                truncate_for_display(&violation.value, 40)
            );
        }
        if report.violations.len() > 20 {
            println!("  ... and {} more", report.violations.len() - 20);
        }
        if report.truncated {
            println!(
                "  (violations cap reached at {}; counts above are exact)",
                csvx_core::validate::MAX_VIOLATIONS
            );
        }
    }
    println!(
        "\n{} rows checked{}, {} violation(s)",
        report.checked,
        if report.partial {
            format!(" of {} (--limit)", report.rows)
        } else {
            String::new()
        },
        report.total()
    );
    if failed {
        1
    } else {
        0
    }
}
