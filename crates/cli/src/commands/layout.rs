use crate::commands::lookup::layout_columns;
use crate::{print_command_help, report_failure, warn_pending_edits, Parser};
use std::path::PathBuf;

pub(crate) fn cmd_layout(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut settings: Vec<String> = Vec::new();
    let mut freeze_first: Option<usize> = None;
    let mut json = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--set" => match p.value("--set") {
                Ok(v) => settings.push(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--freeze-first" => match p.value("--freeze-first").and_then(|v| {
                v.parse()
                    .map_err(|_| format!("--freeze-first takes a count, e.g. 2, not {v:?}"))
            }) {
                Ok(v) => freeze_first = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "-h" | "--help" => {
                print_command_help("layout");
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
        eprintln!("error: layout needs an <input> file");
        return 2;
    };
    warn_pending_edits(&input);
    let mut layout = match csvx_core::layout::Layout::load(&input) {
        Ok(l) => l,
        Err(e) => return report_failure("layout", format!("cannot read the layout: {e}")),
    };

    // `--set amount:format=#,##0.00` — one property per invocation, repeatable.
    for setting in &settings {
        let Some((column, property)) = setting.split_once(':') else {
            eprintln!("error: --set needs <column>:<key>=<value> (got '{setting}')");
            return 2;
        };
        let Some((key, value)) = property.split_once('=') else {
            eprintln!("error: --set needs <column>:<key>=<value> (got '{setting}')");
            return 2;
        };
        if let Err(e) = layout.set(column.trim(), key.trim(), value.trim()) {
            eprintln!("error: {e}");
            return 2;
        }
    }
    // Resolve columns first: `--freeze-first` needs to know what they are.
    let all_columns = match layout_columns(&input) {
        Ok(c) => c,
        Err(e) => return report_failure("layout", e),
    };
    if let Some(count) = freeze_first {
        if let Err(e) = layout.set_frozen_prefix(&all_columns, count) {
            eprintln!("error: {e}");
            return 2;
        }
    }
    if !settings.is_empty() || freeze_first.is_some() {
        if let Err(e) = layout.save(&input) {
            return report_failure("layout", format!("cannot save the layout: {e}"));
        }
    }

    let columns = all_columns;
    let resolved = layout.resolve(&columns);

    if json {
        let items: Vec<String> = resolved
            .iter()
            .zip(&columns)
            .map(|(meta, original)| meta.to_json(original))
            .collect();
        println!(r#"{{"columns":[{}]}}"#, items.join(","));
    } else {
        println!("{}", input.display());
        for (meta, original) in resolved.iter().zip(&columns) {
            let name = meta
                .display_name
                .clone()
                .unwrap_or_else(|| original.clone());
            let width = meta.width.map(|w| format!("{w}px")).unwrap_or_default();
            let frozen = if meta.frozen { " frozen" } else { "" };
            println!(
                "  {:<16} width {:<6} format {:<12}{frozen}",
                name,
                if width.is_empty() {
                    "-".to_string()
                } else {
                    width
                },
                meta.format.name()
            );
        }
        if layout.is_empty() {
            println!("  (no layout set; use --set column:key=value)");
        }
    }
    0
}
