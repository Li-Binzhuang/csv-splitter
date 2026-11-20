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

/// Inputs whose headers were reordered to match the first file.
///
/// `align_by_name` writes a rewritten copy only for files whose header order differs; the temp
/// files are removed when the guard drops, so a caller can pass `.paths()` straight to
/// `concat` and forget about them.
#[derive(Debug)]
pub struct Aligned {
    paths: Vec<PathBuf>,
    temps: Vec<PathBuf>,
}

impl Aligned {
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
}

impl Drop for Aligned {
    fn drop(&mut self) {
        for path in &self.temps {
            std::fs::remove_file(path).ok();
        }
    }
}

/// Reorder each input's columns by name so every file matches the first file's header.
///
/// Only the *order* may differ: a file missing a column or carrying an extra one is still an
/// error, because filling it with empties invents data nobody asked for.
pub fn align_by_name(inputs: &[PathBuf], delimiter: Delimiter) -> io::Result<Aligned> {
    if inputs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no input files",
        ));
    }
    let header_of = |path: &Path| -> io::Result<Vec<String>> {
        let file = File::open(path)?;
        let mut reader = RecordReader::new(BufReader::with_capacity(256 * 1024, file), 256 * 1024);
        let mut record = Vec::new();
        if !reader.next_record(&mut record)? {
            return Ok(Vec::new());
        }
        Ok(crate::csv_reader::parse_csv_record(
            String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
            delimiter.byte(),
        ))
    };

    let expected = header_of(&inputs[0])?;
    if expected.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} has no header", inputs[0].display()),
        ));
    }
    let mut paths = vec![inputs[0].clone()];
    let mut temps = Vec::new();
    for path in inputs.iter().skip(1) {
        let header = header_of(path)?;
        if header == expected {
            paths.push(path.clone());
            continue;
        }
        let mut want: Vec<&str> = expected.iter().map(String::as_str).collect();
        let mut have: Vec<&str> = header.iter().map(String::as_str).collect();
        want.sort_unstable();
        have.sort_unstable();
        if want != have {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{}: columns differ from {} (only the order may differ for by-name)",
                    path.display(),
                    inputs[0].display()
                ),
            ));
        }
        let seek: Vec<usize> = expected
            .iter()
            .map(|name| header.iter().position(|h| h == name).unwrap())
            .collect();
        let temp = std::env::temp_dir().join(format!(
            "csvs_align_{}_{}.csv",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let file = File::open(path)?;
        let mut reader = RecordReader::new(BufReader::with_capacity(256 * 1024, file), 256 * 1024);
        let mut out = BufWriter::with_capacity(1024 * 1024, File::create(&temp)?);
        // The rewritten file needs its own header: `concat` compares the first record of every
        // input against file one's header.
        let header_refs: Vec<&str> = expected.iter().map(String::as_str).collect();
        out.write_all(crate::csv_reader::join_csv_line(&header_refs, delimiter.byte()).as_bytes())?;
        let mut record = Vec::new();
        let mut first = true;
        while reader.next_record(&mut record)? {
            let text = String::from_utf8_lossy(&record)
                .trim_end_matches(['\n', '\r'])
                .to_string();
            let fields = crate::csv_reader::parse_csv_record_with(
                &text,
                delimiter.byte(),
                crate::csv_reader::QuoteStyle::Rfc4180,
            );
            let reordered: Vec<String> = seek
                .iter()
                .map(|i| fields.get(*i).cloned().unwrap_or_default())
                .collect();
            if first {
                // The reordered header is the first file's header, written once.
                first = false;
                continue;
            }
            let refs: Vec<&str> = reordered.iter().map(String::as_str).collect();
            out.write_all(crate::csv_reader::join_csv_line(&refs, delimiter.byte()).as_bytes())?;
        }
        out.flush()?;
        temps.push(temp.clone());
        paths.push(temp);
    }
    Ok(Aligned { paths, temps })
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
                    // InvalidInput, matching the SQL engine: a mismatched header is a bad
                    // request, not corrupt data, and the same condition must not be 422 on one
                    // engine and 400 on the other.
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
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
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("b.csv"), "{err}");
    }

    #[test]
    fn align_by_name_reorders_columns_and_keeps_values() {
        let dir = std::env::temp_dir().join(format!("csvs_align_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.csv");
        let b = dir.join("b.csv");
        std::fs::write(&a, "a,b,c\n1,2,3\n").unwrap();
        std::fs::write(&b, "c,a,b\n30,10,20\n").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel::<ProgressEvent>();

        let aligned = align_by_name(&[a.clone(), b.clone()], Delimiter::Comma).unwrap();
        let out = dir.join("out.csv");
        concat(aligned.paths(), &out, Delimiter::Comma, false, &tx).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(text, "a,b,c\n1,2,3\n10,20,30\n");
    }

    #[test]
    fn align_by_name_still_refuses_a_different_column_set() {
        let dir = std::env::temp_dir().join(format!("csvs_align2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.csv");
        let b = dir.join("b.csv");
        std::fs::write(&a, "a,b,c\n1,2,3\n").unwrap();
        std::fs::write(&b, "c,a\n30,10\n").unwrap();
        let err = align_by_name(&[a, b], Delimiter::Comma).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("columns differ"), "{err}");
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
