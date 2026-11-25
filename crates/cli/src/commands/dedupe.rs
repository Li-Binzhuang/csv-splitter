use crate::{
    check_output, human_bytes, parse_delimiter, parse_quotes, print_command_help, report_failure,
    self_check, spawn_reporter, Parser,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

pub(crate) fn cmd_dedupe(args: &[String]) -> i32 {
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
pub(crate) fn files_differ(
    first: &Path,
    second: &Path,
) -> std::io::Result<Option<(u64, u64, u64)>> {
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
