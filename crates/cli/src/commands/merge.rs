use crate::{print_command_help, spawn_reporter, Parser};
use csvx_core::merge::merge_archive_expecting;
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_merge(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut quiet = false;
    let mut expect_chunks: Option<usize> = None;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--chunks" => match p.value("--chunks") {
                Ok(v) => match v.parse::<usize>() {
                    Ok(n) if n > 0 => expect_chunks = Some(n),
                    _ => {
                        eprintln!("error: --chunks needs a positive number");
                        return 2;
                    }
                },
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
            "-q" | "--quiet" => quiet = true,
            "-h" | "--help" => {
                print_command_help("merge");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => input = Some(PathBuf::from(other)),
        }
    }

    let (Some(input), Some(out)) = (input, out) else {
        eprintln!("error: merge needs <archive|dir> and -o <output.csv>");
        return 2;
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("merge", rx, quiet);
    let result = merge_archive_expecting(&input, &out, expect_chunks, &tx);
    drop(tx);
    reporter.join().ok();

    match result {
        Ok(report) => {
            if !quiet {
                println!(
                    "✓ merged {} chunk(s), {} records, {:.1} MB → {}",
                    report.chunks,
                    report.records,
                    report.bytes as f64 / (1024.0 * 1024.0),
                    report.output.display()
                );
            }
            0
        }
        Err(e) => {
            // The reporter only prints events the callee sends; merge returns an error
            // without one, so staying silent would leave exit 1 with no explanation.
            eprintln!("error: {e}");
            1
        }
    }
}
