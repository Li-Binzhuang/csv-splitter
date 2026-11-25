//! Headless CLI for csvx.
//!
//! The desktop app is great for one-off jobs, but servers and scripts need a
//! binary that can be piped into cron/Makefile/CI. Every subcommand here shares
//! the exact same core pipeline as the GUI, so behaviour cannot drift.

use csvx_core::csv_reader::detect_delimiter;

use csvx_core::splitter::run_split_threads;
use csvx_core::types::SplitConfig;
use csvx_core::*;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

mod commands;
mod serve;
use commands::agent::cmd_agent;
use commands::analyze::cmd_analyze;
use commands::book::cmd_book;
use commands::cache::cmd_cache;
use commands::chart::cmd_chart;
use commands::columns::cmd_columns;
use commands::concat::cmd_concat;
use commands::convert::cmd_convert;
use commands::dedupe::cmd_dedupe;
use commands::demo::cmd_demo;
use commands::edit::cmd_edit;
use commands::extract::cmd_extract;
use commands::files::cmd_files;
use commands::filter::cmd_filter;
use commands::format::cmd_format;
use commands::gate::cmd_gate;
use commands::groupby::cmd_groupby;
use commands::index::cmd_index;
use commands::layout::cmd_layout;
use commands::lookup::cmd_lookup;
use commands::mcp::cmd_mcp;
use commands::merge::cmd_merge;
use commands::ops::cmd_ops;
use commands::partition::cmd_partition;
use commands::pipeline::cmd_pipeline;
use commands::pivot::cmd_pivot;
use commands::preview::cmd_preview;
use commands::repair::cmd_repair;
use commands::report::cmd_report;
use commands::schema::cmd_schema;
use commands::search::cmd_search;
use commands::serve::cmd_serve;
use commands::shared::{display_width, json_quote};
use commands::sort::cmd_sort;
use commands::split::cmd_split;
use commands::sql::cmd_sql;
use commands::stats::cmd_stats;
use commands::validate::cmd_validate;
use commands::verify::cmd_verify;
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

pub(crate) fn out_path_or_default(
    input: &Path,
    by: &str,
    format: CompressFormat,
) -> Result<PathBuf, String> {
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
pub(crate) fn fit_to_terminal(
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

pub(crate) fn note_hidden_columns(hidden: usize) {
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

pub(crate) fn quality_json(
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

pub(crate) fn print_quality_text(
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

pub(crate) fn print_quality_markdown(
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
