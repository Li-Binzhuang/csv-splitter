use crate::commands::sort::parse_sort_columns;
use crate::serve;
use crate::{
    note_index_build, parse_delimiter, parse_quotes, print_command_help, print_table,
    print_table_styled, project_columns, report_failure, spawn_reporter, warn_journal_damage,
    warn_quote_style, Parser,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_view(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut start: u64 = 0;
    let mut count: u64 = 20;
    let mut count_given = false;
    let mut delim = "auto".to_string();
    let mut where_expr: Option<String> = None;
    let mut order: Option<String> = None;
    let mut order_descending = false;
    let mut find: Option<String> = None;
    let mut shown_columns: Vec<String> = Vec::new();
    let mut rules_text: Vec<String> = Vec::new();
    let mut nth: u64 = 1;
    let mut context: u64 = 3;
    let mut match_case = false;
    let mut find_regex = false;
    let mut json = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--start" => match p.value("--start") {
                Ok(v) => match v.parse() {
                    Ok(n) => start = n,
                    Err(_) => {
                        eprintln!("error: --start needs a number");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--count" => match p.value("--count") {
                Ok(v) => match v.parse::<u64>() {
                    Ok(0) => {
                        eprintln!("error: --count must be at least 1");
                        return 2;
                    }
                    Ok(n) => {
                        count = n;
                        count_given = true;
                    }
                    Err(_) => {
                        eprintln!("error: --count needs a number");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--where" => match p.value("--where") {
                Ok(v) => where_expr = Some(v),
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
            "--order" => match p.value("--order") {
                Ok(v) => order = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--desc" => order_descending = true,
            "--nth" => match p.value("--nth") {
                Ok(v) => match v.parse::<u64>() {
                    Ok(0) => {
                        eprintln!("error: --nth starts at 1");
                        return 2;
                    }
                    Ok(n) => nth = n,
                    Err(_) => {
                        eprintln!("error: --nth needs a number");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--columns" => match p.value("--columns") {
                Ok(v) => {
                    shown_columns = v
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
            "--find" => match p.value("--find") {
                Ok(v) => find = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--context" => match p.value("--context") {
                Ok(v) => match v.parse() {
                    Ok(n) => context = n,
                    Err(_) => {
                        eprintln!("error: --context needs a number");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--match-case" => match_case = true,
            "--regex" => find_regex = true,
            "--rule" => match p.value("--rule") {
                Ok(v) => rules_text.push(v),
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
                print_command_help("view");
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
        eprintln!("error: view needs an <input> path");
        return 2;
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
    warn_quote_style(&input);
    // No `warn_pending_edits` here: unlike the read-only commands, `view` applies them.

    let sidecar = csvx_core::workbook::Journal::sidecar(&input);
    // An interrupted compaction must be repaired before this command loads the journal.
    if let Err(e) = csvx_core::workbook::recover_compaction(&input) {
        eprintln!("error: {e}");
        return 1;
    }
    warn_journal_damage(&input);
    let journal = match csvx_core::workbook::Journal::load(&sidecar) {
        Ok(j) => j,
        Err(e) => return report_failure("view", format!("cannot read the journal: {e}")),
    };
    let ops = journal.ops().to_vec();

    // The predicate is compiled once, against the *edited* column names, so a filter
    // on a renamed column behaves the way the user sees the sheet.
    let compiled = match &where_expr {
        None => None,
        Some(expr) => {
            let spec = match csvx_core::filter::FilterSpec::parse(expr) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            };
            let set = csvx_core::filter::FilterSet::single(spec);
            let columns = match csvx_core::workbook::resolve_columns(
                &ops,
                &match csvx_core::workbook::base_header(&input, delimiter) {
                    Ok(c) => c,
                    Err(e) => return report_failure("view", e.to_string()),
                },
            ) {
                Ok(c) => c,
                Err(e) => return report_failure("view", e.to_string()),
            };
            match set.compile(&columns) {
                Ok(c) => Some(c),
                Err(e) => return report_failure("view", e.to_string()),
            }
        }
    };

    // Ordered paging cannot be done by streaming: the smallest key may be on the last
    // row. So the rows are materialised once (edited, then filtered), sorted, and the
    // requested page is read back. That costs a full scan and a temporary copy of the
    // matched rows — nothing proportional to the file beyond the matches.
    // Ordered paging through DuckDB: when the sheet has no pending edits and no
    // `--where`, there is nothing the database cannot see, and `ORDER BY ... LIMIT` reads
    // the columns it needs instead of materialising 2.5 GB three times. Measured on the
    // 782-column fixture: 3.9 s here against 29.8 s for export+sort+page.
    //
    // The moment a journal edit exists this must stay native: the database would see the
    // base file without it.
    let sql_page: Option<(Vec<String>, Vec<Vec<String>>)> = if order.is_some() && ops.is_empty() {
        match csvx_core::engine::resolve(csvx_core::engine::Engine::Auto, &input) {
            Ok(csvx_core::engine::Engine::Sql) => {
                let order_columns = match parse_sort_columns(
                    order.as_deref().unwrap_or_default(),
                    order_descending,
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                };
                let order_sql = order_columns
                    .iter()
                    .map(|c| {
                        format!(
                            "\"{}\" {}",
                            c.name.replace('"', "\"\""),
                            if c.descending { "DESC" } else { "ASC" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                // `--where` joins the same query when it translates exactly; when the
                // native comparison has no SQL equivalent the read falls back instead.
                let predicate = match where_expr.as_deref() {
                    None => None,
                    Some(expr) => {
                        let spec = match csvx_core::filter::FilterSpec::parse(expr) {
                            Ok(spec) => spec,
                            Err(e) => {
                                eprintln!("error: {e}");
                                return 2;
                            }
                        };
                        match csvx_core::engine::filter_predicate(&spec) {
                            Some(predicate) => Some(predicate),
                            None => {
                                eprintln!(
                                    "note: ordered read fell back to the native path: \
                                     this --where has no exact SQL equivalent"
                                );
                                return 2;
                            }
                        }
                    }
                };
                let where_sql = predicate.map(|p| format!(" WHERE {p}")).unwrap_or_default();
                let query = format!(
                    "SELECT * FROM t{where_sql} ORDER BY {} LIMIT {} OFFSET {}",
                    order_sql, count, start
                );
                match csvx_core::sql::query_with(&input, &query, count as usize, delimiter) {
                    Ok(result) => Some((result.columns, result.rows)),
                    Err(e) => {
                        eprintln!("note: ordered read fell back to the native path: {e}");
                        None
                    }
                }
            }
            _ => None,
        }
    } else {
        None
    };

    let paging_file = if order.is_some() && sql_page.is_none() {
        let work = std::env::temp_dir().join(format!(
            "csvs_view_{}_{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let exported = work.with_extension("src.csv");
        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("view", rx, true);
        let result =
            csvx_core::workbook::export_quotes(&input, &ops, &exported, delimiter, quotes, &tx);
        drop(tx);
        let _ = reporter.join();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&exported);
            return report_failure("view", format!("cannot materialise the sheet: {e}"));
        }
        if let Some(expr) = &where_expr {
            let set = match csvx_core::filter::FilterSpec::parse(expr) {
                Ok(spec) => csvx_core::filter::FilterSet::single(spec),
                Err(e) => {
                    let _ = std::fs::remove_file(&exported);
                    eprintln!("error: {e}");
                    return 2;
                }
            };
            let filtered = work.with_extension("filtered.csv");
            let (tx, rx) = mpsc::channel();
            let reporter = spawn_reporter("view", rx, true);
            let result = csvx_core::filter::extract_style(
                &exported, delimiter, &set, &filtered, quotes, &tx,
            );
            drop(tx);
            let _ = reporter.join();
            let _ = std::fs::remove_file(&exported);
            if let Err(e) = result {
                let _ = std::fs::remove_file(&filtered);
                return report_failure("view", format!("cannot filter: {e}"));
            }
            std::fs::rename(&filtered, &work).ok();
        } else {
            std::fs::rename(&exported, &work).ok();
        }

        let sort_columns =
            match parse_sort_columns(order.as_deref().unwrap_or_default(), order_descending) {
                Ok(c) => c,
                Err(e) => {
                    let _ = std::fs::remove_file(&work);
                    eprintln!("error: {e}");
                    return 2;
                }
            };
        let sorted = work.with_extension("sorted.csv");
        let (tx, _rx) = mpsc::channel();
        let result = csvx_core::sort::external_sort_columns(
            &work,
            &sorted,
            &sort_columns,
            delimiter,
            64,
            &tx,
        );
        let _ = std::fs::remove_file(&work);
        if let Err(e) = result {
            let _ = std::fs::remove_file(&sorted);
            return report_failure("view", format!("cannot sort: {e}"));
        }
        Some(sorted)
    } else {
        None
    };

    // Two paths: a windowed read when nothing needs sorting (cheap, edits applied),
    // and paging a materialised+sorted copy when `--order` was asked for.
    note_index_build(&input);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut columns: Vec<String> = Vec::new();
    let mut highlight: Option<u64> = None;
    let page_total: Option<u64>;
    if let Some((sql_columns, sql_rows)) = &sql_page {
        columns = sql_columns.clone();
        rows = sql_rows.clone();
        page_total = None;
    } else if let Some(path) = &paging_file {
        if let Ok(file) = std::fs::File::open(path) {
            let mut reader = csvx_core::csv_reader::RecordReader::new(
                std::io::BufReader::with_capacity(4 * 1024 * 1024, file),
                4 * 1024 * 1024,
            );
            let mut record = Vec::new();
            let mut index: u64 = 0;
            while reader.next_record(&mut record).is_ok_and(|more| more) {
                let cells = csvx_core::csv_reader::parse_csv_record_with(
                    String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
                    delimiter.byte(),
                    quotes,
                );
                if columns.is_empty() {
                    columns = cells;
                    continue;
                }
                if index >= start {
                    rows.push(cells);
                    if rows.len() as u64 >= count {
                        break;
                    }
                }
                index += 1;
            }
        }
        let _ = std::fs::remove_file(path);
        page_total = None;
    } else if order.is_none() && where_expr.is_none() && find.is_none() {
        let request = csvx_core::ops::ViewRequest {
            path: input.clone(),
            delimiter,
            start,
            count,
        };
        let response = match request.execute() {
            Ok(response) => response,
            Err(e) => return report_failure("view", e.to_string()),
        };
        columns = response.window.columns;
        rows = response.window.rows;
        page_total = Some(response.window.total);
    } else {
        // `--find` turns "search then copy the row number" into one command: the
        // match is located first, and the page is centred on it.
        if let Some(needle) = &find {
            let needle = if match_case {
                needle.clone()
            } else {
                needle.to_lowercase()
            };
            // One pager for the scan: `window()` per chunk would rebuild the index and
            // the formulas fifty times over for a million rows.
            let scan_pager = match csvx_core::workbook::Pager::open(&input, &ops, delimiter) {
                Ok(p) => p,
                Err(e) => return report_failure("view", e.to_string()),
            };
            let mut at: u64 = 0;
            let mut seen_matches: u64 = 0;
            let found = 'scan: loop {
                let win = match scan_pager.page(at, 2_000) {
                    Ok(w) => w,
                    Err(e) => return report_failure("view", e.to_string()),
                };
                if win.rows.is_empty() {
                    break None;
                }
                for (i, row) in win.rows.iter().enumerate() {
                    let hit = row.iter().any(|cell| {
                        if find_regex {
                            // Same pattern subset as conditional formatting, so `--find`
                            // and `format --rule` agree on what a pattern means.
                            return if match_case {
                                csvx_core::format::pattern_matches(&needle, cell)
                            } else {
                                csvx_core::format::pattern_matches(&needle, &cell.to_lowercase())
                            };
                        }
                        if match_case {
                            cell.contains(&needle)
                        } else {
                            cell.to_lowercase().contains(&needle)
                        }
                    });
                    if hit {
                        seen_matches += 1;
                        if seen_matches == nth {
                            break 'scan Some(at + i as u64);
                        }
                    }
                }
                at += win.rows.len() as u64;
                if win.rows.len() < 2_000 {
                    break None;
                }
            };
            match found {
                Some(index) => {
                    start = index.saturating_sub(context);
                    if !count_given {
                        // A find should show the match and its context, not a page.
                        count = context + 1 + context;
                    }
                    highlight = Some(index);
                }
                None => {
                    let needle = find.clone().unwrap_or_default();
                    if seen_matches > 0 {
                        println!(
                            "(only {} row(s) contain \"{needle}\" — --nth {nth} is past the last one)",
                            seen_matches
                        );
                    } else {
                        println!("(no row contains \"{needle}\")");
                    }
                    return 0;
                }
            }
        }

        // One `Pager` for the whole scan: the index, column layout and compiled
        // formulas are built once instead of once per page.
        let pager = match csvx_core::workbook::Pager::open(&input, &ops, delimiter) {
            Ok(p) => p,
            Err(e) => return report_failure("view", e.to_string()),
        };
        page_total = Some(pager.total());
        columns = pager.columns().to_vec();
        match &compiled {
            // No filter: jump straight to the offset. Scanning forward from row zero
            // to reach `--start 3900000` read the whole file to show one page.
            None => {
                let win = match pager.page(start, count) {
                    Ok(w) => w,
                    Err(e) => return report_failure("view", e.to_string()),
                };
                rows = win.rows;
            }
            // A filter has to look at every row until it has enough matches, so it
            // scans — but it starts counting from zero, which is what `--start` means
            // for a filtered view: skip the first `start` matches.
            Some(set) => {
                let mut seen: u64 = 0;
                let chunk = count.max(1_000);
                loop {
                    let win = match pager.page(seen, chunk) {
                        Ok(w) => w,
                        Err(e) => return report_failure("view", e.to_string()),
                    };
                    if win.rows.is_empty() {
                        break;
                    }
                    for row in &win.rows {
                        if set.holds(row) {
                            rows.push(row.clone());
                            if rows.len() as u64 >= count {
                                break;
                            }
                        }
                    }
                    seen += win.rows.len() as u64;
                    if rows.len() as u64 >= count || win.rows.len() < chunk as usize {
                        break;
                    }
                }
            }
        }
    }

    if let Err(code) = project_columns(&mut columns, &mut rows, &shown_columns) {
        return code;
    }
    if json {
        let body = rows
            .iter()
            .map(|row| {
                let cells: Vec<String> = row.iter().map(|c| serve::json_str(c)).collect();
                format!("[{}]", cells.join(","))
            })
            .collect::<Vec<_>>()
            .join(",");
        let next_start = start + rows.len() as u64;
        let has_more = match page_total {
            Some(total) => next_start < total,
            None => rows.len() as u64 >= count,
        };
        println!(
            r#"{{"columns":[{}],"rows":[{}],"start":{},"nextStart":{},"hasMore":{}}}"#,
            columns
                .iter()
                .map(|c| serve::json_str(c))
                .collect::<Vec<_>>()
                .join(","),
            body,
            start,
            next_start,
            has_more
        );
        return 0;
    }

    if rows.is_empty() {
        // Say which emptiness this is: an offset past the end reads very differently
        // from "your filter matched nothing".
        match page_total {
            Some(0) => println!("(the file has no data rows)"),
            Some(total) if start >= total => {
                println!("(start row {start} is past the end: {total} data rows)")
            }
            Some(total) => println!("(no rows matched; the file has {total} data rows)"),
            // `--find` scans without a pager, so there is no total to quote.
            None => println!("(no rows matched)"),
        }
        return 0;
    }
    // Conditional formatting on screen: the same rules the API evaluates, applied to
    // this page. Colour when the terminal can show it, brackets otherwise.
    let marks = if rules_text.is_empty() || rows.is_empty() {
        Vec::new()
    } else {
        let mut parsed = Vec::new();
        for text in &rules_text {
            match csvx_core::format::Rule::parse(text) {
                Ok(rule) => parsed.push(rule),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        }
        let mut marks = Vec::new();
        for (offset, row) in rows.iter().enumerate() {
            let logical = start + offset as u64;
            for rule in &parsed {
                for (slot, column) in columns.iter().enumerate() {
                    let value = row.get(slot).map(String::as_str).unwrap_or("");
                    if rule.matches_cell(column, value) {
                        marks.push((logical, column.clone(), rule.style.clone()));
                    }
                }
            }
        }
        marks
    };
    if !marks.is_empty() {
        print_table_styled(&columns, &rows, start, &marks);
    } else if let Some(index) = highlight {
        // Print a marker beside the row `--find` landed on, so the eye finds it.
        let offset = index.saturating_sub(start) as usize;
        print_table(&columns, &rows);
        let which = if nth > 1 {
            format!(" (match {nth})")
        } else {
            String::new()
        };
        println!(
            "\n(^ row {} of {}{}{})",
            index + 1,
            page_total
                .map(|t| t.to_string())
                .unwrap_or_else(|| "?".into()),
            which,
            if offset < rows.len() {
                ""
            } else {
                " (outside this page)"
            }
        );
    } else {
        print_table(&columns, &rows);
    }
    let applied = journal.len();
    if let (Some(total), false) = (page_total, json) {
        println!(
            "\n(page at rows {}-{} of {})",
            start + 1,
            (start + rows.len() as u64).min(total),
            total
        );
    }
    if applied > 0 || where_expr.is_some() {
        let mut note = String::new();
        if applied > 0 {
            note.push_str(&format!("{applied} edit(s) applied"));
        }
        if let Some(expr) = &where_expr {
            if !note.is_empty() {
                note.push_str(" · ");
            }
            note.push_str(&format!("{} row(s) where {expr}", rows.len()));
        }
        println!("\n({note})");
    }
    0
}
