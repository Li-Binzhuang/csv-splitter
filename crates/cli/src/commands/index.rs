use crate::{print_command_help, report_failure, Parser};
use csvx_core::index::{SparseIndex, DEFAULT_INTERVAL};
use std::path::PathBuf;
use std::time::Instant;

pub(crate) fn cmd_index(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut interval = DEFAULT_INTERVAL;
    let mut out: Option<PathBuf> = None;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--interval" => match p
                .value("--interval")
                .and_then(|v| v.parse::<u64>().map_err(|_| "bad --interval".to_string()))
            {
                Ok(v) => interval = v.max(1),
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
            "-h" | "--help" => {
                print_command_help("index");
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
        eprintln!("error: index needs an <input> path");
        return 2;
    };
    let started = Instant::now();
    let idx = match SparseIndex::build(&input, interval) {
        Ok(i) => i,
        Err(e) => {
            return report_failure("index", e);
        }
    };
    let elapsed = started.elapsed();
    let path = out.unwrap_or_else(|| SparseIndex::sidecar_path(&input));
    if let Err(e) = idx.save(&path) {
        let _ = report_failure("index", format!("cannot write {}: {e}", path.display()));
        return 1;
    }
    println!(
        "✓ {} data rows indexed in {:.2}s (interval {}, {} checkpoints, {:.1} KB) → {}",
        idx.data_rows(),
        elapsed.as_secs_f64(),
        idx.interval(),
        idx.data_rows().div_ceil(idx.interval()) + 1,
        idx.size_bytes() as f64 / 1024.0,
        path.display()
    );
    0
}
