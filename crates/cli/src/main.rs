//! Headless CLI for csv-splitter.
//!
//! The desktop app is great for one-off jobs, but servers and scripts need a
//! binary that can be piped into cron/Makefile/CI. Every subcommand here shares
//! the exact same core pipeline as the GUI, so behaviour cannot drift.

use csv_splitter_core::csv_reader::{detect_delimiter, parse_csv_record, read_headers};
use csv_splitter_core::decompress::decompress_file;
use csv_splitter_core::encoding::{self, parse_name, TextEncoding};
use csv_splitter_core::filter::{self, scan as filter_scan, FilterSpec};
use csv_splitter_core::index::{SparseIndex, DEFAULT_INTERVAL};
use csv_splitter_core::merge::merge_archive;
use csv_splitter_core::partition::{run_partition, PartitionConfig};
use csv_splitter_core::preview::{compute_column_stats, load_preview};
use csv_splitter_core::search::{search as search_scan, SearchSpec};
use csv_splitter_core::sort::SortColumn;
use csv_splitter_core::splitter::run_split_threads;
use csv_splitter_core::sql::{self, DEFAULT_LIMIT};
use csv_splitter_core::*;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

mod serve;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("split") => cmd_split(&args[1..]),
        Some("merge") => cmd_merge(&args[1..]),
        Some("preview") => cmd_preview(&args[1..]),
        Some("index") => cmd_index(&args[1..]),
        Some("serve") => cmd_serve(&args[1..]),
        Some("convert") => cmd_convert(&args[1..]),
        Some("filter") => cmd_filter(&args[1..]),
        Some("sort") => cmd_sort(&args[1..]),
        Some("analyze") => cmd_analyze(&args[1..]),
        Some("dedupe") => cmd_dedupe(&args[1..]),
        Some("columns") => cmd_columns(&args[1..]),
        Some("partition") => cmd_partition(&args[1..]),
        Some("concat") => cmd_concat(&args[1..]),
        Some("groupby") => cmd_groupby(&args[1..]),
        Some("demo") => cmd_demo(&args[1..]),
        Some("sql") => cmd_sql(&args[1..]),
        Some("search") => cmd_search(&args[1..]),
        Some("files") => cmd_files(&args[1..]),
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
  index    <input>              Build a sparse row index for instant paging
  serve    --root <dir>          Local HTTP API (JSON) for a web front-end
  convert  <input>               Transcode a CSV to UTF-8 (GBK / UTF-16 input)
  filter   <input> --where EXPR  Row numbers matching a column predicate
  sort     <input> --by COLS -o F Sort the whole file (a,b or a:desc,b:asc)
  analyze  <input>               Per-column profile: empties, distinct, numeric summary
  dedupe   <input> -o F          Drop duplicate rows (whole row or --by columns)
  columns  <input>                Keep/drop/rename columns (-o writes the result)
  partition <input> --by COL      One file per distinct value of COL (tar/zip)
  concat   <a.csv> <b.csv> ...    Glue same-shaped files into one (header checked)
  groupby  <input> --by K --agg A Group and aggregate into a CSV
  demo     [-o <dir>]           Write a sample file to try the commands on
  search   <input> <text>        Find text anywhere (or in chosen columns)
  files    --root <dir>          List readable files under a directory
  sql      <input> "<query>"     Run SQL over the file (DuckDB, read-only)
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
      --by-size <size>    Split by target chunk size instead (e.g. 100MB, 1.5GB)
      --by-column <name>  One file per distinct value of <name> (e.g. city)
  -f, --format <fmt>      zst | gz | bz2 | xz | zip | none   (default: zst)
  -l, --level <n>         Compression level (default: 3)
      --columns <a,b>     Keep only these columns (by header name)
      --columns-file <f>  One column name per line
      --sort <column>     External merge sort by column before splitting
      --desc              Descending, for columns without an explicit :asc/:desc
      --buf-mb <n>        Total I/O buffer budget in MB (default: 64)
  -t, --threads <n>       Reader threads (default: 1; try 4 on big files)

MERGE OPTIONS
  -o, --out <path>        Output CSV (required)

SQL OPTIONS
      --limit <n>         Max rows returned (default: 1000)
      --json              Emit JSON instead of a table
  The table is registered as `t`, e.g.:
      SELECT city, count(*) n FROM t GROUP BY 1 ORDER BY n DESC

SEARCH OPTIONS
      --columns <a,b>     Only look in these columns (default: all)
      --case              Case-sensitive
      --exact             Match the whole cell, not a substring
      --limit <n>         Stop after n matches (default: 1000)
      --show <n>          Also print the first n matching rows
      --json              Emit JSON
  -d, --delim <c>         Delimiter (default: auto)

GROUPBY OPTIONS
      --by <a,b>          Key columns (required)
      --agg <c:op,...>    Aggregates: column:count|sum|mean|min|max (required)
      --order <col>       Output column to sort by (default: the keys)
      --desc              Sort descending
      --max-groups <n>    Refuse past n distinct groups (default: 1000000)
  -o, --out <file>        Output file (default: <input>.grouped.csv)
      --json              Emit JSON
  -d, --delim <c>         Delimiter (default: auto)

CONCAT OPTIONS
      --ignore-header     Append even when headers differ (reported, but done)
  -o, --out <file>        Output file (default: combined.csv next to the input)
      --json              Emit JSON
  -d, --delim <c>         Delimiter (default: auto)

PARTITION OPTIONS
      --by <col>          Column whose values become file names (required)
  -o, --out <file.tar>    Archive path (format follows -f: tar, or zip)
  -f, --format <fmt>      Per-part compression: none|zst|gz|bz2|xz|zip (default: none)
      --max-partitions <n> Refuse to create more than n files (default: 512)

COLUMNS OPTIONS
      --keep <a,b>        Only these columns, in this order
      --drop <a,b>        Remove these columns
      --rename <a:b,c:d>  Rename columns (old:new)
  -o, --out <file>        Output file (default: <input>.reshaped.csv)
      --verify            Re-read the output and check rows and columns
      --json              Emit JSON
  -d, --delim <c>         Delimiter (default: auto)

ANALYZE OPTIONS
      --distinct-cap <n>  Stop counting distinct values per column at n (default: 100000)
      --json              Emit JSON
  -d, --delim <c>         Delimiter (default: auto)

DEDUPE OPTIONS
      --by <a,b>          Columns that define a duplicate (default: the whole row)
  -o, --out <file>        Output file (default: <input>.deduped.csv)
      --max-keys <n>      Refuse to run past n distinct rows (default: 100000000)
      --verify            Re-read the output and check the kept/removed counts
      --json              Emit JSON
  -d, --delim <c>         Delimiter (default: auto)

FILES OPTIONS
      --root <dir>        Directory to list (required)
      --ext <csv,tsv,...> Extensions to include (default: csv,tsv,txt,gz,zst,zip)

FILTER OPTIONS
      --where <expr>      Predicate, repeatable (=, !=, ~, >, >=, <, <=, empty,
                          notempty, ~re:PATTERN, "col in a,b,c")
      --any               OR the predicates instead of AND
  -o, --out <file>        Write matching rows to a CSV instead of listing row numbers
      --verify            Re-read the output and check it holds exactly the matches
      --limit <n>         Stop after n matches (default: 10000)
      --show <n>          Also print the first n matching rows
      --json              Emit JSON instead of text
  -d, --delim <c>         Delimiter (default: auto)

CONVERT OPTIONS
  -o, --out <path>        Output file (default: <name>.utf8.csv)
      --from <enc>        auto | utf-8 | gbk | utf-16le | utf-16be  (default: auto)
      --to <enc>          utf-8 | utf-8-bom                        (default: utf-8)

SERVE OPTIONS
  --root <dir>            Directory the API may read (required)
  --addr <ip:port>        Listen address (default: 127.0.0.1:8787)

INDEX OPTIONS
  --interval <n>          Checkpoint every n records (default: 1000)
  -o, --out <path>        Index path (default: <input>.idx)

PREVIEW OPTIONS
  --rows <n>              How many rows to print (default: 10)
  --start-row <n>         Jump to this data row (0-based) using the row index;
                          the index is loaded from <input>.idx or built once

NEW HERE? / 刚上手？
  csv-splitter demo             Creates ./csv-splitter-demo/orders.csv to play with
  csv-splitter preview csv-splitter-demo/orders.csv

EXAMPLES
  csv-splitter split big.csv -n 8 -f zst
  csv-splitter split huge.csv --by-size 100MB -f zip
  csv-splitter split orders.csv --by-column city -f zst
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

/// Parse human sizes: `512k`, `100MB`, `1.5G`, `1048576`.
/// Decimal units (KB/MB/GB = 1000^n) and binary units (KiB/MiB/GiB = 1024^n)
/// are both accepted, plus the bare `K/M/G` shorthand which means binary.
fn parse_size(spec: &str) -> Result<u64, String> {
    let text = spec.trim().to_ascii_lowercase();
    if text.is_empty() {
        return Err("--by-size needs a value".to_string());
    }
    let units: &[(&str, u64)] = &[
        ("gib", 1u64 << 30),
        ("mib", 1u64 << 20),
        ("kib", 1u64 << 10),
        ("gb", 1_000_000_000),
        ("mb", 1_000_000),
        ("kb", 1_000),
        ("g", 1u64 << 30),
        ("m", 1u64 << 20),
        ("k", 1u64 << 10),
        ("b", 1),
    ];
    let (number, multiplier) = units
        .iter()
        .find_map(|(suffix, mult)| text.strip_suffix(suffix).map(|n| (n, *mult)))
        .unwrap_or((text.as_str(), 1));
    let value: f64 = number
        .trim()
        .parse()
        .map_err(|_| format!("bad size: {spec}"))?;
    if value <= 0.0 {
        return Err(format!("size must be positive: {spec}"));
    }
    Ok((value * multiplier as f64) as u64)
}

fn human_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", human_bytes(bytes_per_sec as u64))
}

/// Remaining time from elapsed time and completed fraction; `?` until the
/// estimate is meaningful (first 1% is pure noise).
fn human_eta(elapsed: f64, frac: f64) -> String {
    if frac < 0.01 {
        return "--:--".to_string();
    }
    let remaining = elapsed * (1.0 - frac) / frac;
    let secs = remaining.round() as u64;
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
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
    command: &'static str,
    rx: mpsc::Receiver<ProgressEvent>,
    quiet: bool,
) -> thread::JoinHandle<Result<PathBuf, String>> {
    thread::spawn(move || {
        let mut output: Option<PathBuf> = None;
        let mut phase_start = Instant::now();
        let mut phase_total = 0u64;
        let mut last_print = Instant::now();
        let mut progress_shown = false;
        while let Ok(event) = rx.recv() {
            match event {
                ProgressEvent::PhaseStart { phase, total_bytes } => {
                    phase_start = Instant::now();
                    phase_total = total_bytes;
                    if !quiet {
                        eprintln!("▶ {phase}");
                    }
                }
                ProgressEvent::PhaseDone { phase } => {
                    if !quiet {
                        if progress_shown {
                            eprintln!();
                            progress_shown = false;
                        }
                        eprintln!("✓ {phase}");
                    }
                }
                ProgressEvent::Info(msg) => {
                    if !quiet {
                        eprintln!("  {msg}");
                    }
                }
                ProgressEvent::Progress { bytes } => {
                    // Throttle to 4 lines/second: a per-MB print floods CI logs.
                    if !quiet
                        && phase_total > 0
                        && last_print.elapsed() >= Duration::from_millis(250)
                    {
                        last_print = Instant::now();
                        let elapsed = phase_start.elapsed().as_secs_f64().max(0.001);
                        let frac = (bytes as f64 / phase_total as f64).clamp(0.0, 1.0);
                        let rate = bytes as f64 / elapsed;
                        eprint!(
                            "\r  {:>3.0}% · {} · ETA {}",
                            frac * 100.0,
                            human_rate(rate),
                            human_eta(elapsed, frac)
                        );
                        progress_shown = true;
                        let _ = std::io::stderr().flush();
                    }
                }
                ProgressEvent::Finished { output: p } => output = Some(p),
                ProgressEvent::Error(e) => {
                    // Report here, unconditionally: most callers ignore the
                    // reporter's Result, and a swallowed error leaves the user
                    // staring at "see messages above" with nothing above.
                    // The shape matches every other failure in the CLI.
                    match hint_for(&e) {
                        Some(hint) => {
                            eprintln!("✗ {command}: {e}");
                            eprintln!("  hint: {hint}");
                        }
                        None => eprintln!("✗ {command}: {e}"),
                    }
                    return Err(e);
                }
                _ => {}
            }
        }
        output.ok_or_else(|| format!("{command} finished without producing an output"))
    })
}

/// Run the opted-in self-check and report it, returning false when it fails.
/// Print a failure in the one format the whole CLI uses, and return exit code 1.
///
/// `✗ <command>: <reason>` on the first line and an optional `hint:` line under
/// it. Having exactly one shape means a script (or a person scrolling back) can
/// find every failure with the same pattern, and the hint has a defined place
/// instead of being smuggled into the message.
fn fail(command: &str, reason: impl std::fmt::Display) -> i32 {
    eprintln!("✗ {command}: {reason}");
    1
}

/// Same as [`fail`], with a suggested next step underneath.
fn fail_with_hint(command: &str, reason: impl std::fmt::Display, hint: &str) -> i32 {
    eprintln!("✗ {command}: {reason}");
    eprintln!("  hint: {hint}");
    1
}

/// Turn a known engine error into an actionable hint.
fn hint_for(error: &str) -> Option<&'static str> {
    let lower = error.to_ascii_lowercase();
    if lower.contains("no such file or directory") {
        Some("check the path; `csv-splitter files --root DIR` lists what is readable")
    } else if lower.contains("permission denied") {
        Some("the file or its directory is not readable by this user")
    } else if lower.contains("utf-16") {
        Some("convert it first: csv-splitter convert FILE -o utf8.csv")
    } else if lower.contains("not found") && lower.contains("column") {
        Some("run `csv-splitter analyze FILE` to see the exact column names")
    } else if lower.contains("more than") && lower.contains("groups") {
        Some("group by a lower-cardinality column, or raise --max-groups")
    } else if lower.contains("distinct rows") {
        Some("dedupe by a column (--by) instead of the whole row, or raise --max-keys")
    } else if lower.contains("outside root") {
        Some("paths must stay inside the directory passed to --root")
    } else {
        None
    }
}

/// The standard tail of a command that reports engine progress.
fn report_failure(command: &str, reason: impl std::fmt::Display) -> i32 {
    let text = reason.to_string();
    match hint_for(&text) {
        Some(hint) => fail_with_hint(command, &text, hint),
        None => fail(command, text),
    }
}

fn self_check(
    result: &Path,
    delimiter: Delimiter,
    expectation: csv_splitter_core::check::Expectation,
    quiet: bool,
) -> bool {
    let (tx, _rx) = mpsc::channel();
    match csv_splitter_core::check::check(result, delimiter, &expectation, &tx) {
        Ok(report) if report.ok() => {
            if !quiet {
                println!(
                    "  ✓ checked: {} rows, {} columns",
                    report.counts.rows, report.counts.columns
                );
            }
            true
        }
        Ok(report) => {
            eprintln!("✗ self-check failed:");
            for p in &report.problems {
                eprintln!("    {p}");
            }
            false
        }
        Err(e) => {
            eprintln!("✗ self-check could not read the result: {e}");
            false
        }
    }
}

fn run_split_and_wait(config: SplitConfig, threads: usize, quiet: bool) -> Result<PathBuf, String> {
    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("split", rx, quiet);
    run_split_threads(config, threads, tx);
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
    let mut by_size: Option<u64> = None;
    let mut by_column: Option<String> = None;
    let mut format = CompressFormat::Zstd;
    let mut level = 3;
    let mut columns: Option<String> = None;
    let mut columns_file: Option<PathBuf> = None;
    let mut sort: Option<String> = None;
    let mut desc = false;
    let mut delim = "auto".to_string();
    let mut buf_mb = 64usize;
    let mut threads = 1usize;
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
                "--by-size" => by_size = Some(parse_size(&p.value("--by-size")?)?),
                "--by-column" => by_column = Some(p.value("--by-column")?),
                "--columns" => columns = Some(p.value("--columns")?),
                "--columns-file" => columns_file = Some(PathBuf::from(p.value("--columns-file")?)),
                "--sort" => sort = Some(p.value("--sort")?),
                "--desc" => desc = true,
                "-d" | "--delim" => delim = p.value("--delim")?,
                "-t" | "--threads" => {
                    threads = p
                        .value("--threads")?
                        .parse()
                        .map_err(|_| "--threads must be a number".to_string())?
                }
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
    if by_column.is_some() && by_size.is_some() {
        eprintln!("error: --by-column and --by-size are mutually exclusive");
        return 2;
    }
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
        // Unique per invocation: two concurrent runs in one process (tests) were
        // overwriting each other's column list.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("csvs_cols_{}_{}.txt", std::process::id(), unique));
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

    if let Some(column) = by_column {
        if let Some(p) = temp_columns {
            std::fs::remove_file(p).ok();
        }
        if !quiet {
            eprintln!("partitioning {} by column '{}'", input.display(), column);
        }
        let config = PartitionConfig {
            input,
            output: out,
            column,
            compress_format: format,
            compress_level: level,
            buf_mb,
            delimiter,
            max_partitions: 512,
        };
        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("partition", rx, quiet);
        run_partition(config, tx);
        return match reporter
            .join()
            .map_err(|_| "reporter thread panicked".to_string())
        {
            Ok(Ok(path)) => {
                if !quiet {
                    println!("✓ wrote {}", path.display());
                }
                0
            }
            // The reporter already printed the reason with the standard shape.
            Ok(Err(_)) => 1,
            Err(_) => 1,
        };
    }

    if !quiet {
        match by_size {
            Some(bytes) => eprintln!(
                "splitting {} → target {} per chunk",
                input.display(),
                human_bytes(bytes)
            ),
            None => eprintln!("splitting {} → {} chunk(s)", input.display(), splits),
        }
    }

    let config = SplitConfig {
        input: input.clone(),
        output: out,
        splits,
        max_bytes: by_size,
        compress_format: format,
        compress_level: level,
        buf_mb,
        columns_file,
        delimiter,
        sort_column: sort,
        sort_descending: desc,
    };

    let result = run_split_and_wait(config, threads.max(1), quiet);
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
        Err(_) => 1, // the reporter already printed the reason
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
    let reporter = spawn_reporter("merge", rx, quiet);
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
        Err(_) => 1, // the reporter already printed the reason
    }
}

fn cmd_preview(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut rows = 10usize;
    let mut delim = "auto".to_string();
    let mut start_row: u64 = 0;
    let mut index_path: Option<PathBuf> = None;

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
            "--start-row" => match p
                .value("--start-row")
                .and_then(|v| v.parse().map_err(|_| "bad --start-row".to_string()))
            {
                Ok(v) => start_row = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--index" => match p.value("--index") {
                Ok(v) => index_path = Some(PathBuf::from(v)),
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

    if start_row > 0 {
        return preview_from_index(&input, rows, start_row, override_delim, index_path);
    }

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
        Err(e) => report_failure("preview", e),
    }
}

/// Print a window of rows by seeking into the file through the sparse index.
fn preview_from_index(
    input: &Path,
    rows: usize,
    start_row: u64,
    delim: Option<Delimiter>,
    index_path: Option<PathBuf>,
) -> i32 {
    let sidecar = index_path.unwrap_or_else(|| SparseIndex::sidecar_path(input));
    let started = Instant::now();
    let idx = match SparseIndex::load(&sidecar) {
        Ok(i) if i.matches(input) => i,
        _ => match SparseIndex::build(input, DEFAULT_INTERVAL) {
            Ok(i) => {
                i.save(&sidecar).ok();
                i
            }
            Err(e) => {
                let _ = report_failure("index", format!("cannot build the index: {e}"));
                return 1;
            }
        },
    };
    let delim_byte = match delim {
        Some(d) => d.byte(),
        None => detect_delimiter(&mut std::fs::File::open(input).unwrap())
            .map(|(d, _)| d.byte())
            .unwrap_or(b','),
    };

    let header = idx.read_header(input).unwrap_or_default();
    println!(
        "{} — {} data rows, index ready in {:?} (interval {}), from row {}",
        input.display(),
        idx.data_rows(),
        started.elapsed(),
        idx.interval(),
        start_row
    );
    println!("{}", String::from_utf8_lossy(&header).trim_end());
    println!("{}", "-".repeat(40));

    match idx.read_rows(input, start_row, rows as u64) {
        Ok(found) => {
            for (i, raw) in found.iter().enumerate() {
                let text = String::from_utf8_lossy(raw);
                let fields = parse_csv_record(text.trim_end(), delim_byte);
                println!("{:>9}  {}", start_row + i as u64 + 1, fields.join(" | "));
            }
            0
        }
        Err(e) => {
            let _ = report_failure("preview", format!("reading rows: {e}"));
            1
        }
    }
}

fn cmd_index(args: &[String]) -> i32 {
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

fn cmd_sql(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut query: Option<String> = None;
    let mut limit = DEFAULT_LIMIT;
    let mut json = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--limit" => match p
                .value("--limit")
                .and_then(|v| v.parse().map_err(|_| "bad --limit".to_string()))
            {
                Ok(v) => limit = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "-h" | "--help" => {
                print_help();
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => {
                if input.is_none() {
                    input = Some(PathBuf::from(other));
                } else if query.is_none() {
                    query = Some(other.to_string());
                } else {
                    eprintln!("error: unexpected argument: {other}");
                    return 2;
                }
            }
        }
    }

    let (Some(input), Some(query)) = (input, query) else {
        eprintln!("error: sql needs <input> and a query");
        return 2;
    };

    let started = Instant::now();
    let result = match sql::query(&input, &query, limit) {
        Ok(r) => r,
        Err(e) => {
            return report_failure("sql", e);
        }
    };

    if json {
        let rows: Vec<String> = result
            .rows
            .iter()
            .map(|row| {
                let cells: Vec<String> = row.iter().map(|c| json_quote(c)).collect();
                format!("[{}]", cells.join(","))
            })
            .collect();
        let cols: Vec<String> = result.columns.iter().map(|c| json_quote(c)).collect();
        println!(
            r#"{{"columns":[{}],"rows":[{}],"truncated":{}}}"#,
            cols.join(","),
            rows.join(","),
            result.truncated
        );
        return 0;
    }

    println!("{}", result.columns.join(" | "));
    println!("{}", "-".repeat(40));
    for row in &result.rows {
        println!("{}", row.join(" | "));
    }
    println!(
        "{} row(s){} in {:.2}s",
        result.rows.len(),
        if result.truncated { " (truncated)" } else { "" },
        started.elapsed().as_secs_f64()
    );
    0
}

fn json_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn cmd_filter(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut where_exprs: Vec<String> = Vec::new();
    let mut combine = filter::Combine::All;
    let mut out: Option<PathBuf> = None;
    let mut verify = false;
    let mut limit = 10_000usize;
    let mut json = false;
    let mut show = 0usize;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--where" => match p.value("--where") {
                Ok(v) => where_exprs.push(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--any" => combine = filter::Combine::Any,
            "--verify" => verify = true,
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--limit" => match p
                .value("--limit")
                .and_then(|v| v.parse().map_err(|_| "bad --limit".to_string()))
            {
                Ok(v) => limit = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "--show" => match p
                .value("--show")
                .and_then(|v| v.parse().map_err(|_| "bad --show".to_string()))
            {
                Ok(v) => show = v,
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
        eprintln!("error: filter needs <input>");
        return 2;
    };
    if where_exprs.is_empty() {
        eprintln!("error: filter needs at least one --where <expr>");
        return 2;
    }
    let mut specs = Vec::with_capacity(where_exprs.len());
    for expr in &where_exprs {
        match FilterSpec::parse(expr) {
            Ok(s) => specs.push(s),
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        }
    }
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let set = filter::FilterSet { specs, combine };

    // `--out` is the "keep the result" path: stream matches to a file.
    if let Some(out) = out {
        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("filter", rx, true);
        let result = filter::extract(&input, delimiter, &set, &out, &tx);
        drop(tx);
        let _ = reporter.join();
        return match result {
            Ok(s) => {
                println!(
                    "✓ {} of {} rows matched → {} ({} bytes)",
                    s.matched,
                    s.scanned,
                    out.display(),
                    s.bytes
                );
                if verify {
                    // The claim: exactly the matched rows, same columns, and
                    // no raggedness beyond what the source already had.
                    let ok = self_check(
                        &out,
                        delimiter,
                        csv_splitter_core::check::Expectation::rows(s.matched)
                            .with_ragged(s.matched_ragged),
                        false,
                    );
                    if !ok {
                        return 1;
                    }
                }
                0
            }
            Err(e) => report_failure("filter", e),
        };
    }

    let (tx, _rx) = mpsc::channel();
    let spec = set.specs.first().cloned();
    let Some(spec) = spec else { return 2 };
    let result = match filter_scan(&input, delimiter, &spec, limit, &tx) {
        Ok(r) => r,
        Err(e) => {
            return report_failure("filter", e);
        }
    };

    if json {
        let rows: Vec<String> = result.row_numbers().iter().map(|n| n.to_string()).collect();
        println!(
            r#"{{"scanned":{},"matched":{},"truncated":{},"rows":[{}]}}"#,
            result.scanned,
            result.matched,
            result.truncated,
            rows.join(",")
        );
        return 0;
    }

    println!(
        "scanned {} rows, matched {} ({} shown){}",
        result.scanned,
        result.matched,
        result.rows.len(),
        if result.truncated { ", truncated" } else { "" }
    );
    let shown: Vec<String> = result
        .row_numbers()
        .iter()
        .take(50)
        .map(|n| n.to_string())
        .collect();
    println!("rows (1-based, header excluded): {}", shown.join(", "));
    if result.matched > 50 {
        println!(
            "... {} more; use --json for the full list",
            result.matched - 50
        );
    }
    if show > 0 {
        let picked = &result.rows[..result.rows.len().min(show)];
        match csv_splitter_core::filter::read_rows(&input, delimiter, picked) {
            Ok(rows) => {
                println!();
                for r in rows {
                    println!("{:>9}  {}", r.row + 1, r.cells.join(" | "));
                }
            }
            Err(e) => {
                let _ = report_failure("filter", format!("reading matched rows: {e}"));
                return 1;
            }
        }
    }
    0
}

fn cmd_search(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut query: Option<String> = None;
    let mut columns: Vec<String> = Vec::new();
    let mut case_sensitive = false;
    let mut exact = false;
    let mut limit = 1_000usize;
    let mut show = 0usize;
    let mut json = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--columns" => match p.value("--columns") {
                Ok(v) => columns = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--case" => case_sensitive = true,
            "--exact" => exact = true,
            "--limit" => match p
                .value("--limit")
                .and_then(|v| v.parse().map_err(|_| "bad --limit".to_string()))
            {
                Ok(v) => limit = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--show" => match p
                .value("--show")
                .and_then(|v| v.parse().map_err(|_| "bad --show".to_string()))
            {
                Ok(v) => show = v,
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
                print_help();
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => {
                if input.is_none() {
                    input = Some(PathBuf::from(other));
                } else if query.is_none() {
                    query = Some(other.to_string());
                } else {
                    eprintln!("error: unexpected argument: {other}");
                    return 2;
                }
            }
        }
    }

    let (Some(input), Some(query)) = (input, query) else {
        eprintln!("error: search needs <input> and <text>");
        return 2;
    };
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let mut spec = SearchSpec::new(query);
    spec.columns = columns;
    spec.case_sensitive = case_sensitive;
    spec.whole_cell = exact;

    let (tx, _rx) = mpsc::channel();
    let result = match search_scan(&input, delimiter, &spec, limit, &tx) {
        Ok(r) => r,
        Err(e) => {
            return report_failure("search", e);
        }
    };

    if json {
        let rows: Vec<String> = result.row_numbers().iter().map(|n| n.to_string()).collect();
        println!(
            r#"{{"scanned":{},"matched":{},"truncated":{},"columns":["{}"],"rows":[{}]}}"#,
            result.scanned,
            result.matched,
            result.truncated,
            result.columns.join(r#"",""#),
            rows.join(",")
        );
        return 0;
    }

    println!(
        "scanned {} rows, matched {} ({} shown){}",
        result.scanned,
        result.matched,
        result.rows.len(),
        if result.truncated { ", truncated" } else { "" }
    );
    let shown: Vec<String> = result
        .row_numbers()
        .iter()
        .take(50)
        .map(|n| n.to_string())
        .collect();
    println!("rows (1-based, header excluded): {}", shown.join(", "));

    if show > 0 {
        let picked = &result.rows[..result.rows.len().min(show)];
        match csv_splitter_core::filter::read_rows(&input, delimiter, picked) {
            Ok(rows) => {
                println!();
                for r in rows {
                    println!("{:>9}  {}", r.row + 1, r.cells.join(" | "));
                }
            }
            Err(e) => {
                return report_failure("search", format!("reading matched rows: {e}"));
            }
        }
    }
    0
}

fn cmd_files(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    let mut exts = "csv,tsv,txt,gz,zst,zip,bz2,xz".to_string();
    let mut json = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--root" => match p.value("--root") {
                Ok(v) => root = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--ext" => match p.value("--ext") {
                Ok(v) => exts = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "-h" | "--help" => {
                print_help();
                return 0;
            }
            other => root = Some(PathBuf::from(other)),
        }
    }

    let Some(root) = root else {
        eprintln!("error: files needs --root <dir>");
        return 2;
    };
    let listing = match csv_splitter_core::listing::list(&root, &exts, 200) {
        Ok(l) => l,
        Err(e) => {
            return report_failure("files", e);
        }
    };

    if json {
        let items: Vec<String> = listing
            .files
            .iter()
            .map(|f| {
                format!(
                    r#"{{"path":"{}","bytes":{},"modified":{}}}"#,
                    f.relative.replace('"', "\\\""),
                    f.bytes,
                    f.modified_secs
                )
            })
            .collect();
        println!(
            r#"{{"root":"{}","truncated":{},"files":[{}]}}"#,
            listing.root.display(),
            listing.truncated,
            items.join(",")
        );
        return 0;
    }

    println!(
        "{} file(s) under {}{}",
        listing.files.len(),
        listing.root.display(),
        if listing.truncated {
            " (truncated)"
        } else {
            ""
        }
    );
    for f in listing.files {
        println!("{:>12}  {}", human_bytes(f.bytes), f.relative);
    }
    0
}

fn cmd_convert(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut from = "auto".to_string();
    let mut to = "utf-8".to_string();
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
            "--from" => match p.value("--from") {
                Ok(v) => from = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--to" => match p.value("--to") {
                Ok(v) => to = v,
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
        eprintln!("error: convert needs an <input> path");
        return 2;
    };

    let from_enc = match from.as_str() {
        "auto" | "" => None,
        other => match parse_name(other) {
            Some(e) => Some(e),
            None => {
                eprintln!("error: unknown encoding: {other}");
                return 2;
            }
        },
    };
    let to_enc: TextEncoding = match to.to_ascii_lowercase().as_str() {
        "utf-8" | "utf8" => TextEncoding::Utf8 { bom: false },
        "utf-8-bom" | "utf8-bom" | "bom" => TextEncoding::Utf8 { bom: true },
        other => {
            eprintln!("error: --to only supports utf-8 or utf-8-bom (got {other})");
            return 2;
        }
    };

    let output = out.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "output".to_string());
        input.with_file_name(format!("{stem}.utf8.csv"))
    });

    let started = Instant::now();
    match encoding::transcode(&input, &output, from_enc, to_enc, 4 * 1024 * 1024) {
        Ok(bytes) => {
            if !quiet {
                println!(
                    "✓ {} bytes → {} ({}) in {:.2}s",
                    bytes,
                    output.display(),
                    if from_enc.is_some() {
                        "forced"
                    } else {
                        "detected"
                    },
                    started.elapsed().as_secs_f64()
                );
            }
            0
        }
        Err(_) => 1, // the reporter already printed the reason
    }
}

fn cmd_serve(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    let mut addr = "127.0.0.1:8787".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--root" => match p.value("--root") {
                Ok(v) => root = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--addr" => match p.value("--addr") {
                Ok(v) => addr = v,
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
            other => root = Some(PathBuf::from(other)),
        }
    }

    let Some(root) = root else {
        eprintln!("error: serve needs --root <dir>");
        return 2;
    };
    match serve::run(serve::ServerConfig { addr, root }) {
        Ok(()) => 0,
        Err(_) => 1, // the reporter already printed the reason
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
    let reporter = spawn_reporter("stats", rx, true);
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
        Err(_) => 1, // the reporter already printed the reason
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
        Err(_) => 1, // the reporter already printed the reason
    }
}

fn cmd_sort(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut by: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut descending = false;
    let mut verify = false;
    let mut buf_mb = 64usize;
    let mut delim = "auto".to_string();
    let mut quiet = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--by" | "-b" => match p.value("--by") {
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
            "--desc" => descending = true,
            "--asc" => descending = false,
            "--verify" => verify = true,
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
            "-q" | "--quiet" => quiet = true,
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

    let (Some(input), Some(by)) = (input, by) else {
        eprintln!("error: sort needs <input> and --by <column>");
        return 2;
    };
    let out = match out {
        Some(o) => o,
        None => {
            let stem = input
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "sorted".to_string());
            let ext = input
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_else(|| "csv".to_string());
            input.with_file_name(format!("{stem}.sorted.{ext}"))
        }
    };
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    // `--by a,b` sorts by both; `--by a:desc,b:asc` sets direction per column.
    let columns: Vec<SortColumn> = match parse_sort_columns(&by, descending) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("sort", rx, quiet);
    let result = csv_splitter_core::sort::external_sort_columns(
        &input, &out, &columns, delimiter, buf_mb, &tx,
    );
    drop(tx);
    let _ = reporter.join();
    match result {
        Ok(()) => {
            let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            let described = columns
                .iter()
                .map(|c| format!("{} {}", c.name, if c.descending { "desc" } else { "asc" }))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "✓ sorted by {described} → {} ({})",
                out.display(),
                human_bytes(bytes)
            );
            if verify {
                // Sorting promises "same rows, reordered": check the counts on
                // both sides rather than trusting the merge step.
                let (tx, _rx) = mpsc::channel();
                let before = match csv_splitter_core::check::count(&input, delimiter, &tx) {
                    Ok(c) => c,
                    Err(e) => {
                        return report_failure("sort", format!("cannot read the source: {e}"))
                    }
                };
                let after = match csv_splitter_core::check::count(&out, delimiter, &tx) {
                    Ok(c) => c,
                    Err(e) => {
                        return report_failure("sort", format!("cannot read the result: {e}"))
                    }
                };
                if before.rows != after.rows || before.columns != after.columns {
                    let _ = report_failure(
                        "sort",
                        format!(
                            "self-check failed: {} rows × {} columns became {} rows × {} columns",
                            before.rows, before.columns, after.rows, after.columns
                        ),
                    );
                    return 1;
                }
                if !quiet {
                    println!(
                        "  ✓ checked: {} rows, {} columns",
                        after.rows, after.columns
                    );
                }
            }
            0
        }
        Err(_) => 1, // the reporter already printed the reason
    }
}

/// `a` / `a,b` / `a:desc,b:asc`. A bare `--desc` sets the default direction.
fn parse_sort_columns(spec: &str, default_descending: bool) -> Result<Vec<SortColumn>, String> {
    let mut columns = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, descending) = match part.split_once(':') {
            Some((n, dir)) => match dir.trim().to_ascii_lowercase().as_str() {
                "asc" | "a" => (n.trim().to_string(), false),
                "desc" | "d" => (n.trim().to_string(), true),
                other => return Err(format!("unknown sort direction '{other}' (use asc/desc)")),
            },
            None => (part.to_string(), default_descending),
        };
        if name.is_empty() {
            return Err("empty sort column".to_string());
        }
        columns.push(SortColumn::new(name, descending));
    }
    if columns.is_empty() {
        return Err("no sort columns".to_string());
    }
    Ok(columns)
}

fn cmd_demo(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
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
            other => out = Some(PathBuf::from(other)),
        }
    }

    let dir = out.unwrap_or_else(|| PathBuf::from("csv-splitter-demo"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("error: cannot create {}: {e}", dir.display());
        return 1;
    }
    let path = dir.join("orders.csv");
    let bytes = csv_splitter_core::demo::write_sample(&path);
    match bytes {
        Ok((rows, bytes)) => {
            println!(
                "✓ wrote {} ({rows} data rows, {})",
                path.display(),
                human_bytes(bytes)
            );
            println!();
            println!("  Try the 10-minute tour:");
            println!("    csv-splitter preview {}", path.display());
            println!("    csv-splitter analyze {}", path.display());
            println!("    csv-splitter filter {} --where \"city=CDMX\" --where \"amount>1000\" -o cdmx.csv", path.display());
            println!("    csv-splitter groupby {} --by city --agg \"amount:sum,amount:count\" --order sum_amount --desc", path.display());
            println!();
            println!("  Or serve it to a front-end:");
            println!("    csv-splitter serve --root {}", dir.display());
            0
        }
        Err(_) => 1, // the reporter already printed the reason
    }
}

fn cmd_groupby(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut keys: Vec<String> = Vec::new();
    let mut agg_spec = String::new();
    let mut order: Option<String> = None;
    let mut descending = false;
    let mut max_groups = csv_splitter_core::groupby::DEFAULT_MAX_GROUPS;
    let mut json = false;
    let mut delim = "auto".to_string();
    let mut quiet = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--by" => match p.value("--by") {
                Ok(v) => keys = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--agg" => match p.value("--agg") {
                Ok(v) => agg_spec = v,
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
            "--desc" => descending = true,
            "--asc" => descending = false,
            "--max-groups" => match p
                .value("--max-groups")
                .and_then(|v| v.parse().map_err(|_| "bad --max-groups".to_string()))
            {
                Ok(v) => max_groups = v,
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
            "-q" | "--quiet" => quiet = true,
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
        eprintln!("error: groupby needs <input>");
        return 2;
    };
    if keys.is_empty() {
        eprintln!("error: groupby needs --by <col[,col]>");
        return 2;
    }
    let aggs = match csv_splitter_core::groupby::AggSpec::parse_list(&agg_spec) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let output = out.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "grouped".to_string());
        input.with_file_name(format!("{stem}.grouped.csv"))
    });
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("groupby", rx, quiet || json);
    let options = csv_splitter_core::groupby::GroupOptions {
        keys,
        aggregates: aggs,
        order_by: order,
        descending,
        max_groups,
    };
    let result = csv_splitter_core::groupby::groupby(&input, delimiter, &output, &options, &tx);
    drop(tx);
    let _ = reporter.join();
    match result {
        Ok(s) => {
            if json {
                println!(
                    r#"{{"rows":{},"groups":{},"bytes":{},"keyColumns":["{}"],"aggregates":["{}"]}}"#,
                    s.rows,
                    s.groups,
                    s.bytes,
                    s.key_columns.join(r#"",""#),
                    s.aggregates.join(r#"",""#)
                );
            } else {
                println!(
                    "✓ {} rows → {} groups → {} ({})",
                    s.rows,
                    s.groups,
                    output.display(),
                    human_bytes(s.bytes)
                );
            }
            0
        }
        Err(_) => 1, // the reporter already printed the reason
    }
}

fn cmd_concat(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut ignore_header = false;
    let mut json = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--ignore-header" => ignore_header = true,
            "--json" => json = true,
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
            other => inputs.push(PathBuf::from(other)),
        }
    }

    if inputs.len() < 2 {
        eprintln!("error: concat needs at least two <input> files");
        return 2;
    }
    let output = out.unwrap_or_else(|| inputs[0].with_file_name("combined.csv"));
    let delimiter = match parse_delimiter(&delim, &inputs[0]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("concat", rx, json);
    let result = csv_splitter_core::concat::concat(&inputs, &output, delimiter, ignore_header, &tx);
    drop(tx);
    let _ = reporter.join();
    match result {
        Ok(s) => {
            if json {
                println!(
                    r#"{{"files":{},"rows":{},"bytes":{},"columns":["{}"],"mismatched":{}}}"#,
                    s.files,
                    s.rows,
                    s.bytes,
                    s.columns.join(r#"",""#),
                    s.mismatched.len()
                );
            } else {
                println!(
                    "✓ {} files, {} rows → {} ({})",
                    s.files,
                    s.rows,
                    output.display(),
                    human_bytes(s.bytes)
                );
                for f in &s.mismatched {
                    eprintln!("  ⚠ header differs, appended anyway: {}", f.display());
                }
            }
            0
        }
        Err(_) => 1, // the reporter already printed the reason
    }
}

fn cmd_partition(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut by: Option<String> = None;
    let mut format = "none".to_string();
    let mut level = 3i32;
    let mut buf_mb = 64usize;
    let mut max_partitions = csv_splitter_core::partition::DEFAULT_MAX_PARTITIONS;
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
    let config = csv_splitter_core::partition::PartitionConfig {
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
    csv_splitter_core::partition::run_partition(config, tx);
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
            let tmp = match csv_splitter_core::tempdir::TempDir::new("partition-verify") {
                Ok(t) => t,
                Err(e) => {
                    return report_failure("partition", format!("cannot create a temp dir: {e}"))
                }
            };
            let (tx, _rx) = mpsc::channel();
            let files =
                match csv_splitter_core::decompress::decompress_file(&expected, tmp.path(), &tx) {
                    Ok(f) => f,
                    Err(e) => {
                        return report_failure(
                            "partition",
                            format!("cannot unpack {}: {e}", expected.display()),
                        )
                    }
                };
            let source = match csv_splitter_core::check::count(&input, delimiter, &tx) {
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
                let plain =
                    match csv_splitter_core::decompress::decompress_to_csv(file, tmp.path(), &tx) {
                        Ok(p) => p,
                        Err(e) => {
                            return report_failure(
                                "partition",
                                format!("cannot unpack {}: {e}", file.display()),
                            )
                        }
                    };
                match csv_splitter_core::check::count(&plain, delimiter, &tx) {
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

fn out_path_or_default(input: &Path, by: &str, format: CompressFormat) -> Result<PathBuf, String> {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .ok_or_else(|| "input has no file stem".to_string())?;
    let ext = if format == CompressFormat::Zip {
        "zip"
    } else {
        "tar"
    };
    Ok(input.with_file_name(format!(
        "{stem}_by_{}.{ext}",
        csv_splitter_core::partition::sanitize(by)
    )))
}

fn cmd_analyze(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut distinct_cap = csv_splitter_core::profile::DEFAULT_DISTINCT_CAP;
    let mut json = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--distinct-cap" => match p
                .value("--distinct-cap")
                .and_then(|v| v.parse().map_err(|_| "bad --distinct-cap".to_string()))
            {
                Ok(v) => distinct_cap = v,
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
        eprintln!("error: analyze needs <input>");
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
    let reporter = spawn_reporter("analyze", rx, true);
    let result = csv_splitter_core::profile::profile(&input, delimiter, distinct_cap, &tx);
    drop(tx);
    let _ = reporter.join();
    let profiles = match result {
        Ok(p) => p,
        Err(_) => {
            // The reporter already printed the reason.
            return 1;
        }
    };

    if json {
        let cols: Vec<String> = profiles.iter().map(profile_json).collect();
        println!(r#"{{"columns":[{}]}}"#, cols.join(","));
        return 0;
    }

    if profiles.is_empty() {
        println!("(file has no columns: it is empty or contains only whitespace)");
        return 0;
    }
    // Ragged records are the first thing worth knowing about a foreign file.
    if let Some(first) = profiles.first() {
        if first.ragged > 0 {
            println!(
                "⚠ {} of {} rows have a field count different from the header — columns may be shifted on those rows",
                first.ragged, first.rows
            );
        }
    }
    println!(
        "{:<20} {:>9} {:>8} {:>9}  summary",
        "column", "rows", "empty", "distinct"
    );
    for c in &profiles {
        let distinct = if c.distinct_truncated {
            format!("≥{}", c.distinct)
        } else {
            c.distinct.to_string()
        };
        let summary = match &c.numeric {
            Some(n) => format!(
                "min {:.2}  max {:.2}  mean {:.2}  median {:.2}  sum {:.2}",
                n.min,
                n.max,
                n.mean,
                n.median.unwrap_or(f64::NAN),
                n.sum
            ),
            None => {
                let top = c
                    .top
                    .iter()
                    .take(3)
                    .map(|(v, n)| format!("{v} ({n})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                if c.mixed_types {
                    format!("text (mixed with numbers): {top}")
                } else {
                    format!("text: {top}")
                }
            }
        };
        println!(
            "{:<20} {:>9} {:>8} {:>9}  {}",
            c.name, c.rows, c.empty, distinct, summary
        );
    }
    0
}

fn profile_json(c: &csv_splitter_core::profile::ColumnProfile) -> String {
    let numeric = match &c.numeric {
        Some(n) => format!(
            r#"{{"count":{},"min":{},"max":{},"sum":{},"mean":{},"median":{}}}"#,
            n.count,
            n.min,
            n.max,
            n.sum,
            n.mean,
            n.median
                .map(|m| m.to_string())
                .unwrap_or_else(|| "null".to_string())
        ),
        None => "null".to_string(),
    };
    let top: Vec<String> = c
        .top
        .iter()
        .map(|(v, n)| format!(r#"["{}",{}]"#, json_quote(v).trim_matches('"'), n))
        .collect();
    format!(
        r#"{{"name":"{}","rows":{},"ragged":{},"empty":{},"nonEmpty":{},"distinct":{},"distinctTruncated":{},"numeric":{},"mixedTypes":{},"top":[{}]}}"#,
        json_quote(&c.name).trim_matches('"'),
        c.rows,
        c.ragged,
        c.empty,
        c.non_empty,
        c.distinct,
        c.distinct_truncated,
        numeric,
        c.mixed_types,
        top.join(",")
    )
}

fn cmd_columns(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut keep: Vec<String> = Vec::new();
    let mut drop_cols: Vec<String> = Vec::new();
    let mut rename: Vec<(String, String)> = Vec::new();
    let mut json = false;
    let mut verify = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--verify" => verify = true,
            "--keep" => match p.value("--keep") {
                Ok(v) => keep = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--drop" => match p.value("--drop") {
                Ok(v) => drop_cols = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--rename" => match p
                .value("--rename")
                .and_then(|v| csv_splitter_core::reshape::ReshapeSpec::parse_renames(&v))
            {
                Ok(v) => rename = v,
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
        eprintln!("error: columns needs <input>");
        return 2;
    };
    let spec = csv_splitter_core::reshape::ReshapeSpec {
        keep,
        drop: drop_cols,
        rename,
    };
    if spec.is_empty() {
        eprintln!("error: give at least one of --keep, --drop, --rename");
        return 2;
    }
    let output = out.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "columns".to_string());
        input.with_file_name(format!("{stem}.reshaped.csv"))
    });
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("columns", rx, json);
    let result = csv_splitter_core::reshape::reshape(&input, delimiter, &spec, &output, &tx);
    drop(tx);
    let _ = reporter.join();
    match result {
        Ok(s) => {
            if json {
                println!(
                    r#"{{"rows":{},"bytes":{},"columns":["{}"],"dropped":["{}"]}}"#,
                    s.rows,
                    s.bytes,
                    s.columns.join(r#"",""#),
                    s.dropped.join(r#"",""#)
                );
            } else {
                println!(
                    "✓ {} rows × {} columns → {} ({})",
                    s.rows,
                    s.columns.len(),
                    output.display(),
                    human_bytes(s.bytes)
                );
                println!("    columns: {}", s.columns.join(", "));
                if !s.dropped.is_empty() {
                    println!("    dropped: {}", s.dropped.join(", "));
                }
            }
            if verify {
                // The claim: every row kept, exactly the surviving columns.
                let ok = self_check(
                    &output,
                    delimiter,
                    csv_splitter_core::check::Expectation::rows(s.rows)
                        .with_columns(s.columns.len()),
                    json,
                );
                if !ok {
                    return 1;
                }
            }
            0
        }
        Err(_) => 1, // the reporter already printed the reason
    }
}

fn cmd_dedupe(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut keys: Vec<String> = Vec::new();
    let mut max_keys = csv_splitter_core::dedupe::DEFAULT_MAX_KEYS;
    let mut json = false;
    let mut verify = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--by" => match p.value("--by") {
                Ok(v) => keys = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--verify" => verify = true,
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--max-keys" => match p
                .value("--max-keys")
                .and_then(|v| v.parse().map_err(|_| "bad --max-keys".to_string()))
            {
                Ok(v) => max_keys = v,
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
        eprintln!("error: dedupe needs <input>");
        return 2;
    };
    let output = out.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "deduped".to_string());
        input.with_file_name(format!("{stem}.deduped.csv"))
    });
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("dedupe", rx, !json);
    let result =
        csv_splitter_core::dedupe::dedupe(&input, delimiter, &keys, &output, max_keys, &tx);
    drop(tx);
    let _ = reporter.join();
    match result {
        Ok(s) => {
            if json {
                println!(
                    r#"{{"scanned":{},"kept":{},"removed":{},"bytes":{},"keyColumns":["{}"]}}"#,
                    s.scanned,
                    s.kept,
                    s.removed,
                    s.bytes,
                    s.key_columns.join(r#"",""#)
                );
            } else {
                println!(
                    "✓ {} of {} rows kept, {} removed → {} ({})",
                    s.kept,
                    s.scanned,
                    s.removed,
                    output.display(),
                    human_bytes(s.bytes)
                );
            }
            if verify {
                // The claim: kept + removed == scanned, and the output holds
                // exactly the kept rows (no duplicates survived).
                if s.kept + s.removed != s.scanned {
                    eprintln!(
                        "✗ self-check failed: kept {} + removed {} != scanned {}",
                        s.kept, s.removed, s.scanned
                    );
                    return 1;
                }
                let ok = self_check(
                    &output,
                    delimiter,
                    csv_splitter_core::check::Expectation::rows(s.kept).with_ragged(s.kept_ragged),
                    json,
                );
                if !ok {
                    return 1;
                }
            }
            0
        }
        Err(_) => 1, // the reporter already printed the reason
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
        Err(_) => 1, // the reporter already printed the reason
    };

    std::fs::remove_dir_all(&work).ok();
    code
}

#[allow(dead_code)]
fn header_len(path: &Path) -> usize {
    read_headers(path, None).map(|h| h.len()).unwrap_or(0)
}
