use crate::commands::lookup::layout_columns;
use crate::commands::shared::json_escape;
use crate::commands::sort::parse_sort_columns;
use crate::{
    check_expect, check_formula, check_output, human_bytes, journal_push, parse_delimiter,
    parse_quotes, print_command_help, report_failure, spawn_reporter, warn_journal_damage, Parser,
    MAX_DELETE_RUNS,
};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_edit(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut action: Option<String> = None;
    let mut input: Option<PathBuf> = None;
    let mut row: Option<u64> = None;
    let mut key_anchor: Option<(String, String)> = None;
    let mut cols_list: Vec<String> = Vec::new();
    let mut col: Option<String> = None;
    let mut value: Option<String> = None;
    let mut from: Option<String> = None;
    let mut by: Option<String> = None;
    let mut into: Option<String> = None;
    let mut row_from: Option<u64> = None;
    let mut row_to: Option<u64> = None;
    let mut columns: Option<Vec<String>> = None;
    let mut invalid_rule: Option<String> = None;
    let mut expect_value: Option<String> = None;
    let mut find: Option<String> = None;
    let mut find_col: Option<Vec<String>> = None;
    let mut find_nth: u64 = 1;
    let mut match_case = false;
    let mut order: Option<Vec<csvx_core::sort::SortColumn>> = None;
    let mut count: Option<u64> = None;
    let mut regex = false;
    let mut name: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut where_expr: Option<String> = None;
    let mut formula_expr: Option<String> = None;
    let mut after_col: Option<String> = None;
    let mut to_block: Option<String> = None;
    let mut json = false;
    let mut dry_run = false;
    let mut delim = "auto".to_string();
    let mut ops_text: Option<String> = None;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--ops" => match p.value("--ops") {
                Ok(v) => ops_text = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--regex" => regex = true,
            // `--from`/`--to` already mean column and text; a row range needs its own
            // names, because renaming a column and filling rows are different verbs.
            "--from-row" => match p.value("--from-row") {
                Ok(v) => match v.parse() {
                    Ok(n) => row_from = Some(n),
                    Err(_) => {
                        eprintln!("error: --from-row must be a row number");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--to-row" => match p.value("--to-row") {
                Ok(v) => match v.parse() {
                    Ok(n) => row_to = Some(n),
                    Err(_) => {
                        eprintln!("error: --to-row must be a row number");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--by" => match p.value("--by") {
                Ok(v) => by = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--into" => match p.value("--into") {
                Ok(v) => into = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--after-key" | "--before-key" => match p.value(&arg) {
                Ok(v) => key_anchor = Some((arg.clone(), v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--cols" => match p.value("--cols") {
                Ok(v) => {
                    cols_list = v
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
            "--row" => match p
                .value("--row")
                .and_then(|v| v.parse::<u64>().map_err(|_| "bad --row".to_string()))
            {
                // Rows are 1-based everywhere the user sees them, the same as A1
                // references and the same as a spreadsheet. The journal stores
                // 0-based physical indices; the translation happens exactly here.
                Ok(0) => {
                    eprintln!("error: rows are 1-based; row 1 is the first data row");
                    return 2;
                }
                Ok(v) => row = Some(v - 1),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--col" => match p.value("--col") {
                Ok(v) => col = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--value" => match p.value("--value") {
                Ok(v) => value = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--from" => match p.value("--from") {
                Ok(v) => from = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--to" => match p.value("--to") {
                Ok(v) => to_block = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--count" => match p
                .value("--count")
                .and_then(|v| v.parse().map_err(|_| "bad --count".to_string()))
            {
                Ok(v) => count = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--find-col" => match p.value("--find-col") {
                Ok(v) => {
                    find_col = Some(
                        v.split(',')
                            .map(|c| c.trim().to_string())
                            .filter(|c| !c.is_empty())
                            .collect(),
                    )
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--expect" => match p.value("--expect") {
                Ok(v) => expect_value = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--invalid" => match p.value("--invalid") {
                Ok(v) => invalid_rule = Some(v),
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
            "--nth" => match p
                .value("--nth")
                .and_then(|v| v.parse().map_err(|_| "bad --nth".to_string()))
            {
                Ok(v) => find_nth = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--match-case" => match_case = true,
            "--columns" => match p.value("--columns") {
                Ok(v) => {
                    columns = Some(
                        v.split(',')
                            .map(|c| c.trim().to_string())
                            .filter(|c| !c.is_empty())
                            .collect(),
                    )
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--order" => match p.value("--order") {
                Ok(v) => match parse_sort_columns(&v, false) {
                    Ok(keys) => order = Some(keys),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                },
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
            "--formula" => match p.value("--formula") {
                Ok(v) => formula_expr = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--after" => match p.value("--after") {
                Ok(v) => after_col = Some(v),
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
            "--name" => match p.value("--name") {
                Ok(v) => name = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
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
                print_command_help("edit");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => {
                if action.is_none() {
                    action = Some(other.to_string());
                } else if input.is_none() {
                    input = Some(PathBuf::from(other));
                } else {
                    eprintln!("error: unexpected argument: {other}");
                    return 2;
                }
            }
        }
    }

    let Some(action) = action else {
        eprintln!(
            "error: edit needs an action: set | insert | delete | replace | undo | status | export | compact"
        );
        return 2;
    };
    let Some(input) = input else {
        eprintln!("error: edit needs a base file");
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
    let sidecar = csvx_core::workbook::Journal::sidecar(&input);
    if let Err(e) = csvx_core::workbook::recover_compaction(&input) {
        eprintln!("error: {e}");
        return 1;
    }
    warn_journal_damage(&input);
    let mut journal = match csvx_core::workbook::Journal::load(&sidecar) {
        Ok(j) => j,
        Err(e) => return report_failure("edit", format!("cannot read {}: {e}", sidecar.display())),
    };

    // `copy` reads a source range and records the values as a paste op: the op
    // must not point at the source, or a later edit there would rewrite history.
    if action == "copy" {
        let (Some(from), Some(to)) = (from.clone(), to_block.clone()) else {
            eprintln!("error: copy needs --from \"A2:C5\" and --to \"B10\"");
            return 2;
        };
        let columns = match layout_columns(&input) {
            Ok(c) => c,
            Err(e) => return report_failure("edit", e),
        };
        let (start_row, start_col, end_row, end_col) =
            match csvx_core::workbook::parse_a1_range(&from, &columns) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            };
        let (target_row, target_col) = match csvx_core::workbook::parse_a1(&to, &columns) {
            Ok((r, c)) => (r, c),
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        };
        let width = end_col - start_col + 1;
        let height = (end_row - start_row + 1) as usize;
        let cells = width * height;
        if cells > csvx_core::workbook::MAX_BLOCK_CELLS {
            eprintln!(
                "error: {width} × {height} is {cells} cells; the limit is {} — use `edit set --where` for a bulk change",
                csvx_core::workbook::MAX_BLOCK_CELLS
            );
            return 2;
        }
        if target_col + width > columns.len() {
            eprintln!(
                "error: the pasted block does not fit in {} columns",
                columns.len()
            );
            return 2;
        }
        let window = match csvx_core::workbook::window(
            &input,
            journal.ops(),
            start_row,
            height as u64,
            delimiter,
        ) {
            Ok(w) => w,
            Err(e) => return report_failure("edit", e),
        };
        if window.rows.len() < height {
            eprintln!(
                "error: source range needs {height} rows, the sheet has {}",
                window.rows.len()
            );
            return 2;
        }
        let mut block = Vec::with_capacity(height);
        for row in &window.rows {
            let cells: Vec<String> = (0..width)
                .map(|offset| row.get(start_col + offset).cloned().unwrap_or_default())
                .collect();
            block.push(cells);
        }
        let height_cells = block.len();
        let op = csvx_core::workbook::Op::PasteBlock {
            row: target_row,
            column: columns[target_col].clone(),
            rows: block,
        };
        let description = op.describe();
        if let Err(e) = journal_push(&mut journal, &sidecar, op) {
            return e;
        }
        println!("✓ {description} ({height_cells} × {width} block)");
        return 0;
    }

    // Actions that do not modify the journal are handled before any save.
    match action.as_str() {
        "status" => {
            let logical = csvx_core::workbook::logical_rows(&input, journal.ops()).unwrap_or(0);
            if json {
                println!(
                    r#"{{"file":"{}","ops":{},"logicalRows":{},"journal":"{}"}}"#,
                    input.display(),
                    journal.len(),
                    logical,
                    sidecar.display()
                );
            } else {
                println!("{}", input.display());
                println!(
                    "  journal: {} ({} operations)",
                    sidecar.display(),
                    journal.len()
                );
                for (i, op) in journal.ops().iter().enumerate() {
                    println!("  {:>3}. {}", i + 1, op.describe());
                }
                if journal.is_empty() {
                    println!("  (no edits yet)");
                }
                if journal.redo_len() > 0 {
                    println!(
                        "  {} operation(s) undone and available to `edit redo`",
                        journal.redo_len()
                    );
                }
            }
            return 0;
        }
        "export" => {
            let only = match &where_expr {
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
                    let columns = match csvx_core::workbook::column_layout(
                        journal.ops(),
                        &match csvx_core::workbook::base_header(&input, delimiter) {
                            Ok(c) => c,
                            Err(e) => {
                                return report_failure(
                                    "edit",
                                    format!("cannot read the header: {e}"),
                                )
                            }
                        },
                    ) {
                        Ok(layout) => layout.into_iter().map(|(name, _)| name).collect::<Vec<_>>(),
                        Err(e) => return report_failure("edit", e.to_string()),
                    };
                    match set.compile(&columns) {
                        Ok(compiled) => Some(compiled),
                        Err(e) => return report_failure("edit", e.to_string()),
                    }
                }
            };
            let output = out.unwrap_or_else(|| {
                let stem = input
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "edited".to_string());
                input.with_file_name(format!("{stem}.edited.csv"))
            });
            let (tx, rx) = mpsc::channel();
            let reporter = spawn_reporter("edit", rx, json);
            if let Err(code) = check_output(Some(&input), &output) {
                return code;
            }
            // Ordering happens on disk, and it may name a column that `--columns` does
            // not keep — "sort by amount, deliver id and city" is a normal request. So
            // when both are given the export keeps every column, the sort does its
            // work, and a final pass drops the rest. `--columns` alone is applied
            // during the export, which is the cheap path.
            let ordered = order.is_some();
            let project_after = ordered && columns.is_some();
            let staging = ordered;
            let target = if staging {
                output.with_extension("csvs-staging")
            } else {
                output.clone()
            };
            let export_columns: Option<&[String]> = if project_after {
                None
            } else {
                columns.as_deref()
            };
            let result = csvx_core::workbook::export_selected(
                &input,
                journal.ops(),
                &target,
                delimiter,
                quotes,
                csvx_core::workbook::ExportOptions {
                    only: only.as_ref(),
                    columns: export_columns,
                    cancel: None,
                },
                &tx,
            );
            drop(tx);
            let _ = reporter.join();
            let mut summary = match result {
                Ok(summary) => summary,
                Err(e) => {
                    if staging {
                        let _ = std::fs::remove_file(&target);
                    }
                    return report_failure("edit", e);
                }
            };
            // Sorting happens after the export, on the staged file: the sort command
            // already streams with bounded memory, so this adds no new machinery.
            if let Some(keys) = &order {
                let sorted = if project_after {
                    output.with_extension("csvs-sorted")
                } else {
                    output.clone()
                };
                let (stx, srx) = mpsc::channel();
                let reporter = spawn_reporter("sort", srx, true);
                let outcome = csvx_core::sort::external_sort_columns(
                    &target, &sorted, keys, delimiter, 256, &stx,
                );
                drop(stx);
                let _ = reporter.join();
                let _ = std::fs::remove_file(&target);
                if let Err(e) = outcome {
                    let _ = std::fs::remove_file(&sorted);
                    return report_failure("edit", format!("cannot order the export: {e}"));
                }
                if project_after {
                    // The file now has only the selected columns, so the summary must
                    // say so rather than repeating the staging width.
                    if let Some(selected) = &columns {
                        summary.columns = selected.clone();
                    }
                    // Drop the columns that were only there to sort by.
                    let spec = csvx_core::reshape::ReshapeSpec {
                        keep: columns.clone().unwrap_or_default(),
                        drop: Vec::new(),
                        rename: Vec::new(),
                    };
                    let (ptx, prx) = mpsc::channel();
                    let reporter = spawn_reporter("columns", prx, true);
                    let projected =
                        csvx_core::reshape::reshape(&sorted, delimiter, &spec, &output, &ptx);
                    drop(ptx);
                    let _ = reporter.join();
                    let _ = std::fs::remove_file(&sorted);
                    if let Err(e) = projected {
                        return report_failure("edit", format!("cannot select columns: {e}"));
                    }
                }
            }
            return {
                if json {
                    println!(
                        r#"{{"rows":{},"bytes":{},"columns":["{}"]}}"#,
                        summary.rows,
                        summary.bytes,
                        summary.columns.join(r#"",""#)
                    );
                } else {
                    let bytes = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
                    println!(
                        "✓ {} rows × {} columns → {} ({})",
                        summary.rows,
                        summary.columns.len(),
                        output.display(),
                        human_bytes(bytes)
                    );
                }
                0
            };
        }
        "compact" => {
            let (tx, rx) = mpsc::channel();
            let reporter = spawn_reporter("compact", rx, true);
            let result = csvx_core::workbook::compact(&input, delimiter, &tx);
            drop(tx);
            let _ = reporter.join();
            return match result {
                Ok(summary) => {
                    println!(
                        "✓ compacted {} operations → {} (archive {}); journal is now empty",
                        summary.operations,
                        human_bytes(summary.bytes),
                        summary.base_archive.display()
                    );
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            };
        }
        "undo" => {
            // A transaction boundary makes one undo take the whole batch back; the group
            // is only consumed when the journal still ends there.
            // Undo may cross a compacted boundary: put the original base and journal
            // back first, then let the normal tail-undo run against the restored stack.
            match csvx_core::workbook::restore_compacted(&input) {
                Ok(true) => eprintln!("restored the pre-compaction base and journal"),
                Ok(false) => {}
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            }
            let mut undone = 0usize;
            let mut last = String::new();
            let locked = csvx_core::workbook::Journal::update_locked(&sidecar, |fresh| {
                let count = csvx_core::workbook::take_block(&sidecar, fresh.len())?.unwrap_or(1);
                for _ in 0..count {
                    if let Some(op) = fresh.undo() {
                        undone += 1;
                        last = op.describe();
                    }
                }
                if undone > 1 {
                    // One redo should put the whole undone batch back.
                    csvx_core::workbook::record_redo_block(&sidecar, fresh.redo_len(), undone)?;
                }
                Ok(())
            });
            return match locked {
                Ok(journal) => {
                    if undone == 0 {
                        eprintln!("nothing to undo");
                        return 1;
                    }
                    if undone > 1 {
                        println!(
                            "✓ undone: {last} (+{} more in this transaction)",
                            undone - 1
                        );
                    } else {
                        println!("✓ undone: {last}");
                    }
                    if journal.redo_len() > 0 {
                        println!("  {} operation(s) can be redone", journal.redo_len());
                    }
                    0
                }
                Err(e) => report_failure("edit", format!("cannot save the journal: {e}")),
            };
        }
        "redo" => {
            let mut redone = 0usize;
            let mut last = String::new();
            let locked = csvx_core::workbook::Journal::update_locked(&sidecar, |fresh| {
                let count =
                    csvx_core::workbook::take_redo_block(&sidecar, fresh.redo_len())?.unwrap_or(1);
                for _ in 0..count {
                    if let Some(op) = fresh.redo() {
                        redone += 1;
                        last = op.describe();
                    }
                }
                Ok(())
            });
            return match locked {
                Ok(journal) => {
                    if redone == 0 {
                        eprintln!("nothing to redo");
                        if journal.undo_len() > 0 {
                            eprintln!(
                                "hint: {} operation(s) are in effect; `edit undo` takes one back",
                                journal.undo_len()
                            );
                        }
                        return 1;
                    }
                    if redone > 1 {
                        println!(
                            "✓ redone: {last} (+{} more in this transaction)",
                            redone - 1
                        );
                    } else {
                        println!("✓ redone: {last}");
                    }
                    0
                }
                Err(e) => report_failure("edit", format!("cannot save the journal: {e}")),
            };
        }
        _ => {}
    }

    // `--find` locates the row, so the caller does not have to copy a number out of a
    // viewer and risk miscounting. It is resolved once, before the action is chosen.
    let located_row: Option<u64> = match (&find, row) {
        (Some(needle), None) => {
            // `--col` is the column to *write*, not where to look: a search narrowed to
            // it would find nothing whenever the target column does not contain the
            // needle. `--find-col` is the narrow-the-search flag.
            let columns_filter: Option<Vec<String>> = find_col.clone();
            match csvx_core::workbook::find_row(
                &input,
                journal.ops(),
                needle,
                columns_filter.as_deref(),
                match_case,
                find_nth,
                delimiter,
            ) {
                Ok(Some(at)) => Some(at),
                Ok(None) => {
                    eprintln!(
                        "error: no row contains '{}'{}",
                        needle,
                        if find_nth > 1 {
                            format!(" (match {find_nth})")
                        } else {
                            String::new()
                        }
                    );
                    return 1;
                }
                Err(e) => return report_failure("edit", e),
            }
        }
        _ => None,
    };

    if dry_run && action != "transaction" {
        eprintln!("error: --dry-run is only supported by edit transaction");
        return 2;
    }
    if action == "transaction" {
        let Some(raw) = ops_text else {
            eprintln!("error: transaction needs --ops");
            return 2;
        };
        let read = csvx_core::sheet::ReadOptions { delimiter };
        let sheet = match csvx_core::sheet::Sheet::open(&input, read) {
            Ok(s) => s,
            Err(e) => return report_failure("edit", e),
        };
        let total = sheet.rows().unwrap_or(0);
        let columns = match sheet.columns() {
            Ok(c) => c,
            Err(e) => return report_failure("edit", e),
        };
        // One parser, shared with the HTTP endpoint: a syntax change cannot land on
        // one surface and be forgotten on the other.
        let ops = match csvx_core::workbook::parse_ops(&raw, total, &columns) {
            Ok(ops) => ops,
            Err(e) => {
                eprintln!("error: {e}");
                return match e.kind {
                    csvx_core::workbook::TransactionErrorKind::OutOfRange => 1,
                    _ => 2,
                };
            }
        };
        if dry_run {
            let preview = match csvx_core::ops::preview_edit(&input, &ops, total, &columns) {
                Ok(preview) => preview,
                Err(e) => return report_failure("edit", e.to_string()),
            };
            if json {
                let ranges: Vec<String> = preview
                    .affected_ranges
                    .iter()
                    .map(|(start, end)| format!("[{start},{end}]"))
                    .collect();
                let journal_hash = format!("\"{}\"", json_escape(&preview.journal_hash));
                println!(
                    r#"{{"op":"transaction-preview","operations":{},"rows":{},"rowsAfter":{},"rowDelta":{},"affectedRows":{},"affectedRanges":[{}],"ruleOperations":{},"structuralOperations":{},"columns":{},"journalBytes":{},"journalHash":{},"atomic":true,"wouldApply":{},"applied":0}}"#,
                    preview.operations,
                    preview.rows_before,
                    preview.rows_after,
                    preview.row_delta,
                    preview.affected_rows(),
                    ranges.join(","),
                    preview.rule_operations,
                    preview.structural_operations,
                    preview.columns,
                    preview.journal_bytes,
                    journal_hash,
                    preview.operations
                );
            } else {
                println!(
                    "preview: {} operation(s) valid; rows {}→{}, affected {}, journal hash {}; nothing written",
                    preview.operations,
                    preview.rows_before,
                    preview.rows_after,
                    preview.affected_label(),
                    preview.journal_hash
                );
            }
            return 0;
        }
        let applied = match (csvx_core::ops::EditApplyRequest {
            path: input.clone(),
            delimiter,
            ops,
        })
        .execute()
        {
            Ok(applied) => applied,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                eprintln!("error: another writer holds the journal; retry in a moment");
                return 1;
            }
            Err(e) => return report_failure("edit", e),
        };
        if json {
            println!(
                r#"{{"op":"transaction","operations":{},"applied":{},"atomic":true}}"#,
                applied.operations, applied.applied
            );
        } else {
            println!(
                "✓ transaction: {} operation(s), {} pending total (atomic)",
                applied.operations, applied.applied
            );
        }
        return 0;
    }

    let op = match action.as_str() {
        "set" => {
            let (Some(column), Some(value)) = (col, value) else {
                eprintln!("error: set needs --col and --value");
                return 2;
            };
            // A row located by `--find` fills in for `--row`; both are 0-based here.
            let row = row.or(located_row);
            match (row, where_expr) {
                // Bulk edit: one rule, however many rows it touches.
                (None, Some(filter)) => {
                    if let Err(e) = csvx_core::filter::FilterSpec::parse(&filter) {
                        eprintln!("error: {e}");
                        return 2;
                    }
                    csvx_core::workbook::Op::SetWhere {
                        filter,
                        column,
                        value,
                    }
                }
                (Some(row), None) => {
                    if let Some(expect) = &expect_value {
                        if let Err(code) = check_expect(&input, delimiter, row, &column, expect) {
                            return code;
                        }
                    }
                    csvx_core::workbook::Op::SetCell { row, column, value }
                }
                (Some(_), Some(_)) => {
                    eprintln!("error: set takes either --row or --where, not both");
                    return 2;
                }
                (None, None) => {
                    eprintln!("error: set needs --row <n> or --where \"column=value\"");
                    return 2;
                }
            }
        }
        "delete" => {
            // `--invalid` removes the rows a validation rule rejects: the cleanup half
            // of `validate`, as one undoable operation.
            if let Some(check) = &invalid_rule {
                if row.is_some() || where_expr.is_some() {
                    eprintln!("error: delete takes one of --row, --where or --invalid");
                    return 2;
                }
                let rule = match csvx_core::validate::Rule::parse(check) {
                    Ok(rule) => rule,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                };
                let runs = match csvx_core::validate::rows_failing(
                    &input,
                    journal.ops(),
                    std::slice::from_ref(&rule),
                    delimiter,
                ) {
                    Ok(runs) => runs,
                    Err(e) => return report_failure("edit", e),
                };
                if runs.is_empty() {
                    eprintln!("note: every row passes '{check}'; the journal is unchanged");
                    return 0;
                }
                let removed: u64 = runs.iter().map(|(_, count)| *count).sum();
                if runs.len() > MAX_DELETE_RUNS {
                    return report_failure(
                        "edit",
                        format!(
                            "'{check}' rejects {removed} row(s) in {} separate stretches (limit {MAX_DELETE_RUNS}); \
                             fix the values with `edit set --where` instead of deleting them",
                            runs.len()
                        ),
                    );
                }
                let op = csvx_core::workbook::Op::DeleteRanges { runs };
                if let Err(e) = journal_push(&mut journal, &sidecar, op) {
                    return e;
                }
                if json {
                    println!(r#"{{"op":"dels","rows":{removed}"}}"#);
                } else {
                    println!(
                        "✓ deleted {removed} row(s) failing '{check}' ({} operation(s) pending)",
                        journal.len()
                    );
                }
                return 0;
            }
            // `--where` deletes everything that matches, as journal ranges.
            if let Some(filter) = &where_expr {
                if row.is_some() {
                    eprintln!("error: delete takes either --row or --where, not both");
                    return 2;
                }
                let runs = match csvx_core::workbook::runs_matching(
                    &input,
                    journal.ops(),
                    filter,
                    delimiter,
                ) {
                    Ok(runs) => runs,
                    Err(e) => return report_failure("edit", e),
                };
                if runs.is_empty() {
                    eprintln!("note: nothing matches '{filter}'; the journal is unchanged");
                    return 0;
                }
                if runs.len() > MAX_DELETE_RUNS {
                    eprintln!(
                        "error: '{filter}' matches {} separate stretches (limit {MAX_DELETE_RUNS}); \
                         delete them one condition at a time, or use `filter` to export the rows you want instead",
                        runs.len()
                    );
                    return 2;
                }
                // One operation, so one `edit undo` brings back every deleted row.
                let op = csvx_core::workbook::Op::DeleteRanges { runs: runs.clone() };
                if let Err(e) = journal_push(&mut journal, &sidecar, op) {
                    return e;
                }
                let removed: u64 = runs.iter().map(|(_, count)| *count).sum();
                if json {
                    println!(
                        r#"{{"op":"delete","runs":{},"rows":{removed},"journal":"{}"}}"#,
                        runs.len(),
                        sidecar.display()
                    );
                } else {
                    println!(
                        "✓ deleted {removed} row(s) matching '{filter}' ({} operation(s) pending)",
                        journal.len()
                    );
                }
                return 0;
            }
            let row = row.or(located_row);
            let (Some(row), Some(count)) = (row, count) else {
                eprintln!("error: delete needs --row and --count");
                return 2;
            };
            csvx_core::workbook::Op::DeleteRows { at: row, count }
        }
        "replace" => {
            let (Some(from), Some(to)) = (from.clone(), to_block.clone()) else {
                eprintln!("error: replace needs --from and --to");
                return 2;
            };
            if regex {
                if csvx_core::format::pattern_is_valid(&from).is_none() {
                    eprintln!(
                        "error: '{from}' is not a supported pattern (literals, ., [..], *, +, ?, ^, $, \\d \\w \\s)"
                    );
                    return 2;
                }
                return match journal_push(
                    &mut journal,
                    &sidecar,
                    csvx_core::workbook::Op::SubstituteRegex {
                        column: col.clone(),
                        pattern: from,
                        to,
                    },
                ) {
                    Ok(code) => code,
                    Err(code) => return code,
                };
            }
            csvx_core::workbook::Op::Substitute {
                column: col,
                from,
                to,
            }
        }
        "insert" => {
            let raw = value.clone().unwrap_or_default();
            let cells: Vec<String> = if raw.trim().is_empty() {
                Vec::new()
            } else {
                csvx_core::csv_reader::parse_csv_record(&raw, b',')
            };
            // A key anchor wins over `--row`: it names a row that still exists, while a
            // row number is a position other writes may have moved.
            if let Some((flag, key)) = key_anchor.clone() {
                let position = match csvx_core::workbook::position_of_key(
                    &input,
                    journal.ops(),
                    delimiter,
                    &key,
                ) {
                    Ok(Some(position)) => position,
                    Ok(None) => {
                        eprintln!("error: no row with key {key} any more");
                        return 2;
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                };
                let at = if flag == "--after-key" {
                    position + 1
                } else {
                    position
                };
                csvx_core::workbook::Op::InsertRows {
                    at,
                    rows: vec![cells],
                }
            } else {
                let Some(row) = row else {
                    eprintln!("error: insert needs --row or --after-key/--before-key");
                    return 2;
                };
                csvx_core::workbook::Op::InsertRows {
                    at: row,
                    rows: vec![cells],
                }
            }
        }
        "formula" => {
            let (Some(name), Some(value)) = (name, value) else {
                eprintln!("error: formula needs --name and --value");
                return 2;
            };
            if let Err(code) = check_formula(&input, delimiter, journal.ops(), &value) {
                return code;
            }
            csvx_core::workbook::Op::AddColumn {
                name,
                formula: value,
            }
        }
        "set-formula" | "setformula" => {
            let (Some(target), Some(expression)) = (col.clone(), value.clone()) else {
                eprintln!("error: set-formula needs --col and --value");
                return 2;
            };
            let columns = match csvx_core::workbook::base_header(&input, delimiter)
                .and_then(|header| csvx_core::workbook::column_layout(journal.ops(), &header))
            {
                Ok(layout) => layout.into_iter().map(|(name, _)| name).collect::<Vec<_>>(),
                Err(e) => {
                    eprintln!("error: cannot read the header: {e}");
                    return 2;
                }
            };
            if let Err(message) =
                csvx_core::workbook::check_formula(&expression, &columns, Some(&target))
            {
                eprintln!("error: {message}");
                return 2;
            }
            csvx_core::workbook::Op::SetColumnFormula {
                column: target,
                formula: expression,
            }
        }
        "insert-column" | "insertcolumn" => {
            let Some(name) = name else {
                eprintln!("error: insert-column needs --name");
                return 2;
            };
            // `--value` is a constant, `--formula` an expression. Exactly one.
            let source = match (value, formula_expr) {
                (Some(v), None) => csvx_core::workbook::ColumnSource::Literal(v),
                (None, Some(f)) => csvx_core::workbook::ColumnSource::Formula(f),
                (Some(_), Some(_)) => {
                    eprintln!("error: insert-column takes --value or --formula, not both");
                    return 2;
                }
                (None, None) => {
                    eprintln!("error: insert-column needs --value <text> or --formula <expr>");
                    return 2;
                }
            };
            csvx_core::workbook::Op::AddColumnAt {
                after: after_col,
                name,
                source,
            }
        }
        "fill" => {
            let (Some(from), Some(to)) = (row_from, row_to) else {
                eprintln!("error: fill needs --from-row and --to-row");
                return 2;
            };
            if from == 0 || to == 0 || from > to {
                eprintln!(
                    "error: --from-row/--to-row are 1-based data rows, and from must be <= to"
                );
                return 2;
            }
            let text = value.unwrap_or_default();
            // `--cols a,b` fills several columns as one operation, so one undo covers the
            // whole gesture; `--col` stays the single-column form.
            if !cols_list.is_empty() {
                csvx_core::workbook::Op::FillColumns {
                    columns: cols_list.clone(),
                    from: from - 1,
                    to: to - 1,
                    value: text,
                }
            } else {
                let Some(column) = col else {
                    eprintln!("error: fill needs --col or --cols");
                    return 2;
                };
                csvx_core::workbook::Op::FillRange {
                    column,
                    from: from - 1,
                    to: to - 1,
                    value: text,
                }
            }
        }

        "split-column" | "splitcolumn" => {
            let (Some(column), Some(by), Some(into)) = (col.clone(), by, into) else {
                eprintln!("error: split-column needs --col, --by and --into");
                return 2;
            };
            let names: Vec<String> = into
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if names.len() < 2 {
                eprintln!("error: --into needs at least two names, e.g. --into \"first,last\"");
                return 2;
            }
            csvx_core::workbook::Op::SplitColumn {
                column,
                by,
                into: names,
            }
        }
        "drop" => {
            let Some(name) = name else {
                eprintln!("error: drop needs --name");
                return 2;
            };
            csvx_core::workbook::Op::DropColumn { name }
        }
        "rename" => {
            let (Some(from), Some(to)) = (from, to_block) else {
                eprintln!("error: rename needs --from and --to");
                return 2;
            };
            csvx_core::workbook::Op::RenameColumn { from, to }
        }
        other => {
            eprintln!("error: unknown edit action '{other}'");
            return 2;
        }
    };

    if let Err(e) = journal_push(&mut journal, &sidecar, op.clone()) {
        return e;
    }
    if json {
        println!(
            r#"{{"op":"{}","ops":{},"journal":"{}"}}"#,
            op.tag(),
            journal.len(),
            sidecar.display()
        );
    } else {
        println!(
            "✓ {} ({} operation(s) pending)",
            op.describe(),
            journal.len()
        );
        println!("  journal: {}", sidecar.display());
    }
    0
}
