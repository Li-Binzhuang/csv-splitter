use crate::{print_command_help, report_failure, Parser};
use std::path::PathBuf;

/// `repair <file>` — rewrite a damaged journal down to its readable records.
///
/// Reading already skips unreadable records; repair exists so the damage can be made *tidy*
/// on purpose, with a backup, instead of being reported forever.
pub(crate) fn cmd_repair(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut dry_run = false;
    let mut json = false;
    let mut input: Option<PathBuf> = None;
    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--json" => json = true,
            "-h" | "--help" => {
                print_command_help("repair");
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
        eprintln!("error: repair needs a file");
        return 2;
    };
    let sidecar = csvx_core::workbook::Journal::sidecar(&input);
    match csvx_core::workbook::repair_journal(&sidecar, dry_run) {
        Ok(summary) => {
            if json {
                println!(
                    r#"{{"kept":{},"dropped":{},"dryRun":{},"backup":{}}}"#,
                    summary.kept,
                    summary.dropped,
                    summary.dry_run,
                    match &summary.backup {
                        Some(p) => format!("\"{}\"", p.display()),
                        None => "null".to_string(),
                    }
                );
                return 0;
            }
            println!(
                "{} readable record(s), {} unreadable",
                summary.kept, summary.dropped
            );
            if summary.dry_run {
                println!("dry run: nothing written");
            } else if let Some(backup) = &summary.backup {
                println!("backup: {}", backup.display());
            } else {
                println!("nothing to repair");
            }
            0
        }
        Err(e) => report_failure("repair", e),
    }
}
