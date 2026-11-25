use crate::commands::dedupe::files_differ;
use crate::{
    parse_delimiter, print_command_help, report_failure, run_split_and_wait, spawn_reporter, Parser,
};
use csvx_core::types::{CompressFormat, SplitConfig};
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_verify(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut splits = 5usize;
    let mut quiet = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "-n" | "--splits" => match p
                .value("--splits")
                .and_then(|v| v.parse().map_err(|_| "bad --splits".to_string()))
            {
                Ok(v) => splits = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-q" | "--quiet" => quiet = true,
            "-h" | "--help" => {
                print_command_help("verify");
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
        eprintln!("error: verify needs an <input> path");
        return 2;
    };

    let work = std::env::temp_dir().join(format!(
        "csvs_verify_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    if let Err(e) = std::fs::create_dir_all(&work) {
        eprintln!("error: cannot create {}: {e}", work.display());
        return 1;
    }

    let archive = work.join("verify.tar");
    let merged = work.join("merged.csv");
    let config = SplitConfig {
        input: input.clone(),
        output: Some(archive.clone()),
        splits,
        max_bytes: None,
        compress_format: CompressFormat::Zstd,
        compress_level: 3,
        buf_mb: 8,
        columns_file: None,
        delimiter: match parse_delimiter("auto", &input) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        },
        sort_column: None,
        sort_descending: false,
    };

    let split_ok = run_split_and_wait(config, 1, quiet);
    if split_ok.is_err() {
        std::fs::remove_dir_all(&work).ok();
        // The reporter already printed the reason.
        return 1;
    }

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("verify", rx, true);
    let merge_result = csvx_core::merge::merge_archive(&archive, &merged, &tx);
    drop(tx);
    reporter.join().ok();

    let code = match merge_result {
        Ok(report) => {
            let bytes = std::fs::metadata(&input).map(|m| m.len()).unwrap_or(0);
            match files_differ(&input, &merged) {
                Ok(None) => {
                    println!(
                        "✓ lossless: {bytes} bytes across {} chunk(s) round-tripped identically",
                        report.chunks
                    );
                    0
                }
                Ok(Some((at, left, right))) => {
                    eprintln!(
                        "✗ mismatch at byte {at}: original {left} bytes, merged {right} bytes"
                    );
                    1
                }
                Err(e) => report_failure("verify", format!("cannot compare the files: {e}")),
            }
        }
        Err(e) => report_failure("analyze", e), // engine returned the error itself
    };

    std::fs::remove_dir_all(&work).ok();
    code
}
