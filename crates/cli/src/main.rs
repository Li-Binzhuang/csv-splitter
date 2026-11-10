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

use csv_splitter_core::search::SearchSpec;
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
        Some("format") => cmd_format(&args[1..]),
        Some("chart") => cmd_chart(&args[1..]),
        Some("layout") => cmd_layout(&args[1..]),
        Some("lookup") => cmd_lookup(&args[1..]),
        Some("book") => cmd_book(&args[1..]),
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
            ("--json", "Emit JSON"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            ("--sample", "Sample this many rows instead of reading all"),
        ],
    ),
    (
        "book",
        &[
            ("--json", "Emit JSON"),
            ("--max", "Maximum sheets to list"),
            ("--root", "Directory to serve / list"),
        ],
    ),
    (
        "chart",
        &[
            ("--by", "Separator (split-column) or grouping column"),
            ("--apply-edits", "Materialise pending edits before reading"),
            ("--delim", "Field delimiter (default: auto)"),
            ("--desc", "Descending order"),
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
            ("--ignore-header", "Skip the header check"),
            ("--json", "Emit JSON"),
            ("--out", "Output path"),
        ],
    ),
    (
        "convert",
        &[
            ("--from", "Source encoding"),
            ("--out", "Output path"),
            ("--quiet", "Only errors"),
            ("--to", "Target encoding"),
        ],
    ),
    (
        "dedupe",
        &[
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
            ("--to", "New column name / replacement text"),
            ("--value", "Value to write"),
            (
                "--invalid",
                "With delete: remove rows a validation rule rejects",
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
            ("--order", "Sort rows, e.g. \"city,amount:desc\""),
            ("--out", "Output path"),
            ("--quiet", "Only errors"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
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
            ("--freeze-first", "Freeze these leading columns"),
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
        &[("--out", "Output path"), ("--quiet", "Only errors")],
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
        "preview",
        &[
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
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            (
                "--regex",
                "Treat the pattern as a pattern: . * + ? ^ $ [a-z]",
            ),
            ("--show", "Also print the matching rows"),
        ],
    ),
    (
        "serve",
        &[
            ("--addr", "Bind address (127.0.0.1:0 picks a free port)"),
            ("--root", "Directory to serve / list"),
        ],
    ),
    (
        "sort",
        &[
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
        "stats",
        &[
            ("--delim", "Field delimiter (default: auto)"),
            ("--quotes", "Quote style: rfc4180 | backslash | none"),
            ("--sample", "Sample this many rows instead of reading all"),
            ("--top", "How many values to show per column"),
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
    println!("csv-splitter {VERSION} — {command}\n");
    for (flag, description) in options.iter() {
        println!("  {flag:20} {description}");
    }
    println!("\n  --help               Show help and exit");
    println!("\nRun `csv-splitter --help` for the list of commands.");
}

fn print_help() {
    println!(
        r#"csv-splitter {VERSION} — split / merge / inspect large CSV files

USAGE
  csv-splitter <command> [options]

WHAT DO YOU WANT TO DO?
  open a huge file without loading it      preview, index
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
  serve    --root <dir>          Local HTTP API (JSON) for a web front-end
  convert  <input>               Transcode a CSV to UTF-8 (GBK / UTF-16 input)
  filter   <input> --where EXPR  Row numbers matching a column predicate
  sort     <input> --by COLS -o F Sort the whole file (a,b or a:desc,b:asc)
  analyze  <input>               Per-column profile: empties, distinct, numeric summary
  dedupe   <input> -o F          Drop duplicate rows (whole row or --by columns)
  columns  <input>                Keep/drop/rename columns (-o writes the result)
  edit     <action> <input>      Spreadsheet-style edits over an immutable base
  partition <input> --by COL      One file per distinct value of COL (tar/zip)
  concat   <a.csv> <b.csv> ...    Glue same-shaped files into one (header checked)
  groupby  <input> --by K --agg A Group and aggregate into a CSV
  pivot    <input> --rows R --values A Rows × columns × measures matrix
  format   <input> --rule R          Conditional formatting: which cells to paint
  chart    <input> --x C --y C:op    Chart data (categories + series) as JSON
  layout   <input> [--set C:k=v]    Column widths, names, freeze and number formats
  lookup   <input> --key C --from R Join a reference table onto the file (VLOOKUP)
  book     <dir>                  List every CSV in a directory as one workbook
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
    let mut quotes_text = String::new();
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

    let quotes = match csv_splitter_core::csv_reader::QuoteStyle::parse(&quotes_text) {
        Ok(q) => q,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    match csv_splitter_core::preview::load_preview_with(&input, rows, override_delim, quotes) {
        Ok(preview) => {
            println!(
                "{} — {:.1} MB, ~{} rows, delimiter {:?}",
                input.display(),
                preview.file_size as f64 / (1024.0 * 1024.0),
                preview.total_rows_estimate,
                preview.detected_delimiter
            );
            print_table(&preview.headers, &preview.rows);
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

    let sidecar = csv_splitter_core::workbook::Journal::sidecar(&input);
    warn_journal_damage(&input);
    let journal = match csv_splitter_core::workbook::Journal::load(&sidecar) {
        Ok(j) => j,
        Err(e) => return report_failure("view", format!("cannot read the journal: {e}")),
    };
    let ops = journal.ops().to_vec();

    // The predicate is compiled once, against the *edited* column names, so a filter
    // on a renamed column behaves the way the user sees the sheet.
    let compiled = match &where_expr {
        None => None,
        Some(expr) => {
            let spec = match csv_splitter_core::filter::FilterSpec::parse(expr) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            };
            let set = csv_splitter_core::filter::FilterSet::single(spec);
            let columns = match csv_splitter_core::workbook::resolve_columns(
                &ops,
                &match csv_splitter_core::workbook::base_header(&input, delimiter) {
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
    let paging_file = if order.is_some() {
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
        let result = csv_splitter_core::workbook::export_quotes(
            &input, &ops, &exported, delimiter, quotes, &tx,
        );
        drop(tx);
        let _ = reporter.join();
        if let Err(e) = result {
            let _ = std::fs::remove_file(&exported);
            return report_failure("view", format!("cannot materialise the sheet: {e}"));
        }
        if let Some(expr) = &where_expr {
            let set = match csv_splitter_core::filter::FilterSpec::parse(expr) {
                Ok(spec) => csv_splitter_core::filter::FilterSet::single(spec),
                Err(e) => {
                    let _ = std::fs::remove_file(&exported);
                    eprintln!("error: {e}");
                    return 2;
                }
            };
            let filtered = work.with_extension("filtered.csv");
            let (tx, rx) = mpsc::channel();
            let reporter = spawn_reporter("view", rx, true);
            let result = csv_splitter_core::filter::extract_style(
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
        let result = csv_splitter_core::sort::external_sort_columns(
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
    if let Some(path) = &paging_file {
        if let Ok(file) = std::fs::File::open(path) {
            let mut reader = csv_splitter_core::csv_reader::RecordReader::new(
                std::io::BufReader::with_capacity(4 * 1024 * 1024, file),
                4 * 1024 * 1024,
            );
            let mut record = Vec::new();
            let mut index: u64 = 0;
            while reader.next_record(&mut record).is_ok_and(|more| more) {
                let cells = csv_splitter_core::csv_reader::parse_csv_record_with(
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
            let scan_pager = match csv_splitter_core::workbook::Pager::open(&input, &ops, delimiter)
            {
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
                                csv_splitter_core::format::pattern_matches(&needle, cell)
                            } else {
                                csv_splitter_core::format::pattern_matches(
                                    &needle,
                                    &cell.to_lowercase(),
                                )
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
        let pager = match csv_splitter_core::workbook::Pager::open(&input, &ops, delimiter) {
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

    if json {
        let body = rows
            .iter()
            .map(|row| {
                let cells: Vec<String> = row.iter().map(|c| serve::json_str(c)).collect();
                format!("[{}]", cells.join(","))
            })
            .collect::<Vec<_>>()
            .join(",");
        println!(
            r#"{{"columns":[{}],"rows":[{}]}}"#,
            columns
                .iter()
                .map(|c| serve::json_str(c))
                .collect::<Vec<_>>()
                .join(","),
            body
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
            match csv_splitter_core::format::Rule::parse(text) {
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
    let quotes = match csv_splitter_core::csv_reader::QuoteStyle::parse(&quotes_text) {
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
                match csv_splitter_core::sort::external_sort_columns(
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
    let result = match csv_splitter_core::search::search_quotes(
        &input, delimiter, &spec, limit, quotes, &tx,
    ) {
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
    match serve::run(serve::ServerConfig { addr, root }) {
        Ok(()) => 0,
        Err(_) => 1, // the reporter already printed the reason
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
    let result = csv_splitter_core::preview::compute_column_stats_with(
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

fn cmd_book(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    let mut json = false;
    let mut max_sheets = csv_splitter_core::book::DEFAULT_MAX_SHEETS;

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

    let Some(root) = root else {
        eprintln!("error: book needs a directory (--root <dir>)");
        return 2;
    };
    let book = match csv_splitter_core::book::scan(&root, max_sheets) {
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
    let mut max_keys = csv_splitter_core::lookup::DEFAULT_MAX_KEYS;
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

    let options = csv_splitter_core::lookup::LookupOptions {
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
    let result =
        csv_splitter_core::lookup::lookup_quotes(&input, delimiter, &output, &options, quotes, &tx);
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
    let base_columns =
        csv_splitter_core::workbook::base_header(input, csv_splitter_core::types::Delimiter::Comma)
            .map_err(|e| format!("cannot read the header: {e}"))?;
    csv_splitter_core::workbook::Journal::load(&csv_splitter_core::workbook::Journal::sidecar(
        input,
    ))
    .map(|journal| {
        csv_splitter_core::workbook::column_layout(journal.ops(), &base_columns)
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
            "--freeze-first" => match p
                .value("--freeze-first")
                .and_then(|v| v.parse().map_err(|_| "bad --freeze-first".to_string()))
            {
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
    let mut layout = match csv_splitter_core::layout::Layout::load(&input) {
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
    let mut max_points = csv_splitter_core::chart::DEFAULT_MAX_POINTS;
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
    let spec = match csv_splitter_core::aggregate::AggSpec::parse_list(&y) {
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
    let chart_kind = match csv_splitter_core::chart::ChartKind::parse(&kind) {
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
    let options = csv_splitter_core::chart::ChartOptions {
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
    let result =
        csv_splitter_core::chart::chart_quotes(sheet.path(), delimiter, &options, quotes, &tx);
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
        match csv_splitter_core::format::Rule::parse(text) {
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
    let sidecar = csv_splitter_core::workbook::Journal::sidecar(&input);
    let journal = match csv_splitter_core::workbook::Journal::load(&sidecar) {
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
            csv_splitter_core::format::count_matches(&input, journal.ops(), &rules, delimiter, &tx);
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
    let cells = match csv_splitter_core::format::styled_window(
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
    let mut max_columns = csv_splitter_core::pivot::DEFAULT_MAX_COLUMNS;
    let mut max_cells = csv_splitter_core::pivot::DEFAULT_MAX_CELLS;
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
    let specs = match csv_splitter_core::aggregate::AggSpec::parse_list(&values) {
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
    let options = csv_splitter_core::pivot::PivotOptions {
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
        let result = csv_splitter_core::pivot::pivot_quotes(
            sheet.path(),
            delimiter,
            &output,
            &options,
            quotes,
            &tx,
        );
        drop(tx);
        let _ = reporter.join();
        match result {
            Ok(summary) => Some((summary, None)),
            // The engine returns the error itself; nothing else printed it.
            Err(e) => return report_failure("pivot", e),
        }
    } else {
        let result =
            csv_splitter_core::pivot::pivot_rows(sheet.path(), delimiter, &options, quotes, &tx);
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

fn cmd_validate(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
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
    let rules: Vec<csv_splitter_core::validate::Rule> = {
        let mut parsed = Vec::new();
        for text in &rules_text {
            match csv_splitter_core::validate::Rule::parse(text) {
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
    let ops = match csv_splitter_core::workbook::Journal::load(
        &csv_splitter_core::workbook::Journal::sidecar(&input),
    ) {
        Ok(journal) => journal.ops().to_vec(),
        Err(e) => return report_failure("validate", e.to_string()),
    };

    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("validate", rx, quiet || json);
    let result = csv_splitter_core::validate::validate(&input, &ops, &rules, delimiter, &tx);
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
    if report.unique_full {
        println!(
            "✗ unique ran out of room after {} distinct values; raise the cap or check a narrower range",
            csv_splitter_core::validate::MAX_UNIQUE_VALUES
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
                csv_splitter_core::validate::MAX_VIOLATIONS
            );
        }
    }
    println!(
        "\n{} rows checked, {} violation(s)",
        report.rows,
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
    let mut max_groups = csv_splitter_core::groupby::DEFAULT_MAX_GROUPS;
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
    let result = csv_splitter_core::groupby::groupby_quotes(
        sheet.path(),
        delimiter,
        &output,
        &options,
        quotes,
        &tx,
    );
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
    let sidecar = csv_splitter_core::workbook::Journal::sidecar(input);
    if let Ok(journal) = csv_splitter_core::workbook::Journal::load(&sidecar) {
        if !journal.is_empty() {
            eprintln!(
                "note: {} pending edit(s) in {} are not applied by this command; run `csv-splitter edit export {}` to materialise them",
                journal.len(),
                sidecar.display(),
                input.display()
            );
        }
    }
}

/// The `--quotes` value for a command, or the RFC default.
fn parse_quotes(text: &str) -> Result<csv_splitter_core::csv_reader::QuoteStyle, String> {
    csv_splitter_core::csv_reader::QuoteStyle::parse(text)
}

/// Record one operation, save the journal, and report it the way other edits do.
fn journal_push(
    journal: &mut csv_splitter_core::workbook::Journal,
    sidecar: &std::path::Path,
    op: csv_splitter_core::workbook::Op,
) -> Result<i32, i32> {
    let description = op.describe();
    journal.push(op);
    match journal.save(sidecar) {
        Ok(()) => {
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
fn print_chart(data: &csv_splitter_core::chart::ChartData) {
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
fn print_table_styled(
    columns: &[String],
    rows: &[Vec<String>],
    start: u64,
    marks: &[(u64, String, csv_splitter_core::format::Style)],
) {
    use std::io::IsTerminal;
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
    let sidecar = csv_splitter_core::index::SparseIndex::sidecar_path(input);
    if sidecar.exists() {
        return;
    }
    let size = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    if size < 50 * 1024 * 1024 {
        return;
    }
    eprintln!(
        "note: building a row index for {} ({}) — one-time; run `csv-splitter index` earlier to avoid the wait",
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
    let Ok(file) = csv_splitter_core::encoding::open_decoded(input) else {
        return;
    };
    use std::io::Read;
    // Read a window, never the whole file: this runs before every preview and
    // analyze, and a 10 GB file would otherwise be paged in for one warning.
    let mut head = Vec::with_capacity(64 * 1024);
    if file.take(64 * 1024).read_to_end(&mut head).is_ok() {
        if let Some(style) = csv_splitter_core::csv_reader::detect_quote_style(&head) {
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
    temporary: Option<csv_splitter_core::tempdir::TempDir>,
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
    let ops = match csv_splitter_core::workbook::Journal::load(
        &csv_splitter_core::workbook::Journal::sidecar(input),
    ) {
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
    let dir = match csv_splitter_core::tempdir::TempDir::new("applied") {
        Ok(dir) => dir,
        Err(e) => return Err(report_failure("temp", e.to_string())),
    };
    let target = dir.path().join("sheet.csv");
    let (tx, rx) = mpsc::channel();
    let reporter = spawn_reporter("export", rx, true);
    let result = csv_splitter_core::workbook::export(input, &ops, &target, delimiter, &tx);
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
    let sidecar = csv_splitter_core::workbook::Journal::sidecar(input);
    if let Ok((_, skipped, skipped_redo)) =
        csv_splitter_core::workbook::Journal::load_reporting(&sidecar)
    {
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

/// Check a formula before it becomes a journal step: unknown columns and missing
/// sheets are cheap to spot now and expensive to discover after an export.
///
/// A missing sheet is a warning rather than an error — `ref()` returns `#VALUE!` for
/// it, which is a legitimate answer if the file is expected later — but every cell of
/// the new column would be `#VALUE!`, and nobody wants to find that out row by row.
fn check_formula(
    input: &Path,
    delimiter: Delimiter,
    ops: &[csv_splitter_core::workbook::Op],
    formula: &str,
) -> Result<(), i32> {
    let parsed = match csv_splitter_core::formula::Formula::parse(formula) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("error: {e}");
            return Err(2);
        }
    };
    let columns = match csv_splitter_core::workbook::column_layout(
        ops,
        &match csv_splitter_core::workbook::base_header(input, delimiter) {
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
    let mut col: Option<String> = None;
    let mut value: Option<String> = None;
    let mut from: Option<String> = None;
    let mut by: Option<String> = None;
    let mut into: Option<String> = None;
    let mut row_from: Option<u64> = None;
    let mut row_to: Option<u64> = None;
    let mut columns: Option<Vec<String>> = None;
    let mut invalid_rule: Option<String> = None;
    let mut find: Option<String> = None;
    let mut find_col: Option<Vec<String>> = None;
    let mut find_nth: u64 = 1;
    let mut match_case = false;
    let mut order: Option<Vec<csv_splitter_core::sort::SortColumn>> = None;
    let mut count: Option<u64> = None;
    let mut regex = false;
    let mut name: Option<String> = None;
    let mut out: Option<PathBuf> = None;
    let mut where_expr: Option<String> = None;
    let mut formula_expr: Option<String> = None;
    let mut after_col: Option<String> = None;
    let mut to_block: Option<String> = None;
    let mut json = false;
    let mut delim = "auto".to_string();

    while let Some(arg) = p.next() {
        match arg.as_str() {
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
            "error: edit needs an action: set | insert | delete | replace | undo | status | export"
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
    let sidecar = csv_splitter_core::workbook::Journal::sidecar(&input);
    warn_journal_damage(&input);
    let mut journal = match csv_splitter_core::workbook::Journal::load(&sidecar) {
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
            match csv_splitter_core::workbook::parse_a1_range(&from, &columns) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            };
        let (target_row, target_col) = match csv_splitter_core::workbook::parse_a1(&to, &columns) {
            Ok((r, c)) => (r, c),
            Err(e) => {
                eprintln!("error: {e}");
                return 2;
            }
        };
        let width = end_col - start_col + 1;
        let height = (end_row - start_row + 1) as usize;
        let cells = width * height;
        if cells > csv_splitter_core::workbook::MAX_BLOCK_CELLS {
            eprintln!(
                "error: {width} × {height} is {cells} cells; the limit is {} — use `edit set --where` for a bulk change",
                csv_splitter_core::workbook::MAX_BLOCK_CELLS
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
        let window = match csv_splitter_core::workbook::window(
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
        let op = csv_splitter_core::workbook::Op::PasteBlock {
            row: target_row,
            column: columns[target_col].clone(),
            rows: block,
        };
        let description = op.describe();
        journal.push(op);
        if let Err(e) = journal.save(&sidecar) {
            return report_failure("edit", format!("cannot save the journal: {e}"));
        }
        println!("✓ {description} ({height_cells} × {width} block)");
        return 0;
    }

    // Actions that do not modify the journal are handled before any save.
    match action.as_str() {
        "status" => {
            let logical =
                csv_splitter_core::workbook::logical_rows(&input, journal.ops()).unwrap_or(0);
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
                    let spec = match csv_splitter_core::filter::FilterSpec::parse(expr) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("error: {e}");
                            return 2;
                        }
                    };
                    let set = csv_splitter_core::filter::FilterSet::single(spec);
                    let columns = match csv_splitter_core::workbook::column_layout(
                        journal.ops(),
                        &match csv_splitter_core::workbook::base_header(&input, delimiter) {
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
            let result = csv_splitter_core::workbook::export_selected(
                &input,
                journal.ops(),
                &target,
                delimiter,
                quotes,
                csv_splitter_core::workbook::ExportOptions {
                    only: only.as_ref(),
                    columns: export_columns,
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
                let outcome = csv_splitter_core::sort::external_sort_columns(
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
                    let spec = csv_splitter_core::reshape::ReshapeSpec {
                        keep: columns.clone().unwrap_or_default(),
                        drop: Vec::new(),
                        rename: Vec::new(),
                    };
                    let (ptx, prx) = mpsc::channel();
                    let reporter = spawn_reporter("columns", prx, true);
                    let projected = csv_splitter_core::reshape::reshape(
                        &sorted, delimiter, &spec, &output, &ptx,
                    );
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
        "undo" => {
            return match journal.undo() {
                Some(op) => {
                    if let Err(e) = journal.save(&sidecar) {
                        return report_failure("edit", format!("cannot save the journal: {e}"));
                    }
                    println!("✓ undone: {}", op.describe());
                    if journal.redo_len() > 0 {
                        println!("  {} operation(s) can be redone", journal.redo_len());
                    }
                    0
                }
                None => {
                    eprintln!("nothing to undo");
                    1
                }
            };
        }
        "redo" => {
            return match journal.redo() {
                Some(op) => {
                    if let Err(e) = journal.save(&sidecar) {
                        return report_failure("edit", format!("cannot save the journal: {e}"));
                    }
                    println!("✓ redone: {}", op.describe());
                    0
                }
                None => {
                    eprintln!("nothing to redo");
                    if journal.undo_len() > 0 {
                        eprintln!(
                            "hint: {} operation(s) are in effect; `edit undo` takes one back",
                            journal.undo_len()
                        );
                        return 1;
                    }
                    1
                }
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
            match csv_splitter_core::workbook::find_row(
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
                    if let Err(e) = csv_splitter_core::filter::FilterSpec::parse(&filter) {
                        eprintln!("error: {e}");
                        return 2;
                    }
                    csv_splitter_core::workbook::Op::SetWhere {
                        filter,
                        column,
                        value,
                    }
                }
                (Some(row), None) => {
                    csv_splitter_core::workbook::Op::SetCell { row, column, value }
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
                let rule = match csv_splitter_core::validate::Rule::parse(check) {
                    Ok(rule) => rule,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 2;
                    }
                };
                let runs = match csv_splitter_core::validate::rows_failing(
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
                let op = csv_splitter_core::workbook::Op::DeleteRanges { runs };
                journal.push(op);
                if let Err(e) = journal.save(&sidecar) {
                    return report_failure("edit", format!("cannot save the journal: {e}"));
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
                let runs = match csv_splitter_core::workbook::runs_matching(
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
                let op = csv_splitter_core::workbook::Op::DeleteRanges { runs: runs.clone() };
                journal.push(op.clone());
                if let Err(e) = journal.save(&sidecar) {
                    return report_failure("edit", format!("cannot save the journal: {e}"));
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
            csv_splitter_core::workbook::Op::DeleteRows { at: row, count }
        }
        "replace" => {
            let (Some(from), Some(to)) = (from.clone(), to_block.clone()) else {
                eprintln!("error: replace needs --from and --to");
                return 2;
            };
            if regex {
                if csv_splitter_core::format::pattern_is_valid(&from).is_none() {
                    eprintln!(
                        "error: '{from}' is not a supported pattern (literals, ., [..], *, +, ?, ^, $, \\d \\w \\s)"
                    );
                    return 2;
                }
                return match journal_push(
                    &mut journal,
                    &sidecar,
                    csv_splitter_core::workbook::Op::SubstituteRegex {
                        column: col.clone(),
                        pattern: from,
                        to,
                    },
                ) {
                    Ok(code) => code,
                    Err(code) => return code,
                };
            }
            csv_splitter_core::workbook::Op::Substitute {
                column: col,
                from,
                to,
            }
        }
        "insert" => {
            let Some(row) = row else {
                eprintln!("error: insert needs --row (and --value as a CSV row)");
                return 2;
            };
            let raw = value.unwrap_or_default();
            let cells: Vec<String> = if raw.trim().is_empty() {
                Vec::new()
            } else {
                csv_splitter_core::csv_reader::parse_csv_record(&raw, b',')
            };
            csv_splitter_core::workbook::Op::InsertRows {
                at: row,
                rows: vec![cells],
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
            csv_splitter_core::workbook::Op::AddColumn {
                name,
                formula: value,
            }
        }
        "insert-column" | "insertcolumn" => {
            let Some(name) = name else {
                eprintln!("error: insert-column needs --name");
                return 2;
            };
            // `--value` is a constant, `--formula` an expression. Exactly one.
            let source = match (value, formula_expr) {
                (Some(v), None) => csv_splitter_core::workbook::ColumnSource::Literal(v),
                (None, Some(f)) => csv_splitter_core::workbook::ColumnSource::Formula(f),
                (Some(_), Some(_)) => {
                    eprintln!("error: insert-column takes --value or --formula, not both");
                    return 2;
                }
                (None, None) => {
                    eprintln!("error: insert-column needs --value <text> or --formula <expr>");
                    return 2;
                }
            };
            csv_splitter_core::workbook::Op::AddColumnAt {
                after: after_col,
                name,
                source,
            }
        }
        "fill" => {
            let (Some(column), Some(from), Some(to)) = (col.clone(), row_from, row_to) else {
                eprintln!("error: fill needs --col, --from-row and --to-row");
                return 2;
            };
            if from == 0 || to == 0 || from > to {
                eprintln!(
                    "error: --from-row/--to-row are 1-based data rows, and from must be <= to"
                );
                return 2;
            }
            csv_splitter_core::workbook::Op::FillRange {
                column,
                from: from - 1,
                to: to - 1,
                value: value.unwrap_or_default(),
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
            csv_splitter_core::workbook::Op::SplitColumn {
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
            csv_splitter_core::workbook::Op::DropColumn { name }
        }
        "rename" => {
            let (Some(from), Some(to)) = (from, to_block) else {
                eprintln!("error: rename needs --from and --to");
                return 2;
            };
            csv_splitter_core::workbook::Op::RenameColumn { from, to }
        }
        other => {
            eprintln!("error: unknown edit action '{other}'");
            return 2;
        }
    };

    journal.push(op.clone());
    if let Err(e) = journal.save(&sidecar) {
        return report_failure("edit", format!("cannot save the journal: {e}"));
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

fn cmd_analyze(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut sample: Option<u64> = None;
    let mut input: Option<PathBuf> = None;
    let mut distinct_cap = csv_splitter_core::profile::DEFAULT_DISTINCT_CAP;
    let mut json = false;
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
    let result = csv_splitter_core::profile::profile_quotes(
        &input,
        delimiter,
        distinct_cap,
        sample,
        quotes,
        &tx,
    );
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
    let result =
        csv_splitter_core::reshape::reshape_quotes(&input, delimiter, &spec, &output, quotes, &tx);
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
        Err(e) => report_failure("lookup", e), // engine returned the error itself
    }
}

fn cmd_dedupe(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut quotes_text = String::new();
    let mut input: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut keys: Vec<String> = Vec::new();
    let mut max_keys = csv_splitter_core::dedupe::DEFAULT_MAX_KEYS;
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
    let result = csv_splitter_core::dedupe::dedupe_quotes(
        &input, delimiter, &keys, &output, max_keys, quotes, &tx,
    );
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
    let merge_result = merge_archive(&archive, &merged, &tx);
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
