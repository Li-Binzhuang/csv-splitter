//! Headless CLI for csvx.
//!
//! The desktop app is great for one-off jobs, but servers and scripts need a
//! binary that can be piped into cron/Makefile/CI. Every subcommand here shares
//! the exact same core pipeline as the GUI, so behaviour cannot drift.

use csvx_core::csv_reader::{detect_delimiter, parse_csv_record, read_headers};
use csvx_core::index::{SparseIndex, DEFAULT_INTERVAL};
use csvx_core::merge::merge_archive_expecting;
use csvx_core::partition::{run_partition, PartitionConfig};

use csvx_core::splitter::run_split_threads;
use csvx_core::sql::{self, DEFAULT_LIMIT};
use csvx_core::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

mod commands;
mod serve;
use commands::agent::cmd_agent;
use commands::book::cmd_book;
use commands::chart::cmd_chart;
use commands::concat::cmd_concat;
use commands::convert::cmd_convert;
use commands::demo::cmd_demo;
use commands::edit::cmd_edit;
use commands::extract::cmd_extract;
use commands::files::cmd_files;
use commands::filter::cmd_filter;
use commands::format::cmd_format;
use commands::gate::cmd_gate;
use commands::groupby::cmd_groupby;
use commands::layout::cmd_layout;
use commands::lookup::cmd_lookup;
use commands::mcp::cmd_mcp;
use commands::pivot::cmd_pivot;
use commands::repair::cmd_repair;
use commands::report::cmd_report;
use commands::report::truncate_for_display;
use commands::search::cmd_search;
use commands::serve::cmd_serve;
use commands::sort::cmd_sort;
use commands::stats::cmd_stats;
use commands::validate::cmd_validate;
use commands::view::cmd_view;

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
        Some("agent") => cmd_agent(&args[1..]),
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
        Some("report") => cmd_report(&args[1..]),
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
            ("--format", "Data format: csv | tsv | ndjson | parquet | xlsx"),
            ("--highlight", "XLSX cellIs rules: amount>1000:red;status=bad:amber"),
            ("--sheet", "Worksheet name for xlsx input (default: first sheet)"),
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
        "agent",
        &[
            ("--root", "Directory the agent API may operate in"),
            ("--addr", "Listen address (default 127.0.0.1:8788)"),
            ("--port", "Listen port shorthand"),
            ("--endpoint", "OpenAI-compatible chat endpoint for agent run"),
            ("--model", "Model id for agent run"),
            ("--prompt", "Prompt for agent run"),
            ("--max-steps", "Maximum tool steps (default 4)"),
            ("--allow-apply", "Allow destructive tools during agent run"),
            ("--skill", "Markdown skill file injected into the agent system prompt"),
            ("--trace", "Write a JSONL trace of model responses and tool calls"),
            ("--replay", "Replay tool calls from a trace without a model"),
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
        "report",
        &[
            ("--delim", "Field delimiter (default: auto)"),
            ("--format", "Output format: md | html | xlsx (default md)"),
            ("--json", "Emit a small JSON confirmation"),
            ("--out", "Output path (default: write to stdout)"),
            ("--spec", "Report spec CSV: summary/columns/rows/quality/groups/chart/compare/datecounts[,date[:group]]"),
            ("--spec-text", "Inline report spec instead of a file"),
            ("--root", "Require compare inputs to stay inside this directory"),
            ("--title", "Report title"),
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
            ("--unique-global", "Require uniqueness across the whole directory"),
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

pub(crate) fn print_command_help(command: &str) {
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
  agent    serve --root <dir>     HTTP Agent API (sessions, actions, audit)
  agent    run --root <dir>       OpenAI-compatible model loop over csvx tools
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

pub(crate) struct Parser {
    args: Vec<String>,
    idx: usize,
}

impl Parser {
    pub(crate) fn new(args: &[String]) -> Self {
        Self {
            args: args.to_vec(),
            idx: 0,
        }
    }

    pub(crate) fn next(&mut self) -> Option<String> {
        let v = self.args.get(self.idx).cloned();
        if v.is_some() {
            self.idx += 1;
        }
        v
    }

    pub(crate) fn value(&mut self, flag: &str) -> Result<String, String> {
        self.next().ok_or_else(|| format!("{flag} needs a value"))
    }
}

pub(crate) fn parse_delimiter(spec: &str, input: &Path) -> Result<Delimiter, String> {
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

pub(crate) fn human_bytes(bytes: u64) -> String {
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
pub(crate) fn spawn_reporter(
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
pub(crate) fn report_failure(command: &str, reason: impl std::fmt::Display) -> i32 {
    let text = reason.to_string();
    match hint_for(&text) {
        Some(hint) => fail_with_hint(command, &text, hint),
        None => fail(command, text),
    }
}

pub(crate) fn self_check(
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
pub(crate) fn print_table(columns: &[String], rows: &[Vec<String>]) {
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
pub(crate) struct MaterializedInput {
    path: std::path::PathBuf,
}

impl Drop for MaterializedInput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) fn cleanup_stale_materialized() {
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

pub(crate) fn materialize_compressed(
    input: &std::path::Path,
) -> std::io::Result<Option<MaterializedInput>> {
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
pub(crate) fn warn_pending_edits(input: &std::path::Path) {
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
pub(crate) fn parse_quotes(text: &str) -> Result<csvx_core::csv_reader::QuoteStyle, String> {
    csvx_core::csv_reader::QuoteStyle::parse(text)
}

/// Record one operation, save the journal, and report it the way other edits do.
pub(crate) fn journal_push(
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
pub(crate) fn project_columns(
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

pub(crate) fn print_table_styled(
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
pub(crate) fn note_index_build(input: &Path) {
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
pub(crate) fn check_output(input: Option<&Path>, output: &Path) -> Result<(), i32> {
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
pub(crate) fn warn_quote_style(input: &std::path::Path) {
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
pub(crate) fn warn_journal_damage(input: &std::path::Path) {
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
pub(crate) fn check_expect(
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
pub(crate) fn check_formula(
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
pub(crate) const MAX_DELETE_RUNS: usize = 1_000;

pub(crate) fn quality_checks(
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

pub(crate) fn json_escape(value: &str) -> String {
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
