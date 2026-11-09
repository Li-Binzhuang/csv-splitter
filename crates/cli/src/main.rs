//! Headless CLI for csv-splitter.
//!
//! The desktop app is great for one-off jobs, but servers and scripts need a
//! binary that can be piped into cron/Makefile/CI. Every subcommand here shares
//! the exact same core pipeline as the GUI, so behaviour cannot drift.

use csv_splitter_core::csv_reader::{detect_delimiter, read_headers};
use csv_splitter_core::decompress::decompress_file;
use csv_splitter_core::merge::merge_archive;
use csv_splitter_core::preview::{compute_column_stats, load_preview};
use csv_splitter_core::splitter::run_split;
use csv_splitter_core::*;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("split") => cmd_split(&args[1..]),
        Some("merge") => cmd_merge(&args[1..]),
        Some("preview") => cmd_preview(&args[1..]),
        Some("stats") => cmd_stats(&args[1..]),
        Some("extract") => cmd_extract(&args[1..]),
        Some("verify") => cmd_verify(&args[1..]),
        Some("-h" | "--help" | "help") | None => {
            print_help();
            0
        }
        Some("-V" | "--version") => {
            println!("csv-splitter {VERSION}");
            0
        }
        Some(other) => {
            eprintln!("unknown command: {other}\n");
            print_help();
            2
        }
    };
    ExitCode::from(code as u8)
}

fn print_help() {
    println!(
        r#"csv-splitter {VERSION} — split / merge / inspect large CSV files

USAGE
  csv-splitter <command> [options]

COMMANDS
  split    <input>              Split a CSV (optionally compress) into chunks
  merge    <archive|dir> -o F   Re-assemble chunks produced by `split`
  preview  <input>              Print the first rows and detected delimiter
  stats    <input>              Column statistics via reservoir sampling
  extract  <input> -o DIR       Extract any supported compressed archive
  verify   <input>              Split + merge and prove the result is lossless

COMMON OPTIONS
  -d, --delim <c>    auto | comma | tab | semicolon | pipe | <single char>   (default: auto)
  -q, --quiet        Only print errors
  -h, --help         Show help
  -V, --version      Show version

SPLIT OPTIONS
  -o, --out <path>        Output archive (default: <name>_split.tar)
  -n, --splits <n>        Number of chunks (default: 10)
  -f, --format <fmt>      zst | gz | bz2 | xz | zip | none   (default: zst)
  -l, --level <n>         Compression level (default: 3)
      --columns <a,b>     Keep only these columns (by header name)
      --columns-file <f>  One column name per line
      --sort <column>     External merge sort by column before splitting
      --desc              Sort descending
      --buf-mb <n>        Total I/O buffer budget in MB (default: 64)

MERGE OPTIONS
  -o, --out <path>        Output CSV (required)

EXAMPLES
  csv-splitter split big.csv -n 8 -f zst
  csv-splitter split big.csv --columns id,phone,score -n 4 -f none -o keep.tar
  csv-splitter merge big_split.tar -o restored.csv
  csv-splitter verify big.csv --splits 6
"#
    );
}

// ---------------------------------------------------------------- arg helpers

struct Parser {
    args: Vec<String>,
    idx: usize,
}

impl Parser {
    fn new(args: &[String]) -> Self {
        Self {
            args: args.to_vec(),
            idx: 0,
        }
    }

    fn next(&mut self) -> Option<String> {
        let v = self.args.get(self.idx).cloned();
        if v.is_some() {
            self.idx += 1;
        }
        v
    }

    fn value(&mut self, flag: &str) -> Result<String, String> {
        self.next().ok_or_else(|| format!("{flag} needs a value"))
    }
}

fn parse_delimiter(spec: &str, input: &Path) -> Result<Delimiter, String> {
    match spec.to_ascii_lowercase().as_str() {
        "" | "auto" => {
            let mut f = std::fs::File::open(input).map_err(|e| e.to_string())?;
            match detect_delimiter(&mut f) {
                Ok((d, _)) => Ok(d),
                Err(_) => Ok(Delimiter::Comma),
            }
        }
        "comma" | "," => Ok(Delimiter::Comma),
        "tab" | "\\t" => Ok(Delimiter::Tab),
        "semicolon" | ";" => Ok(Delimiter::Semicolon),
        "pipe" | "|" => Ok(Delimiter::Pipe),
        other => {
            let bytes = other.as_bytes();
            if bytes.len() == 1 {
                Ok(Delimiter::Custom(bytes[0]))
            } else {
                Err(format!("unknown delimiter: {spec}"))
            }
        }
    }
}

fn parse_format(spec: &str) -> Result<CompressFormat, String> {
    match spec.to_ascii_lowercase().as_str() {
        "zst" | "zstd" => Ok(CompressFormat::Zstd),
        "gz" | "gzip" => Ok(CompressFormat::Gzip),
        "bz2" | "bzip2" => Ok(CompressFormat::Bzip2),
        "xz" | "lzma" => Ok(CompressFormat::Xz),
        "zip" => Ok(CompressFormat::Zip),
        "none" | "raw" => Ok(CompressFormat::None),
        other => Err(format!("unknown format: {other}")),
    }
}

/// Drain progress events on a helper thread and print a compact log.
fn spawn_reporter(
    rx: mpsc::Receiver<ProgressEvent>,
    quiet: bool,
) -> thread::JoinHandle<Result<PathBuf, String>> {
    thread::spawn(move || {
        let mut output: Option<PathBuf> = None;
        let mut last_printed = 0u64;
        while let Ok(event) = rx.recv() {
            match event {
                ProgressEvent::PhaseStart { phase, .. } => {
                    if !quiet {
                        eprintln!("▶ {phase}");
                    }
                }
                ProgressEvent::PhaseDone { phase } => {
                    if !quiet {
                        eprintln!("✓ {phase}");
                    }
                }
                ProgressEvent::Info(msg) => {
                    if !quiet {
                        eprintln!("  {msg}");
                    }
                }
                ProgressEvent::Progress { bytes } => {
                    if !quiet && bytes >= last_printed + 64 * 1024 * 1024 {
                        last_printed = bytes;
                        eprint!("\r  {:.0} MB", bytes as f64 / (1024.0 * 1024.0));
                        let _ = std::io::stderr().flush();
                    }
                }
                ProgressEvent::Finished { output: p } => output = Some(p),
                ProgressEvent::Error(e) => return Err(e),
                _ => {}
            }
        }
        output.ok_or_else(|| "job finished without an output path".to_string())
    })
}

fn run_split_and_wait(config: SplitConfig, quiet: bool) -> Result<PathBuf, String> {
    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter(rx, quiet);
    run_split(config, tx);
    let result = reporter
        .join()
        .map_err(|_| "reporter thread panicked".to_string())?;
    if !quiet {
        eprintln!();
    }
    result
}

// -------------------------------------------------------------------- commands

fn cmd_split(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut splits = 10usize;
    let mut format = CompressFormat::Zstd;
    let mut level = 3;
    let mut columns: Option<String> = None;
    let mut columns_file: Option<PathBuf> = None;
    let mut sort: Option<String> = None;
    let mut desc = false;
    let mut delim = "auto".to_string();
    let mut buf_mb = 64usize;
    let mut quiet = false;

    while let Some(arg) = p.next() {
        let res: Result<(), String> = (|| {
            match arg.as_str() {
                "-o" | "--out" => out = Some(PathBuf::from(p.value("--out")?)),
                "-n" | "--splits" => {
                    splits = p
                        .value("--splits")?
                        .parse()
                        .map_err(|_| "--splits must be a number".to_string())?
                }
                "-f" | "--format" => format = parse_format(&p.value("--format")?)?,
                "-l" | "--level" => {
                    level = p
                        .value("--level")?
                        .parse()
                        .map_err(|_| "--level must be a number".to_string())?
                }
                "--columns" => columns = Some(p.value("--columns")?),
                "--columns-file" => columns_file = Some(PathBuf::from(p.value("--columns-file")?)),
                "--sort" => sort = Some(p.value("--sort")?),
                "--desc" => desc = true,
                "-d" | "--delim" => delim = p.value("--delim")?,
                "--buf-mb" => {
                    buf_mb = p
                        .value("--buf-mb")?
                        .parse()
                        .map_err(|_| "--buf-mb must be a number".to_string())?
                }
                "-q" | "--quiet" => quiet = true,
                "-h" | "--help" => {
                    print_help();
                    return Err(String::new());
                }
                other if other.starts_with('-') => return Err(format!("unknown option: {other}")),
                other => {
                    if input.is_some() {
                        return Err(format!("unexpected argument: {other}"));
                    }
                    input = Some(PathBuf::from(other));
                }
            }
            Ok(())
        })();
        if let Err(e) = res {
            if e.is_empty() {
                return 0;
            }
            eprintln!("error: {e}");
            return 2;
        }
    }

    let Some(input) = input else {
        eprintln!("error: split needs an <input> path");
        return 2;
    };
    if !input.exists() {
        eprintln!("error: input not found: {}", input.display());
        return 1;
    }

    // Materialise --columns into the temp file the core expects.
    let mut temp_columns: Option<PathBuf> = None;
    if columns.is_some() && columns_file.is_some() {
        eprintln!("error: use either --columns or --columns-file");
        return 2;
    }
    if let Some(list) = columns {
        let path = std::env::temp_dir().join(format!("csvs_cols_{}.txt", std::process::id()));
        let text = list
            .split(',')
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("\n");
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("error: cannot write columns file: {e}");
            return 1;
        }
        columns_file = Some(path.clone());
        temp_columns = Some(path);
    }

    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    if !quiet {
        eprintln!("splitting {} → {} chunk(s)", input.display(), splits);
    }

    let config = SplitConfig {
        input: input.clone(),
        output: out,
        splits,
        compress_format: format,
        compress_level: level,
        buf_mb,
        columns_file,
        delimiter,
        sort_column: sort,
        sort_descending: desc,
    };

    let result = run_split_and_wait(config, quiet);
    if let Some(p) = temp_columns {
        std::fs::remove_file(p).ok();
    }

    match result {
        Ok(path) => {
            if !quiet {
                println!("✓ wrote {}", path.display());
            }
            0
        }
        Err(e) => {
            eprintln!("✗ split failed: {e}");
            1
        }
    }
}

fn cmd_merge(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut quiet = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-q" | "--quiet" => quiet = true,
            "-h" | "--help" => {
                print_help();
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
    let reporter = spawn_reporter(rx, quiet);
    let result = merge_archive(&input, &out, &tx);
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
            eprintln!("✗ merge failed: {e}");
            1
        }
    }
}

fn cmd_preview(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut rows = 10usize;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--rows" => match p
                .value("--rows")
                .and_then(|v| v.parse().map_err(|_| "bad --rows".to_string()))
            {
                Ok(v) => rows = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_help();
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
        eprintln!("error: preview needs an <input> path");
        return 2;
    };

    let override_delim = if delim == "auto" {
        None
    } else {
        match parse_delimiter(&delim, &input) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        }
    };

    match load_preview(&input, rows, override_delim) {
        Ok(preview) => {
            println!(
                "{} — {:.1} MB, ~{} rows, delimiter {:?}",
                input.display(),
                preview.file_size as f64 / (1024.0 * 1024.0),
                preview.total_rows_estimate,
                preview.detected_delimiter
            );
            println!("{}", preview.headers.join(" | "));
            println!("{}", "-".repeat(40));
            for row in &preview.rows {
                println!("{}", row.join(" | "));
            }
            0
        }
        Err(e) => {
            eprintln!("✗ preview failed: {e}");
            1
        }
    }
}

fn cmd_stats(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut sample = 50_000usize;
    let mut top = 5usize;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
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
            "--top" => match p
                .value("--top")
                .and_then(|v| v.parse().map_err(|_| "bad --top".to_string()))
            {
                Ok(v) => top = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_help();
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
        eprintln!("error: stats needs an <input> path");
        return 2;
    };
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter(rx, true);
    let result = compute_column_stats(&input, delimiter, sample, 100_000, &tx);
    drop(tx);
    reporter.join().ok();

    match result {
        Ok(stats) => {
            println!(
                "{} column(s), sampled {} rows",
                stats.len(),
                stats.first().map(|s| s.total_sampled).unwrap_or(0)
            );
            for col in stats {
                println!(
                    "\n▸ {}  (unique≈{}, empty={})",
                    col.name, col.unique_count, col.empty_count
                );
                for (value, count) in col.top_values.iter().take(top) {
                    println!("    {count:>8}  {value}");
                }
            }
            0
        }
        Err(e) => {
            eprintln!("✗ stats failed: {e}");
            1
        }
    }
}

fn cmd_extract(args: &[String]) -> i32 {
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
                print_help();
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
    let reporter = spawn_reporter(rx, true);
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
        Err(e) => {
            eprintln!("✗ extract failed: {e}");
            1
        }
    }
}

fn cmd_verify(args: &[String]) -> i32 {
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
                print_help();
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

    let split_ok = run_split_and_wait(config, quiet);
    if let Err(e) = split_ok {
        eprintln!("✗ split failed: {e}");
        std::fs::remove_dir_all(&work).ok();
        return 1;
    }

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter(rx, true);
    let merge_result = merge_archive(&archive, &merged, &tx);
    drop(tx);
    reporter.join().ok();

    let code = match merge_result {
        Ok(report) => {
            let original = std::fs::read(&input).unwrap_or_default();
            let restored = std::fs::read(&merged).unwrap_or_default();
            if original == restored {
                println!(
                    "✓ lossless: {} bytes across {} chunk(s) round-tripped identically",
                    original.len(),
                    report.chunks
                );
                0
            } else {
                eprintln!(
                    "✗ mismatch: original {} bytes, merged {} bytes",
                    original.len(),
                    restored.len()
                );
                1
            }
        }
        Err(e) => {
            eprintln!("✗ merge failed: {e}");
            1
        }
    };

    std::fs::remove_dir_all(&work).ok();
    code
}

#[allow(dead_code)]
fn header_len(path: &Path) -> usize {
    read_headers(path, None).map(|h| h.len()).unwrap_or(0)
}
