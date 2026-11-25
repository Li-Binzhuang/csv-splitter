//! Headless CLI for csvx.
//!
//! The desktop app is great for one-off jobs, but servers and scripts need a
//! binary that can be piped into cron/Makefile/CI. Every subcommand here shares
//! the exact same core pipeline as the GUI, so behaviour cannot drift.

use std::process::ExitCode;

mod cli_support;
mod commands;
mod serve;

pub(crate) use cli_support::*;

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
