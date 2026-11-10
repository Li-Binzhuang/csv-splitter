//! Concatenating several CSV files into one table.
//!
//! `merge` reassembles the chunks a `split` produced: it trusts them and copies
//! records. This is the other direction — "these five exports are the same table,
//! glue them into one" — where the files come from different systems and the
//! headers may or may not agree.
//!
//! Headers are compared, not assumed. By default a mismatch is an error naming
//! the file and the differing columns, because silently appending a column of
//! amounts under a column of ids corrupts the table in a way nobody notices until
//! it matters. `--ignore-header` opts into the dangerous behaviour on purpose.

use crate::csv_reader::RecordReader;
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConcatSummary {
    pub files: u64,
    pub rows: u64,
    pub bytes: u64,
    /// Column header written, as seen in the first file.
    pub columns: Vec<String>,
    /// Files whose header did not match, when `--ignore-header` was set.
    pub mismatched: Vec<PathBuf>,
}

/// Append every record of `inputs` into `output`, writing one header.
pub fn concat(
    inputs: &[PathBuf],
    output: &Path,
    delimiter: Delimiter,
    ignore_header: bool,
    tx: &ProgressSender,
) -> io::Result<ConcatSummary> {
    if inputs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no input files",
        ));
    }
    tx.send(ProgressEvent::PhaseStart {
        phase: "Concatenating".into(),
        total_bytes: inputs
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
            .sum(),
    })
    .ok();

    let mut out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(output)?);
    let mut summary = ConcatSummary::default();
    let mut record = Vec::with_capacity(64 * 1024);

    for (index, path) in inputs.iter().enumerate() {
        let meta = std::fs::metadata(path).map_err(|e| {
            io::Error::new(io::ErrorKind::NotFound, format!("{}: {e}", path.display()))
        })?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a file", path.display()),
            ));
        }

        let reader = encoding::open_decoded(path)?;
        let mut records = RecordReader::new(
            BufReader::with_capacity(4 * 1024 * 1024, reader),
            4 * 1024 * 1024,
        );
        if !records.next_record(&mut record)? {
            // An empty file contributes nothing; skipping is better than
            // inventing a header for it.
            continue;
        }
        let header = String::from_utf8_lossy(&record).to_string();

        if index == 0 {
            summary.columns = split_header(&header, delimiter);
            let mut line = record.clone();
            if !line.ends_with(b"\n") {
                line.push(b'\n');
            }
            out.write_all(&line)?;
        } else {
            let mut this = split_header(&header, delimiter);
            if this != summary.columns {
                if !ignore_header {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "{} has a different header ({}); pass --ignore-header to append anyway",
                            path.display(),
                            this.join(",")
                        ),
                    ));
                }
                summary.mismatched.push(path.clone());
                this.clear();
            }
        }

        while records.next_record(&mut record)? {
            let mut line = record.clone();
            if !line.ends_with(b"\n") {
                line.push(b'\n');
            }
            out.write_all(&line)?;
            summary.rows += 1;
        }
        summary.files += 1;
        if (index as u64).is_multiple_of(50) {
            tx.send(ProgressEvent::Progress {
                bytes: summary.rows,
            })
            .ok();
        }
    }

    out.flush()?;
    summary.bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseDone {
        phase: "Concatenating".into(),
    })
    .ok();
    Ok(summary)
}

fn split_header(header: &str, delimiter: Delimiter) -> Vec<String> {
    crate::csv_reader::parse_csv_record(header.trim_end_matches(['\n', '\r']), delimiter.byte())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            // A monotonic counter as well as the clock: two tests can start inside
            // the same nanosecond on a fast machine, and they did.
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let d = std::env::temp_dir().join(format!(
                "csvs_concat_{tag}_{}_{}_{seq}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
        fn join(&self, n: &str) -> PathBuf {
            self.0.join(n)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn run(files: &[(&str, &str)], ignore: bool) -> Result<(ConcatSummary, String), io::Error> {
        let d = Dir::new("run");
        let mut inputs = Vec::new();
        for (name, body) in files {
            let p = d.join(name);
            std::fs::write(&p, body).unwrap();
            inputs.push(p);
        }
        let output = d.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let summary = concat(&inputs, &output, Delimiter::Comma, ignore, &tx)?;
        let text = std::fs::read_to_string(&output).unwrap();
        Ok((summary, text))
    }

    #[test]
    fn writes_one_header_and_appends_rows() {
        let (s, text) = run(
            &[("a.csv", "id,note\n1,x\n"), ("b.csv", "id,note\n2,y\n")],
            false,
        )
        .unwrap();
        assert_eq!(s.files, 2);
        assert_eq!(s.rows, 2);
        assert_eq!(text, "id,note\n1,x\n2,y\n");
        assert_eq!(s.columns, vec!["id", "note"]);
    }

    #[test]
    fn mismatched_header_is_an_error_by_default() {
        let err = run(
            &[
                ("a.csv", "id,amount\n1,10\n"),
                ("b.csv", "id,total\n2,20\n"),
            ],
            false,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("b.csv"), "{err}");
    }

    #[test]
    fn ignore_header_appends_and_reports_the_mismatch() {
        let (s, text) = run(
            &[
                ("a.csv", "id,amount\n1,10\n"),
                ("b.csv", "id,total\n2,20\n"),
            ],
            true,
        )
        .unwrap();
        assert_eq!(s.rows, 2);
        assert_eq!(s.mismatched.len(), 1);
        assert!(text.contains("2,20"), "{text}");
    }

    #[test]
    fn a_missing_trailing_newline_does_not_glue_rows() {
        let (_s, text) = run(&[("a.csv", "id\n1"), ("b.csv", "id\n2\n")], false).unwrap();
        assert_eq!(text, "id\n1\n2\n");
    }

    #[test]
    fn quoted_newlines_survive_and_count_as_one_row() {
        let (s, text) = run(&[("a.csv", "id,note\n1,\"a\nb\"\n")], false).unwrap();
        assert_eq!(s.rows, 1);
        assert_eq!(text, "id,note\n1,\"a\nb\"\n");
    }

    #[test]
    fn empty_file_is_skipped() {
        let (s, _text) = run(&[("a.csv", "id\n1\n"), ("empty.csv", "")], false).unwrap();
        assert_eq!(s.files, 1);
    }
}
