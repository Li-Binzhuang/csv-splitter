use crate::{print_command_help, report_failure, spawn_reporter, Parser};
use csvx_core::decompress::decompress_file;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

pub(crate) fn cmd_extract(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("extract");
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
        eprintln!("error: extract needs an <input> path");
        return 2;
    };
    let out = out.unwrap_or_else(|| {
        input.parent().unwrap_or(Path::new(".")).join(format!(
            "{}_extracted",
            input
                .file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        ))
    });
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("error: cannot create {}: {e}", out.display());
        return 1;
    }

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("extract", rx, true);
    let result = decompress_file(&input, &out, &tx);
    drop(tx);
    reporter.join().ok();

    match result {
        Ok(files) => {
            println!("✓ extracted {} file(s) → {}", files.len(), out.display());
            for f in files {
                println!("    {}", f.display());
            }
            0
        }
        Err(e) => report_failure("extract", e),
    }
}
