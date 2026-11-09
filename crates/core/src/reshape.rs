//! Reshaping a file's columns: drop, keep, rename, reorder.
//!
//! This is the step before handing data to someone else — an internal id column
//! that should not leave the building, a Chinese header that should be English,
//! a column order that another system expects.
//!
//! Unlike `filter --out` (which copies source bytes verbatim), reshaping has to
//! re-serialise every row: the column set changed. That means the quoting rules
//! have to be applied consistently — a cell containing a comma, a quote or a
//! newline is quoted, and quotes inside are doubled. Records are still read with
//! the quote-aware scanner, so a multi-line cell stays one record on the way in
//! and one quoted field on the way out.

use crate::csv_reader::{join_csv_line, parse_csv_record, RecordReader};
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReshapeSpec {
    /// Keep only these columns, in this order. Empty means "all, in place".
    pub keep: Vec<String>,
    /// Columns to drop (applied before `keep`).
    pub drop: Vec<String>,
    /// `old:new` renames, applied to whatever survives.
    pub rename: Vec<(String, String)>,
}

impl ReshapeSpec {
    pub fn is_empty(&self) -> bool {
        self.keep.is_empty() && self.drop.is_empty() && self.rename.is_empty()
    }

    /// Parse `old:new` pairs from `a:b,c:d`.
    pub fn parse_renames(raw: &str) -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        for part in raw.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (old, new) = part
                .split_once(':')
                .ok_or_else(|| format!("rename '{part}' must be old:new"))?;
            let old = old.trim();
            let new = new.trim();
            if old.is_empty() || new.is_empty() {
                return Err(format!("rename '{part}' must have both names"));
            }
            out.push((old.to_string(), new.to_string()));
        }
        Ok(out)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReshapeSummary {
    pub rows: u64,
    pub bytes: u64,
    /// Output column names, in order.
    pub columns: Vec<String>,
    /// Source columns that were not written.
    pub dropped: Vec<String>,
}

/// Resolve the spec against the file's header.
///
/// Returns `(source indices, output names)` so the row loop needs no lookups.
pub fn resolve(spec: &ReshapeSpec, headers: &[String]) -> io::Result<(Vec<usize>, Vec<String>)> {
    let mut indices: Vec<usize> = if spec.keep.is_empty() {
        (0..headers.len()).collect()
    } else {
        let mut list = Vec::new();
        for name in &spec.keep {
            let idx = headers.iter().position(|h| h == name).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("column '{name}' not found"),
                )
            })?;
            list.push(idx);
        }
        list
    };

    // A column that is both kept and dropped would silently vanish, so name it
    // explicitly instead of letting the drop win.
    for name in &spec.drop {
        if spec.keep.contains(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("column '{name}' is both kept and dropped"),
            ));
        }
    }

    for name in &spec.drop {
        let idx = headers.iter().position(|h| h == name).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("column '{name}' not found"),
            )
        })?;
        indices.retain(|&i| i != idx);
        if indices.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the spec drops every column",
            ));
        }
    }

    let mut names: Vec<String> = indices.iter().map(|&i| headers[i].clone()).collect();
    for (old, new) in &spec.rename {
        let idx = names.iter().position(|n| n == old).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot rename '{old}': not in the output columns"),
            )
        })?;
        names[idx] = new.clone();
    }

    Ok((indices, names))
}

/// Write a reshaped copy of `path` to `output`.
pub fn reshape(
    path: &Path,
    delimiter: Delimiter,
    spec: &ReshapeSpec,
    output: &Path,
    tx: &ProgressSender,
) -> io::Result<ReshapeSummary> {
    let total_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseStart {
        phase: "Reshaping columns".into(),
        total_bytes,
    })
    .ok();

    let reader = encoding::open_decoded(path)?;
    let mut records = RecordReader::new(
        BufReader::with_capacity(4 * 1024 * 1024, reader),
        4 * 1024 * 1024,
    );
    let delim = delimiter.byte();
    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty file"));
    }
    let headers = parse_csv_record(&String::from_utf8_lossy(&record), delim);
    let (indices, names) = resolve(spec, &headers)?;

    let mut out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(output)?);
    let names_ref: Vec<&str> = names.iter().map(String::as_str).collect();
    // `join_csv_line` already ends with a newline.
    out.write_all(join_csv_line(&names_ref, delim).as_bytes())?;

    let mut summary = ReshapeSummary {
        columns: names,
        dropped: headers
            .iter()
            .enumerate()
            .filter(|(i, _)| !indices.contains(i))
            .map(|(_, n)| n.clone())
            .collect(),
        ..Default::default()
    };

    while records.next_record(&mut record)? {
        let text = String::from_utf8_lossy(&record);
        let fields = parse_csv_record(text.trim_end_matches(['\n', '\r']), delim);
        let selected: Vec<&str> = indices
            .iter()
            .map(|&i| fields.get(i).map(String::as_str).unwrap_or(""))
            .collect();
        out.write_all(join_csv_line(&selected, delim).as_bytes())?;
        summary.rows += 1;
        if summary.rows.is_multiple_of(200_000) {
            tx.send(ProgressEvent::Progress {
                bytes: summary.rows,
            })
            .ok();
        }
    }
    out.flush()?;
    summary.bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseDone {
        phase: "Reshaping columns".into(),
    })
    .ok();
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_reshape_{tag}_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn join(&self, n: &str) -> std::path::PathBuf {
            self.0.join(n)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn run(csv: &str, spec: ReshapeSpec, tag: &str) -> (ReshapeSummary, String) {
        let d = Dir::new(tag);
        let input = d.join("in.csv");
        let output = d.join("out.csv");
        std::fs::write(&input, csv).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let summary = reshape(&input, Delimiter::Comma, &spec, &output, &tx).unwrap();
        (summary, std::fs::read_to_string(&output).unwrap())
    }

    #[test]
    fn drops_and_renames() {
        let spec = ReshapeSpec {
            rename: vec![("city".into(), "城市".into())],
            drop: vec!["internal_id".into()],
            ..Default::default()
        };
        let (s, text) = run("internal_id,city,amount\n7,CDMX,10\n", spec, "drop");
        assert_eq!(text, "城市,amount\nCDMX,10\n");
        assert_eq!(s.dropped, vec!["internal_id"]);
    }

    #[test]
    fn keep_reorders() {
        let spec = ReshapeSpec {
            keep: vec!["amount".into(), "city".into()],
            ..Default::default()
        };
        let (_, text) = run("city,amount,note\nCDMX,10,ok\n", spec, "keep");
        assert_eq!(text, "amount,city\n10,CDMX\n");
    }

    #[test]
    fn multi_line_and_quoted_cells_survive() {
        let spec = ReshapeSpec {
            drop: vec!["id".into()],
            ..Default::default()
        };
        let (s, text) = run(
            "id,note,tag\n1,\"a\nb\",\"say \"\"hi\"\"\"\n",
            spec,
            "quoted",
        );
        assert_eq!(s.rows, 1, "a quoted newline is still one row");
        assert_eq!(text, "note,tag\n\"a\nb\",\"say \"\"hi\"\"\"\n");
    }

    #[test]
    fn unknown_column_and_conflicting_spec_are_client_errors() {
        let d = Dir::new("bad");
        let input = d.join("in.csv");
        std::fs::write(&input, "a,b\n1,2\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();

        let spec = ReshapeSpec {
            drop: vec!["nope".into()],
            ..Default::default()
        };
        let err = reshape(&input, Delimiter::Comma, &spec, &d.join("o1.csv"), &tx).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let spec = ReshapeSpec {
            keep: vec!["a".into()],
            drop: vec!["a".into()],
            ..Default::default()
        };
        let err = reshape(&input, Delimiter::Comma, &spec, &d.join("o2.csv"), &tx).unwrap_err();
        assert!(err.to_string().contains("both kept and dropped"), "{err}");
    }

    #[test]
    fn rename_parser_rejects_malformed_input() {
        assert!(ReshapeSpec::parse_renames("city:城市,amount:金额").is_ok());
        assert!(ReshapeSpec::parse_renames("city").is_err());
        assert!(ReshapeSpec::parse_renames("city:").is_err());
    }

    #[test]
    fn dropping_every_column_is_refused() {
        let d = Dir::new("all");
        let input = d.join("in.csv");
        std::fs::write(&input, "a,b\n1,2\n").unwrap();
        let spec = ReshapeSpec {
            drop: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = reshape(&input, Delimiter::Comma, &spec, &d.join("o.csv"), &tx).unwrap_err();
        assert!(err.to_string().contains("every column"), "{err}");
    }
}
