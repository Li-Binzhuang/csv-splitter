use crate::{
    human_bytes, out_path_or_default, parse_delimiter, print_command_help, report_failure,
    spawn_reporter, Parser,
};
use csvx_core::types::CompressFormat;
use std::path::PathBuf;
use std::sync::mpsc;

pub(crate) fn cmd_partition(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut by: Option<String> = None;
    let mut format = "none".to_string();
    let mut level = 3i32;
    let mut buf_mb = 64usize;
    let mut max_partitions = csvx_core::partition::DEFAULT_MAX_PARTITIONS;
    let mut verify = false;
    let mut delim = "auto".to_string();
    let mut quiet = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--by" => match p.value("--by") {
                Ok(v) => by = Some(v),
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
            "-f" | "--format" => match p.value("--format") {
                Ok(v) => format = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--level" => match p
                .value("--level")
                .and_then(|v| v.parse().map_err(|_| "bad --level".to_string()))
            {
                Ok(v) => level = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--buf-mb" => match p
                .value("--buf-mb")
                .and_then(|v| v.parse().map_err(|_| "bad --buf-mb".to_string()))
            {
                Ok(v) => buf_mb = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--verify" => verify = true,
            "--max-partitions" => match p
                .value("--max-partitions")
                .and_then(|v| v.parse().map_err(|_| "bad --max-partitions".to_string()))
            {
                Ok(v) => max_partitions = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-q" | "--quiet" => quiet = true,
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("partition");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => input = Some(PathBuf::from(other)),
        }
    }

    let (Some(input), Some(by)) = (input, by) else {
        eprintln!("error: partition needs <input> and --by <column>");
        return 2;
    };
    let compress_format = match format.as_str() {
        "none" | "raw" => CompressFormat::None,
        "zst" | "zstd" => CompressFormat::Zstd,
        "gz" | "gzip" => CompressFormat::Gzip,
        "bz2" => CompressFormat::Bzip2,
        "xz" => CompressFormat::Xz,
        "zip" => CompressFormat::Zip,
        other => {
            eprintln!("error: unknown format '{other}'");
            return 2;
        }
    };
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    // Remember where the archive will land: the engine reports success by
    // writing the file, not by returning a value.
    let expected = match &out {
        Some(path) => path.clone(),
        None => match out_path_or_default(&input, &by, compress_format) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        },
    };
    let config = csvx_core::partition::PartitionConfig {
        input: input.clone(),
        output: out,
        column: by.clone(),
        compress_format,
        compress_level: level,
        buf_mb,
        delimiter,
        max_partitions,
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("partition", rx, quiet);
    csvx_core::partition::run_partition(config, tx);
    let _ = reporter.join();

    if expected.exists() {
        let bytes = std::fs::metadata(&expected).map(|m| m.len()).unwrap_or(0);
        println!(
            "✓ partitioned by {by} → {} ({})",
            expected.display(),
            human_bytes(bytes)
        );
        if verify {
            // The claim: every source row lands in exactly one partition, and no
            // partition is ragged. Unpack and count, because an archive's size
            // says nothing about its contents.
            let tmp = match csvx_core::tempdir::TempDir::new("partition-verify") {
                Ok(t) => t,
                Err(e) => {
                    return report_failure("partition", format!("cannot create a temp dir: {e}"))
                }
            };
            let (tx, _rx) = mpsc::channel();
            let files = match csvx_core::decompress::decompress_file(&expected, tmp.path(), &tx) {
                Ok(f) => f,
                Err(e) => {
                    return report_failure(
                        "partition",
                        format!("cannot unpack {}: {e}", expected.display()),
                    )
                }
            };
            let source = match csvx_core::check::count(&input, delimiter, &tx) {
                Ok(c) => c,
                Err(e) => {
                    return report_failure("partition", format!("cannot read the source: {e}"))
                }
            };
            // Members may themselves be compressed (`-f zst`), so each one is
            // unpacked to plain CSV before counting. Counting a .csv.zst directly
            // reads binary noise and reports nonsense row counts.
            let mut total = 0u64;
            for (index, file) in files.iter().enumerate() {
                let plain = match csvx_core::decompress::decompress_to_csv(file, tmp.path(), &tx) {
                    Ok(p) => p,
                    Err(e) => {
                        return report_failure(
                            "partition",
                            format!("cannot unpack {}: {e}", file.display()),
                        )
                    }
                };
                match csvx_core::check::count(&plain, delimiter, &tx) {
                    Ok(c) => total += c.rows,
                    Err(e) => {
                        return report_failure(
                            "partition",
                            format!("cannot read {}: {e}", plain.display()),
                        )
                    }
                }
                // The unpacked copy shares a temp dir; drop it so a big archive
                // does not double its footprint on disk.
                if index + 1 < files.len() {
                    std::fs::remove_file(&plain).ok();
                }
            }
            if total != source.rows {
                return report_failure(
                    "partition",
                    format!(
                        "self-check failed: {} partition(s) hold {total} rows, the source has {}",
                        files.len(),
                        source.rows
                    ),
                );
            }
            if !quiet {
                println!(
                    "  ✓ checked: {} partitions hold {} rows (source {} rows × {} columns)",
                    files.len(),
                    total,
                    source.rows,
                    source.columns
                );
            }
        }
        0
    } else {
        report_failure("partition", "the archive was not written")
    }
}
