use crate::commands::report::truncate_for_display;
use crate::serve;
use crate::{parse_delimiter, print_command_help, report_failure, Parser};
use std::path::PathBuf;

pub(crate) fn cmd_schema(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut has: Option<String> = None;
    let mut require: Vec<String> = Vec::new();
    let mut diff_with: Option<PathBuf> = None;
    let mut sample: u64 = 10;
    let mut json = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--has" => match p.value("--has") {
                Ok(v) => has = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--diff" => match p.value("--diff") {
                Ok(v) => diff_with = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--require" => match p.value("--require") {
                Ok(v) => {
                    require = v
                        .split(',')
                        .map(|c| c.trim().to_string())
                        .filter(|c| !c.is_empty())
                        .collect()
                }
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
            "--json" => json = true,
            "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("schema");
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
        eprintln!("error: schema needs an <input> path");
        return 2;
    };
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let request = csvx_core::ops::SchemaRequest {
        path: input.clone(),
        delimiter,
        sample,
    };
    let sheet = match request.open() {
        Ok(sheet) => sheet,
        Err(e) => return report_failure("schema", format!("cannot open the sheet: {e}")),
    };

    if let Some(other) = &diff_with {
        let other_sheet =
            match csvx_core::sheet::Sheet::open(other, csvx_core::sheet::ReadOptions { delimiter })
            {
                Ok(sheet) => sheet,
                Err(e) => {
                    return report_failure(
                        "schema",
                        format!("cannot open {}: {e}", other.display()),
                    )
                }
            };
        let mine = sheet.columns().unwrap_or_default();
        let theirs = other_sheet.columns().unwrap_or_default();
        let diff = csvx_core::schema::diff(&mine, &theirs);
        if json {
            println!("{}", diff.to_json());
        } else {
            println!(
                "{} ({} columns) vs {} ({} columns)",
                input.display(),
                mine.len(),
                other.display(),
                theirs.len()
            );
            if diff.same {
                println!("✓ same column set");
            }
            for name in &diff.removed {
                println!("  − {name}   only in {}", input.display());
            }
            for name in &diff.added {
                println!("  + {name}   only in {}", other.display());
            }
            for moved in &diff.moved {
                println!(
                    "  → {}   position {} here, {} there",
                    moved.name, moved.here, moved.there
                );
            }
        }
        return if diff.same { 0 } else { 1 };
    }

    if !require.is_empty() {
        let columns = sheet.columns().unwrap_or_default();
        let missing: Vec<&String> = require
            .iter()
            .filter(|name| !columns.iter().any(|c| c.as_str() == name.as_str()))
            .collect();
        if missing.is_empty() {
            println!("✓ all {} required column(s) present", require.len());
            return 0;
        }
        eprintln!(
            "✗ missing {} of {} required column(s):",
            missing.len(),
            require.len()
        );
        for name in &missing {
            eprintln!(
                "  {name} — {}",
                csvx_core::csv_reader::column_candidates(name, &columns)
            );
        }
        return 1;
    }
    if let Some(name) = &has {
        let index = match sheet.column_index(name) {
            Ok(index) => index,
            Err(e) => return report_failure("schema", format!("cannot read the columns: {e}")),
        };
        match index {
            Some(index) => {
                if json {
                    println!(
                        r#"{{"name":{},"exists":true,"index":{index}}}"#,
                        serve::json_str(name)
                    );
                } else {
                    println!("✓ column {name:?} exists at position {index}");
                }
                0
            }
            None => {
                if json {
                    println!(r#"{{"name":{},"exists":false}}"#, serve::json_str(name));
                } else {
                    let columns = sheet.columns().unwrap_or_default();
                    eprintln!("✗ no column {name:?} in {} column(s)", columns.len());
                    // The same candidate logic every other column lookup uses.
                    eprintln!(
                        "  {}",
                        csvx_core::csv_reader::column_candidates(name, &columns)
                    );
                }
                1
            }
        }
    } else {
        let response = match request.response(&sheet) {
            Ok(response) => response,
            Err(e) => return report_failure("schema", format!("cannot profile the columns: {e}")),
        };
        let schema = response.columns;
        let rows = response.rows;
        if json {
            let body: Vec<String> = schema
                .iter()
                .map(|c| {
                    format!(
                        r#"{{"name":{},"index":{},"kind":{},"empty":{},"sampled":{},"distinct":{},"distinctCapped":{}}}"#,
                        serve::json_str(&c.name),
                        c.index,
                        serve::json_str(c.kind.as_str()),
                        c.empty,
                        c.sampled,
                        c.distinct,
                        c.distinct_capped
                    )
                })
                .collect();
            println!(
                r#"{{"rows":{rows},"count":{},"columns":[{}]}}"#,
                schema.len(),
                body.join(",")
            );
            return 0;
        }
        println!(
            "{} — {rows} rows, {} column(s), kinds sampled from the first {sample}",
            input.display(),
            schema.len()
        );
        let mut table = String::from(
            "  #    name                                     kind    empty   distinct\n",
        );
        for column in &schema {
            let empty = if column.sampled == 0 {
                "-".to_string()
            } else {
                format!("{}/{}", column.empty, column.sampled)
            };
            let distinct = if column.distinct_capped {
                format!("{}+", column.distinct)
            } else {
                column.distinct.to_string()
            };
            table.push_str(&format!(
                "  {:<4} {:<40} {:<7} {empty:<7} {distinct}\n",
                column.index,
                truncate_for_display(&column.name, 40),
                column.kind.as_str()
            ));
        }
        print!("{table}");
        0
    }
}
