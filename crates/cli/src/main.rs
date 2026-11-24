//! Headless CLI for csvx.
//!
//! The desktop app is great for one-off jobs, but servers and scripts need a
//! binary that can be piped into cron/Makefile/CI. Every subcommand here shares
//! the exact same core pipeline as the GUI, so behaviour cannot drift.

use csvx_core::csv_reader::{detect_delimiter, parse_csv_record, read_headers};
use csvx_core::decompress::decompress_file;
use csvx_core::encoding::{self, parse_name, TextEncoding};
use csvx_core::filter::{self, FilterSpec};
use csvx_core::index::{SparseIndex, DEFAULT_INTERVAL};
use csvx_core::merge::merge_archive_expecting;
use csvx_core::partition::{run_partition, PartitionConfig};

use csvx_core::search::SearchSpec;
use csvx_core::sort::SortColumn;
use csvx_core::splitter::run_split_threads;
use csvx_core::sql::{self, DEFAULT_LIMIT};
use csvx_core::*;
use std::io::{Read, Write};
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
        Some("schema") => cmd_schema(&args[1..]),
        Some("index") => cmd_index(&args[1..]),
        Some("cache") => cmd_cache(&args[1..]),
        Some("serve") => cmd_serve(&args[1..]),
        Some("mcp") => cmd_mcp(&args[1..]),
        Some("convert") => cmd_convert(&args[1..]),
        Some("filter") => cmd_filter(&args[1..]),
        Some("view") => cmd_view(&args[1..]),
        Some("sort") => cmd_sort(&args[1..]),
        Some("analyze") => cmd_analyze(&args[1..]),
        Some("dedupe") => cmd_dedupe(&args[1..]),
        Some("columns") => cmd_columns(&args[1..]),
        Some("edit") => cmd_edit(&args[1..]),
        Some("partition") => cmd_partition(&args[1..]),
        Some("concat") => cmd_concat(&args[1..]),
        Some("groupby") => cmd_groupby(&args[1..]),
        Some("pivot") => cmd_pivot(&args[1..]),
        Some("validate") => cmd_validate(&args[1..]),
        Some("gate") => cmd_gate(&args[1..]),
        Some("format") => cmd_format(&args[1..]),
        Some("chart") => cmd_chart(&args[1..]),
        Some("layout") => cmd_layout(&args[1..]),
        Some("lookup") => cmd_lookup(&args[1..]),
        Some("book") => cmd_book(&args[1..]),
        Some("pipeline") => cmd_pipeline(&args[1..]),
        Some("demo") => cmd_demo(&args[1..]),
        Some("sql") => cmd_sql(&args[1..]),
        Some("search") => cmd_search(&args[1..]),
        Some("files") => cmd_files(&args[1..]),
        Some("stats") => cmd_stats(&args[1..]),
        Some("repair") => cmd_repair(&args[1..]),
        Some("extract") => cmd_extract(&args[1..]),
        Some("verify") => cmd_verify(&args[1..]),
        Some("ops") => cmd_ops(&args[1..]),
        Some("-h" | "--help" | "help") | None => {
            print_help();
            0
        }
        Some("-V" | "--version") => {
            // The engine question comes up the moment `--engine sql` answers "this build has
            // no sql feature"; `--version` should answer it before the error does.
            println!(
                "csvx {VERSION} (DuckDB: {})",
                if csvx_core::engine::sql_available() {
                    "compiled in"
                } else {
                    "not compiled"
                }
            );
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

/// Options for one command, so `--help` answers the question that was asked.
///
/// The global help lists what the tool can do; it cannot describe sixty-one flags
/// without becoming unreadable. Each command's own options live here, and a test checks
/// that no command parses a flag this table forgets to mention.
const COMMAND_OPTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "analyze",
        &[
            ("--delim", "Field delimiter (default: auto)"),
            (
                "--distinct-cap",
                "Stop counting distinct values past this many",
            ),
            ("--group", "Grouping column for group-quality checks"),
            ("--json", "Emit JSON"),
            ("--key", "Column to check for uniqueness (comma separated)"),
            (
                "--limit",
                "Check only the first N rows (reports what it skipped)",
            ),
            (
                "--markdown",
                "Markdown quality report (requires --quality)",
            ),
            ("--quality", "Add key/duplicate/type/outlier findings"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            ("--sample", "Sample this many rows instead of reading all"),
        ],
    ),
    (
        "book",
        &[
            ("--json", "Emit JSON"),
            ("--max", "Maximum sheets to list"),
            ("--plan", "Apply a manifest of sheets, lookups and outputs"),
            ("--root", "Directory to serve / list"),
        ],
    ),
    (
        "pipeline",
        &[
            ("--dry-run", "Report fresh/would-run steps without executing"),
            ("--force", "Rerun every step even when fresh"),
            ("--clean", "Remove stale pipeline staging files before running"),
            ("--unlock", "Remove the plan lock before running (use with care)"),
            ("--verify", "Status: re-check current stamps/hashes"),
            ("--progress-markers", "Run: emit stable CSVX_STEP lines"),
            ("--content-check", "Run/status/watch: hash content even when stamps match"),
            ("--fail-on-stale", "Status: exit 3 when verify finds stale steps"),
            ("--dag", "Run ready steps by dependency order (requires --jobs > 1)"),
            ("--jobs", "Concurrency for groups/DAG (default 1)"),
            ("--retries", "Retry a failed step N times (default 0)"),
            ("--retry-delay", "Milliseconds between retries (default 250)"),
            ("--step-timeout", "Kill a step after N milliseconds (0 = none)"),
            ("--json", "Emit JSON (run, status and watch)"),
            ("--interval-ms", "Watch: milliseconds between runs (default 5000)"),
            ("--backoff-max-ms", "Watch: cap for failure backoff (default 60000)"),
            ("--max-failures", "Watch: stop after N consecutive failures (0 = never)"),
            ("--state", "Watch: write round/success state JSON to this file"),
            ("--iterations", "Watch: run N rounds then exit (0 = forever)"),
            ("--verbose", "Watch: print even when every step is fresh"),
            ("--root", "Directory for relative inputs/outputs"),
        ],
    ),
    (
        "chart",
        &[
            ("--by", "Separator (split-column) or grouping column"),
            ("--apply-edits", "Materialise pending edits before reading"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--desc", "Descending order"),
            ("--engine", "auto | native | sql (auto: sql above 8 MB)"),
            ("--json", "Emit JSON"),
            ("--max-points", "Refuse to chart more than this many points"),
            ("--out", "Output path"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            ("--series", "Chart: one series per value of this column"),
            ("--sort-by-value", "Chart: order categories by value"),
            ("--type", "Chart type"),
            ("--x", "Chart: x axis column"),
            ("--y", "Chart: y axis measure, e.g. \"amount:sum\""),
        ],
    ),
    (
        "columns",
        &[
            ("--delim", "Field delimiter (default: auto)"),
            ("--drop", "Columns to drop, comma separated"),
            ("--json", "Emit JSON"),
            ("--keep", "Columns to keep, comma separated"),
            ("--out", "Output path"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            ("--rename", "Renames, e.g. \"old:new\""),
            ("--verify", "Re-check the result after writing"),
        ],
    ),
    (
        "concat",
        &[
            ("--delim", "Field delimiter (default: auto)"),
            ("--engine", "auto | native | sql (auto: sql above 8 MB)"),
            ("--ignore-header", "Skip the header check"),
            ("--by-name", "Align columns by name when the order differs (same set only)"),

            ("--json", "Emit JSON"),
            ("--out", "Output path"),
        ],
    ),
    (
        "convert",
        &[
            ("--format", "Data format: csv | tsv | ndjson | parquet (xlsx disabled)"),
            ("--delim", "Field delimiter for CSV input"),
            ("--from", "Source encoding"),
            ("--out", "Output path"),
            ("--quiet", "Only errors"),
            ("--to", "Target encoding"),
        ],
    ),
    (
        "dedupe",
        &[
            (
                "--engine",
                "Not supported: dedupe keeps the first occurrence, which needs a stable row order SQL does not promise",
            ),
            ("--by", "Separator (split-column) or grouping column"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--json", "Emit JSON"),
            ("--max-keys", "Refuse to run past this many distinct keys"),
            ("--out", "Output path"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            ("--verify", "Re-check the result after writing"),
        ],
    ),
    ("demo", &[("--out", "Output path")]),
    (
        "edit",
        &[
            ("--after", "Insert the column after this one"),
            ("--by", "Separator (split-column) or grouping column"),
            ("--col", "Target column"),
            ("--count", "How many rows"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--dry-run", "Validate transaction without writing the journal"),
            ("--formula", "Formula for the new column, e.g. \"qty*2\""),
            ("--from", "Old column name / text to replace"),
            ("--into", "New column names for split-column"),
            ("--json", "Emit JSON"),
            ("--name", "Column name"),
            ("--out", "Output path"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            (
                "--regex",
                "Treat the pattern as a pattern: . * + ? ^ $ [a-z]",
            ),
            ("--row", "Data row (1-based)"),
            ("--cols", "Several columns to fill, as one operation (a,b)"),
            (
                "--after-key",
                "Insert after this row key (b:12 from /rows, not a row number)",
            ),
            ("--before-key", "Insert before this row key"),
            ("--to", "New column name / replacement text"),
            ("--value", "Value to write"),
            (
                "--invalid",
                "With delete: remove rows a validation rule rejects",
            ),
            (
                "--expect",
                "With set: write only if the cell still holds this value",
            ),
            (
                "--ops",
                "With transaction: set:1:city:X;delete:2:1;insert:4:a,b (one journal write)",
            ),
            ("--find", "Locate a row by its content instead of by number"),
            (
                "--where",
                "Filter expression; with delete, removes every match",
            ),
            ("--nth", "Which match to edit (default 1)"),
            ("--match-case", "Case-sensitive when locating"),
            ("--find-col", "Only look in these columns when locating"),
            ("--columns", "Columns to write / keep, comma separated"),
            ("--order", "Sort rows, e.g. \"amount:desc\""),
            ("--from-row", "First data row of a range (1-based)"),
            ("--to-row", "Last data row of a range (1-based, inclusive)"),
            ("--where", "Filter expression, e.g. city=CDMX"),
        ],
    ),
    ("extract", &[("--out", "Output path")]),
    (
        "files",
        &[
            ("--ext", "Only files with this extension"),
            ("--json", "Emit JSON"),
            ("--root", "Directory to serve / list"),
        ],
    ),
    (
        "filter",
        &[
            ("--any", "OR instead of AND"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--desc", "Descending order"),
            ("--json", "Emit JSON"),
            ("--limit", "Maximum rows to report / return"),
            (
                "--materialize",
                "Decompress .zst/.gz input to a temp file before filtering",
            ),
            ("--order", "Sort rows, e.g. \"city,amount:desc\""),
            ("--out", "Output path"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            ("--show", "Also print the matching rows"),
            ("--verify", "Re-check the result after writing"),
            ("--where", "Filter expression, e.g. city=CDMX"),
        ],
    ),
    (
        "format",
        &[
            ("--count", "How many rows"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--json", "Emit JSON"),
            ("--quiet", "Only errors"),
            (
                "--rule",
                "Format rule (\"amount>1000:bg=red\") or validation rule (\"qty>=0\")",
            ),
            ("--start", "First data row (1-based)"),
            (
                "--summary",
                "Count matches per rule instead of listing cells",
            ),
        ],
    ),
    (
        "groupby",
        &[
            ("--agg", "Aggregations, e.g. \"amount:sum,qty:mean\""),
            ("--asc", "Ascending order"),
            ("--by", "Separator (split-column) or grouping column"),
            ("--apply-edits", "Materialise pending edits before reading"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--desc", "Descending order"),
            ("--json", "Emit JSON"),
            ("--max-groups", "Refuse to run past this many groups"),
            ("--engine", "auto | native | sql (auto: sql above 8 MB)"),
            ("--order", "Sort rows, e.g. \"city,amount:desc\""),
            ("--out", "Output path"),
            ("--quiet", "Only errors"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
        ],
    ),
    (
        "cache",
        &[
            ("--build", "Build the columnar cache (t.csv.parquet)"),
            ("--drop", "Delete the cache"),
            ("--status", "Report whether the cache exists and is fresh"),
        ],
    ),
    (
        "index",
        &[
            ("--interval", "Rows between index checkpoints"),
            ("--out", "Output path"),
        ],
    ),
    (
        "layout",
        &[
            (
                "--freeze-first",
                "Freeze the first N columns (a number, e.g. 2)",
            ),
            ("--json", "Emit JSON"),
            (
                "--set",
                "Layout settings as <column>:<key>=<value>, e.g. \"amount:width=120\"",
            ),
        ],
    ),
    (
        "lookup",
        &[
            ("--delim", "Field delimiter (default: auto)"),
            ("--from", "Source encoding"),
            ("--from-key", "Reference table key column"),
            ("--get", "Columns to bring over, comma separated"),
            ("--json", "Emit JSON"),
            ("--engine", "auto | native | sql (auto: sql above 8 MB)"),
            ("--key", "Join key column"),
            ("--max-keys", "Refuse to run past this many distinct keys"),
            ("--out", "Output path"),
            ("--quiet", "Only errors"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            ("--reference", "Reference table path"),
        ],
    ),
    (
        "merge",
        &[
            ("--out", "Output path"),
            ("--chunks", "Refuse an archive that does not hold this many chunks"),
            ("--quiet", "Only errors"),
        ],
    ),
    (
        "ops",
        &[("--json", "Emit the operation and error-code catalog as JSON")],
    ),
    (
        "partition",
        &[
            ("--buf-mb", "Sort buffer size in MB"),
            ("--by", "Separator (split-column) or grouping column"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--format", "Archive format: tar | tar.gz | tar.zst | zip"),
            ("--level", "Compression level"),
            (
                "--max-partitions",
                "Refuse to write more partitions than this",
            ),
            ("--out", "Output path"),
            ("--quiet", "Only errors"),
            ("--verify", "Re-check the result after writing"),
        ],
    ),
    (
        "pivot",
        &[
            ("--cols", "Pivot: column key"),
            ("--columns", "Pivot: column key"),
            ("--apply-edits", "Materialise pending edits before reading"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--desc", "Descending order"),
            ("--json", "Emit JSON"),
            ("--engine", "auto | native | sql (auto: sql above 8 MB)"),
            ("--max-cells", "Pivot: matrix cells allowed"),
            ("--max-columns", "Pivot: distinct column values allowed"),
            ("--order", "Sort rows, e.g. \"city,amount:desc\""),
            ("--out", "Output path"),
            (
                "--qualify",
                "Pivot: prefix each measure with its column key",
            ),
            ("--quiet", "Only errors"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            ("--rows", "Pivot row keys (comma separated)"),
            ("--values", "Pivot measures, e.g. \"amount:sum\""),
        ],
    ),
    (
        "schema",
        &[
            ("--delim", "Field delimiter (default: auto)"),
            ("--diff", "Compare the column set with another file"),
            ("--json", "Emit JSON"),
            (
                "--require",
                "Assert these columns all exist (comma-separated)",
            ),
            (
                "--sample",
                "How many rows to sample for column kinds (default 10)",
            ),
            (
                "--has",
                "Print whether this column exists; exit 1 when it does not",
            ),
        ],
    ),
    (
        "preview",
        &[
            ("--columns", "Show only these columns (comma-separated)"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--index", "Rebuild the index first"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            ("--rows", "Pivot row keys (comma separated)"),
            ("--start-row", "First data row (1-based)"),
        ],
    ),
    (
        "search",
        &[
            ("--case", "Case-sensitive search"),
            ("--columns", "Pivot: column key"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--exact", "Match the whole cell"),
            ("--json", "Emit JSON"),
            ("--limit", "Maximum rows to report / return"),
            (
                "--materialize",
                "Decompress .zst/.gz input to a temp file before searching",
            ),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            (
                "--regex",
                "Treat the pattern as a pattern: . * + ? ^ $ [a-z]",
            ),
            ("--show", "Also print the matching rows"),
        ],
    ),
    (
        "mcp",
        &[
            ("--root", "Directory the tools may read (default: cwd)"),
        ],
    ),
    (
        "serve",
        &[
            ("--addr", "Bind address (127.0.0.1:0 picks a free port)"),
            ("--port", "Bind to 127.0.0.1:<port> (shorthand for --addr)"),
            (
                "--check",
                "Verify the configuration and exit (root readable, port bindable)",
            ),
            ("--root", "Directory to serve / list"),
        ],
    ),
    (
        "sort",
        &[
            (
                "--engine",
                "Not supported: sort orders numerically only when both sides parse, which SQL cannot reproduce",
            ),
            ("--asc", "Ascending order"),
            ("--buf-mb", "Sort buffer size in MB"),
            ("--by", "Separator (split-column) or grouping column"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--desc", "Descending order"),
            ("--out", "Output path"),
            ("--quiet", "Only errors"),
            ("--verify", "Re-check the result after writing"),
        ],
    ),
    (
        "split",
        &[
            ("--buf-mb", "Sort buffer size in MB"),
            ("--by-column", "Partition by this column"),
            ("--by-size", "Target part size, e.g. 500MB"),
            ("--columns", "Pivot: column key"),
            ("--columns-file", "Read the split columns from a file"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--desc", "Descending order"),
            ("--format", "Archive format: tar | tar.gz | tar.zst | zip"),
            ("--level", "Compression level"),
            ("--out", "Output path"),
            ("--quiet", "Only errors"),
            ("--sort", "Sort rows inside each part"),
            ("--splits", "Number of archive parts (verify)"),
            ("--threads", "Worker threads"),
        ],
    ),
    (
        "sql",
        &[
            ("--json", "Emit JSON"),
            ("--limit", "Maximum rows to report / return"),
        ],
    ),
    (
        "repair",
        &[
            ("--dry-run", "Only report what would be dropped"),
            ("--json", "Emit JSON"),
        ],
    ),
    (
        "stats",
        &[
            ("--delim", "Field delimiter (default: auto)"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            ("--sample", "Sample this many rows instead of reading all"),
            ("--top", "How many values to show per column"),
        ],
    ),
    (
        "gate",
        &[
            ("--delim", "Field delimiter (default: auto)"),
            ("--json", "Emit JSON"),
            ("--max-empty", "Maximum number of empty cells"),
            ("--min-rows", "Minimum number of data rows"),
            ("--unique", "Column whose values must be unique"),
            ("--out", "Write a JSON gate report to this path"),
        ],
    ),
    (
        "validate",
        &[
            (
                "--check",
                "Validation rule, e.g. \"qty>=0\" or \"city:not-empty\"",
            ),
            ("--checks", "Several checks, separated by ;"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--json", "Emit JSON"),
            ("--quiet", "Only errors"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            (
                "--rule",
                "Format rule (\"amount>1000:bg=red\") or validation rule (\"qty>=0\")",
            ),
            (
                "--limit",
                "Check only the first N rows (reports what it skipped)",
            ),
        ],
    ),
    (
        "verify",
        &[
            ("--quiet", "Only errors"),
            ("--splits", "Number of archive parts (verify)"),
        ],
    ),
    (
        "view",
        &[
            ("--context", "Rows of context around the match"),
            ("--count", "How many rows"),
            ("--columns", "Show only these columns (comma-separated)"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--desc", "Descending order"),
            (
                "--find",
                "Centre the page on the first row containing this text",
            ),
            ("--json", "Emit JSON"),
            ("--match-case", "Case-sensitive"),
            ("--nth", "Which match to centre on (default 1)"),
            ("--order", "Sort rows, e.g. \"city,amount:desc\""),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            (
                "--regex",
                "Treat the pattern as a pattern: . * + ? ^ $ [a-z]",
            ),
            (
                "--rule",
                "Format rule (\"amount>1000:bg=red\") or validation rule (\"qty>=0\")",
            ),
            ("--start", "First data row (1-based)"),
            ("--where", "Filter expression, e.g. city=CDMX"),
        ],
    ),
];

fn print_command_help(command: &str) {
    let Some(options) = COMMAND_OPTIONS
        .iter()
        .find(|(name, _)| *name == command)
        .map(|(_, options)| options)
    else {
        print_help();
        return;
    };
    println!("csvx {VERSION} — {command}\n");
    for (flag, description) in options.iter() {
        println!("  {flag:20} {description}");
    }
    println!("\n  --help               Show help and exit");
    println!("\nRun `csvx --help` for the list of commands.");
}

fn print_help() {
    println!(
        r#"csvx {VERSION} — split / merge / inspect large CSV files

USAGE
  csvx <command> [options]

WHAT DO YOU WANT TO DO?
  open a huge file without loading it      preview, index, cache
  find rows worth looking at                filter, search
  understand a column's shape              stats, analyze
  change data without touching the file    edit set, edit insert-column
  add a computed column                    edit formula
  reorganise columns                       columns, edit rename/drop
  summarise                                groupby, pivot, chart
  bring in another table                   lookup
  highlight cells                          format
  export a clean copy                      edit export, filter -o
  split for upload / merge back            split, merge, verify
  let a front-end read it                  serve

COMMANDS
  split    <input>              Split a CSV (optionally compress) into chunks
  merge    <archive|dir> -o F   Re-assemble chunks produced by `split`
  preview  <input>              Print the first rows and detected delimiter
  view     <input> [--where EXPR] Paged view of the edited sheet, optionally filtered
  index    <input>              Build a sparse row index for instant paging
  cache    <input>              Columnar cache for fast filtered/ordered pages
  serve    --root <dir>          Local HTTP API (JSON) for a web front-end
  mcp      --root <dir>          MCP stdio server (read-only tools) for agents
  convert  <input>               Transcode a CSV to UTF-8 (GBK / UTF-16 input)
  filter   <input> --where EXPR  Row numbers matching a column predicate
  sort     <input> --by COLS -o F Sort the whole file (a,b or a:desc,b:asc)
  analyze  <input>               Per-column profile: empties, distinct, numeric summary
  dedupe   <input> -o F          Drop duplicate rows (whole row or --by columns)
  columns  <input>                Keep/drop/rename columns (-o writes the result)
  edit     <action> <input>      Spreadsheet-style edits over an immutable base
  partition <input> --by COL      One file per distinct value of COL (tar/zip)
  concat   <a.csv> <b.csv> ...    Glue same-shaped files into one (header checked)
  groupby  <input> --by K --agg A Group and aggregate into a CSV (--engine auto|native|sql)
  pivot    <input> --rows R --values A Rows × columns × measures matrix
  format   <input> --rule R          Conditional formatting: which cells to paint
  chart    <input> --x C --y C:op    Chart data (categories + series) as JSON
  layout   <input> [--set C:k=v]    Column widths, names, freeze and number formats
  lookup   <input> --key C --from R Join a reference table onto the file (VLOOKUP)
  book     <dir>                  List every CSV in a directory as one workbook
  book     --plan <file>          Apply a declarative sheet/lookup/output manifest
  pipeline run <plan>             Run a declarative, incremental CSV pipeline
  pipeline watch <plan>           Poll and rerun the pipeline when steps go stale
  demo     [-o <dir>]           Write a sample file to try the commands on
  search   <input> <text>        Find text anywhere (or in chosen columns)
  files    --root <dir>          List readable files under a directory
  sql      <input> "<query>"     Run SQL over the file (DuckDB, read-only)
  stats    <input>              Column statistics via reservoir sampling
  extract  <input> -o DIR       Extract any supported compressed archive
  verify   <input>              Split + merge and prove the result is lossless
  ops      --json               Machine-readable operation and error-code catalog

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

EDIT ACTIONS  (the base file is never modified; edits live in <file>.journal)
  set      --row N --col C --value V     Change one cell
           --where "c=v" --col C --value V  Change every matching row
  insert   --row N --value "a,b,c"       Insert a row at logical position N
  delete   --row N --count K             Delete K rows from position N
  replace  --from X --to Y [--col C]     Replace text in one column or every column
  formula  --name N --value EXPR         Append a computed column (EXPR parsed later)
  insert-column --name N --value V        Insert a constant column
                [--after C]              …placing it right after column C
  insert-column --name N --formula E      …or a computed one
  copy     --from "A2:C5" --to "B10"      Copy a block of values (A1-style range)
  drop     --name C                      Drop a column
  rename   --from A --to B               Rename a column
  undo                                   Remove the last edit
  status                                 Show the pending edits
  export   [-o FILE]                     Materialise base + edits into a CSV

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
  csvx demo             Creates ./csvx-demo/orders.csv to play with
  csvx preview csvx-demo/orders.csv

EXAMPLES
  csvx split big.csv -n 8 -f zst
  csvx split huge.csv --by-size 100MB -f zip
  csvx split orders.csv --by-column city -f zst
  csvx split big.csv --columns id,phone,score -n 4 -f none -o keep.tar
  csvx merge big_split.tar -o restored.csv
  csvx verify big.csv --splits 6
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
            let mut f =
                csvx_core::encoding::open_maybe_compressed(input).map_err(|e| e.to_string())?;
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
        Some("check the path; `csvx files --root DIR` lists what is readable")
    } else if lower.contains("permission denied") {
        Some("the file or its directory is not readable by this user")
    } else if lower.contains("utf-16") {
        Some("convert it first: csvx convert FILE -o utf8.csv")
    } else if lower.contains("not found") && lower.contains("column") {
        Some("run `csvx analyze FILE` to see the exact column names")
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
    expectation: csvx_core::check::Expectation,
    quiet: bool,
) -> bool {
    let (tx, _rx) = mpsc::channel();
    match csvx_core::check::check(result, delimiter, &expectation, &tx) {
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
                    print_command_help("split");
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

fn cmd_preview(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut rows = 10usize;
    let mut delim = "auto".to_string();
    let mut start_row: u64 = 0;
    let mut shown_columns: Vec<String> = Vec::new();
    let mut index_path: Option<PathBuf> = None;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--columns" => match p.value("--columns") {
                Ok(v) => {
                    shown_columns = v
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
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
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
                print_command_help("preview");
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
    warn_quote_style(&input);
    warn_pending_edits(&input);

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

    let quotes = match csvx_core::csv_reader::QuoteStyle::parse(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    match csvx_core::preview::load_preview_with(&input, rows, override_delim, quotes) {
        Ok(preview) => {
            println!(
                "{} — {:.1} MB, ~{} rows, delimiter {:?}",
                input.display(),
                preview.file_size as f64 / (1024.0 * 1024.0),
                preview.total_rows_estimate,
                preview.detected_delimiter
            );
            let mut headers = preview.headers.clone();
            let mut shown = preview.rows.clone();
            if let Err(code) = project_columns(&mut headers, &mut shown, &shown_columns) {
                return code;
            }
            print_table(&headers, &shown);
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

/// `cache FILE [--build|--drop|--status]` — the columnar sidecar behind fast pages.
///
/// The cache is what makes a filtered first page cost about ten milliseconds instead of a
/// second: parquet is columnar, so a query touches only the columns it needs. Building it
/// reads the sheet once (about 11 s for 2.5 GB) and writes a sidecar next to it; the
/// sheet itself is never modified.
fn cmd_cache(args: &[String]) -> i32 {
    let mut input: Option<PathBuf> = None;
    let mut build = false;
    let mut drop_it = false;
    let mut status = false;
    let mut p = Parser::new(args);
    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--build" => build = true,
            "--drop" => drop_it = true,
            "--status" => status = true,
            "-h" | "--help" => {
                print_command_help("cache");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option for cache: {other}");
                return 2;
            }
            other => input = Some(PathBuf::from(other)),
        }
    }
    let Some(path) = input else {
        eprintln!("error: cache needs an input file");
        return 2;
    };
    let sidecar = csvx_core::sql_cache::sidecar(&path);
    let fresh = csvx_core::sql_cache::is_fresh(&path);
    if drop_it {
        match csvx_core::sql_cache::drop_cache(&path) {
            Ok(true) => {
                println!("removed {}", sidecar.display());
                return 0;
            }
            Ok(false) => {
                println!("no cache for {}", path.display());
                return 0;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }
    if build {
        let options = csvx_core::sheet::ReadOptions::default();
        let columns =
            match csvx_core::sheet::Sheet::open(&path, csvx_core::sheet::ReadOptions::default()) {
                Ok(sheet) => match sheet.schema(64) {
                    Ok(columns) => columns,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 1;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
        match csvx_core::sql_cache::build(
            &path,
            options.delimiter,
            &columns,
            csvx_core::sql::DEFAULT_TIMEOUT_MS * 20,
        ) {
            Ok(out) => {
                let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                println!("built {} ({bytes} bytes)", out.display());
                return 0;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }
    if status || !build && !drop_it {
        let bytes = std::fs::metadata(&sidecar).map(|m| m.len()).unwrap_or(0);
        println!(
            "{} — {}",
            sidecar.display(),
            if fresh {
                format!("fresh ({bytes} bytes)")
            } else if sidecar.exists() {
                "stale (the sheet changed); rebuild with --build".to_string()
            } else {
                "missing; build with --build".to_string()
            }
        );
        return 0;
    }
    0
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
                print_command_help("sql");
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

/// Width of a string as a terminal shows it: CJK and other wide characters take
/// two columns, combining marks take none. Padding with `chars().count()` would
/// misalign every table that has Chinese in it, which is most of them here.
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| {
            let code = ch as u32;
            let wide = (0x1100..=0x115F).contains(&code)
                || (0x2E80..=0xA4CF).contains(&code)
                || (0xAC00..=0xD7A3).contains(&code)
                || (0xF900..=0xFAFF).contains(&code)
                || (0xFE30..=0xFE4F).contains(&code)
                || (0xFF00..=0xFF60).contains(&code)
                || (0xFFE0..=0xFFE6).contains(&code)
                || (0x1F300..=0x1F64F).contains(&code)
                || (0x20000..=0x3FFFD).contains(&code);
            if wide {
                2
            } else {
                1
            }
        })
        .sum()
}

/// Print rows as an aligned table, with padding measured in terminal columns.
fn print_table(columns: &[String], rows: &[Vec<String>]) {
    let (columns, rows, hidden) = fit_to_terminal(columns, rows);
    let columns = &columns;
    let rows = &rows;
    let mut widths: Vec<usize> = columns.iter().map(|c| display_width(c)).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(i) {
                *width = (*width).max(display_width(cell));
            }
        }
    }
    let render = |cells: &[String]| {
        let mut line = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                line.push_str(" | ");
            }
            line.push_str(cell);
            let pad = widths.get(i).copied().unwrap_or(0) - display_width(cell);
            if i + 1 < cells.len() {
                line.push_str(&" ".repeat(pad));
            }
        }
        line
    };
    println!("{}", render(columns));
    let total: usize = widths.iter().sum::<usize>() + 3 * widths.len().saturating_sub(1);
    println!("{}", "-".repeat(total.max(20)));
    for row in rows {
        println!("{}", render(row));
    }
    note_hidden_columns(hidden);
}

/// `view`: paged look at the edited sheet, optionally filtered.
///
/// Reads the file in windows instead of loading it, so the same command works on a
/// 10 GB file. Filtering streams: each window is tested and matches are printed until
/// the requested count is reached, which means a selective filter stays fast and an
/// unselective one stops early anyway.
fn cmd_view(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut start: u64 = 0;
    let mut count: u64 = 20;
    let mut count_given = false;
    let mut delim = "auto".to_string();
    let mut where_expr: Option<String> = None;
    let mut order: Option<String> = None;
    let mut order_descending = false;
    let mut find: Option<String> = None;
    let mut shown_columns: Vec<String> = Vec::new();
    let mut rules_text: Vec<String> = Vec::new();
    let mut nth: u64 = 1;
    let mut context: u64 = 3;
    let mut match_case = false;
    let mut find_regex = false;
    let mut json = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--start" => match p.value("--start") {
                Ok(v) => match v.parse() {
                    Ok(n) => start = n,
                    Err(_) => {
                        eprintln!("error: --start needs a number");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--count" => match p.value("--count") {
                Ok(v) => match v.parse::<u64>() {
                    Ok(0) => {
                        eprintln!("error: --count must be at least 1");
                        return 2;
                    }
                    Ok(n) => {
                        count = n;
                        count_given = true;
                    }
                    Err(_) => {
                        eprintln!("error: --count needs a number");
                        return 2;
                    }
                },
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
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
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
            "--desc" => order_descending = true,
            "--nth" => match p.value("--nth") {
                Ok(v) => match v.parse::<u64>() {
                    Ok(0) => {
                        eprintln!("error: --nth starts at 1");
                        return 2;
                    }
                    Ok(n) => nth = n,
                    Err(_) => {
                        eprintln!("error: --nth needs a number");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--columns" => match p.value("--columns") {
                Ok(v) => {
                    shown_columns = v
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
            "--find" => match p.value("--find") {
                Ok(v) => find = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--context" => match p.value("--context") {
                Ok(v) => match v.parse() {
                    Ok(n) => context = n,
                    Err(_) => {
                        eprintln!("error: --context needs a number");
                        return 2;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--match-case" => match_case = true,
            "--regex" => find_regex = true,
            "--rule" => match p.value("--rule") {
                Ok(v) => rules_text.push(v),
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
                print_command_help("view");
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
        eprintln!("error: view needs an <input> path");
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
    warn_quote_style(&input);
    // No `warn_pending_edits` here: unlike the read-only commands, `view` applies them.

    let sidecar = csvx_core::workbook::Journal::sidecar(&input);
    // An interrupted compaction must be repaired before this command loads the journal.
    if let Err(e) = csvx_core::workbook::recover_compaction(&input) {
        eprintln!("error: {e}");
        return 1;
    }
    warn_journal_damage(&input);
    let journal = match csvx_core::workbook::Journal::load(&sidecar) {
        Ok(j) => j,
        Err(e) => return report_failure("view", format!("cannot read the journal: {e}")),
    };
    let ops = journal.ops().to_vec();

    // The predicate is compiled once, against the *edited* column names, so a filter
    // on a renamed column behaves the way the user sees the sheet.
    let compiled = match &where_expr {
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
            let columns = match csvx_core::workbook::resolve_columns(
                &ops,
                &match csvx_core::workbook::base_header(&input, delimiter) {
                    Ok(c) => c,
                    Err(e) => return report_failure("view", e.to_string()),
                },
            ) {
                Ok(c) => c,
                Err(e) => return report_failure("view", e.to_string()),
            };
            match set.compile(&columns) {
                Ok(c) => Some(c),
                Err(e) => return report_failure("view", e.to_string()),
            }
        }
    };

    // Ordered paging cannot be done by streaming: the smallest key may be on the last
    // row. So the rows are materialised once (edited, then filtered), sorted, and the
    // requested page is read back. That costs a full scan and a temporary copy of the
    // matched rows — nothing proportional to the file beyond the matches.
    // Ordered paging through DuckDB: when the sheet has no pending edits and no
    // `--where`, there is nothing the database cannot see, and `ORDER BY ... LIMIT` reads
    // the columns it needs instead of materialising 2.5 GB three times. Measured on the
    // 782-column fixture: 3.9 s here against 29.8 s for export+sort+page.
    //
    // The moment a journal edit exists this must stay native: the database would see the
    // base file without it.
    let sql_page: Option<(Vec<String>, Vec<Vec<String>>)> = if order.is_some() && ops.is_empty() {
        match csvx_core::engine::resolve(csvx_core::engine::Engine::Auto, &input) {
            Ok(csvx_core::engine::Engine::Sql) => {
                let order_columns = match parse_sort_columns(
                    order.as_deref().unwrap_or_default(),
                    order_descending,
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                };
                let order_sql = order_columns
                    .iter()
                    .map(|c| {
                        format!(
                            "\"{}\" {}",
                            c.name.replace('"', "\"\""),
                            if c.descending { "DESC" } else { "ASC" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                // `--where` joins the same query when it translates exactly; when the
                // native comparison has no SQL equivalent the read falls back instead.
                let predicate = match where_expr.as_deref() {
                    None => None,
                    Some(expr) => {
                        let spec = match csvx_core::filter::FilterSpec::parse(expr) {
                            Ok(spec) => spec,
                            Err(e) => {
                                eprintln!("error: {e}");
                                return 2;
                            }
                        };
                        match csvx_core::engine::filter_predicate(&spec) {
                            Some(predicate) => Some(predicate),
                            None => {
                                eprintln!(
                                    "note: ordered read fell back to the native path: \
                                     this --where has no exact SQL equivalent"
                                );
                                return 2;
                            }
                        }
                    }
                };
                let where_sql = predicate.map(|p| format!(" WHERE {p}")).unwrap_or_default();
                let query = format!(
                    "SELECT * FROM t{where_sql} ORDER BY {} LIMIT {} OFFSET {}",
                    order_sql, count, start
                );
                match csvx_core::sql::query_with(&input, &query, count as usize, delimiter) {
                    Ok(result) => Some((result.columns, result.rows)),
                    Err(e) => {
                        eprintln!("note: ordered read fell back to the native path: {e}");
                        None
                    }
                }
            }
            _ => None,
        }
    } else {
        None
    };

    let paging_file = if order.is_some() && sql_page.is_none() {
        let work = std::env::temp_dir().join(format!(
            "csvs_view_{}_{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let exported = work.with_extension("src.csv");
        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("view", rx, true);
        let result =
            csvx_core::workbook::export_quotes(&input, &ops, &exported, delimiter, quotes, &tx);
        drop(tx);
        let _ = reporter.join();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&exported);
            return report_failure("view", format!("cannot materialise the sheet: {e}"));
        }
        if let Some(expr) = &where_expr {
            let set = match csvx_core::filter::FilterSpec::parse(expr) {
                Ok(spec) => csvx_core::filter::FilterSet::single(spec),
                Err(e) => {
                    let _ = std::fs::remove_file(&exported);
                    eprintln!("error: {e}");
                    return 2;
                }
            };
            let filtered = work.with_extension("filtered.csv");
            let (tx, rx) = mpsc::channel();
            let reporter = spawn_reporter("view", rx, true);
            let result = csvx_core::filter::extract_style(
                &exported, delimiter, &set, &filtered, quotes, &tx,
            );
            drop(tx);
            let _ = reporter.join();
            let _ = std::fs::remove_file(&exported);
            if let Err(e) = result {
                let _ = std::fs::remove_file(&filtered);
                return report_failure("view", format!("cannot filter: {e}"));
            }
            std::fs::rename(&filtered, &work).ok();
        } else {
            std::fs::rename(&exported, &work).ok();
        }

        let sort_columns =
            match parse_sort_columns(order.as_deref().unwrap_or_default(), order_descending) {
                Ok(c) => c,
                Err(e) => {
                    let _ = std::fs::remove_file(&work);
                    eprintln!("error: {e}");
                    return 2;
                }
            };
        let sorted = work.with_extension("sorted.csv");
        let (tx, _rx) = mpsc::channel();
        let result = csvx_core::sort::external_sort_columns(
            &work,
            &sorted,
            &sort_columns,
            delimiter,
            64,
            &tx,
        );
        let _ = std::fs::remove_file(&work);
        if let Err(e) = result {
            let _ = std::fs::remove_file(&sorted);
            return report_failure("view", format!("cannot sort: {e}"));
        }
        Some(sorted)
    } else {
        None
    };

    // Two paths: a windowed read when nothing needs sorting (cheap, edits applied),
    // and paging a materialised+sorted copy when `--order` was asked for.
    note_index_build(&input);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut columns: Vec<String> = Vec::new();
    let mut highlight: Option<u64> = None;
    let page_total: Option<u64>;
    if let Some((sql_columns, sql_rows)) = &sql_page {
        columns = sql_columns.clone();
        rows = sql_rows.clone();
        page_total = None;
    } else if let Some(path) = &paging_file {
        if let Ok(file) = std::fs::File::open(path) {
            let mut reader = csvx_core::csv_reader::RecordReader::new(
                std::io::BufReader::with_capacity(4 * 1024 * 1024, file),
                4 * 1024 * 1024,
            );
            let mut record = Vec::new();
            let mut index: u64 = 0;
            while reader.next_record(&mut record).is_ok_and(|more| more) {
                let cells = csvx_core::csv_reader::parse_csv_record_with(
                    String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
                    delimiter.byte(),
                    quotes,
                );
                if columns.is_empty() {
                    columns = cells;
                    continue;
                }
                if index >= start {
                    rows.push(cells);
                    if rows.len() as u64 >= count {
                        break;
                    }
                }
                index += 1;
            }
        }
        let _ = std::fs::remove_file(path);
        page_total = None;
    } else if order.is_none() && where_expr.is_none() && find.is_none() {
        let request = csvx_core::ops::ViewRequest {
            path: input.clone(),
            delimiter,
            start,
            count,
        };
        let response = match request.execute() {
            Ok(response) => response,
            Err(e) => return report_failure("view", e.to_string()),
        };
        columns = response.window.columns;
        rows = response.window.rows;
        page_total = Some(response.window.total);
    } else {
        // `--find` turns "search then copy the row number" into one command: the
        // match is located first, and the page is centred on it.
        if let Some(needle) = &find {
            let needle = if match_case {
                needle.clone()
            } else {
                needle.to_lowercase()
            };
            // One pager for the scan: `window()` per chunk would rebuild the index and
            // the formulas fifty times over for a million rows.
            let scan_pager = match csvx_core::workbook::Pager::open(&input, &ops, delimiter) {
                Ok(p) => p,
                Err(e) => return report_failure("view", e.to_string()),
            };
            let mut at: u64 = 0;
            let mut seen_matches: u64 = 0;
            let found = 'scan: loop {
                let win = match scan_pager.page(at, 2_000) {
                    Ok(w) => w,
                    Err(e) => return report_failure("view", e.to_string()),
                };
                if win.rows.is_empty() {
                    break None;
                }
                for (i, row) in win.rows.iter().enumerate() {
                    let hit = row.iter().any(|cell| {
                        if find_regex {
                            // Same pattern subset as conditional formatting, so `--find`
                            // and `format --rule` agree on what a pattern means.
                            return if match_case {
                                csvx_core::format::pattern_matches(&needle, cell)
                            } else {
                                csvx_core::format::pattern_matches(&needle, &cell.to_lowercase())
                            };
                        }
                        if match_case {
                            cell.contains(&needle)
                        } else {
                            cell.to_lowercase().contains(&needle)
                        }
                    });
                    if hit {
                        seen_matches += 1;
                        if seen_matches == nth {
                            break 'scan Some(at + i as u64);
                        }
                    }
                }
                at += win.rows.len() as u64;
                if win.rows.len() < 2_000 {
                    break None;
                }
            };
            match found {
                Some(index) => {
                    start = index.saturating_sub(context);
                    if !count_given {
                        // A find should show the match and its context, not a page.
                        count = context + 1 + context;
                    }
                    highlight = Some(index);
                }
                None => {
                    let needle = find.clone().unwrap_or_default();
                    if seen_matches > 0 {
                        println!(
                            "(only {} row(s) contain \"{needle}\" — --nth {nth} is past the last one)",
                            seen_matches
                        );
                    } else {
                        println!("(no row contains \"{needle}\")");
                    }
                    return 0;
                }
            }
        }

        // One `Pager` for the whole scan: the index, column layout and compiled
        // formulas are built once instead of once per page.
        let pager = match csvx_core::workbook::Pager::open(&input, &ops, delimiter) {
            Ok(p) => p,
            Err(e) => return report_failure("view", e.to_string()),
        };
        page_total = Some(pager.total());
        columns = pager.columns().to_vec();
        match &compiled {
            // No filter: jump straight to the offset. Scanning forward from row zero
            // to reach `--start 3900000` read the whole file to show one page.
            None => {
                let win = match pager.page(start, count) {
                    Ok(w) => w,
                    Err(e) => return report_failure("view", e.to_string()),
                };
                rows = win.rows;
            }
            // A filter has to look at every row until it has enough matches, so it
            // scans — but it starts counting from zero, which is what `--start` means
            // for a filtered view: skip the first `start` matches.
            Some(set) => {
                let mut seen: u64 = 0;
                let chunk = count.max(1_000);
                loop {
                    let win = match pager.page(seen, chunk) {
                        Ok(w) => w,
                        Err(e) => return report_failure("view", e.to_string()),
                    };
                    if win.rows.is_empty() {
                        break;
                    }
                    for row in &win.rows {
                        if set.holds(row) {
                            rows.push(row.clone());
                            if rows.len() as u64 >= count {
                                break;
                            }
                        }
                    }
                    seen += win.rows.len() as u64;
                    if rows.len() as u64 >= count || win.rows.len() < chunk as usize {
                        break;
                    }
                }
            }
        }
    }

    if let Err(code) = project_columns(&mut columns, &mut rows, &shown_columns) {
        return code;
    }
    if json {
        let body = rows
            .iter()
            .map(|row| {
                let cells: Vec<String> = row.iter().map(|c| serve::json_str(c)).collect();
                format!("[{}]", cells.join(","))
            })
            .collect::<Vec<_>>()
            .join(",");
        let next_start = start + rows.len() as u64;
        let has_more = match page_total {
            Some(total) => next_start < total,
            None => rows.len() as u64 >= count,
        };
        println!(
            r#"{{"columns":[{}],"rows":[{}],"start":{},"nextStart":{},"hasMore":{}}}"#,
            columns
                .iter()
                .map(|c| serve::json_str(c))
                .collect::<Vec<_>>()
                .join(","),
            body,
            start,
            next_start,
            has_more
        );
        return 0;
    }

    if rows.is_empty() {
        // Say which emptiness this is: an offset past the end reads very differently
        // from "your filter matched nothing".
        match page_total {
            Some(0) => println!("(the file has no data rows)"),
            Some(total) if start >= total => {
                println!("(start row {start} is past the end: {total} data rows)")
            }
            Some(total) => println!("(no rows matched; the file has {total} data rows)"),
            // `--find` scans without a pager, so there is no total to quote.
            None => println!("(no rows matched)"),
        }
        return 0;
    }
    // Conditional formatting on screen: the same rules the API evaluates, applied to
    // this page. Colour when the terminal can show it, brackets otherwise.
    let marks = if rules_text.is_empty() || rows.is_empty() {
        Vec::new()
    } else {
        let mut parsed = Vec::new();
        for text in &rules_text {
            match csvx_core::format::Rule::parse(text) {
                Ok(rule) => parsed.push(rule),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        }
        let mut marks = Vec::new();
        for (offset, row) in rows.iter().enumerate() {
            let logical = start + offset as u64;
            for rule in &parsed {
                for (slot, column) in columns.iter().enumerate() {
                    let value = row.get(slot).map(String::as_str).unwrap_or("");
                    if rule.matches_cell(column, value) {
                        marks.push((logical, column.clone(), rule.style.clone()));
                    }
                }
            }
        }
        marks
    };
    if !marks.is_empty() {
        print_table_styled(&columns, &rows, start, &marks);
    } else if let Some(index) = highlight {
        // Print a marker beside the row `--find` landed on, so the eye finds it.
        let offset = index.saturating_sub(start) as usize;
        print_table(&columns, &rows);
        let which = if nth > 1 {
            format!(" (match {nth})")
        } else {
            String::new()
        };
        println!(
            "\n(^ row {} of {}{}{})",
            index + 1,
            page_total
                .map(|t| t.to_string())
                .unwrap_or_else(|| "?".into()),
            which,
            if offset < rows.len() {
                ""
            } else {
                " (outside this page)"
            }
        );
    } else {
        print_table(&columns, &rows);
    }
    let applied = journal.len();
    if let (Some(total), false) = (page_total, json) {
        println!(
            "\n(page at rows {}-{} of {})",
            start + 1,
            (start + rows.len() as u64).min(total),
            total
        );
    }
    if applied > 0 || where_expr.is_some() {
        let mut note = String::new();
        if applied > 0 {
            note.push_str(&format!("{applied} edit(s) applied"));
        }
        if let Some(expr) = &where_expr {
            if !note.is_empty() {
                note.push_str(" · ");
            }
            note.push_str(&format!("{} row(s) where {expr}", rows.len()));
        }
        println!("\n({note})");
    }
    0
}

fn cmd_filter(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut order_by: Option<String> = None;
    let mut order_descending = false;
    let mut input: Option<PathBuf> = None;
    let mut where_exprs: Vec<String> = Vec::new();
    let mut combine = filter::Combine::All;
    let mut out: Option<PathBuf> = None;
    let mut verify = false;
    let mut limit = 10_000usize;
    let mut json = false;
    let mut materialize = false;
    let mut show = 0usize;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--materialize" => materialize = true,
            "--where" => match p.value("--where") {
                Ok(v) => where_exprs.push(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--any" => combine = filter::Combine::Any,
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--order" => match p.value("--order") {
                Ok(v) => order_by = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--desc" => order_descending = true,
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
                print_command_help("filter");
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
    warn_pending_edits(&input);
    let _materialized = if materialize {
        match materialize_compressed(&input) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        }
    } else {
        None
    };
    let input = _materialized
        .as_ref()
        .map(|value| value.path.clone())
        .unwrap_or(input);
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
    let quotes = match csvx_core::csv_reader::QuoteStyle::parse(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let set = filter::FilterSet { specs, combine };

    // `--out` is the "keep the result" path: stream matches to a file.
    if let Some(out) = out {
        // Ordering needs the whole match set before the first row can be written,
        // so matches go to a temp file and are sorted into place. Filtering stays
        // streaming; only the matched rows touch disk twice.
        let sort_columns = match &order_by {
            None => None,
            Some(spec) => match parse_sort_columns(spec, order_descending) {
                Ok(columns) => Some(columns),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
        };
        let stage = sort_columns.as_ref().map(|_| {
            std::env::temp_dir().join(format!(
                "csvs_filter_{}_{}.csv",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ))
        });
        let target = stage.clone().unwrap_or_else(|| out.clone());

        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("filter", rx, true);
        if let Err(code) = check_output(Some(&input), &target) {
            return code;
        }
        let result = filter::extract_style(&input, delimiter, &set, &target, quotes, &tx);
        drop(tx);
        let _ = reporter.join();
        let sorted = match (result, &sort_columns, &stage) {
            (Ok(summary), Some(columns), Some(stage)) => {
                let (tx, _rx) = mpsc::channel();
                match csvx_core::sort::external_sort_columns(
                    stage, &out, columns, delimiter, 64, &tx,
                ) {
                    Ok(()) => {
                        std::fs::remove_file(stage).ok();
                        Ok(summary)
                    }
                    Err(e) => {
                        std::fs::remove_file(stage).ok();
                        Err(e)
                    }
                }
            }
            (Ok(summary), _, _) => Ok(summary),
            (Err(e), _, _) => Err(e),
        };
        return match sorted {
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
                        csvx_core::check::Expectation::rows(s.matched)
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

    let spec = set.specs.first().cloned();
    let Some(spec) = spec else { return 2 };
    let request = csvx_core::ops::FilterRequest {
        path: input.clone(),
        delimiter,
        spec,
        limit,
        budget: None,
    };
    let result = match request.execute_fast() {
        Ok(r) => r,
        Err(e) => {
            return report_failure("filter", e);
        }
    };

    if json {
        let rows: Vec<String> = result.rows.iter().map(|n| n.to_string()).collect();
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
    let shown: Vec<String> = result.rows.iter().take(50).map(|n| n.to_string()).collect();
    println!("rows (1-based, header excluded): {}", shown.join(", "));
    if result.matched > 50 {
        println!(
            "... {} more; use --json for the full list",
            result.matched - 50
        );
    }
    if show > 0 {
        let picked: Vec<u64> = result
            .rows
            .iter()
            .take(show)
            .map(|row| row.saturating_sub(1))
            .collect();
        match csvx_core::filter::read_rows(&input, delimiter, &picked) {
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

struct MaterializedInput {
    path: std::path::PathBuf,
}

impl Drop for MaterializedInput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn cleanup_stale_materialized() {
    let dir = std::env::temp_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("csvx-materialized-") else {
            continue;
        };
        let Some(pid) = rest
            .split('-')
            .next()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != std::process::id() && !process_alive(pid) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn materialize_compressed(input: &std::path::Path) -> std::io::Result<Option<MaterializedInput>> {
    if !csvx_core::encoding::is_compressed(input) {
        return Ok(None);
    }
    cleanup_stale_materialized();
    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    );
    let path = std::env::temp_dir().join(format!("csvx-materialized-{stamp}.csv"));
    let mut reader = csvx_core::encoding::open_decoded(input)?;
    let mut writer = std::fs::File::create(&path)?;
    std::io::copy(&mut reader, &mut writer)?;
    Ok(Some(MaterializedInput { path }))
}

fn cmd_search(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut query: Option<String> = None;
    let mut columns: Vec<String> = Vec::new();
    let mut case_sensitive = false;
    let mut regex = false;
    let mut exact = false;
    let mut limit = 1_000usize;
    let mut show = 0usize;
    let mut json = false;
    let mut materialize = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--materialize" => materialize = true,
            "--columns" => match p.value("--columns") {
                Ok(v) => columns = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--regex" => regex = true,
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
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
                print_command_help("search");
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
    let _materialized = if materialize {
        match materialize_compressed(&input) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        }
    } else {
        None
    };
    let input = _materialized
        .as_ref()
        .map(|value| value.path.clone())
        .unwrap_or(input);
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
    spec.regex = regex;

    let (tx, _rx) = mpsc::channel();
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let result =
        match csvx_core::search::search_fast(&input, delimiter, &spec, limit, None, quotes, &tx) {
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
        match csvx_core::filter::read_rows(&input, delimiter, picked) {
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
                print_command_help("files");
                return 0;
            }
            other => root = Some(PathBuf::from(other)),
        }
    }

    let Some(root) = root else {
        eprintln!("error: files needs --root <dir>");
        return 2;
    };
    let listing = match csvx_core::listing::list(&root, &exts, 200) {
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
    let mut convert_format: Option<String> = None;
    let mut delim = "comma".to_string();
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
            "--format" => match p.value("--format") {
                Ok(v) => convert_format = Some(v),
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
                print_command_help("convert");
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

    if let Some(target) = convert_format {
        let Some(out) = out else {
            eprintln!("error: --format needs --out <path>");
            return 2;
        };
        // A TSV/Parquet/NDJSON source has its own delimiter; validating a CSV header
        // with the CSV default would reject a perfectly good TSV before conversion.
        let name = input
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let delimiter = if name.ends_with(".tsv") {
            csvx_core::types::Delimiter::Tab
        } else if name.ends_with(".parquet")
            || name.ends_with(".ndjson")
            || name.ends_with(".jsonl")
            || name.ends_with(".json")
        {
            csvx_core::types::Delimiter::Comma
        } else {
            match parse_delimiter(&delim, &input) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        };
        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("convert", rx, true);
        let result = csvx_core::engine::convert_format(&input, &out, &target, delimiter, &tx);
        drop(tx);
        let _ = reporter.join();
        return match result {
            Ok(()) => {
                if !quiet {
                    println!(
                        "✓ converted {} → {} ({target})",
                        input.display(),
                        out.display()
                    );
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        };
    }

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

    if let Err(code) = check_output(Some(&input), &output) {
        return code;
    }
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
        // `transcode` returns the error itself; nothing else printed it.
        Err(e) => report_failure("convert", e),
    }
}

fn cmd_serve(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    let mut addr = "127.0.0.1:8787".to_string();
    let mut check = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--root" => match p.value("--root") {
                Ok(v) => root = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            // `--port` is the shorthand people type; `--addr` is the full form.
            "--port" => match p.value("--port").and_then(|v| {
                v.parse::<u16>()
                    .map(|port| format!("127.0.0.1:{port}"))
                    .map_err(|_| format!("--port takes a number, not {v:?}"))
            }) {
                Ok(v) => addr = v,
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
            "--check" => check = true,
            "-h" | "--help" => {
                print_command_help("serve");
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
    if check {
        return serve::check(&addr, &root);
    }
    match serve::run(serve::ServerConfig { addr, root }) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn cmd_stats(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut sample = 50_000usize;
    let mut top = 5usize;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },

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
                print_command_help("stats");
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
    warn_pending_edits(&input);
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("stats", rx, true);
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let result = csvx_core::preview::compute_column_stats_with(
        &input, delimiter, sample, 100_000, quotes, &tx,
    );
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
        Err(e) => report_failure("stats", e), // engine returned the error itself
    }
}

/// `repair <file>` — rewrite a damaged journal down to its readable records.
///
/// Reading already skips unreadable records; repair exists so the damage can be made *tidy*
/// on purpose, with a backup, instead of being reported forever.
fn cmd_repair(args: &[String]) -> i32 {
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
            // Accepting `--engine` and quietly using the native path would be worse than
            // refusing: someone who asked for SQL would believe they got it.
            "--engine" => {
                eprintln!(
                    "error: sort has no sql engine; its ordering is numeric only when both \
                     sides parse as numbers, which no SQL ORDER BY on a typed column reproduces"
                );
                return 2;
            }
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
                print_command_help("sort");
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

    if let Err(code) = check_output(Some(&input), &out) {
        return code;
    }
    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("sort", rx, quiet);
    let result =
        csvx_core::sort::external_sort_columns(&input, &out, &columns, delimiter, buf_mb, &tx);
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
                let before = match csvx_core::check::count(&input, delimiter, &tx) {
                    Ok(c) => c,
                    Err(e) => {
                        return report_failure("sort", format!("cannot read the source: {e}"))
                    }
                };
                let after = match csvx_core::check::count(&out, delimiter, &tx) {
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
        Err(e) => {
            // Same as merge: sorting returns an error without sending an event.
            eprintln!("error: {e}");
            1
        }
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

fn cmd_book(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    let mut plan: Option<PathBuf> = None;
    let mut json = false;
    let mut max_sheets = csvx_core::book::DEFAULT_MAX_SHEETS;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--root" => match p.value("--root") {
                Ok(v) => root = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--max" => match p
                .value("--max")
                .and_then(|v| v.parse().map_err(|_| "bad --max".to_string()))
            {
                Ok(v) => max_sheets = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--plan" => match p.value("--plan") {
                Ok(v) => plan = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "-h" | "--help" => {
                print_command_help("book");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => {
                if root.is_none() {
                    root = Some(PathBuf::from(other));
                } else {
                    eprintln!("error: unexpected argument: {other}");
                    return 2;
                }
            }
        }
    }

    if let Some(plan) = plan {
        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("book", rx, true);
        let result = csvx_core::book::apply_manifest(&plan, root.as_deref(), &tx);
        drop(tx);
        let _ = reporter.join();
        return match result {
            Ok(outputs) => {
                if json {
                    let items: Vec<String> = outputs
                        .iter()
                        .map(|output| {
                            format!(
                                r#"{{"name":"{}","path":"{}","rows":{},"bytes":{}}}"#,
                                output.name,
                                output.path.display(),
                                output.rows,
                                output.bytes
                            )
                        })
                        .collect();
                    println!(r#"{{"outputs":[{}]}}"#, items.join(","));
                } else {
                    for output in &outputs {
                        println!(
                            "✓ {} → {} ({} rows, {})",
                            output.name,
                            output.path.display(),
                            output.rows,
                            human_bytes(output.bytes)
                        );
                    }
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        };
    }
    let Some(root) = root else {
        eprintln!("error: book needs a directory (--root <dir>) or a manifest (--plan <file>)");
        return 2;
    };
    let book = match csvx_core::book::scan(&root, max_sheets) {
        Ok(b) => b,
        Err(e) => return report_failure("book", format!("cannot read {}: {e}", root.display())),
    };

    if json {
        let sheets: Vec<String> = book.sheets.iter().map(|s| s.to_json()).collect();
        println!(
            r#"{{"root":"{}","sheets":[{}]}}"#,
            book.root.display(),
            sheets.join(",")
        );
        return 0;
    }

    println!("{}  ({} sheets)", book.root.display(), book.sheets.len());
    println!(
        "  {:<28} {:>9} {:>9} {:>6} {:>6}  state",
        "sheet", "bytes", "rows", "cols", "edits"
    );
    for sheet in &book.sheets {
        let rows = match sheet.rows {
            Some(n) => n.to_string(),
            None => "-".to_string(),
        };
        let mut state = Vec::new();
        if sheet.indexed {
            state.push("indexed");
        }
        if sheet.styled {
            state.push("styled");
        }
        if sheet.edits > 0 {
            state.push("edited");
        }
        println!(
            "  {:<28} {:>9} {:>9} {:>6} {:>6}  {}",
            sheet.name,
            human_bytes(sheet.bytes),
            rows,
            sheet.columns.map(|c| c.to_string()).unwrap_or_default(),
            if sheet.edits == 0 {
                "-".to_string()
            } else {
                sheet.edits.to_string()
            },
            state.join(" ")
        );
    }
    for (path, reason) in &book.unreadable {
        eprintln!("  ! {} — {reason}", path.display());
    }
    0
}

fn cmd_lookup(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut key: Option<String> = None;
    let mut reference: Option<PathBuf> = None;
    let mut reference_key = String::new();
    let mut take = String::new();
    let mut max_keys = csvx_core::lookup::DEFAULT_MAX_KEYS;
    let mut engine = csvx_core::engine::Engine::Auto;
    let mut json = false;
    let mut quiet = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--engine" => match p.value("--engine") {
                Ok(v) => match csvx_core::engine::Engine::parse(&v) {
                    Ok(e) => engine = e,
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
            "--key" => match p.value("--key") {
                Ok(v) => key = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--from" | "--reference" => match p.value("--from") {
                Ok(v) => reference = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--from-key" => match p.value("--from-key") {
                Ok(v) => reference_key = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--get" => match p.value("--get") {
                Ok(v) => take = v,
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
            "--quiet" | "-q" => quiet = true,
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
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
                print_command_help("lookup");
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
        eprintln!("error: lookup needs an <input> file");
        return 2;
    };
    let Some(key) = key else {
        eprintln!("error: lookup needs --key <column>");
        return 2;
    };
    let Some(reference) = reference else {
        eprintln!("error: lookup needs --from <reference.csv>");
        return 2;
    };
    if reference_key.trim().is_empty() {
        // Default: the reference key has the same name as the main key. That is
        // the common case, and demanding it explicitly is pointless ceremony.
        reference_key = key.clone();
    }
    if take.trim().is_empty() {
        eprintln!("error: lookup needs --get <columns from the reference>");
        return 2;
    }
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let output = out.unwrap_or_else(|| {
        let stem = input
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "lookup".to_string());
        input.with_file_name(format!("{stem}.lookup.csv"))
    });
    warn_pending_edits(&input);

    let options = csvx_core::lookup::LookupOptions {
        key,
        reference,
        reference_key,
        take: take.split(',').map(|s| s.trim().to_string()).collect(),
        max_keys,
    };
    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("lookup", rx, quiet || json);
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Err(code) = check_output(Some(&input), &output) {
        return code;
    }
    let result = {
        let planned = match csvx_core::engine::resolve(engine, &input) {
            Ok(e) => e,
            Err(message) => {
                eprintln!("error: {message}");
                return 2;
            }
        };
        match planned {
            csvx_core::engine::Engine::Sql => {
                csvx_core::engine::lookup_sql(&input, delimiter, &output, &options, &tx)
            }
            _ => {
                csvx_core::lookup::lookup_quotes(&input, delimiter, &output, &options, quotes, &tx)
            }
        }
    };
    drop(tx);
    let _ = reporter.join();
    let summary = match result {
        Ok(s) => s,
        Err(e) => return report_failure("lookup", e),
    };

    if json {
        println!(
            r#"{{"mainRows":{},"matched":{},"missing":{},"referenceKeys":{},"bytes":{},"columns":["{}"]}}"#,
            summary.main_rows,
            summary.matched,
            summary.missing,
            summary.reference_keys,
            summary.bytes,
            summary.columns.join(r#"",""#)
        );
    } else {
        println!(
            "✓ {} of {} rows matched ({} missing) → {} ({})",
            summary.matched,
            summary.main_rows,
            summary.missing,
            output.display(),
            human_bytes(summary.bytes)
        );
    }
    0
}

/// Columns as the sheet presents them (journal applied), for layout commands.
fn layout_columns(input: &std::path::Path) -> Result<Vec<String>, String> {
    let base_columns = csvx_core::workbook::base_header(input, csvx_core::types::Delimiter::Comma)
        .map_err(|e| format!("cannot read the header: {e}"))?;
    csvx_core::workbook::Journal::load(&csvx_core::workbook::Journal::sidecar(input))
        .map(|journal| {
            csvx_core::workbook::column_layout(journal.ops(), &base_columns)
                .map(|layout| layout.into_iter().map(|(name, _)| name).collect())
        })
        .unwrap_or_else(|_| Ok(base_columns))
        .map_err(|e: std::io::Error| e.to_string())
}

fn cmd_layout(args: &[String]) -> i32 {
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

fn cmd_chart(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut engine = csvx_core::engine::Engine::Auto;
    let mut apply_edits = false;
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut x: Option<String> = None;
    let mut series: Option<String> = None;
    let mut y = String::new();
    let mut kind = "line".to_string();
    let mut sort_by_value = false;
    let mut descending = false;
    let mut max_points = csvx_core::chart::DEFAULT_MAX_POINTS;
    let mut want_json = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--apply-edits" => apply_edits = true,
            "--json" => want_json = true,
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--engine" => match p.value("--engine") {
                Ok(v) => match csvx_core::engine::Engine::parse(&v) {
                    Ok(e) => engine = e,
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
            "--x" => match p.value("--x") {
                Ok(v) => x = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--series" | "--by" => match p.value("--series") {
                Ok(v) => series = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--y" => match p.value("--y") {
                Ok(v) => y = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--type" => match p.value("--type") {
                Ok(v) => kind = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--sort-by-value" => sort_by_value = true,
            "--desc" => descending = true,
            "--max-points" => match p
                .value("--max-points")
                .and_then(|v| v.parse().map_err(|_| "bad --max-points".to_string()))
            {
                Ok(v) => max_points = v,
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
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("chart");
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
        eprintln!("error: chart needs an <input> file");
        return 2;
    };
    warn_pending_edits_unless(&input, apply_edits);
    let Some(x) = x else {
        eprintln!("error: chart needs --x <column>");
        return 2;
    };
    if y.trim().is_empty() {
        eprintln!("error: chart needs --y <column:op>");
        return 2;
    }
    let spec = match csvx_core::aggregate::AggSpec::parse_list(&y) {
        Ok(mut v) if !v.is_empty() => v.remove(0),
        Ok(_) => {
            eprintln!("error: --y needs one column:op measure");
            return 2;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let chart_kind = match csvx_core::chart::ChartKind::parse(&kind) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
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
    let options = csvx_core::chart::ChartOptions {
        x,
        series,
        y: spec,
        kind: chart_kind,
        sort_by_value,
        descending,
        max_points,
    };

    let (tx, rx) = mpsc::channel();
    // Writing JSON to stdout means stderr must stay quiet enough to pipe.
    let reporter = spawn_reporter("chart", rx, out.is_none());
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let sheet = match sheet_path(&input, apply_edits, delimiter) {
        Ok(sheet) => sheet,
        Err(code) => return code,
    };
    let planned = match csvx_core::engine::resolve(engine, sheet.path()) {
        Ok(e) => e,
        Err(message) => {
            eprintln!("error: {message}");
            return 2;
        }
    };
    let result = match planned {
        csvx_core::engine::Engine::Sql => {
            csvx_core::engine::chart_sql(sheet.path(), delimiter, &options, &tx)
        }
        _ => csvx_core::chart::chart_quotes(sheet.path(), delimiter, &options, quotes, &tx),
    };
    drop(tx);
    let _ = reporter.join();
    let data = match result {
        Ok(d) => d,
        Err(e) => return report_failure("chart", e),
    };

    let json = data.to_json();
    match out {
        Some(path) => match std::fs::write(&path, &json) {
            Ok(()) => {
                println!(
                    "✓ {} categories × {} series → {}",
                    data.categories.len(),
                    data.series.len(),
                    path.display()
                );
                0
            }
            Err(e) => report_failure("chart", format!("cannot write {}: {e}", path.display())),
        },
        None => {
            if want_json {
                println!("{json}");
            } else {
                print_chart(&data);
            }
            0
        }
    }
}

fn cmd_format(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut rules_text: Vec<String> = Vec::new();
    let mut start: u64 = 0;
    let mut count: u64 = 100;
    let mut summary = false;
    let mut json = false;
    let mut quiet = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--rule" => match p.value("--rule") {
                Ok(v) => rules_text.push(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--start" => match p
                .value("--start")
                .and_then(|v| v.parse().map_err(|_| "bad --start".to_string()))
            {
                Ok(v) => start = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--count" => match p
                .value("--count")
                .and_then(|v| v.parse().map_err(|_| "bad --count".to_string()))
            {
                Ok(v) => count = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--summary" => summary = true,
            "--json" => json = true,
            "--quiet" | "-q" => quiet = true,
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("format");
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
        eprintln!("error: format needs an <input> file");
        return 2;
    };
    if rules_text.is_empty() {
        eprintln!("error: format needs at least one --rule \"column>value:bg=...\"");
        return 2;
    }
    let mut rules = Vec::new();
    for text in &rules_text {
        match csvx_core::format::Rule::parse(text) {
            Ok(rule) => rules.push(rule),
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
    let sidecar = csvx_core::workbook::Journal::sidecar(&input);
    let journal = match csvx_core::workbook::Journal::load(&sidecar) {
        Ok(j) => j,
        Err(e) => {
            return report_failure("format", format!("cannot read {}: {e}", sidecar.display()))
        }
    };

    if summary {
        // Whole-file census: which rule is too broad, before anyone sees colours.
        let (tx, rx) = mpsc::channel();
        let reporter = spawn_reporter("format", rx, quiet || json);
        let result =
            csvx_core::format::count_matches(&input, journal.ops(), &rules, delimiter, &tx);
        drop(tx);
        let _ = reporter.join();
        let summary = match result {
            Ok(s) => s,
            Err(_) => return 1,
        };
        if json {
            let counts: Vec<String> = summary.matches.iter().map(|n| n.to_string()).collect();
            println!(
                r#"{{"rows":{},"rules":[{}],"matches":[{}]}}"#,
                summary.rows,
                rules
                    .iter()
                    .map(|r| r.to_json())
                    .collect::<Vec<_>>()
                    .join(","),
                counts.join(",")
            );
        } else {
            println!("{} rows scanned", summary.rows);
            for (rule, hits) in rules.iter().zip(&summary.matches) {
                let pct = if summary.rows == 0 {
                    0.0
                } else {
                    *hits as f64 / summary.rows as f64 * 100.0
                };
                println!("  {:>6} rows ({pct:.1}%)  {}", hits, rule.source);
            }
        }
        return 0;
    }

    note_index_build(&input);
    let cells = match csvx_core::format::styled_window(
        &input,
        journal.ops(),
        &rules,
        start,
        count,
        delimiter,
    ) {
        Ok(c) => c,
        Err(e) => return report_failure("format", e),
    };

    if json {
        let items: Vec<String> = cells.iter().map(|c| c.to_json()).collect();
        println!(
            r#"{{"start":{start},"count":{},"cells":[{}]}}"#,
            cells.len(),
            items.join(",")
        );
    } else {
        println!(
            "{} cell(s) match in rows {start}..{}",
            cells.len(),
            start + count
        );
        for cell in cells.iter().take(40) {
            println!(
                "  row {:>6}  {:<12} = {:<20} → {}",
                cell.row + 1,
                cell.column,
                cell.value,
                cell.style.to_json()
            );
        }
        if cells.len() > 40 {
            println!("  … and {} more", cells.len() - 40);
        }
    }
    0
}

fn cmd_pivot(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut apply_edits = false;
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut rows: Vec<String> = Vec::new();
    let mut columns: Option<String> = None;
    let mut values = String::new();
    let mut order: Option<String> = None;
    let mut descending = false;
    let mut qualify = false;
    let mut max_columns = csvx_core::pivot::DEFAULT_MAX_COLUMNS;
    let mut max_cells = csvx_core::pivot::DEFAULT_MAX_CELLS;
    let mut engine = csvx_core::engine::Engine::Auto;
    let mut json = false;
    let mut delim = "auto".to_string();
    let mut quiet = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--engine" => match p.value("--engine") {
                Ok(v) => match csvx_core::engine::Engine::parse(&v) {
                    Ok(e) => engine = e,
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
            "--rows" => match p.value("--rows") {
                Ok(v) => rows = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--cols" | "--columns" => match p.value("--cols") {
                Ok(v) => columns = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--values" => match p.value("--values") {
                Ok(v) => values = v,
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
            "--qualify" => qualify = true,
            "--max-columns" => match p
                .value("--max-columns")
                .and_then(|v| v.parse().map_err(|_| "bad --max-columns".to_string()))
            {
                Ok(v) => max_columns = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--max-cells" => match p
                .value("--max-cells")
                .and_then(|v| v.parse().map_err(|_| "bad --max-cells".to_string()))
            {
                Ok(v) => max_cells = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--apply-edits" => apply_edits = true,
            "--json" => json = true,
            "--quiet" | "-q" => quiet = true,
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
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
                print_command_help("pivot");
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
        eprintln!("error: pivot needs an <input> file");
        return 2;
    };
    warn_pending_edits_unless(&input, apply_edits);
    if rows.is_empty() {
        eprintln!("error: pivot needs --rows <column[,column]>");
        return 2;
    }
    if values.trim().is_empty() {
        eprintln!("error: pivot needs --values <column:op[,...]>");
        return 2;
    }
    let specs = match csvx_core::aggregate::AggSpec::parse_list(&values) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
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
    // Keep the requested path, if any: printing to the terminal is the default.
    let out_was_given: Option<PathBuf> = out;
    let options = csvx_core::pivot::PivotOptions {
        rows,
        columns,
        values: specs,
        order_by: order,
        descending,
        qualify_names: qualify,
        max_columns,
        max_cells,
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("pivot", rx, quiet || json);
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Some(out) = &out_was_given {
        if let Err(code) = check_output(Some(&input), out) {
            return code;
        }
    }
    let sheet = match sheet_path(&input, apply_edits, delimiter) {
        Ok(sheet) => sheet,
        Err(code) => return code,
    };
    // Without `-o` the matrix goes to the terminal: a pivot is something you look at,
    // and writing a file only to open it again is a step nobody asked for.
    let table = if let Some(output) = out_was_given.clone() {
        let planned = match csvx_core::engine::resolve(engine, sheet.path()) {
            Ok(e) => e,
            Err(message) => {
                eprintln!("error: {message}");
                return 2;
            }
        };
        let result = match planned {
            csvx_core::engine::Engine::Sql => {
                csvx_core::engine::pivot_sql(sheet.path(), delimiter, &output, &options, &tx)
            }
            _ => csvx_core::pivot::pivot_quotes(
                sheet.path(),
                delimiter,
                &output,
                &options,
                quotes,
                &tx,
            ),
        };
        drop(tx);
        let _ = reporter.join();
        match result {
            Ok(summary) => Some((summary, None)),
            // The engine returns the error itself; nothing else printed it.
            Err(e) => return report_failure("pivot", e),
        }
    } else {
        let result = csvx_core::pivot::pivot_rows(sheet.path(), delimiter, &options, quotes, &tx);
        drop(tx);
        let _ = reporter.join();
        match result {
            Ok(table) => Some((table.summary.clone(), Some(table))),
            Err(e) => return report_failure("pivot", e),
        }
    };

    let Some((summary, table)) = table else {
        return 1;
    };

    if json {
        println!(
            r#"{{"inputRows":{},"rowKeys":{},"columnKeys":{},"bytes":{},"rowColumns":["{}"],"columnValues":["{}"],"measures":["{}"]}}"#,
            summary.input_rows,
            summary.row_keys,
            summary.column_keys,
            summary.bytes,
            summary.row_columns.join(r#"",""#),
            summary.column_values.join(r#"",""#),
            summary.measures.join(r#"",""#)
        );
    } else if let Some(table) = table {
        print_table(&table.header, &table.rows);
        println!(
            "\n({} row(s) × {} column(s), {} input rows)",
            summary.row_keys, summary.column_keys, summary.input_rows
        );
    } else {
        println!(
            "✓ {} rows × {} columns → {} ({}, {} cells)",
            summary.row_keys,
            summary.column_values.len() * summary.measures.len(),
            out_was_given
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            human_bytes(summary.bytes),
            summary.row_keys * (summary.column_keys * summary.measures.len() as u64)
        );
    }
    0
}

/// Cut a value for one-line display, with an ellipsis when it was cut.
fn truncate_for_display(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// A quality gate for scripts and pipelines: exit 1 when a threshold fails.
///
/// Thresholds are counts, not ratios: "at least 100 rows, at most 5 empty cells,
/// column id unique". The gate writes its own JSON report when `--out` is used, so
/// a pipeline step can keep the evidence while a failing exit stops the run.
fn cmd_gate(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut min_rows: Option<u64> = None;
    let mut max_empty: Option<u64> = None;
    let mut unique: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut json = false;
    let mut delim = "auto".to_string();
    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--min-rows" => match p
                .value("--min-rows")
                .and_then(|v| v.parse().map_err(|_| "bad --min-rows".to_string()))
            {
                Ok(v) => min_rows = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--max-empty" => match p
                .value("--max-empty")
                .and_then(|v| v.parse().map_err(|_| "bad --max-empty".to_string()))
            {
                Ok(v) => max_empty = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--unique" => match p.value("--unique") {
                Ok(v) => unique = Some(v),
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
                print_command_help("gate");
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
        eprintln!("error: gate needs <input>");
        return 2;
    };
    if min_rows.is_none() && max_empty.is_none() && unique.is_none() {
        eprintln!("error: gate needs at least one of --min-rows, --max-empty, --unique");
        return 2;
    }
    if let Some(out) = &out {
        if out == &input {
            eprintln!("error: --out must not be the input file");
            return 2;
        }
    }
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let sheet =
        match csvx_core::sheet::Sheet::open(&input, csvx_core::sheet::ReadOptions { delimiter }) {
            Ok(sheet) => sheet,
            Err(e) => return report_failure("gate", e.to_string()),
        };
    let total = match sheet.rows() {
        Ok(rows) => rows,
        Err(e) => return report_failure("gate", e.to_string()),
    };
    let columns = match sheet.columns() {
        Ok(columns) => columns,
        Err(e) => return report_failure("gate", e.to_string()),
    };
    let unique_index = match &unique {
        None => None,
        Some(name) => match sheet.column_index(name) {
            Ok(Some(index)) => Some(index - 1),
            Ok(None) => {
                eprintln!("error: --unique names unknown column {name:?}");
                return 2;
            }
            Err(e) => return report_failure("gate", e.to_string()),
        },
    };
    // A uniqueness check over 100 GB cannot hold every value in memory. Sort the
    // edited sheet by that column once, then adjacent duplicates are exact and the
    // working set is whatever the external sorter budgets. The other metrics reuse
    // the sorted stream, so the file is read once for the gate.
    let mut sorted_path: Option<PathBuf> = None;
    let mut _materialized = None;
    let metrics_sheet = if let Some(name) = &unique {
        if total == 0 {
            sheet
        } else {
            let (tx, rx) = mpsc::channel();
            drop(rx);
            let materialized = match csvx_core::sheet::materialize(
                &input,
                &csvx_core::sheet::ReadOptions { delimiter },
                &tx,
            ) {
                Ok(value) => value,
                Err(e) => return report_failure("gate", e.to_string()),
            };
            let source = materialized.path().to_path_buf();
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let sorted = std::env::temp_dir().join(format!(
                "csvx-gate-{}-{}-{stamp}.csv",
                std::process::id(),
                name
            ));
            let result = csvx_core::sort::external_sort_columns(
                &source,
                &sorted,
                &[csvx_core::sort::SortColumn::new(name.clone(), false)],
                delimiter,
                256,
                &tx,
            );
            drop(tx);
            if let Err(e) = result {
                let _ = std::fs::remove_file(&sorted);
                return report_failure("gate", format!("cannot sort for uniqueness: {e}"));
            }
            let sorted_sheet = match csvx_core::sheet::Sheet::open(
                &sorted,
                csvx_core::sheet::ReadOptions { delimiter },
            ) {
                Ok(value) => value,
                Err(e) => {
                    let _ = std::fs::remove_file(&sorted);
                    return report_failure("gate", e.to_string());
                }
            };
            sorted_path = Some(sorted);
            _materialized = Some(materialized);
            drop(sheet);
            sorted_sheet
        }
    } else {
        sheet
    };
    let mut empty: u64 = 0;
    let mut seen: u64 = 0;
    let mut unique_values: u64 = 0;
    let mut unique_duplicate = false;
    let mut previous_unique: Option<String> = None;
    let mut start: u64 = 0;
    let chunk = 5_000u64;
    while start < total {
        let window = match metrics_sheet.window(start, chunk) {
            Ok(window) => window,
            Err(e) => return report_failure("gate", e.to_string()),
        };
        if window.rows.is_empty() {
            break;
        }
        for row in &window.rows {
            seen += 1;
            empty += (columns.len().saturating_sub(row.len())) as u64;
            for cell in row.iter().take(columns.len()) {
                if cell.is_empty() {
                    empty += 1;
                }
            }
            if let Some(index) = unique_index {
                let value = row.get(index).cloned().unwrap_or_default();
                if previous_unique.as_deref() == Some(value.as_str()) {
                    unique_duplicate = true;
                } else {
                    unique_values += 1;
                    previous_unique = Some(value);
                }
            }
        }
        start += window.rows.len() as u64;
    }
    if let Some(path) = &sorted_path {
        let _ = std::fs::remove_file(path);
    }
    let min_ok = min_rows.is_none_or(|minimum| total >= minimum);
    let empty_ok = max_empty.is_none_or(|maximum| empty <= maximum);
    let unique_ok = unique
        .as_ref()
        .is_none_or(|_| !unique_duplicate && unique_values == seen);
    let passed = min_ok && empty_ok && unique_ok;
    if let Some(path) = &out {
        let mut text = String::from("{\"passed\":");
        text.push_str(if passed { "true" } else { "false" });
        text.push_str(&format!(
            ",\"rows\":{total},\"empty\":{empty},\"uniqueValues\":{},\"uniqueDuplicates\":{},\"checks\":{{\"minRows\":{:?},\"maxEmpty\":{:?},\"unique\":{:?}}}",
            unique_values,
            unique_duplicate,
            min_rows,
            max_empty,
            unique.as_deref().map(json_escape)
        ));
        if let Err(e) = std::fs::write(path, format!("{text}\n")) {
            eprintln!("error: cannot write {}: {e}", path.display());
            return 2;
        }
    }
    if json {
        println!(
            "{{\"passed\":{},\"rows\":{},\"empty\":{},\"uniqueValues\":{},\"uniqueDuplicates\":{}}}",
            passed,
            total,
            empty,
            unique_values,
            unique_duplicate
        );
    } else {
        println!(
            "gate: rows {total}, empty {empty}, unique {}",
            if unique.is_some() {
                format!(
                    "{}{}",
                    unique_values,
                    if unique_duplicate { "+" } else { "" }
                )
            } else {
                "-".to_string()
            }
        );
        if !min_ok {
            eprintln!("✗ rows {total} < minimum {}", min_rows.unwrap_or(0));
        }
        if !empty_ok {
            eprintln!("✗ empty {empty} > maximum {}", max_empty.unwrap_or(0));
        }
        if !unique_ok {
            eprintln!("✗ unique column has duplicates");
        }
        if passed {
            println!("✓ gate passed");
        }
    }
    if passed {
        0
    } else {
        1
    }
}

fn cmd_validate(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut limit: Option<u64> = None;
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut rules_text: Vec<String> = Vec::new();
    let mut json = false;
    let mut quiet = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--check" | "--rule" => match p.value("--check") {
                Ok(v) => rules_text.push(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--checks" => match p.value("--checks") {
                Ok(v) => rules_text.extend(v.split(';').map(|r| r.trim().to_string())),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--limit" => match p
                .value("--limit")
                .and_then(|v| v.parse::<u64>().map_err(|_| "bad --limit".to_string()))
            {
                Ok(v) if v > 0 => limit = Some(v),
                Ok(_) => {
                    eprintln!("error: --limit is a positive row count");
                    return 2;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "-q" | "--quiet" => quiet = true,
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
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
                print_command_help("validate");
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
        eprintln!("error: validate needs a file");
        return 2;
    };
    if rules_text.is_empty() {
        eprintln!("error: validate needs at least one --check (e.g. --check \"qty>=0\")");
        return 2;
    }
    let rules: Vec<csvx_core::validate::Rule> = {
        let mut parsed = Vec::new();
        for text in &rules_text {
            match csvx_core::validate::Rule::parse(text) {
                Ok(rule) => parsed.push(rule),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            }
        }
        parsed
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
    let ops =
        match csvx_core::workbook::Journal::load(&csvx_core::workbook::Journal::sidecar(&input)) {
            Ok(journal) => journal.ops().to_vec(),
            Err(e) => return report_failure("validate", e.to_string()),
        };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("validate", rx, quiet || json);
    let result = csvx_core::validate::validate_limited(&input, &ops, &rules, delimiter, limit, &tx);
    drop(tx);
    let _ = reporter.join();
    let _ = quotes;
    let report = match result {
        Ok(r) => r,
        Err(e) => return report_failure("validate", e),
    };

    if json {
        println!("{}", report.to_json());
        return if report.failed() { 1 } else { 0 };
    }

    let failed = report.failed();
    for (rule, count) in &report.counts {
        let mark = if *count == 0 { "✓" } else { "✗" };
        println!("{mark} {rule}: {count} violation(s)");
    }
    if report.partial {
        println!(
            "! checked {} of {} row(s) (--limit); the counts describe that prefix only",
            report.checked, report.rows
        );
    }
    if report.unique_full {
        println!(
            "✗ unique ran out of room after {} distinct values; raise the cap or check a narrower range",
            csvx_core::validate::MAX_UNIQUE_VALUES
        );
    }
    if !report.violations.is_empty() {
        println!();
        for violation in report.violations.iter().take(20) {
            println!(
                "  row {}  {}  ({})  value: {}",
                violation.row,
                violation.column,
                violation.rule,
                truncate_for_display(&violation.value, 40)
            );
        }
        if report.violations.len() > 20 {
            println!("  ... and {} more", report.violations.len() - 20);
        }
        if report.truncated {
            println!(
                "  (violations cap reached at {}; counts above are exact)",
                csvx_core::validate::MAX_VIOLATIONS
            );
        }
    }
    println!(
        "\n{} rows checked{}, {} violation(s)",
        report.checked,
        if report.partial {
            format!(" of {} (--limit)", report.rows)
        } else {
            String::new()
        },
        report.total()
    );
    if failed {
        1
    } else {
        0
    }
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
                print_command_help("demo");
                return 0;
            }
            other => out = Some(PathBuf::from(other)),
        }
    }

    let dir = out.unwrap_or_else(|| PathBuf::from("csvx-demo"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("error: cannot create {}: {e}", dir.display());
        return 1;
    }
    let path = dir.join("orders.csv");
    let bytes = csvx_core::demo::write_sample(&path);
    match bytes {
        Ok((rows, bytes)) => {
            println!(
                "✓ wrote {} ({rows} data rows, {})",
                path.display(),
                human_bytes(bytes)
            );
            println!();
            println!("  Try the 10-minute tour:");
            println!("    csvx preview {}", path.display());
            println!("    csvx analyze {}", path.display());
            println!(
                "    csvx filter {} --where \"city=CDMX\" --where \"amount>1000\" -o cdmx.csv",
                path.display()
            );
            println!("    csvx groupby {} --by city --agg \"amount:sum,amount:count\" --order sum_amount --desc", path.display());
            println!();
            println!("  Or serve it to a front-end:");
            println!("    csvx serve --root {}", dir.display());
            0
        }
        Err(e) => report_failure("pivot", e), // engine returned the error itself
    }
}

fn cmd_groupby(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut apply_edits = false;
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut keys: Vec<String> = Vec::new();
    let mut agg_spec = String::new();
    let mut order: Option<String> = None;
    let mut descending = false;
    let mut max_groups = csvx_core::groupby::DEFAULT_MAX_GROUPS;
    let mut json = false;
    let mut delim = "auto".to_string();
    let mut engine = csvx_core::engine::Engine::Auto;
    let mut quiet = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--by" => match p.value("--by") {
                Ok(v) => keys = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--engine" => match p.value("--engine") {
                Ok(v) => match csvx_core::engine::Engine::parse(&v) {
                    Ok(e) => engine = e,
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
            "--apply-edits" => apply_edits = true,
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
                print_command_help("groupby");
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
    warn_pending_edits_unless(&input, apply_edits);
    if keys.is_empty() {
        eprintln!("error: groupby needs --by <col[,col]>");
        return 2;
    }
    let aggs = match csvx_core::groupby::AggSpec::parse_list(&agg_spec) {
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
    let options = csvx_core::groupby::GroupOptions {
        keys,
        aggregates: aggs,
        order_by: order,
        descending,
        max_groups,
    };
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Err(code) = check_output(Some(&input), &output) {
        return code;
    }
    let sheet = match sheet_path(&input, apply_edits, delimiter) {
        Ok(sheet) => sheet,
        Err(code) => return code,
    };
    // `auto` picks DuckDB for large inputs, native for small ones. Below the threshold
    // the engine's start-up costs more than the query itself: 0.207 s vs 0.026 s on the
    // 20 120-row demo, and 0.9 s vs 13.1 s on the 2.5 GB fixture.
    let planned = match csvx_core::engine::resolve(engine, sheet.path()) {
        Ok(e) => e,
        Err(message) => {
            eprintln!("error: {message}");
            return 2;
        }
    };
    let result = match planned {
        csvx_core::engine::Engine::Sql => {
            csvx_core::engine::groupby_sql(sheet.path(), delimiter, &output, &options, &tx)
        }
        _ => csvx_core::groupby::groupby_quotes(
            sheet.path(),
            delimiter,
            &output,
            &options,
            quotes,
            &tx,
        ),
    };
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
        Err(e) => report_failure("groupby", e), // engine returned the error itself
    }
}

fn cmd_concat(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut out: Option<PathBuf> = None;
    let mut ignore_header = false;
    let mut by_name = false;
    let mut engine = csvx_core::engine::Engine::Auto;
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
            "--engine" => match p.value("--engine") {
                Ok(v) => match csvx_core::engine::Engine::parse(&v) {
                    Ok(e) => engine = e,
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
            "--ignore-header" => ignore_header = true,
            "--by-name" => by_name = true,
            "--json" => json = true,
            "-d" | "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("concat");
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

    // `--by-name` aligns same-named columns first, so files exported in a different column
    // order concatenate instead of failing on a header mismatch.
    let aligned_guard;
    let aligned: Vec<PathBuf> = if by_name {
        match csvx_core::concat::align_by_name(&inputs, delimiter) {
            Ok(guard) => {
                aligned_guard = guard;
                aligned_guard.paths().to_vec()
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    } else {
        inputs.clone()
    };
    let planned = match csvx_core::engine::resolve_for_copy(engine, &inputs[0]) {
        Ok(e) => e,
        Err(message) => {
            eprintln!("error: {message}");
            return 2;
        }
    };
    let result = match planned {
        csvx_core::engine::Engine::Sql => {
            csvx_core::engine::concat_sql(&aligned, &output, delimiter, ignore_header, &tx)
        }
        _ => csvx_core::concat::concat(&aligned, &output, delimiter, ignore_header, &tx),
    };
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
        Err(e) => report_failure("chart", e), // engine returned the error itself
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
        csvx_core::partition::sanitize(by)
    )))
}

/// Remind the user when a read-only command is about to ignore recorded edits.
///
/// Filtering a file that has a journal answers with pre-edit bytes, which is
/// correct but surprising. Saying so costs one line; staying silent costs someone
/// an hour of "why does this row still say CDMX".
fn warn_pending_edits(input: &std::path::Path) {
    warn_pending_edits_unless(input, false)
}

/// The same warning, suppressed when the caller applies the edits itself.
fn warn_pending_edits_unless(input: &std::path::Path, applied: bool) {
    if applied {
        return;
    }
    let sidecar = csvx_core::workbook::Journal::sidecar(input);
    if let Ok(journal) = csvx_core::workbook::Journal::load(&sidecar) {
        if !journal.is_empty() {
            eprintln!(
                "note: {} pending edit(s) in {} are not applied by this command; run `csvx edit export {}` to materialise them",
                journal.len(),
                sidecar.display(),
                input.display()
            );
        }
    }
}

/// The `--quotes` value for a command, or the RFC default.
fn parse_quotes(text: &str) -> Result<csvx_core::csv_reader::QuoteStyle, String> {
    csvx_core::csv_reader::QuoteStyle::parse(text)
}

/// Record one operation, save the journal, and report it the way other edits do.
fn journal_push(
    journal: &mut csvx_core::workbook::Journal,
    sidecar: &std::path::Path,
    op: csvx_core::workbook::Op,
) -> Result<i32, i32> {
    let description = op.describe();
    // A single edit ends any transaction group; its boundary must not be mistaken for
    // this operation's undo unit later.
    csvx_core::workbook::clear_blocks(sidecar);
    // A concurrent writer may have appended since this journal was loaded, so the
    // append happens under a lock and on a freshly loaded journal: two edits become
    // two operations instead of one lost rename.
    let outcome = csvx_core::workbook::Journal::update_locked(sidecar, |fresh| {
        fresh.push(op);
        Ok(())
    });
    match outcome {
        Ok(fresh) => {
            *journal = fresh;
            println!("✓ {description}");
            Ok(0)
        }
        Err(e) => {
            report_failure("edit", format!("cannot save the journal: {e}"));
            Err(1)
        }
    }
}

/// Draw a chart in the terminal: one line per category, bars scaled to the widest.
///
/// This is the "look at it" counterpart of the JSON the API returns — enough to answer
/// "which category is biggest", without a front-end or a file.
fn print_chart(data: &csvx_core::chart::ChartData) {
    if data.categories.is_empty() {
        println!("(no data to chart)");
        return;
    }
    let widest: usize = data
        .categories
        .iter()
        .map(|c| display_width(c))
        .max()
        .unwrap_or(0)
        .min(28);
    let label = |category: &str| {
        let mut text: String = category.chars().take(28).collect();
        let pad = widest.saturating_sub(display_width(&text));
        text.push_str(&" ".repeat(pad));
        text
    };
    for series in &data.series {
        if data.series.len() > 1 {
            println!("{}\n", series.name);
        }
        let peak = series
            .points
            .iter()
            .flatten()
            .fold(0.0f64, |acc, v| acc.max(v.abs()));
        let scale = if peak > 0.0 { 40.0 / peak } else { 0.0 };
        for (index, category) in data.categories.iter().enumerate() {
            let value = series.points.get(index).copied().flatten();
            let bar = match value {
                None => "(no data)".to_string(),
                Some(v) => {
                    let width = ((v.abs() * scale).round() as usize).min(40);
                    let glyphs = if v < 0.0 { "-" } else { "\u{2588}" };
                    format!("{} {}", glyphs.repeat(width.max(1)), format_number_short(v))
                }
            };
            println!("{} {}", label(category), bar);
        }
    }
    println!(
        "\n({} categories × {} series, {} rows)",
        data.categories.len(),
        data.series.len(),
        data.rows
    );
}

/// Numbers in a chart are read at a glance: two decimals at most, thousands grouped.
fn format_number_short(value: f64) -> String {
    if value.abs() >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value.abs() >= 10_000.0 {
        format!("{:.1}k", value / 1_000.0)
    } else if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value:.2}")
    }
}

/// Print a table, marking cells whose conditional-formatting rule matched.
///
/// Terminals get colour when they support it; a pipe gets brackets, because colour
/// codes in a captured file are noise and the marker still says which cell matched.
/// Narrow the printed rows to the columns the caller asked for.
///
/// A real file can have 782 columns; the terminal shows twenty, and `--columns` is how
/// someone reads the rest. Unknown names fail with the columns that do exist.
fn project_columns(
    columns: &mut Vec<String>,
    rows: &mut [Vec<String>],
    wanted: &[String],
) -> Result<(), i32> {
    if wanted.is_empty() {
        return Ok(());
    }
    let mut slots = Vec::with_capacity(wanted.len());
    for name in wanted {
        match columns.iter().position(|c| c == name) {
            Some(slot) => slots.push(slot),
            None => {
                eprintln!(
                    "error: column '{name}' not found; available: {}",
                    columns.join(", ")
                );
                return Err(2);
            }
        }
    }
    let kept: Vec<String> = slots.iter().map(|i| columns[*i].clone()).collect();
    for row in rows.iter_mut() {
        // Take the old row, then build the new one from it: clearing in place would
        // destroy the cells the selection reads.
        let original = std::mem::take(row);
        *row = slots
            .iter()
            .map(|slot| original.get(*slot).cloned().unwrap_or_default())
            .collect();
    }
    *columns = kept;
    Ok(())
}

/// How many columns a terminal table shows before it stops.
///
/// A real file can have 782 columns; printing them all produces a screen of noise in
/// which the first three columns — the ones anyone reads — are lost. The hidden count
/// is printed underneath, so nothing is silently missing.
const MAX_DISPLAY_COLUMNS: usize = 20;

/// Narrow the columns and rows to what is worth showing, and say what was hidden.
fn fit_to_terminal(
    columns: &[String],
    rows: &[Vec<String>],
) -> (Vec<String>, Vec<Vec<String>>, usize) {
    if columns.len() <= MAX_DISPLAY_COLUMNS {
        return (columns.to_vec(), rows.to_vec(), 0);
    }
    let shown = MAX_DISPLAY_COLUMNS;
    let kept_columns = columns[..shown].to_vec();
    let kept_rows = rows
        .iter()
        .map(|row| row[..row.len().min(shown)].to_vec())
        .collect();
    (kept_columns, kept_rows, columns.len() - shown)
}

fn note_hidden_columns(hidden: usize) {
    if hidden > 0 {
        println!("(+{hidden} more column(s) not shown; use --columns or --json to see them all)");
    }
}

fn print_table_styled(
    columns: &[String],
    rows: &[Vec<String>],
    start: u64,
    marks: &[(u64, String, csvx_core::format::Style)],
) {
    use std::io::IsTerminal;
    let (columns, rows, hidden) = fit_to_terminal(columns, rows);
    let columns = &columns;
    let rows = &rows;
    let colour = std::io::stdout().is_terminal();
    let mut widths: Vec<usize> = columns.iter().map(|c| display_width(c)).collect();
    for (row_index, row) in rows.iter().enumerate() {
        let logical = start + row_index as u64;
        for (slot, cell) in row.iter().enumerate() {
            let mut width = display_width(cell);
            if marks
                .iter()
                .any(|(row, column, _)| *row == logical && columns.get(slot) == Some(column))
            {
                width += if colour { 0 } else { 2 };
            }
            if let Some(w) = widths.get_mut(slot) {
                *w = (*w).max(width);
            }
        }
    }
    let render = |cells: &[String], logical: u64| {
        let mut line = String::new();
        for (slot, cell) in cells.iter().enumerate() {
            if slot > 0 {
                line.push_str(" | ");
            }
            let styled = marks
                .iter()
                .find(|(row, column, _)| *row == logical && columns.get(slot) == Some(column));
            let shown = match styled {
                None => cell.clone(),
                Some((_, _, style)) => {
                    if colour {
                        let mut code = String::from("\u{1b}[");
                        if style.bold {
                            code.push_str("1;");
                        }
                        if let Some(bg) = &style.background {
                            code.push_str(&ansi_colour(bg, false));
                        }
                        if let Some(fg) = &style.foreground {
                            code.push_str(&ansi_colour(fg, true));
                        }
                        code.push('m');
                        format!("{code}{cell}\u{1b}[0m")
                    } else {
                        format!("[{cell}]")
                    }
                }
            };
            line.push_str(&shown);
            let pad = widths.get(slot).copied().unwrap_or(0) - display_width(&shown);
            if slot + 1 < cells.len() {
                line.push_str(&" ".repeat(pad));
            }
        }
        line
    };
    println!("{}", render(columns, u64::MAX));
    let total: usize = widths.iter().sum::<usize>() + 3 * widths.len().saturating_sub(1);
    println!("{}", "-".repeat(total.max(20)));
    for (row_index, row) in rows.iter().enumerate() {
        println!("{}", render(row, start + row_index as u64));
    }
    if !marks.is_empty() {
        println!("\n{} cell(s) matched the formatting rule(s)", marks.len());
    }
    note_hidden_columns(hidden);
}

/// Minimal colour mapping: named colours and `#rrggbb`.
fn ansi_colour(name: &str, foreground: bool) -> String {
    let lower = name.trim().to_ascii_lowercase();
    let code = match lower.as_str() {
        "black" => Some("30"),
        "red" => Some("31"),
        "green" => Some("32"),
        "yellow" => Some("33"),
        "blue" => Some("34"),
        "magenta" => Some("35"),
        "cyan" => Some("36"),
        "white" => Some("37"),
        "grey" | "gray" => Some("90"),
        _ if lower.starts_with('#') && lower.len() == 7 => {
            let r = u8::from_str_radix(&lower[1..3], 16).ok();
            let g = u8::from_str_radix(&lower[3..5], 16).ok();
            let b = u8::from_str_radix(&lower[5..7], 16).ok();
            return match (r, g, b) {
                (Some(r), Some(g), Some(b)) => {
                    format!("{};2;{r};{g};{b};", if foreground { "38" } else { "48" })
                }
                _ => String::new(),
            };
        }
        _ => None,
    };
    match (code, foreground) {
        (Some(c), _) => format!("{c};"),
        (None, _) => "39;".to_string(),
    }
}

/// Say so before building a row index, because it is not instant on a big file.
///
/// A missing `.idx` is created on first use — fine for 50 MB, minutes for 10 GB. The
/// worst outcome is a silent wait: the user presses Enter, nothing happens, and they
/// assume it hung.
fn note_index_build(input: &Path) {
    let sidecar = csvx_core::index::SparseIndex::sidecar_path(input);
    if sidecar.exists() {
        return;
    }
    let size = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    if size < 50 * 1024 * 1024 {
        return;
    }
    eprintln!(
        "note: building a row index for {} ({}) — one-time; run `csvx index` earlier to avoid the wait",
        input.display(),
        human_bytes(size)
    );
}

/// Refuse to write over the input file, and say so when overwriting anything else.
///
/// Overwriting an output is normal (rerunning a pipeline), but writing to the file
/// being read is data loss, and it is easy to do by accident when the names differ by
/// one character. So: the input is protected, everything else is announced.
fn check_output(input: Option<&Path>, output: &Path) -> Result<(), i32> {
    let same = match (input, output.canonicalize()) {
        (Some(input), Ok(out)) => input.canonicalize().map(|i| i == out).unwrap_or(false),
        _ => false,
    };
    if same {
        report_failure(
            "output",
            format!(
                "refusing to write over the input file {} (it is being read right now)",
                output.display()
            ),
        );
        eprintln!("hint: choose a different -o path; the input is never modified by design");
        return Err(1);
    }
    if output.exists() {
        eprintln!("note: overwriting existing {}", output.display());
    }
    Ok(())
}

/// Warn when a file looks like it uses a different quote escape than RFC 4180.
///
/// Reading such a file as RFC does not fail — it silently merges or splits fields —
/// so the only defence is telling the user before they act on the numbers.
fn warn_quote_style(input: &std::path::Path) {
    let Ok(file) = csvx_core::encoding::open_decoded(input) else {
        return;
    };
    use std::io::Read;
    // Read a window, never the whole file: this runs before every preview and
    // analyze, and a 10 GB file would otherwise be paged in for one warning.
    let mut head = Vec::with_capacity(64 * 1024);
    if file.take(64 * 1024).read_to_end(&mut head).is_ok() {
        if let Some(style) = csvx_core::csv_reader::detect_quote_style(&head) {
            eprintln!(
                "note: this file looks like it escapes quotes with a backslash ({}); if fields look wrong, re-run with `--quotes backslash`",
                style.name()
            );
        }
    }
}

/// A path the analysis commands should read: the base file, or a materialised copy.
///
/// `groupby` / `pivot` / `chart` read the file directly, so a formula column that only
/// exists in the journal is invisible to them. `--apply-edits` exports the sheet to a
/// temporary file first, which makes "add a column, then group by it" one step instead
/// of two. The caller keeps the handle so the file is removed on the way out.
struct SheetPath {
    path: PathBuf,
    /// Kept alive so the materialised file is removed when the command ends.
    #[allow(dead_code)]
    temporary: Option<csvx_core::tempdir::TempDir>,
}

impl SheetPath {
    fn path(&self) -> &Path {
        &self.path
    }
}

fn sheet_path(input: &Path, apply_edits: bool, delimiter: Delimiter) -> Result<SheetPath, i32> {
    if !apply_edits {
        return Ok(SheetPath {
            path: input.to_path_buf(),
            temporary: None,
        });
    }
    let ops =
        match csvx_core::workbook::Journal::load(&csvx_core::workbook::Journal::sidecar(input)) {
            Ok(journal) => journal.ops().to_vec(),
            Err(e) => return Err(report_failure("edit", e.to_string())),
        };
    if ops.is_empty() {
        // Nothing to apply: reading the base file is the same thing, and cheaper.
        return Ok(SheetPath {
            path: input.to_path_buf(),
            temporary: None,
        });
    }
    let dir = match csvx_core::tempdir::TempDir::new("applied") {
        Ok(dir) => dir,
        Err(e) => return Err(report_failure("temp", e.to_string())),
    };
    let target = dir.path().join("sheet.csv");
    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("export", rx, true);
    let result = csvx_core::workbook::export(input, &ops, &target, delimiter, &tx);
    drop(tx);
    let _ = reporter.join();
    if let Err(e) = result {
        return Err(report_failure(
            "edit",
            format!("cannot apply the edits: {e}"),
        ));
    }
    Ok(SheetPath {
        path: target,
        temporary: Some(dir),
    })
}

/// Say when the journal lost records on the way in.
///
/// A journal cut off mid-write silently loses its last operation, which reads as "my
/// edit vanished". Naming it costs one line and turns a mystery into an explanation.
fn warn_journal_damage(input: &std::path::Path) {
    let sidecar = csvx_core::workbook::Journal::sidecar(input);
    if let Ok((_, skipped, skipped_redo)) = csvx_core::workbook::Journal::load_reporting(&sidecar) {
        for (count, file) in [
            (skipped, sidecar.clone()),
            (skipped_redo, sidecar.with_extension("journal.redo")),
        ] {
            if count > 0 {
                eprintln!(
                    "warning: {count} record(s) in {} could not be read and were ignored \
                     (a journal cut off mid-write looks like this)",
                    file.display()
                );
            }
        }
    }
}

/// Compare-and-set: refuse to overwrite a cell whose current value is not what the
/// caller expected.
///
/// A front-end that edits a window it fetched earlier can race with another writer; a
/// blind `set` would then replace a value nobody looked at. `--expect` turns the write
/// into "change x to y only if it is still x", and says what it found when it is not.
fn check_expect(
    input: &Path,
    delimiter: Delimiter,
    row: u64,
    column: &str,
    expect: &str,
) -> Result<(), i32> {
    let sheet =
        match csvx_core::sheet::Sheet::open(input, csvx_core::sheet::ReadOptions { delimiter }) {
            Ok(sheet) => sheet,
            Err(e) => {
                return Err(report_failure(
                    "edit",
                    format!("cannot open the sheet: {e}"),
                ))
            }
        };
    let columns = match sheet.columns() {
        Ok(columns) => columns,
        Err(e) => {
            return Err(report_failure(
                "edit",
                format!("cannot read the columns: {e}"),
            ))
        }
    };
    let Some(slot) = columns.iter().position(|c| c == column) else {
        eprintln!(
            "error: column '{column}' not found; available: {}",
            columns.join(", ")
        );
        return Err(2);
    };
    let window = match sheet.window(row, 1) {
        Ok(window) => window,
        Err(e) => {
            return Err(report_failure(
                "edit",
                format!("cannot read row {}: {e}", row + 1),
            ))
        }
    };
    let Some(cells) = window.rows.first() else {
        eprintln!("error: row {} is past the end of the sheet", row + 1);
        return Err(1);
    };
    let actual = cells.get(slot).map(String::as_str).unwrap_or("");
    if actual != expect {
        eprintln!(
            "error: row {} column '{column}' is {actual:?}, not {expect:?} (--expect); nothing was written",
            row + 1
        );
        eprintln!("hint: re-read the row (the value may have changed since you fetched it) and retry without --expect");
        return Err(1);
    }
    Ok(())
}

/// Check a formula before it becomes a journal step: unknown columns and missing
/// sheets are cheap to spot now and expensive to discover after an export.
///
/// A missing sheet is a warning rather than an error — `ref()` returns `#VALUE!` for
/// it, which is a legitimate answer if the file is expected later — but every cell of
/// the new column would be `#VALUE!`, and nobody wants to find that out row by row.
fn check_formula(
    input: &Path,
    delimiter: Delimiter,
    ops: &[csvx_core::workbook::Op],
    formula: &str,
) -> Result<(), i32> {
    let parsed = match csvx_core::formula::Formula::parse(formula) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("error: {e}");
            return Err(2);
        }
    };
    let columns = match csvx_core::workbook::column_layout(
        ops,
        &match csvx_core::workbook::base_header(input, delimiter) {
            Ok(header) => header,
            Err(e) => {
                return Err(report_failure(
                    "edit",
                    format!("cannot read the header: {e}"),
                ))
            }
        },
    ) {
        Ok(layout) => layout.into_iter().map(|(name, _)| name).collect::<Vec<_>>(),
        Err(e) => return Err(report_failure("edit", e.to_string())),
    };
    let mut unknown: Vec<String> = Vec::new();
    for reference in parsed.references() {
        if !columns.iter().any(|c| c == &reference) {
            unknown.push(reference);
        }
    }
    if !unknown.is_empty() {
        eprintln!(
            "error: formula reads unknown column(s): {} (available: {})",
            unknown.join(", "),
            columns.join(", ")
        );
        return Err(2);
    }
    // The export refuses a missing sheet too, so refusing here is the same rule applied
    // one step earlier — where the typo is still fresh in the user's mind.
    let directory = input.parent().unwrap_or_else(|| Path::new("."));
    let mut missing: Vec<String> = Vec::new();
    for (sheet, _key, _value) in parsed.sheet_references() {
        if !directory.join(&sheet).is_file() {
            missing.push(sheet);
        }
    }
    if !missing.is_empty() {
        eprintln!(
            "error: referenced sheet(s) not found next to {}: {}",
            input.display(),
            missing.join(", ")
        );
        eprintln!("hint: ref() takes a file name in the same directory, e.g. ref(\"customers.csv\", \"cust_no\", \"name\")");
        return Err(2);
    }
    Ok(())
}

/// How many separate stretches `delete --where` will write before refusing.
///
/// A condition that matches alternating rows would otherwise turn one edit into a
/// million journal entries, which is the memory promise broken from the other side.
const MAX_DELETE_RUNS: usize = 1_000;

fn cmd_edit(args: &[String]) -> i32 {
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
        let applied = match csvx_core::workbook::append_atomically(&sidecar, &ops) {
            Ok(applied) => applied,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                eprintln!("error: another writer holds the journal; retry in a moment");
                return 1;
            }
            Err(e) => return report_failure("edit", e),
        };
        if json {
            println!(
                r#"{{"op":"transaction","operations":{},"applied":{applied},"atomic":true}}"#,
                ops.len()
            );
        } else {
            println!(
                "✓ transaction: {} operation(s), {applied} pending total (atomic)",
                ops.len()
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

fn quality_checks(
    profiles: &[csvx_core::profile::ColumnProfile],
    keys: &[String],
    groups: &[String],
) -> (Vec<String>, Vec<(String, String, String)>) {
    // (JSON check object, (severity, column, message))
    let mut checks = Vec::new();
    let mut findings = Vec::new();
    let mut candidates = Vec::new();
    for profile in profiles {
        if profile.non_empty > 0
            && !profile.distinct_truncated
            && profile.distinct == profile.non_empty
        {
            candidates.push(json_quote(&profile.name));
        }
        if profile.mixed_types {
            findings.push((
                "warning".to_string(),
                profile.name.clone(),
                format!(
                    "{} numeric and {} non-numeric values",
                    profile.numeric.as_ref().map(|n| n.count).unwrap_or(0),
                    profile.empty + profile.non_empty
                        - profile.numeric.as_ref().map(|n| n.count).unwrap_or(0)
                        - profile.empty
                ),
            ));
        }
        if let Some(numeric) = &profile.numeric {
            if numeric.outliers > 0 {
                findings.push((
                    "warning".to_string(),
                    profile.name.clone(),
                    format!(
                        "≈{} value(s) outside mean ± 3σ (std {:.3})",
                        numeric.outliers, numeric.std_dev
                    ),
                ));
            }
        }
        if profile.empty > 0 {
            findings.push((
                "info".to_string(),
                profile.name.clone(),
                format!("{} empty value(s)", profile.empty),
            ));
        }
    }
    for key in keys {
        if let Some(profile) = profiles.iter().find(|profile| &profile.name == key) {
            let duplicates = profile.non_empty.saturating_sub(profile.distinct);
            let severity = if duplicates > 0 || profile.empty > 0 {
                "error"
            } else {
                "info"
            };
            findings.push((
                severity.to_string(),
                key.clone(),
                if profile.distinct_truncated {
                    format!(
                        "key uniqueness is truncated at the distinct cap; ≥{} duplicate value(s)",
                        duplicates
                    )
                } else if duplicates > 0 || profile.empty > 0 {
                    format!(
                        "{} duplicate value(s), {} empty key(s)",
                        duplicates, profile.empty
                    )
                } else {
                    "unique and non-empty".to_string()
                },
            ));
            checks.push(format!(
                r#"{{"column":"{}","kind":"unique","duplicates":{},"empty":{},"truncated":{}}}"#,
                json_quote(key).trim_matches('"'),
                duplicates,
                profile.empty,
                profile.distinct_truncated
            ));
        }
    }
    for group in groups {
        if let Some(profile) = profiles.iter().find(|profile| &profile.name == group) {
            if profile.distinct_truncated {
                findings.push((
                    "info".to_string(),
                    group.clone(),
                    format!(
                        "distinct count capped at {} groups (lower bound)",
                        profile.distinct
                    ),
                ));
            } else if profile.non_empty > 0 {
                if profile.distinct == 1 {
                    findings.push((
                        "warning".to_string(),
                        group.clone(),
                        "only one group value (constant column)".to_string(),
                    ));
                } else if let Some((value, count)) = profile.top.first() {
                    let share = *count as f64 / profile.non_empty as f64;
                    if share >= 0.8 {
                        findings.push((
                            "warning".to_string(),
                            group.clone(),
                            format!(
                                "dominant group {value:?} holds {:.1}% of rows",
                                share * 100.0
                            ),
                        ));
                    }
                }
            }
        }
    }
    (checks, findings)
}

fn quality_json(
    profiles: &[csvx_core::profile::ColumnProfile],
    keys: &[String],
    groups: &[String],
) -> String {
    let (checks, findings) = quality_checks(profiles, keys, groups);
    let candidates: Vec<String> = profiles
        .iter()
        .filter(|profile| {
            profile.non_empty > 0
                && !profile.distinct_truncated
                && profile.distinct == profile.non_empty
        })
        .map(|profile| json_quote(&profile.name))
        .collect();
    let keys_json: Vec<String> = keys.iter().map(|key| json_quote(key)).collect();
    let groups_json: Vec<String> = groups
        .iter()
        .filter_map(|name| profiles.iter().find(|profile| &profile.name == name))
        .map(|profile| {
            let share = if profile.non_empty > 0 && !profile.distinct_truncated {
                profile
                    .top
                    .first()
                    .map(|(_, count)| *count as f64 / profile.non_empty as f64)
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            format!(
                r#"{{"column":"{}","groups":{},"empty":{},"topShare":{:.3},"truncated":{}}}"#,
                json_quote(&profile.name).trim_matches('"'),
                profile.distinct,
                profile.empty,
                share,
                profile.distinct_truncated
            )
        })
        .collect();
    let findings_json: Vec<String> = findings
        .iter()
        .map(|(severity, column, message)| {
            format!(
                r#"{{"severity":"{}","column":"{}","message":"{}"}}"#,
                severity,
                json_quote(column).trim_matches('"'),
                json_quote(message).trim_matches('"')
            )
        })
        .collect();
    format!(
        r#"{{"keys":[{}],"candidateKeys":[{}],"checks":[{}],"groups":[{}],"findings":[{}]}}"#,
        keys_json.join(","),
        candidates.join(","),
        checks.join(","),
        groups_json.join(","),
        findings_json.join(",")
    )
}

fn print_quality_text(
    profiles: &[csvx_core::profile::ColumnProfile],
    keys: &[String],
    groups: &[String],
) {
    let rows = profiles.first().map(|profile| profile.rows).unwrap_or(0);
    let ragged = profiles.first().map(|profile| profile.ragged).unwrap_or(0);
    println!(
        "quality report: {rows} rows, {} columns, {ragged} ragged",
        profiles.len()
    );
    let candidates: Vec<String> = profiles
        .iter()
        .filter(|profile| {
            profile.non_empty > 0
                && !profile.distinct_truncated
                && profile.distinct == profile.non_empty
        })
        .map(|profile| profile.name.clone())
        .collect();
    println!(
        "candidate keys: {}",
        if candidates.is_empty() {
            "-".to_string()
        } else {
            candidates.join(", ")
        }
    );
    for group in groups {
        if let Some(profile) = profiles.iter().find(|profile| &profile.name == group) {
            let share = if profile.non_empty > 0 && !profile.distinct_truncated {
                profile
                    .top
                    .first()
                    .map(|(_, count)| *count as f64 / profile.non_empty as f64)
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            println!(
                "  group {}: {}{} groups, top share {:.1}%, empty {}",
                group,
                profile.distinct,
                if profile.distinct_truncated { "+" } else { "" },
                share * 100.0,
                profile.empty
            );
        }
    }
    let (_, findings) = quality_checks(profiles, keys, groups);
    for (severity, column, message) in findings {
        println!("  {severity:>7}  {column}: {message}");
    }
}

fn print_quality_markdown(
    input: &std::path::Path,
    profiles: &[csvx_core::profile::ColumnProfile],
    keys: &[String],
    groups: &[String],
) {
    let rows = profiles.first().map(|profile| profile.rows).unwrap_or(0);
    let ragged = profiles.first().map(|profile| profile.ragged).unwrap_or(0);
    println!("# Profile: {}", input.display());
    println!();
    println!("{rows} rows, {} columns, {ragged} ragged", profiles.len());
    println!();
    println!("| column | empty | distinct | type | outliers |");
    println!("|---|---:|---:|---|---:|");
    for profile in profiles {
        let kind = match &profile.numeric {
            Some(_) if profile.non_empty > 0 => "numeric",
            Some(_) => "numeric (empty)",
            None if profile.mixed_types => "mixed",
            None => "text",
        };
        let outliers = profile
            .numeric
            .as_ref()
            .map(|numeric| numeric.outliers.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "| {} | {} | {}{} | {} | {} |",
            profile.name,
            profile.empty,
            profile.distinct,
            if profile.distinct_truncated { "+" } else { "" },
            kind,
            outliers
        );
    }
    println!();
    let candidates: Vec<String> = profiles
        .iter()
        .filter(|profile| {
            profile.non_empty > 0
                && !profile.distinct_truncated
                && profile.distinct == profile.non_empty
        })
        .map(|profile| profile.name.clone())
        .collect();
    println!(
        "**Candidate keys:** {}",
        if candidates.is_empty() {
            "-".to_string()
        } else {
            candidates.join(", ")
        }
    );
    println!();
    if !groups.is_empty() {
        println!("## Groups");
        println!();
        println!("| column | groups | empty | top share |");
        println!("|---|---:|---:|---:|");
        for group in groups {
            if let Some(profile) = profiles.iter().find(|profile| &profile.name == group) {
                let share = if profile.non_empty > 0 && !profile.distinct_truncated {
                    profile
                        .top
                        .first()
                        .map(|(_, count)| *count as f64 / profile.non_empty as f64)
                        .unwrap_or(0.0)
                } else {
                    0.0
                };
                println!(
                    "| {} | {}{} | {} | {:.1}% |",
                    group,
                    profile.distinct,
                    if profile.distinct_truncated { "+" } else { "" },
                    profile.empty,
                    share * 100.0
                );
            }
        }
        println!();
    }
    let (_, findings) = quality_checks(profiles, keys, groups);
    if findings.is_empty() {
        println!("No quality findings.");
    } else {
        println!("## Findings");
        for (severity, column, message) in findings {
            println!("- **{severity}** `{column}`: {message}");
        }
    }
}

fn cmd_mcp(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--root" => match p.value("--root") {
                Ok(value) => root = Some(PathBuf::from(value)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("mcp");
                return 0;
            }
            other => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
        }
    }
    let root = root.unwrap_or_else(|| std::path::PathBuf::from("."));
    match serve::run_mcp_stdio(&root) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[derive(Clone, Debug)]
struct PipelineStep {
    name: String,
    input: Option<String>,
    command: String,
    args: String,
    output: std::path::PathBuf,
    group: String,
}

type PipelineResults = Vec<(String, bool, u64, String)>;
type BranchOutcome = (String, bool, u64, String, PipelineStateRow);
type PipelineLineage = Vec<(String, Vec<String>)>;

#[derive(Clone, Debug, Default)]
struct PipelineStateRow {
    name: String,
    command: String,
    args: String,
    input: String,
    output: String,
    input_hash: String,
    args_hash: String,
    output_hash: String,
    elapsed_ms: u64,
    input_stamp: String,
    output_stamp: String,
}

fn pipeline_state_path(plan: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.pipeline.csv", plan.display()))
}

fn pipeline_lineage_path(plan: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.lineage.csv", plan.display()))
}

fn save_pipeline_lineage(plan: &std::path::Path, lineage: &PipelineLineage) -> std::io::Result<()> {
    let mut text = String::from("step,deps\n");
    for (name, deps) in lineage {
        text.push_str(&pipeline_csv_cell(name));
        text.push(',');
        text.push_str(&pipeline_csv_cell(&deps.join(";")));
        text.push('\n');
    }
    std::fs::write(pipeline_lineage_path(plan), text)
}

fn fnv_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn pipeline_hash_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        for byte in &buffer[..read] {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    Ok(format!("{hash:016x}"))
}

fn pipeline_file_stamp(path: &std::path::Path) -> std::io::Result<String> {
    let meta = std::fs::metadata(path)?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(format!("{}:{modified}", meta.len()))
}

fn pipeline_hash_text(text: &str) -> String {
    format!("{:016x}", fnv_hash(text.as_bytes()))
}

fn split_pipeline_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in text.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn load_pipeline_state(path: &std::path::Path) -> Vec<PipelineStateRow> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut reader = csvx_core::csv_reader::RecordReader::new(
        std::io::BufReader::with_capacity(256 * 1024, file),
        256 * 1024,
    );
    let mut record = Vec::new();
    let mut rows = Vec::new();
    let mut header: Option<Vec<String>> = None;
    while reader.next_record(&mut record).unwrap_or(false) {
        let cells: Vec<String> = csvx_core::csv_reader::parse_csv_record_with(
            &String::from_utf8_lossy(&record),
            b',',
            csvx_core::csv_reader::QuoteStyle::Rfc4180,
        );
        if header.is_none() {
            header = Some(cells);
            continue;
        }
        if cells.len() < 8 {
            continue;
        }
        rows.push(PipelineStateRow {
            name: cells[0].clone(),
            command: cells[1].clone(),
            args: cells[2].clone(),
            input: cells[3].clone(),
            output: cells[4].clone(),
            input_hash: cells[5].clone(),
            args_hash: cells[6].clone(),
            output_hash: cells[7].clone(),
            elapsed_ms: cells
                .get(8)
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            input_stamp: cells.get(9).cloned().unwrap_or_default(),
            output_stamp: cells.get(10).cloned().unwrap_or_default(),
        });
    }
    rows
}

fn save_pipeline_state(path: &std::path::Path, rows: &[PipelineStateRow]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = std::path::PathBuf::from(format!("{}.tmp", path.display()));
    let mut file = std::fs::File::create(&tmp)?;
    writeln!(
        file,
        "step,command,args,input,output,input_hash,args_hash,output_hash,elapsed_ms,input_stamp,output_stamp"
    )?;
    for row in rows {
        writeln!(
            file,
            "{},{},{},{},{},{},{},{},{},{},{}",
            row.name,
            row.command,
            row.args,
            row.input,
            row.output,
            row.input_hash,
            row.args_hash,
            row.output_hash,
            row.elapsed_ms,
            row.input_stamp,
            row.output_stamp
        )?;
    }
    file.flush()?;
    std::fs::rename(&tmp, path)
}

/// Run one pipeline child with an optional wall-clock timeout.
///
/// `Command::output()` waits forever; pipeline steps are external processes that
/// can hang on a bad command. The two reader threads drain stdout/stderr while
/// `try_wait` polls, so a verbose step cannot fill a pipe and deadlock the parent.
fn run_step_command(
    mut process: std::process::Command,
    timeout_ms: u64,
) -> std::io::Result<std::process::Output> {
    process
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = process.spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stdout_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let stderr_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if timeout_ms > 0 && started.elapsed() >= Duration::from_millis(timeout_ms) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_thread.join();
                    let _ = stderr_thread.join();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("step exceeded {timeout_ms} ms"),
                    ));
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    };
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Remove remnants of an interrupted parallel run. Only files that exactly match a
/// declared output's staging/legacy temp prefix are touched; a normal run only deletes
/// them after an hour, `--clean` forces it.
/// Recover outputs left mid-wave by a crashed run. A backup exists only after the
/// previous output was moved aside, so restoring it is the safe state: any current
/// output belongs to an uncommitted run and state was never written.
struct PipelineLock {
    path: std::path::PathBuf,
}

impl Drop for PipelineLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn process_alive(pid: u32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output();
    match output {
        Ok(output) => output.status.success() && !output.stdout.is_empty(),
        Err(_) => true,
    }
}

fn lock_owner_alive(path: &std::path::Path) -> Option<bool> {
    let text = std::fs::read_to_string(path).ok()?;
    let pid = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid="))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u32>().ok())?;
    Some(process_alive(pid))
}

fn acquire_pipeline_lock(plan: &std::path::Path) -> std::io::Result<PipelineLock> {
    let path = std::path::PathBuf::from(format!("{}.pipeline.lock", plan.display()));
    if path.exists() {
        let age = path
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .unwrap_or_default();
        let owner = lock_owner_alive(&path);
        let stale_by_age = owner.is_none() && age >= Duration::from_secs(24 * 3600);
        let stale_by_owner = matches!(owner, Some(false));
        if !stale_by_age && !stale_by_owner {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "pipeline is already running (lock {}); remove it if no process is active",
                    path.display()
                ),
            ));
        }
        let _ = std::fs::remove_file(&path);
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            let _ = writeln!(
                file,
                "pid={} started={:?}",
                std::process::id(),
                std::time::SystemTime::now()
            );
            Ok(PipelineLock { path })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "pipeline is already running (lock {}); remove it if no process is active",
                    path.display()
                ),
            ))
        }
        Err(error) => Err(error),
    }
}

fn recover_dag_backups(steps: &[PipelineStep]) -> std::io::Result<usize> {
    let mut recovered = 0usize;
    for step in steps {
        let Some(parent) = step.output.parent() else {
            continue;
        };
        if !parent.exists() {
            continue;
        }
        let name = step
            .output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("out");
        let prefix = format!(".{name}.dag-backup-");
        let mut backups: Vec<std::path::PathBuf> = Vec::new();
        for entry in std::fs::read_dir(parent)?.flatten() {
            let file = entry.file_name();
            if file.to_string_lossy().starts_with(&prefix) {
                backups.push(entry.path());
            }
        }
        if backups.is_empty() {
            continue;
        }
        let newest = backups
            .iter()
            .max_by_key(|path| {
                path.metadata()
                    .and_then(|meta| meta.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            })
            .cloned()
            .expect("non-empty backups");
        let _ = std::fs::remove_file(&step.output);
        std::fs::rename(&newest, &step.output)?;
        for backup in &backups {
            if backup != &newest {
                let _ = std::fs::remove_file(backup);
            }
        }
        recovered += 1;
    }
    Ok(recovered)
}

fn cleanup_stale_staging(steps: &[PipelineStep], force: bool) -> std::io::Result<usize> {
    let mut removed = 0usize;
    for step in steps {
        let Some(parent) = step.output.parent() else {
            continue;
        };
        if !parent.exists() {
            continue;
        }
        let name = step
            .output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("out");
        let branch_prefix = format!(".{name}.branch-");
        let legacy = format!(".{name}.pipeline.tmp");
        for entry in std::fs::read_dir(parent)?.flatten() {
            let file = entry.file_name();
            let file = file.to_string_lossy();
            let matches =
                file == legacy || (file.starts_with(&branch_prefix) && file.ends_with(".tmp"));
            if !matches {
                continue;
            }
            if !force {
                let age = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .unwrap_or_default();
                if age < Duration::from_secs(3600) {
                    continue;
                }
            }
            if std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

const AUTO_CONTENT_CHECK_LIMIT: u64 = 1 << 20;

fn auto_content_check_limit() -> u64 {
    std::env::var("CSVX_CONTENT_CHECK_LIMIT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(AUTO_CONTENT_CHECK_LIMIT)
}

fn pipeline_auto_content_check(input: &std::path::Path, output: &std::path::Path) -> bool {
    let limit = auto_content_check_limit();
    let size = |path: &std::path::Path| {
        std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(u64::MAX)
    };
    size(input) <= limit && size(output) <= limit
}

fn pipeline_state_is_fresh(row: &PipelineStateRow, content_check: bool) -> bool {
    let input = std::path::Path::new(&row.input);
    let output = std::path::Path::new(&row.output);
    if !input.exists() || !output.exists() {
        return false;
    }
    let hash_content = content_check || pipeline_auto_content_check(input, output);
    if hash_content && !row.input_hash.is_empty() && !row.output_hash.is_empty() {
        return pipeline_hash_file(input)
            .map(|hash| hash == row.input_hash)
            .unwrap_or(false)
            && pipeline_hash_file(output)
                .map(|hash| hash == row.output_hash)
                .unwrap_or(false);
    }
    if !row.input_stamp.is_empty() && !row.output_stamp.is_empty() {
        let input_stamp = pipeline_file_stamp(input).ok();
        let output_stamp = pipeline_file_stamp(output).ok();
        return input_stamp.as_deref() == Some(row.input_stamp.as_str())
            && output_stamp.as_deref() == Some(row.output_stamp.as_str());
    }
    pipeline_hash_file(input)
        .map(|hash| hash == row.input_hash)
        .unwrap_or(false)
        && pipeline_hash_file(output)
            .map(|hash| hash == row.output_hash)
            .unwrap_or(false)
}

fn pipeline_dependencies(steps: &[PipelineStep], index: usize) -> Vec<String> {
    match &steps[index].input {
        Some(name) if steps[..index].iter().any(|step| &step.name == name) => vec![name.clone()],
        Some(_) => Vec::new(),
        None if index > 0 => vec![steps[index - 1].name.clone()],
        None => Vec::new(),
    }
}

fn pipeline_csv_cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// Run one `group` of independent steps as concurrent child pipelines. Each child
/// gets a one-step plan so it can reuse the normal command/temp/rename path. Grouped
/// steps must name an explicit input (a source file or an earlier step's output);
/// we do not try to infer freshness here, so they rerun when the group is reached.
fn pipeline_resolve_input(
    step: &PipelineStep,
    steps: &[PipelineStep],
    root: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    let position = steps
        .iter()
        .position(|candidate| candidate.name == step.name)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "unknown pipeline step")
        })?;
    let name = match &step.input {
        Some(name) => name,
        None if position > 0 => &steps[position - 1].name,
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("step {} has no input", step.name),
            ))
        }
    };
    let path = if let Some(position) = steps.iter().position(|candidate| &candidate.name == name) {
        steps[position].output.clone()
    } else if std::path::Path::new(name).is_absolute() {
        std::path::PathBuf::from(name)
    } else {
        root.join(name)
    };
    if !path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("step {}: input {} not found", step.name, path.display()),
        ));
    }
    Ok(path)
}

fn pipeline_fresh_row(
    step: &PipelineStep,
    input_path: &std::path::Path,
    previous: &[PipelineStateRow],
    force: bool,
    content_check: bool,
) -> std::io::Result<Option<PipelineStateRow>> {
    if force {
        return Ok(None);
    }
    let input_string = input_path.display().to_string();
    let output_exists = step.output.exists();
    let input_stamp = pipeline_file_stamp(input_path)?;
    let output_stamp = if output_exists {
        pipeline_file_stamp(&step.output)?
    } else {
        String::new()
    };
    let hash_content = content_check || pipeline_auto_content_check(input_path, &step.output);
    let Some(row) = previous.iter().find(|row| {
        row.name == step.name
            && row.command == step.command
            && row.args == step.args
            && row.input == input_string
    }) else {
        return Ok(None);
    };
    let fresh = if hash_content && !row.input_hash.is_empty() && !row.output_hash.is_empty() {
        output_exists
            && pipeline_hash_file(input_path)? == row.input_hash
            && pipeline_hash_file(&step.output)
                .map(|hash| hash == row.output_hash)
                .unwrap_or(false)
    } else if !row.input_stamp.is_empty() && !row.output_stamp.is_empty() {
        output_exists && row.input_stamp == input_stamp && row.output_stamp == output_stamp
    } else {
        let input_hash = pipeline_hash_file(input_path)?;
        output_exists
            && input_hash == row.input_hash
            && pipeline_hash_file(&step.output)
                .map(|hash| hash == row.output_hash)
                .unwrap_or(false)
    };
    if !fresh {
        return Ok(None);
    }
    Ok(Some(PipelineStateRow {
        name: step.name.clone(),
        command: step.command.clone(),
        args: step.args.clone(),
        input: input_string,
        output: step.output.display().to_string(),
        input_hash: row.input_hash.clone(),
        args_hash: row.args_hash.clone(),
        output_hash: row.output_hash.clone(),
        elapsed_ms: 0,
        input_stamp,
        output_stamp,
    }))
}

fn run_branch_group(
    all: &[PipelineStep],
    group_indices: &[usize],
    root: &std::path::Path,
    force: bool,
    step_timeout_ms: u64,
    retries: usize,
    retry_delay_ms: u64,
) -> std::io::Result<Vec<BranchOutcome>> {
    let bin = std::env::current_exe()?;
    let first = all[group_indices[0]].name.as_bytes();
    let stamp = format!("{}-{}", std::process::id(), fnv_hash(first));
    let mut plans = Vec::with_capacity(group_indices.len());
    let mut inputs = Vec::with_capacity(group_indices.len());
    let mut staged = Vec::with_capacity(group_indices.len());
    for (index, &step_index) in group_indices.iter().enumerate() {
        let step = &all[step_index];
        let name = step.input.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("grouped step {} needs an explicit input", step.name),
            )
        })?;
        let input_path =
            if let Some(position) = all.iter().position(|candidate| &candidate.name == name) {
                if group_indices.contains(&position) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "grouped step {} depends on {} in the same group",
                            step.name, name
                        ),
                    ));
                }
                all[position].output.clone()
            } else if std::path::Path::new(name).is_absolute() {
                std::path::PathBuf::from(name)
            } else {
                root.join(name)
            };
        if !input_path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "step {}: input {} not found",
                    step.name,
                    input_path.display()
                ),
            ));
        }
        inputs.push((step, input_path.clone()));
        let stage = step.output.with_file_name(format!(
            ".{}.branch-{stamp}-{index}.tmp",
            step.output
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("out")
        ));
        staged.push(stage.clone());
        let plan = std::env::temp_dir().join(format!("csvx-branch-{stamp}-{index}.csv"));
        let row = format!(
            "{},{},{},{},{}\n",
            pipeline_csv_cell(&step.name),
            pipeline_csv_cell(&input_path.display().to_string()),
            pipeline_csv_cell(&step.command),
            pipeline_csv_cell(&step.args),
            pipeline_csv_cell(&stage.display().to_string())
        );
        std::fs::write(&plan, format!("step,input,command,args,output\n{row}"))?;
        plans.push(plan);
    }
    struct BranchChild {
        index: usize,
        child: std::process::Child,
        stdout: std::path::PathBuf,
        stderr: std::path::PathBuf,
    }
    let mut children = Vec::with_capacity(plans.len());
    for (index, plan) in plans.iter().enumerate() {
        let out_path = std::env::temp_dir().join(format!("csvx-branch-{stamp}-{index}.out"));
        let err_path = std::env::temp_dir().join(format!("csvx-branch-{stamp}-{index}.err"));
        let out = std::fs::File::create(&out_path)?;
        let err = std::fs::File::create(&err_path)?;
        let mut process = std::process::Command::new(&bin);
        process.args([
            "pipeline",
            "run",
            plan.to_str().unwrap_or(""),
            "--root",
            root.to_str().unwrap_or(""),
            "--json",
        ]);
        if force {
            process.arg("--force");
        }
        if step_timeout_ms > 0 {
            process.args(["--step-timeout", &step_timeout_ms.to_string()]);
        }
        if retries > 0 {
            process.args([
                "--retries",
                &retries.to_string(),
                "--retry-delay",
                &retry_delay_ms.to_string(),
            ]);
        }
        let child = process
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(out))
            .stderr(std::process::Stdio::from(err))
            .spawn()?;
        children.push(BranchChild {
            index,
            child,
            stdout: out_path,
            stderr: err_path,
        });
    }
    let started = Instant::now();
    let mut statuses: Vec<Option<std::process::ExitStatus>> = vec![None; children.len()];
    let mut failure: Option<String> = None;
    loop {
        let mut running = 0usize;
        for branch in children.iter_mut() {
            if statuses[branch.index].is_some() {
                continue;
            }
            match branch.child.try_wait() {
                Ok(Some(status)) => {
                    statuses[branch.index] = Some(status);
                    if !status.success() && failure.is_none() {
                        let error = std::fs::read_to_string(&branch.stderr)
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        failure = Some(format!(
                            "step {} failed: {error}",
                            inputs[branch.index].0.name
                        ));
                    }
                }
                Ok(None) => running += 1,
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(format!(
                            "step {} failed: {error}",
                            inputs[branch.index].0.name
                        ));
                    }
                }
            }
        }
        if failure.is_some() || running == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    for branch in children.iter_mut() {
        if failure.is_some() {
            let _ = branch.child.kill();
        }
        let _ = branch.child.wait();
        let _ = std::fs::remove_file(&branch.stdout);
        let _ = std::fs::remove_file(&branch.stderr);
    }
    for plan in &plans {
        let _ = std::fs::remove_file(plan);
        let _ = std::fs::remove_file(pipeline_state_path(plan));
    }
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if let Some(error) = failure {
        for stage in &staged {
            let _ = std::fs::remove_file(stage);
        }
        return Err(std::io::Error::other(error));
    }
    // Commit the whole wave only after every child succeeded. Existing outputs are
    // replaced, but a failed wave never touches them.
    for ((step, _), stage) in inputs.iter().zip(staged.iter()) {
        if step.output.exists() {
            let _ = std::fs::remove_file(&step.output);
        }
        if let Err(error) = std::fs::rename(stage, &step.output) {
            for stage in &staged {
                let _ = std::fs::remove_file(stage);
            }
            return Err(error);
        }
    }
    let mut results = Vec::with_capacity(group_indices.len());
    for (step, input_path) in inputs.iter() {
        let input_hash = pipeline_hash_file(input_path)?;
        let output_hash = pipeline_hash_file(&step.output)?;
        let input_stamp = pipeline_file_stamp(input_path)?;
        let output_stamp = pipeline_file_stamp(&step.output)?;
        let args_hash = pipeline_hash_text(&step.args);
        let output_string = step.output.display().to_string();
        results.push((
            step.name.clone(),
            false,
            elapsed_ms,
            output_string.clone(),
            PipelineStateRow {
                name: step.name.clone(),
                command: step.command.clone(),
                args: step.args.clone(),
                input: input_path.display().to_string(),
                output: output_string,
                input_hash,
                args_hash,
                output_hash,
                elapsed_ms,
                input_stamp,
                output_stamp,
            },
        ));
    }
    Ok(results)
}

/// Topological scheduler: ready steps (all dependencies done) are freshness-checked;
/// fresh ones reuse their prior rows and stale ones run as one concurrent wave, bounded
/// by `jobs`. Unlike the linear path, ordering follows the dependency graph, not the
/// manifest row order.
#[allow(clippy::too_many_arguments)]
fn dag_output_backup(output: &std::path::Path, stamp: &str, index: usize) -> std::path::PathBuf {
    output.with_file_name(format!(
        ".{}.dag-backup-{stamp}-{index}",
        output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("out")
    ))
}

fn restore_dag_outputs(touched: &[(std::path::PathBuf, Option<std::path::PathBuf>)]) {
    for (output, backup) in touched {
        let _ = std::fs::remove_file(output);
        if let Some(backup) = backup {
            let _ = std::fs::rename(backup, output);
        }
    }
}

fn print_step_marker(name: &str, fresh: bool, index: usize, total: usize) {
    println!(
        "CSVX_STEP {} {name} {}/{}",
        if fresh { "skip" } else { "done" },
        index + 1,
        total
    );
}

#[allow(clippy::too_many_arguments)]
fn run_pipeline_dag(
    steps: &[PipelineStep],
    root: &std::path::Path,
    force: bool,
    step_timeout_ms: u64,
    retries: usize,
    retry_delay_ms: u64,
    jobs: usize,
    previous: &[PipelineStateRow],
    state_path: &std::path::Path,
    progress_markers: bool,
    content_check: bool,
) -> std::io::Result<PipelineResults> {
    use std::collections::HashSet;
    let dependencies: Vec<Vec<usize>> = (0..steps.len())
        .map(|index| {
            pipeline_dependencies(steps, index)
                .into_iter()
                .filter_map(|name| steps.iter().position(|step| step.name == name))
                .collect()
        })
        .collect();
    let mut done = HashSet::new();
    let mut results = Vec::with_capacity(steps.len());
    let mut state_rows = Vec::with_capacity(steps.len());
    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    );
    let mut touched: Vec<(std::path::PathBuf, Option<std::path::PathBuf>)> = Vec::new();
    while done.len() < steps.len() {
        let ready: Vec<usize> = (0..steps.len())
            .filter(|index| {
                !done.contains(index)
                    && dependencies[*index]
                        .iter()
                        .all(|dependency| done.contains(dependency))
            })
            .collect();
        if ready.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "pipeline dependency cycle",
            ));
        }
        let mut stale = Vec::new();
        for &index in &ready {
            let input_path = pipeline_resolve_input(&steps[index], steps, root)?;
            match pipeline_fresh_row(&steps[index], &input_path, previous, force, content_check)? {
                Some(row) => {
                    results.push((
                        steps[index].name.clone(),
                        true,
                        0,
                        steps[index].output.display().to_string(),
                    ));
                    state_rows.push(row);
                    done.insert(index);
                }
                None => stale.push(index),
            }
        }
        if stale.is_empty() {
            continue;
        }
        let wave: Vec<usize> = stale.into_iter().take(jobs).collect();
        for &index in &wave {
            let output = &steps[index].output;
            if output.exists() {
                let backup = dag_output_backup(output, &stamp, index);
                let _ = std::fs::remove_file(&backup);
                if let Err(error) = std::fs::rename(output, &backup) {
                    restore_dag_outputs(&touched);
                    return Err(error);
                }
                touched.push((output.clone(), Some(backup)));
            } else {
                touched.push((output.clone(), None));
            }
        }
        let outcomes = match run_branch_group(
            steps,
            &wave,
            root,
            force,
            step_timeout_ms,
            retries,
            retry_delay_ms,
        ) {
            Ok(outcomes) => outcomes,
            Err(error) => {
                restore_dag_outputs(&touched);
                return Err(error);
            }
        };
        for (offset, (name, fresh, ms, output, state_row)) in outcomes.into_iter().enumerate() {
            if progress_markers {
                print_step_marker(&name, fresh, wave[offset], steps.len());
            }
            results.push((name, fresh, ms, output));
            state_rows.push(state_row);
            done.insert(wave[offset]);
        }
    }
    for (_, backup) in &touched {
        if let Some(backup) = backup {
            let _ = std::fs::remove_file(backup);
        }
    }
    save_pipeline_state(state_path, &state_rows)?;
    Ok(results)
}

fn normalize_lexical_path(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    out
}

fn normalize_plan_path(path: &std::path::Path) -> std::path::PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| normalize_lexical_path(path))
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn read_watch_round(path: &std::path::Path) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let Some(rest) = text.split(r#""round":"#).nth(1) else {
        return 0;
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<usize>().unwrap_or(0)
}

fn read_watch_failures(path: &std::path::Path) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let Some(rest) = text.split(r#""consecutiveFailures":"#).nth(1) else {
        return 0;
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<usize>().unwrap_or(0)
}

fn read_watch_last_error(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let rest = text.split(r#""lastError":""#).nth(1)?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('u') => {
                    let _ = chars.by_ref().take(4).count();
                    out.push('?');
                }
                Some(other) => out.push(other),
                None => break,
            },
            other => out.push(other),
        }
    }
    (!out.is_empty()).then_some(out)
}

fn read_watch_next_delay(path: &std::path::Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let rest = text.split(r#""nextDelayMs":"#).nth(1)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn read_watch_previous_failure(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text.contains(r#""success":false"#))
        .unwrap_or(false)
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars().take(240) {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            c if c.is_control() => escaped.push_str(&format!("\\u{:04x}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped
}

#[allow(clippy::too_many_arguments)]
fn write_watch_state(
    path: &std::path::Path,
    round: usize,
    ran: usize,
    total: usize,
    success: bool,
    failures: usize,
    last_success_ms: u128,
    last_failure_ms: u128,
    next_delay_ms: u64,
    last_error: &str,
) -> std::io::Result<()> {
    let body = format!(
        r#"{{"round":{},"ran":{},"total":{},"success":{},"consecutiveFailures":{},"lastSuccessAtMs":{},"lastFailureAtMs":{},"nextDelayMs":{},"lastError":"{}","updatedAtMs":{}}}"#,
        round,
        ran,
        total,
        success,
        failures,
        last_success_ms,
        last_failure_ms,
        next_delay_ms,
        json_escape(last_error),
        now_millis()
    );
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, body)?;
    std::fs::rename(temp, path)
}

#[allow(clippy::too_many_arguments)]
fn watch_pipeline(
    plan: &std::path::Path,
    root: Option<&std::path::Path>,
    force: bool,
    dry_run: bool,
    step_timeout_ms: u64,
    retries: usize,
    retry_delay_ms: u64,
    jobs: usize,
    dag: bool,
    clean: bool,
    unlock: bool,
    progress_markers: bool,
    content_check: bool,
    json: bool,
    interval_ms: u64,
    backoff_max_ms: u64,
    iterations: usize,
    max_failures: usize,
    state_path: Option<&std::path::Path>,
    verbose: bool,
) -> std::io::Result<()> {
    let interval_ms = interval_ms.max(10);
    let backoff_max_ms = backoff_max_ms.max(interval_ms);
    let mut delay_ms = interval_ms;
    let mut failures = state_path.map(read_watch_failures).unwrap_or(0);
    let mut last_success_ms = 0u128;
    let mut last_failure_ms = 0u128;
    let mut round = state_path.map(read_watch_round).unwrap_or(0);
    if let Some(path) = state_path {
        if failures > 0 {
            eprintln!("watch: previous state has {failures} consecutive failure(s)");
            if let Some(error) = read_watch_last_error(path) {
                eprintln!("watch: previous failure: {error}");
            }
            if max_failures > 0 && failures >= max_failures {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("watch: previous state already reached the failure cap ({failures}); remove {} to restart", path.display()),
                ));
            }
            if let Some(saved) = read_watch_next_delay(path) {
                delay_ms = saved.clamp(interval_ms, backoff_max_ms);
            }
        } else if read_watch_previous_failure(path) {
            eprintln!("watch: previous state ended in failure");
        }
    }
    loop {
        round += 1;
        match run_pipeline_plan(
            plan,
            root,
            force,
            dry_run,
            step_timeout_ms,
            retries,
            retry_delay_ms,
            jobs,
            dag,
            clean,
            unlock,
            progress_markers,
            content_check,
        ) {
            Ok((results, _)) => {
                let ran = results.iter().filter(|(_, fresh, _, _)| !*fresh).count();
                if json {
                    let steps = results
                        .iter()
                        .map(|(name, fresh, _, detail)| {
                            format!(
                                r#"{{"step":{},"fresh":{},"detail":{}}}"#,
                                json_quote(name),
                                fresh,
                                json_quote(detail)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    println!(
                        r#"{{"watch":true,"round":{},"success":true,"ran":{},"total":{},"consecutiveFailures":0,"nextDelayMs":{},"steps":[{}]}}"#,
                        round,
                        ran,
                        results.len(),
                        interval_ms,
                        steps
                    );
                } else {
                    if ran > 0 || verbose {
                        println!("watch: round {round} ran {ran}/{} step(s)", results.len());
                    }
                    for (name, fresh, _, detail) in &results {
                        if !*fresh {
                            println!("→ {name} ({detail})");
                        }
                    }
                }
                delay_ms = interval_ms;
                failures = 0;
                last_success_ms = now_millis();
                if let Some(path) = state_path {
                    if let Err(error) = write_watch_state(
                        path,
                        round,
                        ran,
                        results.len(),
                        true,
                        0,
                        last_success_ms,
                        last_failure_ms,
                        interval_ms,
                        "",
                    ) {
                        eprintln!("watch: state write failed: {error}");
                    }
                }
            }
            Err(error) => {
                eprintln!("watch: {error}");
                failures += 1;
                last_failure_ms = now_millis();
                let next_delay_ms = delay_ms.saturating_mul(2).min(backoff_max_ms);
                if json {
                    println!(
                        r#"{{"watch":true,"round":{},"success":false,"consecutiveFailures":{},"nextDelayMs":{},"error":{}}}"#,
                        round,
                        failures,
                        next_delay_ms,
                        json_quote(&error.to_string())
                    );
                }
                if let Some(path) = state_path {
                    if let Err(write_error) = write_watch_state(
                        path,
                        round,
                        0,
                        0,
                        false,
                        failures,
                        last_success_ms,
                        last_failure_ms,
                        next_delay_ms,
                        &error.to_string(),
                    ) {
                        eprintln!("watch: state write failed: {write_error}");
                    }
                }
                if iterations > 0 {
                    return Err(error);
                }
                if max_failures > 0 && failures >= max_failures {
                    eprintln!("watch: stopping after {failures} consecutive failure(s)");
                    return Err(error);
                }
                delay_ms = next_delay_ms;
            }
        }
        if iterations > 0 && round >= iterations {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    }
}

#[allow(clippy::too_many_arguments)]
fn run_pipeline_plan(
    plan: &std::path::Path,
    root: Option<&std::path::Path>,
    force: bool,
    dry_run: bool,
    step_timeout_ms: u64,
    retries: usize,
    retry_delay_ms: u64,
    jobs: usize,
    dag: bool,
    clean: bool,
    unlock: bool,
    progress_markers: bool,
    content_check: bool,
) -> std::io::Result<(PipelineResults, PipelineLineage)> {
    let root = root
        .map(std::path::Path::to_path_buf)
        .or_else(|| plan.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let reader = csvx_core::encoding::open_decoded(plan)?;
    let mut records = csvx_core::csv_reader::RecordReader::new(
        std::io::BufReader::with_capacity(256 * 1024, reader),
        256 * 1024,
    );
    let mut record = Vec::new();
    if !records.next_record(&mut record)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the pipeline plan is empty",
        ));
    }
    let header: Vec<String> = csvx_core::csv_reader::parse_csv_record_with(
        &String::from_utf8_lossy(&record),
        b',',
        csvx_core::csv_reader::QuoteStyle::Rfc4180,
    );
    let expected = ["step", "input", "command", "args", "output"];
    let header_ok = (header.len() == 5 || header.len() == 6)
        && header
            .iter()
            .take(5)
            .zip(expected)
            .all(|(cell, want)| cell.trim() == want)
        && header.get(5).is_none_or(|cell| cell.trim() == "group");
    if !header_ok {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the pipeline plan header must be step,input,command,args,output[,group]",
        ));
    }
    let mut steps: Vec<PipelineStep> = Vec::new();
    while records.next_record(&mut record)? {
        let cells: Vec<String> = csvx_core::csv_reader::parse_csv_record_with(
            &String::from_utf8_lossy(&record),
            b',',
            csvx_core::csv_reader::QuoteStyle::Rfc4180,
        );
        if cells.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        if cells.len() != 5 && cells.len() != 6 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a pipeline step needs 5 or 6 columns",
            ));
        }
        let name = cells[0].trim().to_string();
        if name.is_empty() || cells[2].trim().is_empty() || cells[4].trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a pipeline step needs a name, command and output",
            ));
        }
        if steps.iter().any(|step| step.name == name) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("duplicate pipeline step {name}"),
            ));
        }
        steps.push(PipelineStep {
            name,
            input: if cells[1].trim().is_empty() {
                None
            } else {
                Some(cells[1].trim().to_string())
            },
            command: cells[2].trim().to_string(),
            args: cells[3].to_string(),
            output: root.join(cells[4].trim()),
            group: cells
                .get(5)
                .map(|cell| cell.trim().to_string())
                .unwrap_or_default(),
        });
    }
    if steps.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the pipeline plan has no steps",
        ));
    }
    for (index, step) in steps.iter().enumerate() {
        if let Some(input) = &step.input {
            if let Some(position) = steps.iter().position(|candidate| &candidate.name == input) {
                if position >= index {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "step {} refers to {} which is not an earlier step",
                            step.name, input
                        ),
                    ));
                }
            }
        }
    }
    for (index, step) in steps.iter().enumerate() {
        if let Some(other) = steps[index + 1..].iter().find(|candidate| {
            normalize_lexical_path(&candidate.output) == normalize_lexical_path(&step.output)
        }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "steps {} and {} declare the same output {}",
                    step.name,
                    other.name,
                    step.output.display()
                ),
            ));
        }
    }
    for step in &steps {
        for candidate in &steps {
            let Some(input) = candidate.input.as_deref() else {
                continue;
            };
            if steps.iter().any(|known| known.name == input) {
                continue;
            }
            if normalize_plan_path(&root.join(input)) == normalize_plan_path(&step.output) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "step {} writes the source input {} in place",
                        step.name,
                        step.output.display()
                    ),
                ));
            }
        }
    }
    if unlock {
        let _ = std::fs::remove_file(format!("{}.pipeline.lock", plan.display()));
    }
    let _lock = acquire_pipeline_lock(plan)?;
    let recovered = recover_dag_backups(&steps)?;
    if recovered > 0 {
        eprintln!("recovered {recovered} interrupted DAG output(s)");
    }
    let cleaned = cleanup_stale_staging(&steps, clean)?;
    if cleaned > 0 {
        eprintln!("cleaned {cleaned} stale pipeline staging file(s)");
    }
    let state_path = pipeline_state_path(plan);
    let previous = load_pipeline_state(&state_path);
    let bin = std::env::current_exe()?;
    let mut state_rows = Vec::with_capacity(steps.len());
    let mut results: Vec<(String, bool, u64, String)> = Vec::with_capacity(steps.len());
    let mut previous_output: Option<std::path::PathBuf> = None;
    if dag && jobs > 1 && !dry_run {
        let results = run_pipeline_dag(
            &steps,
            &root,
            force,
            step_timeout_ms,
            retries,
            retry_delay_ms,
            jobs,
            &previous,
            &state_path,
            progress_markers,
            content_check,
        )?;
        let lineage = steps
            .iter()
            .enumerate()
            .map(|(index, step)| (step.name.clone(), pipeline_dependencies(&steps, index)))
            .collect();
        save_pipeline_lineage(plan, &lineage)?;
        return Ok((results, lineage));
    }
    let mut skip = 0usize;
    for (index, step) in steps.iter().enumerate() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if jobs > 1 && !step.group.is_empty() && !dry_run {
            let mut end = index + 1;
            while end < steps.len() && steps[end].group == step.group {
                end += 1;
            }
            let mut fresh_rows = Vec::with_capacity(end - index);
            let mut stale = Vec::new();
            for (offset, candidate) in steps[index..end].iter().enumerate() {
                let input_path = pipeline_resolve_input(candidate, &steps, &root)?;
                if let Some(row) =
                    pipeline_fresh_row(candidate, &input_path, &previous, force, content_check)?
                {
                    fresh_rows.push((offset, Some(row)));
                } else {
                    stale.push(index + offset);
                    fresh_rows.push((offset, None));
                }
            }
            let outcomes = if stale.is_empty() {
                Vec::new()
            } else {
                run_branch_group(
                    &steps,
                    &stale,
                    &root,
                    force,
                    step_timeout_ms,
                    retries,
                    retry_delay_ms,
                )?
            };
            let mut outcomes = outcomes.into_iter();
            for (offset, (_, fresh_row)) in fresh_rows.into_iter().enumerate() {
                let step = &steps[index + offset];
                if let Some(row) = fresh_row {
                    if !dry_run && progress_markers {
                        print_step_marker(step.name.as_str(), true, index + offset, steps.len());
                    }
                    results.push((
                        step.name.clone(),
                        true,
                        0,
                        step.output.display().to_string(),
                    ));
                    state_rows.push(row);
                } else {
                    let (name, fresh, ms, output, state_row) =
                        outcomes.next().expect("one outcome per stale step");
                    if !dry_run && progress_markers {
                        print_step_marker(&name, fresh, index + offset, steps.len());
                    }
                    results.push((name, fresh, ms, output));
                    state_rows.push(state_row);
                }
            }
            previous_output = Some(steps[end - 1].output.clone());
            skip = end - index - 1;
            continue;
        }
        let input_path = match &step.input {
            None => previous_output.clone().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the first pipeline step needs an input",
                )
            })?,
            Some(name) => steps
                .iter()
                .find(|candidate| &candidate.name == name)
                .map(|candidate| candidate.output.clone())
                .unwrap_or_else(|| root.join(name)),
        };
        if !input_path.exists() {
            if dry_run {
                results.push((
                    step.name.clone(),
                    false,
                    0,
                    format!("missing input {}", input_path.display()),
                ));
                previous_output = Some(step.output.clone());
                continue;
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "step {}: input {} not found",
                    step.name,
                    input_path.display()
                ),
            ));
        }
        let input_string = input_path.display().to_string();
        let args_hash = pipeline_hash_text(&step.args);
        let input_stamp = pipeline_file_stamp(&input_path)?;
        let output_exists = step.output.exists();
        let output_stamp = if output_exists {
            pipeline_file_stamp(&step.output)?
        } else {
            String::new()
        };
        let hash_content = content_check || pipeline_auto_content_check(&input_path, &step.output);
        let prior = previous.iter().find(|row| {
            row.name == step.name
                && row.command == step.command
                && row.args == step.args
                && row.input == input_string
        });
        let fresh = !force
            && match prior {
                Some(row) => {
                    if hash_content && !row.input_hash.is_empty() && !row.output_hash.is_empty() {
                        output_exists
                            && pipeline_hash_file(&input_path)? == row.input_hash
                            && pipeline_hash_file(&step.output)
                                .map(|hash| hash == row.output_hash)
                                .unwrap_or(false)
                    } else if !row.input_stamp.is_empty() && !row.output_stamp.is_empty() {
                        // Fast path: metadata alone proves the input and output unchanged.
                        output_exists
                            && row.input_stamp == input_stamp
                            && row.output_stamp == output_stamp
                    } else {
                        // Legacy state without stamps: fall back to content hashes.
                        let input_hash = pipeline_hash_file(&input_path)?;
                        output_exists
                            && input_hash == row.input_hash
                            && pipeline_hash_file(&step.output)
                                .map(|hash| hash == row.output_hash)
                                .unwrap_or(false)
                    }
                }
                None => false,
            };
        if fresh {
            let row = prior.expect("fresh requires a prior state row");
            let output_string = step.output.display().to_string();
            if !dry_run && progress_markers {
                print_step_marker(&step.name, true, index, steps.len());
            }
            results.push((step.name.clone(), true, 0, output_string.clone()));
            state_rows.push(PipelineStateRow {
                name: step.name.clone(),
                command: step.command.clone(),
                args: step.args.clone(),
                input: input_string,
                output: output_string,
                input_hash: row.input_hash.clone(),
                args_hash,
                output_hash: row.output_hash.clone(),
                elapsed_ms: 0,
                input_stamp,
                output_stamp,
            });
            previous_output = Some(step.output.clone());
            continue;
        }
        if dry_run {
            results.push((step.name.clone(), false, 0, "would run".to_string()));
            previous_output = Some(step.output.clone());
            continue;
        }
        let input_hash = pipeline_hash_file(&input_path)?;
        if let Some(parent) = step.output.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let temp = step.output.with_file_name(format!(
            ".{}.pipeline.tmp",
            step.output
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("out")
        ));
        let _ = std::fs::remove_file(&temp);
        let out_string = temp.display().to_string();
        let args = split_pipeline_args(&step.args)
            .into_iter()
            .map(|arg| {
                arg.replace("{in}", &input_string)
                    .replace("{out}", &out_string)
            })
            .collect::<Vec<_>>();
        let uses_out = step.args.contains("{out}");
        let command = split_pipeline_args(&step.command);
        let mut output = None;
        let mut elapsed_ms = 0u64;
        for attempt in 0..=retries {
            let _ = std::fs::remove_file(&temp);
            let mut process = std::process::Command::new(&bin);
            process.arg(command.first().map(String::as_str).unwrap_or(""));
            process.arg(&input_path);
            if command.len() > 1 {
                process.args(&command[1..]);
            }
            process.args(&args);
            let started = Instant::now();
            match run_step_command(process, step_timeout_ms) {
                Ok(result) => {
                    elapsed_ms = started.elapsed().as_millis() as u64;
                    if result.status.success() {
                        output = Some(result);
                        break;
                    }
                    let last = format!(
                        "step {} failed: {}",
                        step.name,
                        String::from_utf8_lossy(&result.stderr).trim()
                    );
                    if attempt < retries {
                        eprintln!("{last}; retrying ({}/{})", attempt + 1, retries);
                        thread::sleep(Duration::from_millis(retry_delay_ms));
                        continue;
                    }
                    let _ = std::fs::remove_file(&temp);
                    return Err(std::io::Error::other(format!(
                        "{last} (after {} attempts)",
                        attempt + 1
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    let last = format!("step {} timed out after {} ms", step.name, step_timeout_ms);
                    if attempt < retries {
                        eprintln!("{last}; retrying ({}/{})", attempt + 1, retries);
                        thread::sleep(Duration::from_millis(retry_delay_ms));
                        continue;
                    }
                    let _ = std::fs::remove_file(&temp);
                    return Err(std::io::Error::other(format!(
                        "{last} (after {} attempts)",
                        attempt + 1
                    )));
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&temp);
                    return Err(error);
                }
            }
        }
        let output = output.expect("successful attempt sets output");
        let _ = &output;
        if uses_out {
            if !temp.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("step {} did not write its output", step.name),
                ));
            }
        } else {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(&output.stdout)?;
            file.flush()?;
        }
        std::fs::rename(&temp, &step.output)?;
        let output_hash = pipeline_hash_file(&step.output)?;
        state_rows.push(PipelineStateRow {
            name: step.name.clone(),
            command: step.command.clone(),
            args: step.args.clone(),
            input: input_string,
            output: step.output.display().to_string(),
            input_hash,
            args_hash,
            output_hash,
            elapsed_ms,
            input_stamp,
            output_stamp: pipeline_file_stamp(&step.output)?,
        });
        if !dry_run && progress_markers {
            print_step_marker(&step.name, false, index, steps.len());
        }
        results.push((
            step.name.clone(),
            false,
            elapsed_ms,
            step.output.display().to_string(),
        ));
        previous_output = Some(step.output.clone());
        if index + 1 == steps.len() {
            break;
        }
    }
    if !dry_run {
        save_pipeline_state(&state_path, &state_rows)?;
    }
    let lineage = steps
        .iter()
        .enumerate()
        .map(|(index, step)| (step.name.clone(), pipeline_dependencies(&steps, index)))
        .collect();
    if !dry_run {
        save_pipeline_lineage(plan, &lineage)?;
    }
    Ok((results, lineage))
}

fn cmd_pipeline(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut action: Option<String> = None;
    let mut plan: Option<std::path::PathBuf> = None;
    let mut root: Option<std::path::PathBuf> = None;
    let mut force = false;
    let mut dry_run = false;
    let mut step_timeout_ms = 0u64;
    let mut retries = 0usize;
    let mut retry_delay_ms = 250u64;
    let mut jobs = 1usize;
    let mut dag = false;
    let mut clean = false;
    let mut unlock = false;
    let mut verify = false;
    let mut fail_on_stale = false;
    let mut progress_markers = false;
    let mut content_check = false;
    let mut json = false;
    let mut watch_interval_ms = 5_000u64;
    let mut watch_backoff_max_ms = 60_000u64;
    let mut watch_iterations = 0usize;
    let mut watch_max_failures = 0usize;
    let mut watch_state: Option<std::path::PathBuf> = None;
    let mut verbose = false;
    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--root" => match p.value("--root") {
                Ok(value) => root = Some(std::path::PathBuf::from(value)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--force" => force = true,
            "--dry-run" => dry_run = true,
            "--progress-markers" => progress_markers = true,
            "--content-check" => content_check = true,
            "--fail-on-stale" => fail_on_stale = true,
            "--verify" => verify = true,
            "--unlock" => unlock = true,
            "--clean" => clean = true,
            "--dag" => dag = true,
            "--jobs" => match p
                .value("--jobs")
                .and_then(|value| value.parse::<usize>().map_err(|_| "bad --jobs".to_string()))
            {
                Ok(value) => jobs = value.max(1),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--retries" => match p.value("--retries").and_then(|value| {
                value
                    .parse::<usize>()
                    .map_err(|_| "bad --retries".to_string())
            }) {
                Ok(value) => retries = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--retry-delay" => match p.value("--retry-delay").and_then(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| "bad --retry-delay (milliseconds)".to_string())
            }) {
                Ok(value) => retry_delay_ms = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--step-timeout" => match p.value("--step-timeout").and_then(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| "bad --step-timeout (milliseconds)".to_string())
            }) {
                Ok(value) => step_timeout_ms = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--json" => json = true,
            "--interval-ms" => match p.value("--interval-ms").and_then(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| "bad --interval-ms".to_string())
            }) {
                Ok(value) => watch_interval_ms = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--iterations" => match p.value("--iterations").and_then(|value| {
                value
                    .parse::<usize>()
                    .map_err(|_| "bad --iterations".to_string())
            }) {
                Ok(value) => watch_iterations = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--backoff-max-ms" => match p.value("--backoff-max-ms").and_then(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| "bad --backoff-max-ms".to_string())
            }) {
                Ok(value) => watch_backoff_max_ms = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--max-failures" => match p.value("--max-failures").and_then(|value| {
                value
                    .parse::<usize>()
                    .map_err(|_| "bad --max-failures".to_string())
            }) {
                Ok(value) => watch_max_failures = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--state" => match p.value("--state") {
                Ok(value) => watch_state = Some(std::path::PathBuf::from(value)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--verbose" => verbose = true,
            "-h" | "--help" => {
                print_command_help("pipeline");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => {
                if action.is_none() {
                    action = Some(other.to_string());
                } else if plan.is_none() {
                    plan = Some(std::path::PathBuf::from(other));
                } else {
                    eprintln!("error: unexpected argument: {other}");
                    return 2;
                }
            }
        }
    }
    let Some(action) = action else {
        eprintln!("error: pipeline needs run|status|watch <plan>");
        return 2;
    };
    let Some(plan) = plan else {
        eprintln!("error: pipeline needs a plan file");
        return 2;
    };
    match action.as_str() {
        "watch" => match watch_pipeline(
            &plan,
            root.as_deref(),
            force,
            dry_run,
            step_timeout_ms,
            retries,
            retry_delay_ms,
            jobs,
            dag,
            clean,
            unlock,
            progress_markers,
            content_check,
            json,
            watch_interval_ms,
            watch_backoff_max_ms,
            watch_iterations,
            watch_max_failures,
            watch_state.as_deref(),
            verbose,
        ) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("error: {error}");
                1
            }
        },
        "run" => match run_pipeline_plan(
            &plan,
            root.as_deref(),
            force,
            dry_run,
            step_timeout_ms,
            retries,
            retry_delay_ms,
            jobs,
            dag,
            clean,
            unlock,
            progress_markers,
            content_check,
        ) {
            Ok((results, lineage)) => {
                if dry_run {
                    if json {
                        let items = results
                            .iter()
                            .enumerate()
                            .map(|(index, (name, fresh, _, detail))| {
                                let deps = lineage
                                    .get(index)
                                    .map(|(_, deps)| deps)
                                    .unwrap_or(&Vec::new())
                                    .iter()
                                    .map(|dep| json_quote(dep))
                                    .collect::<Vec<_>>()
                                    .join(",");
                                format!(
                                    r#"{{"step":{},"fresh":{},"detail":{},"deps":[{}]}}"#,
                                    json_quote(name),
                                    fresh,
                                    json_quote(detail),
                                    deps
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(",");
                        println!(r#"{{"dryRun":true,"steps":[{items}]}}"#);
                    } else {
                        for (index, (name, fresh, _, detail)) in results.iter().enumerate() {
                            let deps = lineage
                                .get(index)
                                .map(|(_, deps)| deps.clone())
                                .unwrap_or_default();
                            if progress_markers {
                                println!(
                                    "CSVX_STEP {} {name} {}/{}",
                                    if *fresh { "skip" } else { "done" },
                                    index + 1,
                                    results.len()
                                );
                            }
                            if *fresh {
                                println!("↷ {name}");
                            } else if deps.is_empty() {
                                println!("→ {name} ({detail})");
                            } else {
                                println!("→ {name} ({detail}; deps: {})", deps.join(","));
                            }
                        }
                    }
                    return 0;
                }
                if json {
                    let items: Vec<String> = results
                        .iter()
                        .map(|(name, fresh, ms, output)| {
                            format!(
                                r#"{{"step":"{}","fresh":{},"ms":{},"output":"{}"}}"#,
                                name,
                                fresh,
                                ms,
                                output.replace('\\', "\\\\")
                            )
                        })
                        .collect();
                    println!(r#"{{"steps":[{}]}}"#, items.join(","));
                } else {
                    for (name, fresh, ms, _) in results.iter() {
                        if *fresh {
                            println!("↷ {name}");
                        } else {
                            println!("✓ {name} ({ms} ms)");
                        }
                    }
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        "status" => {
            let rows = load_pipeline_state(&pipeline_state_path(&plan));
            if rows.is_empty() {
                println!("(no pipeline state yet)");
                return 0;
            }
            let fresh: Vec<bool> = if verify {
                rows.iter()
                    .map(|row| pipeline_state_is_fresh(row, content_check))
                    .collect()
            } else {
                vec![false; rows.len()]
            };
            let items: Vec<String> = rows
                .iter()
                .zip(fresh.iter())
                .map(|(row, is_fresh)| {
                    let verify_field = if verify {
                        format!(r#","fresh":{}"#, is_fresh)
                    } else {
                        String::new()
                    };
                    format!(
                        r#"{{"step":"{}","output":"{}","inputHash":"{}","outputHash":"{}","ms":{}{} }}"#,
                        row.name,
                        row.output.replace('\\', "\\\\"),
                        row.input_hash,
                        row.output_hash,
                        row.elapsed_ms,
                        verify_field
                    )
                })
                .collect();
            if json {
                println!(r#"{{"steps":[{}]}}"#, items.join(","));
                if verify && fail_on_stale && fresh.iter().any(|is_fresh| !*is_fresh) {
                    return 3;
                }
                return 0;
            }
            if verify {
                println!("{:<20} {:<32} {:>8} status", "step", "output", "ms");
                for (row, is_fresh) in rows.iter().zip(fresh.iter()) {
                    println!(
                        "{:<20} {:<32} {:>8} {}",
                        row.name,
                        row.output,
                        row.elapsed_ms,
                        if *is_fresh { "fresh" } else { "stale" }
                    );
                }
            } else {
                println!(
                    "{:<20} {:<32} {:<10} {:>8} state",
                    "step", "output", "input", "ms"
                );
                for row in &rows {
                    println!(
                        "{:<20} {:<32} {:<10} {:>8} {}",
                        row.name, row.output, row.input_hash, row.elapsed_ms, row.output_hash
                    );
                }
            }
            if verify && fail_on_stale && fresh.iter().any(|is_fresh| !*is_fresh) {
                return 3;
            }
            0
        }
        other => {
            eprintln!("error: unknown pipeline action {other}; use run|status");
            2
        }
    }
}

fn cmd_analyze(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut sample: Option<u64> = None;
    let mut input: Option<PathBuf> = None;
    let mut distinct_cap = csvx_core::profile::DEFAULT_DISTINCT_CAP;
    let mut json = false;
    let mut quality = false;
    let mut markdown = false;
    let mut keys: Vec<String> = Vec::new();
    let mut groups: Vec<String> = Vec::new();
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--quality" => quality = true,
            "--markdown" => markdown = true,
            "--key" => match p.value("--key") {
                Ok(v) => keys.extend(
                    v.split(',')
                        .map(|item| item.trim().to_string())
                        .filter(|item| !item.is_empty()),
                ),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--group" => match p.value("--group") {
                Ok(v) => groups.push(v.trim().to_string()),
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

            "--sample" => match p
                .value("--sample")
                .and_then(|v| v.parse().map_err(|_| "bad --sample".to_string()))
            {
                Ok(v) => sample = Some(v),
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
                print_command_help("analyze");
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
    warn_quote_style(&input);
    warn_pending_edits(&input);
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("analyze", rx, true);
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let result =
        csvx_core::profile::profile_quotes(&input, delimiter, distinct_cap, sample, quotes, &tx);
    drop(tx);
    let _ = reporter.join();
    let profiles = match result {
        Ok(p) => p,
        Err(_) => {
            // The reporter already printed the reason.
            return 1;
        }
    };

    if quality {
        for key in &keys {
            if !profiles.iter().any(|profile| &profile.name == key) {
                eprintln!("error: unknown key column {key}");
                return 2;
            }
        }
        for group in &groups {
            if !profiles.iter().any(|profile| &profile.name == group) {
                eprintln!("error: unknown group column {group}");
                return 2;
            }
        }
        if markdown {
            print_quality_markdown(&input, &profiles, &keys, &groups);
            return 0;
        }
        if json {
            let cols: Vec<String> = profiles.iter().map(profile_json).collect();
            println!(
                r#"{{"columns":[{}],"quality":{}}}"#,
                cols.join(","),
                quality_json(&profiles, &keys, &groups)
            );
            return 0;
        }
        print_quality_text(&profiles, &keys, &groups);
        return 0;
    }
    if markdown {
        eprintln!("error: --markdown needs --quality");
        return 2;
    }
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
    if let Some(limit) = sample {
        // Say what the number is. A prefix sample is an estimate, and a user who
        // reads "1000 rows" as "this file has 1000 rows" will draw wrong conclusions.
        println!(
            "⚠ sampled the first {limit} rows (a prefix, not a random sample) — counts are estimates"
        );
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

fn profile_json(c: &csvx_core::profile::ColumnProfile) -> String {
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

/// `schema FILE [--has COL] [--sample N] [--json]`
///
/// The question a front-end asks before drawing anything: which columns exist, in what
/// order, and what is in them. `--has` answers one column and exits non-zero when it is
/// missing, so a shell script can branch on it the way `grep -q` does.
fn cmd_schema(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut input: Option<PathBuf> = None;
    let mut has: Option<String> = None;
    let mut require: Vec<String> = Vec::new();
    let mut diff_with: Option<PathBuf> = None;
    let mut sample: u64 = 10;
    let mut json = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--has" => match p.value("--has") {
                Ok(v) => has = Some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--diff" => match p.value("--diff") {
                Ok(v) => diff_with = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--require" => match p.value("--require") {
                Ok(v) => {
                    require = v
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
            "--json" => json = true,
            "--delim" => match p.value("--delim") {
                Ok(v) => delim = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("schema");
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
        eprintln!("error: schema needs an <input> path");
        return 2;
    };
    let delimiter = match parse_delimiter(&delim, &input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let request = csvx_core::ops::SchemaRequest {
        path: input.clone(),
        delimiter,
        sample,
    };
    let sheet = match request.open() {
        Ok(sheet) => sheet,
        Err(e) => return report_failure("schema", format!("cannot open the sheet: {e}")),
    };

    if let Some(other) = &diff_with {
        let other_sheet =
            match csvx_core::sheet::Sheet::open(other, csvx_core::sheet::ReadOptions { delimiter })
            {
                Ok(sheet) => sheet,
                Err(e) => {
                    return report_failure(
                        "schema",
                        format!("cannot open {}: {e}", other.display()),
                    )
                }
            };
        let mine = sheet.columns().unwrap_or_default();
        let theirs = other_sheet.columns().unwrap_or_default();
        let diff = csvx_core::schema::diff(&mine, &theirs);
        if json {
            println!("{}", diff.to_json());
        } else {
            println!(
                "{} ({} columns) vs {} ({} columns)",
                input.display(),
                mine.len(),
                other.display(),
                theirs.len()
            );
            if diff.same {
                println!("✓ same column set");
            }
            for name in &diff.removed {
                println!("  − {name}   only in {}", input.display());
            }
            for name in &diff.added {
                println!("  + {name}   only in {}", other.display());
            }
            for moved in &diff.moved {
                println!(
                    "  → {}   position {} here, {} there",
                    moved.name, moved.here, moved.there
                );
            }
        }
        return if diff.same { 0 } else { 1 };
    }

    if !require.is_empty() {
        let columns = sheet.columns().unwrap_or_default();
        let missing: Vec<&String> = require
            .iter()
            .filter(|name| !columns.iter().any(|c| c.as_str() == name.as_str()))
            .collect();
        if missing.is_empty() {
            println!("✓ all {} required column(s) present", require.len());
            return 0;
        }
        eprintln!(
            "✗ missing {} of {} required column(s):",
            missing.len(),
            require.len()
        );
        for name in &missing {
            eprintln!(
                "  {name} — {}",
                csvx_core::csv_reader::column_candidates(name, &columns)
            );
        }
        return 1;
    }
    if let Some(name) = &has {
        let index = match sheet.column_index(name) {
            Ok(index) => index,
            Err(e) => return report_failure("schema", format!("cannot read the columns: {e}")),
        };
        match index {
            Some(index) => {
                if json {
                    println!(
                        r#"{{"name":{},"exists":true,"index":{index}}}"#,
                        serve::json_str(name)
                    );
                } else {
                    println!("✓ column {name:?} exists at position {index}");
                }
                0
            }
            None => {
                if json {
                    println!(r#"{{"name":{},"exists":false}}"#, serve::json_str(name));
                } else {
                    let columns = sheet.columns().unwrap_or_default();
                    eprintln!("✗ no column {name:?} in {} column(s)", columns.len());
                    // The same candidate logic every other column lookup uses.
                    eprintln!(
                        "  {}",
                        csvx_core::csv_reader::column_candidates(name, &columns)
                    );
                }
                1
            }
        }
    } else {
        let response = match request.response(&sheet) {
            Ok(response) => response,
            Err(e) => return report_failure("schema", format!("cannot profile the columns: {e}")),
        };
        let schema = response.columns;
        let rows = response.rows;
        if json {
            let body: Vec<String> = schema
                .iter()
                .map(|c| {
                    format!(
                        r#"{{"name":{},"index":{},"kind":{},"empty":{},"sampled":{},"distinct":{},"distinctCapped":{}}}"#,
                        serve::json_str(&c.name),
                        c.index,
                        serve::json_str(c.kind.as_str()),
                        c.empty,
                        c.sampled,
                        c.distinct,
                        c.distinct_capped
                    )
                })
                .collect();
            println!(
                r#"{{"rows":{rows},"count":{},"columns":[{}]}}"#,
                schema.len(),
                body.join(",")
            );
            return 0;
        }
        println!(
            "{} — {rows} rows, {} column(s), kinds sampled from the first {sample}",
            input.display(),
            schema.len()
        );
        let mut table = String::from(
            "  #    name                                     kind    empty   distinct\n",
        );
        for column in &schema {
            let empty = if column.sampled == 0 {
                "-".to_string()
            } else {
                format!("{}/{}", column.empty, column.sampled)
            };
            let distinct = if column.distinct_capped {
                format!("{}+", column.distinct)
            } else {
                column.distinct.to_string()
            };
            table.push_str(&format!(
                "  {:<4} {:<40} {:<7} {empty:<7} {distinct}\n",
                column.index,
                truncate_for_display(&column.name, 40),
                column.kind.as_str()
            ));
        }
        print!("{table}");
        0
    }
}

fn cmd_columns(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
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
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
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
                .and_then(|v| csvx_core::reshape::ReshapeSpec::parse_renames(&v))
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
                print_command_help("columns");
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
    let spec = csvx_core::reshape::ReshapeSpec {
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

    if let Err(code) = check_output(Some(&input), &output) {
        return code;
    }
    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("columns", rx, json);
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let result = csvx_core::reshape::reshape_quotes(&input, delimiter, &spec, &output, quotes, &tx);
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
                    csvx_core::check::Expectation::rows(s.rows).with_columns(s.columns.len()),
                    json,
                );
                if !ok {
                    return 1;
                }
            }
            0
        }
        Err(e) => report_failure("lookup", e), // engine returned the error itself
    }
}

fn cmd_dedupe(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut keys: Vec<String> = Vec::new();
    let mut max_keys = csvx_core::dedupe::DEFAULT_MAX_KEYS;
    let mut json = false;
    let mut verify = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--quotes" => match p.value("--quotes") {
                Ok(v) => quotes_text = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--by" => match p.value("--by") {
                Ok(v) => keys = v.split(',').map(|s| s.trim().to_string()).collect(),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--verify" => verify = true,
            "--engine" => {
                eprintln!(
                    "error: dedupe has no sql engine; it keeps the first occurrence, which \
                     needs a stable row order that SQL does not promise"
                );
                return 2;
            }
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
                print_command_help("dedupe");
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
    let quotes = match parse_quotes(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if let Err(code) = check_output(Some(&input), &output) {
        return code;
    }
    let result =
        csvx_core::dedupe::dedupe_quotes(&input, delimiter, &keys, &output, max_keys, quotes, &tx);
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
                    csvx_core::check::Expectation::rows(s.kept).with_ragged(s.kept_ragged),
                    json,
                );
                if !ok {
                    return 1;
                }
            }
            0
        }
        Err(e) => report_failure("dedupe", e), // engine returned the error itself
    }
}

/// Compare two files in fixed-size chunks, so verifying a 10 GB split does not need
/// 20 GB of RAM.
///
/// Returns the first differing byte offset and the two file lengths.
fn files_differ(first: &Path, second: &Path) -> std::io::Result<Option<(u64, u64, u64)>> {
    use std::io::Read;
    const CHUNK: usize = 256 * 1024;
    let mut a = std::io::BufReader::with_capacity(CHUNK, std::fs::File::open(first)?);
    let mut b = std::io::BufReader::with_capacity(CHUNK, std::fs::File::open(second)?);
    let mut left = vec![0u8; CHUNK];
    let mut right = vec![0u8; CHUNK];
    let mut offset: u64 = 0;
    loop {
        let a_read = a.read(&mut left)?;
        let b_read = b.read(&mut right)?;
        if a_read == 0 && b_read == 0 {
            return Ok(None);
        }
        let slice_a = &left[..a_read];
        let slice_b = &right[..b_read];
        if a_read != b_read || slice_a != slice_b {
            let len_a = std::fs::metadata(first).map(|m| m.len()).unwrap_or(0);
            let len_b = std::fs::metadata(second).map(|m| m.len()).unwrap_or(0);
            // Where inside this chunk they diverge.
            let at = slice_a
                .iter()
                .zip(slice_b.iter())
                .position(|(x, y)| x != y)
                .unwrap_or(slice_a.len().min(slice_b.len()));
            return Ok(Some((offset + at as u64, len_a, len_b)));
        }
        offset += a_read as u64;
    }
}

fn cmd_ops(args: &[String]) -> i32 {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                print_command_help("ops");
                return 0;
            }
            other => {
                eprintln!("error: unknown option for ops: {other}");
                return 2;
            }
        }
    }
    if json {
        println!("{}", csvx_core::contract::operations_json());
    } else {
        println!("operations ({}):", csvx_core::contract::OPERATIONS.len());
        for op in csvx_core::contract::OPERATIONS {
            println!("{:<18} {:<8} {}", op.name, op.effect.as_str(), op.summary);
        }
        println!("error codes: {}", csvx_core::contract::ERROR_CODES.len());
    }
    0
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

#[allow(dead_code)]
fn header_len(path: &Path) -> usize {
    read_headers(path, None).map(|h| h.len()).unwrap_or(0)
}
