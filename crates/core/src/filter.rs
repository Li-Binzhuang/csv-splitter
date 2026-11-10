//! Column filters over a whole file.
//!
//! The viewer needs "which rows match `city = CDMX`" without loading the file:
//! this scans sequentially (decoding handles GBK/UTF-16), returns **data-row
//! numbers**, and the caller pages through the hits with the sparse index.
//!
//! Matching is deliberately simple — equality, contains, numeric comparison and
//! empty checks. Anything more elaborate belongs in SQL, not in a file tool.

use crate::csv_reader::{parse_csv_record_with, QuoteStyle, RecordReader};
use crate::encoding;
use crate::index::SparseIndex;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::cmp::Ordering;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Contains,
    Gt,
    Gte,
    Lt,
    Lte,
    Empty,
    NotEmpty,
    /// Regular expression match (`col~re:PATTERN`).
    Regex,
    /// Membership: `col in a,b,c` (values are trimmed; no escaping).
    In,
}

impl Op {
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "!=",
            Self::Contains => "~",
            Self::Gt => ">",
            Self::Gte => ">=",
            Self::Lt => "<",
            Self::Lte => "<=",
            Self::Empty => "empty",
            Self::NotEmpty => "notempty",
            Self::Regex => "~re:",
            Self::In => "in",
        }
    }
}

/// `city=CDMX`, `amount>1000`, `note~refund`, `email empty`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterSpec {
    pub column: String,
    pub op: Op,
    pub value: String,
}

impl FilterSpec {
    pub fn parse(expr: &str) -> Result<Self, String> {
        let expr = expr.trim();
        if expr.is_empty() {
            return Err("empty filter expression".to_string());
        }
        // `col in a,b,c` has no operator symbol, so match it first.
        if let Some(pos) = expr.to_ascii_lowercase().find(" in ") {
            let column = expr[..pos].trim().to_string();
            if column.is_empty() {
                return Err(format!("missing column name in '{expr}'"));
            }
            let value = expr[pos + 4..].trim().to_string();
            if value.is_empty() {
                return Err(format!("'in' needs at least one value: {expr}"));
            }
            return Ok(Self {
                column,
                op: Op::In,
                value,
            });
        }

        // Longest operators first so `>=` beats `>`.
        const OPS: &[(&str, Op)] = &[
            ("!=", Op::Ne),
            (">=", Op::Gte),
            ("<=", Op::Lte),
            ("=", Op::Eq),
            ("~", Op::Contains),
            (">", Op::Gt),
            ("<", Op::Lt),
        ];
        for (token, op) in OPS {
            if let Some((col, val)) = expr.split_once(token) {
                let column = col.trim().to_string();
                if column.is_empty() {
                    return Err(format!("missing column name in '{expr}'"));
                }
                let value = val.trim().to_string();
                // `col~re:PATTERN` is a regex match; validated here so a typo
                // fails immediately instead of on the first row.
                let op = if *op == Op::Contains {
                    match value.strip_prefix("re:") {
                        Some(pattern) => {
                            regex::Regex::new(pattern)
                                .map_err(|e| format!("bad regular expression: {e}"))?;
                            return Ok(Self {
                                column,
                                op: Op::Regex,
                                value: pattern.to_string(),
                            });
                        }
                        None => Op::Contains,
                    }
                } else {
                    *op
                };
                return Ok(Self { column, op, value });
            }
        }
        // `col empty` / `col notempty`
        let mut parts = expr.splitn(2, char::is_whitespace);
        let column = parts.next().unwrap_or("").trim().to_string();
        let word = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        match word.as_str() {
            "empty" => Ok(Self {
                column,
                op: Op::Empty,
                value: String::new(),
            }),
            "notempty" | "not-empty" => Ok(Self {
                column,
                op: Op::NotEmpty,
                value: String::new(),
            }),
            _ => Err(format!(
                "cannot parse '{expr}'; use col=value, col~text, col>number, col empty"
            )),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    /// 0-based data rows (header excluded) that matched, in file order.
    pub rows: Vec<u64>,
    pub scanned: u64,
    pub matched: u64,
    /// True when `limit` cut the result short.
    pub truncated: bool,
}

impl ScanResult {
    /// Row numbers as a user would count them (1-based, header excluded).
    pub fn row_numbers(&self) -> Vec<u64> {
        self.rows.iter().map(|r| r + 1).collect()
    }
}

/// Scan `path` and collect up to `limit` matching data rows.
pub fn scan(
    path: &Path,
    delimiter: Delimiter,
    spec: &FilterSpec,
    limit: usize,
    tx: &ProgressSender,
) -> io::Result<ScanResult> {
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
    let headers = parse_csv_record_with(
        &String::from_utf8_lossy(&record),
        delim,
        QuoteStyle::Rfc4180,
    );
    let col_idx = crate::csv_reader::resolve_column(&spec.column, &headers)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    tx.send(ProgressEvent::PhaseStart {
        phase: format!("Filtering {}", spec.column),
        total_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    })
    .ok();

    let mut result = ScanResult::default();
    let limit = limit.max(1);
    while records.next_record(&mut record)? {
        result.scanned += 1;
        let text = String::from_utf8_lossy(&record);
        let fields = parse_csv_record_with(
            text.trim_end_matches(['\n', '\r']),
            delim,
            QuoteStyle::Rfc4180,
        );
        let cell = fields.get(col_idx).map(String::as_str).unwrap_or("");
        if matches(cell, spec) {
            result.matched += 1;
            if result.rows.len() < limit {
                result.rows.push(result.scanned - 1);
            } else {
                result.truncated = true;
            }
        }
        if result.scanned % 200_000 == 0 {
            tx.send(ProgressEvent::Progress {
                bytes: result.scanned,
            })
            .ok();
        }
    }

    tx.send(ProgressEvent::PhaseDone {
        phase: format!("Filtering {}", spec.column),
    })
    .ok();
    Ok(result)
}

/// How several predicates combine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    /// Every predicate must hold.
    All,
    /// Any one predicate is enough.
    Any,
}

/// A set of predicates over one or more columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterSet {
    pub specs: Vec<FilterSpec>,
    pub combine: Combine,
}

impl FilterSet {
    pub fn single(spec: FilterSpec) -> Self {
        Self {
            specs: vec![spec],
            combine: Combine::All,
        }
    }

    /// Column indices for every distinct column the set mentions.
    fn resolve(&self, headers: &[String]) -> io::Result<Vec<(String, usize)>> {
        let mut out = Vec::with_capacity(self.specs.len());
        for spec in &self.specs {
            let idx = crate::csv_reader::resolve_column(&spec.column, headers)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            out.push((spec.column.clone(), idx));
        }
        Ok(out)
    }

    /// Compile every regex once, so a scan does no regex parsing per row.
    pub fn compile(&self, headers: &[String]) -> io::Result<CompiledSet> {
        let resolved = self.resolve(headers)?;
        let mut regexes = Vec::with_capacity(self.specs.len());
        for spec in &self.specs {
            if spec.op == Op::Regex {
                let re = regex::Regex::new(&spec.value).map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("bad regex: {e}"))
                })?;
                regexes.push(Some(re));
            } else {
                regexes.push(None);
            }
        }
        Ok(CompiledSet {
            specs: self.specs.clone(),
            regexes,
            columns: resolved,
            combine: self.combine,
        })
    }
}

/// A [`FilterSet`] with its regexes compiled once.
pub struct CompiledSet {
    specs: Vec<FilterSpec>,
    regexes: Vec<Option<regex::Regex>>,
    columns: Vec<(String, usize)>,
    combine: Combine,
}

impl CompiledSet {
    /// Does this row pass? Public so a viewer can reuse the same semantics the
    /// exporter uses — two implementations would eventually disagree.
    pub fn holds(&self, fields: &[String]) -> bool {
        let mut any = false;
        for (i, spec) in self.specs.iter().enumerate() {
            let idx = self.columns[i].1;
            let cell = fields.get(idx).map(String::as_str).unwrap_or("");
            let hit = match &self.regexes[i] {
                Some(re) => re.is_match(cell),
                None => matches(cell, spec),
            };
            match self.combine {
                Combine::All if !hit => return false,
                Combine::All => {}
                Combine::Any if hit => return true,
                Combine::Any => any |= hit,
            }
        }
        match self.combine {
            Combine::All => true,
            Combine::Any => any,
        }
    }
}

/// Write the rows matching `set` to `output` as CSV, streaming.
///
/// This is the "filter, then keep the result" workflow: memory stays flat
/// because records are copied one at a time, and the header is written once.
pub fn extract(
    path: &Path,
    delimiter: Delimiter,
    set: &FilterSet,
    output: &Path,
    tx: &ProgressSender,
) -> io::Result<ExtractSummary> {
    extract_style(path, delimiter, set, output, QuoteStyle::Rfc4180, tx)
}

/// Same as [`extract`], with an explicit quote style.
pub fn extract_style(
    path: &Path,
    delimiter: Delimiter,
    set: &FilterSet,
    output: &Path,
    quotes: QuoteStyle,
    tx: &ProgressSender,
) -> io::Result<ExtractSummary> {
    if set.specs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no filter expressions",
        ));
    }
    let total_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseStart {
        phase: "Filtering".into(),
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
    let headers = parse_csv_record_with(&String::from_utf8_lossy(&record), delim, quotes);
    let compiled = set.compile(&headers)?;

    let mut out = BufWriter::with_capacity(4 * 1024 * 1024, File::create(output)?);
    let mut header_line = record.clone();
    if !header_line.ends_with(b"\n") {
        header_line.push(b'\n');
    }
    out.write_all(&header_line)?;

    let mut summary = ExtractSummary::default();
    let mut written: u64 = 0;
    let width = headers.len();
    while records.next_record(&mut record)? {
        summary.scanned += 1;
        written += record.len() as u64;
        let text = String::from_utf8_lossy(&record);
        let fields = parse_csv_record_with(text.trim_end_matches(['\n', '\r']), delim, quotes);
        if compiled.holds(&fields) {
            if fields.len() != width {
                summary.matched_ragged += 1;
            }
            let mut line = record.clone();
            if !line.ends_with(b"\n") {
                line.push(b'\n');
            }
            // Keep the source bytes verbatim: re-serialising would normalise
            // quoting and line endings for no benefit.
            out.write_all(&line)?;
            summary.matched += 1;
        }
        if summary.scanned.is_multiple_of(200_000) {
            tx.send(ProgressEvent::Progress { bytes: written }).ok();
        }
    }
    out.flush()?;
    let bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseDone {
        phase: "Filtering".into(),
    })
    .ok();
    Ok(ExtractSummary { bytes, ..summary })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractSummary {
    pub scanned: u64,
    pub matched: u64,
    pub bytes: u64,
    /// Matched rows whose field count differed from the header (inherited from
    /// the source, and passed through verbatim).
    pub matched_ragged: u64,
}

/// Single-cell predicate.
pub fn matches(cell: &str, spec: &FilterSpec) -> bool {
    match spec.op {
        Op::Empty => cell.trim().is_empty(),
        Op::NotEmpty => !cell.trim().is_empty(),
        Op::Eq => cell == spec.value,
        Op::Ne => cell != spec.value,
        Op::Contains => cell.contains(&spec.value),
        Op::In => {
            let cell_trimmed = cell.trim();
            spec.value
                .split(',')
                .map(|v| v.trim())
                // Matching is case-insensitive, like `=value` is exact and `~`
                // is literal; the list form is the friendly one.
                .any(|v| v.eq_ignore_ascii_case(cell_trimmed))
        }
        Op::Regex => regex::Regex::new(&spec.value)
            .map(|re| re.is_match(cell))
            .unwrap_or(false),
        Op::Gt | Op::Gte | Op::Lt | Op::Lte => {
            // Numeric when both sides parse, string order otherwise (so ISO dates
            // and zero-padded ids behave sensibly).
            let ord = match (cell.trim().parse::<f64>(), spec.value.parse::<f64>()) {
                (Ok(a), Ok(b)) => a.partial_cmp(&b),
                _ => Some(cell.trim().cmp(spec.value.as_str())),
            };
            match ord {
                Some(Ordering::Greater) => matches!(spec.op, Op::Gt | Op::Gte),
                Some(Ordering::Equal) => matches!(spec.op, Op::Gte | Op::Lte),
                Some(Ordering::Less) => matches!(spec.op, Op::Lt | Op::Lte),
                None => false,
            }
        }
    }
}

/// One matched row, decoded and split into cells.
#[derive(Clone, Debug)]
pub struct MatchedRow {
    /// 0-based data row.
    pub row: u64,
    pub cells: Vec<String>,
}

/// Load specific rows (usually filter hits) through the sparse index.
///
/// The index is built on first use and cached in the sidecar file, so repeated
/// filtered views only pay for the rows they display.
pub fn read_rows(path: &Path, delimiter: Delimiter, rows: &[u64]) -> io::Result<Vec<MatchedRow>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let encoding = encoding::detect(path)?;
    let sidecar = SparseIndex::sidecar_path(path);
    let index = match SparseIndex::load(&sidecar) {
        Ok(idx) if idx.matches(path) => idx,
        _ => {
            let idx = SparseIndex::build(path, crate::index::DEFAULT_INTERVAL)?;
            idx.save(&sidecar).ok();
            idx
        }
    };
    let raw = index.read_rows_at(path, rows)?;
    let delim = delimiter.byte();
    Ok(raw
        .into_iter()
        .map(|(row, bytes)| {
            let text = encoding::decode_cell(&bytes, encoding);
            let cells = parse_csv_record_with(
                text.trim_end_matches(['\n', '\r']),
                delim,
                QuoteStyle::Rfc4180,
            );
            MatchedRow { row, cells }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(expr: &str) -> FilterSpec {
        FilterSpec::parse(expr).unwrap()
    }

    #[test]
    fn parses_operators() {
        let s = spec("city=CDMX");
        assert_eq!((s.column.as_str(), s.op), ("city", Op::Eq));
        assert_eq!(s.value, "CDMX");

        assert_eq!(spec("amount>=100").op, Op::Gte);
        assert_eq!(spec("amount>100").value, "100");
        assert_eq!(spec("note~refund").op, Op::Contains);
        assert_eq!(spec("email empty").op, Op::Empty);
        assert_eq!(spec("email notempty").op, Op::NotEmpty);
        assert!(FilterSpec::parse("nonsense").is_err());
        assert!(FilterSpec::parse("=x").is_err());
    }

    #[test]
    fn matches_semantics() {
        assert!(matches("CDMX", &spec("city=CDMX")));
        assert!(!matches("Puebla", &spec("city=CDMX")));
        assert!(matches("refund-2026", &spec("note~refund")));
        assert!(matches("1200", &spec("amt>1000")));
        assert!(matches("1000", &spec("amt>=1000")));
        assert!(matches("1000", &spec("amt<=1000")));
        assert!(!matches("1000", &spec("amt>1000")));
        assert!(matches("  ", &spec("x empty")));
        assert!(!matches("a", &spec("x empty")));
        // Non-numeric falls back to string ordering.
        assert!(matches("2026-02-01", &spec("d>2026-01-01")));
    }
}

#[cfg(test)]
mod set_tests {
    use super::*;
    use std::path::PathBuf;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_fset_{tag}_{}_{}",
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

    fn fixture() -> (Dir, PathBuf) {
        let d = Dir::new("set");
        let p = d.join("a.csv");
        let mut csv = String::from("id,city,amount\n");
        csv.push_str("1,CDMX,10\n");
        csv.push_str("2,Puebla,500\n");
        csv.push_str("3,CDMX,500\n");
        csv.push_str("4,Tijuana,20\n");
        std::fs::write(&p, csv).unwrap();
        (d, p)
    }

    #[test]
    fn and_combines_all_predicates() {
        let (_d, p) = fixture();
        let set = FilterSet {
            specs: vec![
                FilterSpec::parse("city=CDMX").unwrap(),
                FilterSpec::parse("amount>=500").unwrap(),
            ],
            combine: Combine::All,
        };
        let out_dir = Dir::new("and_out");
        let out = out_dir.join("r.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let s = extract(&p, Delimiter::Comma, &set, &out, &tx).unwrap();
        assert_eq!(s.scanned, 4);
        assert_eq!(s.matched, 1);
        let text = std::fs::read_to_string(&out).unwrap();
        assert_eq!(text, "id,city,amount\n3,CDMX,500\n");
    }

    #[test]
    fn or_keeps_either() {
        let (_d, p) = fixture();
        let set = FilterSet {
            specs: vec![
                FilterSpec::parse("city=Tijuana").unwrap(),
                FilterSpec::parse("amount>=500").unwrap(),
            ],
            combine: Combine::Any,
        };
        let out_dir = Dir::new("or_out");
        let out = out_dir.join("r.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let s = extract(&p, Delimiter::Comma, &set, &out, &tx).unwrap();
        assert_eq!(s.matched, 3);
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("2,Puebla,500"), "{text}");
        assert!(text.contains("3,CDMX,500"), "{text}");
        assert!(text.contains("4,Tijuana,20"), "{text}");
        assert!(!text.contains("1,CDMX,10"), "{text}");
    }

    #[test]
    fn unknown_column_is_a_client_error() {
        let (_d, p) = fixture();
        let set = FilterSet::single(FilterSpec::parse("nope=1").unwrap());
        let out_dir = Dir::new("bad_col");
        let out = out_dir.join("r.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = extract(&p, Delimiter::Comma, &set, &out, &tx).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

#[cfg(test)]
mod regex_tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_re_{tag}_{}_{}",
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

    #[test]
    fn regex_operator_is_parsed_and_validated() {
        let spec = FilterSpec::parse("note~re:^REF").unwrap();
        assert_eq!(spec.op, Op::Regex);
        assert_eq!(spec.value, "^REF");
        // A plain `~` is still a substring search.
        assert_eq!(FilterSpec::parse("note~refund").unwrap().op, Op::Contains);
        // A broken pattern fails at parse time, not on the first row.
        assert!(FilterSpec::parse("note~re:[unclosed").is_err());
    }

    #[test]
    fn regex_filters_and_combines_with_other_predicates() {
        let d = Dir::new("run");
        let input = d.join("in.csv");
        let output = d.join("out.csv");
        let mut csv = String::from("id,city,note\n");
        csv.push_str("1,CDMX,REFUND issued\n");
        csv.push_str("2,CDMX,paid\n");
        csv.push_str("3,Puebla,REFUND-2\n");
        csv.push_str("4,Puebla,ok\n");
        std::fs::write(&input, csv).unwrap();

        let set = FilterSet {
            specs: vec![
                FilterSpec::parse("note~re:^REF").unwrap(),
                FilterSpec::parse("city=Puebla").unwrap(),
            ],
            combine: Combine::All,
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let summary = extract(&input, Delimiter::Comma, &set, &output, &tx).unwrap();
        assert_eq!(summary.matched, 1);
        assert_eq!(
            std::fs::read_to_string(&output).unwrap(),
            "id,city,note\n3,Puebla,REFUND-2\n"
        );
    }

    #[test]
    fn regex_matches_an_empty_cell_only_if_the_pattern_allows_it() {
        assert!(matches("", &FilterSpec::parse("x~re:^$").unwrap()));
        assert!(!matches("", &FilterSpec::parse("x~re:^a").unwrap()));
    }
}

#[cfg(test)]
mod in_tests {
    use super::*;

    #[test]
    fn in_lists_are_trimmed_and_case_insensitive() {
        let spec = FilterSpec::parse("city in CDMX, Puebla , Tijuana").unwrap();
        assert_eq!(spec.op, Op::In);
        assert!(matches("CDMX", &spec));
        assert!(matches(" puebla ", &spec));
        assert!(!matches("Monterrey", &spec));
    }

    #[test]
    fn in_needs_values_and_a_column() {
        assert!(FilterSpec::parse(" in a,b").is_err());
        assert!(FilterSpec::parse("city in ").is_err());
    }

    #[test]
    fn in_combines_with_other_predicates() {
        let set = FilterSet {
            specs: vec![
                FilterSpec::parse("city in CDMX,Puebla").unwrap(),
                FilterSpec::parse("amount>100").unwrap(),
            ],
            combine: Combine::All,
        };
        let headers = vec!["city".to_string(), "amount".to_string()];
        let compiled = set.compile(&headers).unwrap();
        let yes = vec!["CDMX".to_string(), "500".to_string()];
        let too_small = vec!["CDMX".to_string(), "50".to_string()];
        let wrong_city = vec!["Tijuana".to_string(), "500".to_string()];
        assert!(compiled.holds(&yes));
        assert!(!compiled.holds(&too_small));
        assert!(!compiled.holds(&wrong_city));
    }
}
