//! Cross-table lookup: the `VLOOKUP` a spreadsheet user reaches for first.
//!
//! The asymmetry is the whole design: the **reference table is loaded**, the
//! **main table is streamed**. References are small by nature (customers, products,
//! codes); the table being enriched is the one that can be a hundred million rows.
//!
//! Consequences that follow:
//!
//! * the reference is bounded (`max_keys`), and exceeding it fails with the
//!   parameter to raise — a lookup table with ten million keys is a data-model
//!   problem, not a memory-tuning problem;
//! * the main file is read once and written once: two sequential passes, nothing else;
//! * a missing key becomes `#N/A`, exactly like a spreadsheet, because silently
//!   writing an empty cell turns "no customer" into "empty customer" and nobody notices.
//!
//! Duplicate reference keys keep the **first** occurrence, which is what VLOOKUP does
//! and what people expect after sorting a reference sheet.

use crate::csv_reader::{
    join_csv_line, parse_csv_record_with, resolve_column, QuoteStyle, RecordReader,
};
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;

/// Ceiling on reference keys held in memory.
pub const DEFAULT_MAX_KEYS: usize = 5_000_000;

/// How a missing key is written.
pub const MISSING: &str = "#N/A";

#[derive(Clone, Debug)]
pub struct LookupOptions {
    /// Column in the main file to match on.
    pub key: String,
    /// Reference file.
    pub reference: std::path::PathBuf,
    /// Column in the reference file to match on.
    pub reference_key: String,
    /// Columns to copy from the reference into the result.
    pub take: Vec<String>,
    pub max_keys: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LookupSummary {
    pub main_rows: u64,
    pub matched: u64,
    pub missing: u64,
    pub reference_keys: u64,
    pub bytes: u64,
    pub columns: Vec<String>,
}

/// Join the reference onto the main file, writing the result.
pub fn lookup(
    main: &Path,
    delimiter: Delimiter,
    output: &Path,
    options: &LookupOptions,
    tx: &ProgressSender,
) -> io::Result<LookupSummary> {
    lookup_quotes(main, delimiter, output, options, QuoteStyle::Rfc4180, tx)
}

/// Same as [`lookup`], with an explicit quote style for all three files.
pub fn lookup_quotes(
    main: &Path,
    delimiter: Delimiter,
    output: &Path,
    options: &LookupOptions,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<LookupSummary> {
    if options.key.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a lookup needs a --key column",
        ));
    }
    if options.take.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a lookup needs at least one --get column",
        ));
    }
    let delim = delimiter.byte();

    // References first: once they are in memory the main file is a single pass.
    let reference = Reference::load(
        &options.reference,
        &options.reference_key,
        &options.take,
        options.max_keys,
        delimiter,
        quotes,
    )?;

    let main_header = crate::workbook::base_header(main, delimiter)?;
    if main_header.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} has no header", main.display()),
        ));
    }
    let key_idx = resolve_column(&options.key, &main_header)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    // Output header: the main file's, plus the reference columns. A clash is
    // suffixed rather than allowed to produce two identical headers.
    let mut columns = main_header.clone();
    let mut output_names: Vec<String> = Vec::with_capacity(options.take.len());
    for wanted in &options.take {
        let mut name = wanted.clone();
        while columns.iter().any(|c| c == &name) {
            name = format!("{name}_ref");
        }
        columns.push(name.clone());
        output_names.push(name);
    }

    let mut out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(output)?);
    out.write_all(
        join_csv_line(
            &columns.iter().map(String::as_str).collect::<Vec<_>>(),
            delim,
        )
        .as_bytes(),
    )?;

    let mut records = RecordReader::new_with(
        BufReader::with_capacity(4 * 1024 * 1024, encoding::open_decoded(main)?),
        4 * 1024 * 1024,
        quotes,
    );
    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Ok(LookupSummary {
            columns,
            ..Default::default()
        });
    }

    tx.send(ProgressEvent::PhaseStart {
        phase: "Looking up".into(),
        total_bytes: std::fs::metadata(main).map(|m| m.len()).unwrap_or(0),
    })
    .ok();

    let mut summary = LookupSummary {
        reference_keys: reference.rows,
        columns: columns.clone(),
        ..Default::default()
    };
    let mut scanned = 0u64;
    while records.next_record(&mut record)? {
        let fields = parse_csv_record_with(
            String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
            delim,
            quotes,
        );
        summary.main_rows += 1;
        scanned += record.len() as u64;
        if summary.main_rows.is_multiple_of(500_000) {
            tx.send(ProgressEvent::Progress { bytes: scanned }).ok();
        }

        let key = fields.get(key_idx).cloned().unwrap_or_default();
        let mut line = fields;
        match reference.get(&key) {
            Some(values) => {
                summary.matched += 1;
                line.extend(values.iter().cloned());
            }
            None => {
                summary.missing += 1;
                for _ in &output_names {
                    line.push(MISSING.to_string());
                }
            }
        }
        out.write_all(
            join_csv_line(&line.iter().map(String::as_str).collect::<Vec<_>>(), delim).as_bytes(),
        )?;
    }
    out.flush()?;
    summary.bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseDone {
        phase: "Looking up".into(),
    })
    .ok();
    Ok(summary)
}

struct Reference {
    values: HashMap<String, Vec<String>>,
    rows: u64,
}

impl Reference {
    fn load(
        path: &Path,
        key: &str,
        take: &[String],
        max_keys: usize,
        delimiter: Delimiter,
        quotes: QuoteStyle,
    ) -> io::Result<Self> {
        let delim = delimiter.byte();
        let mut records = RecordReader::new_with(
            BufReader::with_capacity(2 * 1024 * 1024, encoding::open_decoded(path)?),
            2 * 1024 * 1024,
            quotes,
        );
        let mut record = Vec::with_capacity(16 * 1024);
        if !records.next_record(&mut record)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("reference {} has no header", path.display()),
            ));
        }
        let header = parse_csv_record_with(
            String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
            delim,
            quotes,
        );
        let key_idx = resolve_column(key, &header)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("reference: {e}")))?;
        let take_idx: Vec<usize> = take
            .iter()
            .map(|name| {
                resolve_column(name, &header).map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("reference: {e}"))
                })
            })
            .collect::<io::Result<_>>()?;

        let mut values = HashMap::with_capacity(1024);
        let mut rows = 0u64;
        while records.next_record(&mut record)? {
            let fields = parse_csv_record_with(
                String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
                delim,
                quotes,
            );
            rows += 1;
            let key_value = fields.get(key_idx).cloned().unwrap_or_default();
            if key_value.is_empty() {
                continue;
            }
            // First occurrence wins, like VLOOKUP.
            if values.contains_key(&key_value) {
                continue;
            }
            if values.len() >= max_keys {
                return Err(io::Error::other(format!(
                    "the reference has more than {max_keys} keys; raise --max-keys, or look up against a smaller table",
                )));
            }
            let taken: Vec<String> = take_idx
                .iter()
                .map(|i| fields.get(*i).cloned().unwrap_or_default())
                .collect();
            values.insert(key_value, taken);
        }
        Ok(Self { values, rows })
    }

    fn get(&self, key: &str) -> Option<&Vec<String>> {
        self.values.get(key.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn unique_seq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed)
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_lookup_{tag}_{}_{}",
                std::process::id(),
                unique_seq()
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

    const MAIN: &str = "order,cust_no,amount\nA,1,100\nB,2,50\nC,9,10\n";
    const REF: &str = "cust_no,name,city\n1,Ana,CDMX\n2,Beto,Puebla\n1,Ana Duplicate,CDMX\n";

    fn run(main: &str, reference: &str, take: &[&str], key: &str) -> (Dir, String, LookupSummary) {
        let dir = Dir::new("run");
        let main_path = dir.join("main.csv");
        let ref_path = dir.join("ref.csv");
        std::fs::write(&main_path, main).unwrap();
        std::fs::write(&ref_path, reference).unwrap();
        let out = dir.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let summary = lookup(
            &main_path,
            Delimiter::Comma,
            &out,
            &LookupOptions {
                key: key.to_string(),
                reference: ref_path,
                reference_key: "cust_no".to_string(),
                take: take.iter().map(|s| s.to_string()).collect(),
                max_keys: DEFAULT_MAX_KEYS,
            },
            &tx,
        )
        .unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        (dir, text, summary)
    }

    #[test]
    fn reference_columns_are_appended_in_order() {
        let (_dir, text, summary) = run(MAIN, REF, &["name"], "cust_no");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "order,cust_no,amount,name");
        assert_eq!(lines[1], "A,1,100,Ana");
        assert_eq!(lines[2], "B,2,50,Beto");
        // A key that is not in the reference is visibly missing, not blank.
        assert_eq!(lines[3], "C,9,10,#N/A");
        assert_eq!(summary.matched, 2);
        assert_eq!(summary.missing, 1);
        assert_eq!(summary.main_rows, 3);
    }

    #[test]
    fn several_columns_can_be_taken_at_once() {
        let (_dir, text, _) = run(MAIN, REF, &["name", "city"], "cust_no");
        let header = text.lines().next().unwrap();
        assert_eq!(header, "order,cust_no,amount,name,city");
        assert_eq!(text.lines().nth(1).unwrap(), "A,1,100,Ana,CDMX");
    }

    #[test]
    fn duplicate_reference_keys_keep_the_first_row() {
        let (_dir, text, _) = run(MAIN, REF, &["name"], "cust_no");
        assert_eq!(text.lines().nth(1).unwrap(), "A,1,100,Ana", "{text}");
        assert!(!text.contains("Ana Duplicate"), "{text}");
    }

    #[test]
    fn a_name_clash_is_suffixed_not_duplicated() {
        let reference = "cust_no,amount\n1,999\n";
        let (_dir, text, _) = run(MAIN, reference, &["amount"], "cust_no");
        assert_eq!(
            text.lines().next().unwrap(),
            "order,cust_no,amount,amount_ref",
            "two columns may not share a name"
        );
        assert_eq!(text.lines().nth(1).unwrap(), "A,1,100,999");
    }

    #[test]
    fn empty_keys_in_the_reference_are_ignored() {
        let reference = "cust_no,name\n,Empty\n1,Ana\n";
        let (_dir, text, _) = run(MAIN, reference, &["name"], "cust_no");
        // An empty key must not become a lookup that matches every empty cell.
        assert!(text.contains("A,1,100,Ana"), "{text}");
        assert!(text.contains("C,9,10,#N/A"), "{text}");
    }

    #[test]
    fn unknown_columns_name_the_file_they_are_missing_from() {
        let dir = Dir::new("bad");
        let main_path = dir.join("main.csv");
        let ref_path = dir.join("ref.csv");
        std::fs::write(&main_path, MAIN).unwrap();
        std::fs::write(&ref_path, REF).unwrap();
        let out = dir.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = lookup(
            &main_path,
            Delimiter::Comma,
            &out,
            &LookupOptions {
                key: "cust_no".to_string(),
                reference: ref_path,
                reference_key: "cust_no".to_string(),
                take: vec!["nope".to_string()],
                max_keys: DEFAULT_MAX_KEYS,
            },
            &tx,
        )
        .unwrap_err();
        assert!(err.to_string().contains("reference"), "{err}");
        assert!(err.to_string().contains("nope"), "{err}");
    }

    #[test]
    fn an_oversized_reference_is_refused_with_the_parameter_to_raise() {
        let mut reference = String::from("cust_no,name\n");
        for i in 0..20 {
            reference.push_str(&format!("{i},n{i}\n"));
        }
        let dir = Dir::new("big");
        let main_path = dir.join("main.csv");
        let ref_path = dir.join("ref.csv");
        std::fs::write(&main_path, "order,cust_no\nA,0\n").unwrap();
        std::fs::write(&ref_path, reference).unwrap();
        let out = dir.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = lookup(
            &main_path,
            Delimiter::Comma,
            &out,
            &LookupOptions {
                key: "cust_no".to_string(),
                reference: ref_path,
                reference_key: "cust_no".to_string(),
                take: vec!["name".to_string()],
                max_keys: 10,
            },
            &tx,
        )
        .unwrap_err();
        assert!(err.to_string().contains("max-keys"), "{err}");
    }
}
