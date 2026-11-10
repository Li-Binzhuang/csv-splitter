pub mod aggregate;
pub mod book;
pub mod cache;
pub mod chart;
pub mod check;
pub mod compress;
pub mod concat;
pub mod csv_reader;
pub mod decompress;
pub mod dedupe;
pub mod demo;
pub mod encoding;
pub mod filter;
pub mod format;
pub mod formula;
pub mod groupby;
pub mod index;
pub mod jobs;
pub mod layout;
pub mod listing;
pub mod lookup;
pub mod merge;
pub mod pack;
pub mod partition;
pub mod pivot;
pub mod preview;
pub mod profile;
pub mod reshape;
pub mod search;
pub mod sheet;
pub mod sort;
pub mod split_parallel;
pub mod splitter;
#[cfg(feature = "sql")]
pub mod sql;
pub mod tempdir;
pub mod types;
pub mod validate;
pub mod viewer;
pub mod workbook;

pub use types::*;

/// JSON string quoting, shared by the modules that emit JSON without serde.
pub(crate) fn format_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
