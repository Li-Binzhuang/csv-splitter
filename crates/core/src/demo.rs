//! A realistic sample file, so the first thing a new user does is not "find a
//! CSV".
//!
//! The sample is deliberately *not* pristine: it has duplicate rows, empty cells,
//! a quoted field containing a newline and one row with an extra unquoted comma.
//! A tour that only works on clean data teaches the wrong expectation — you find
//! out how the tool behaves on real files when it matters least.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Rows written (excluding the header).
pub const SAMPLE_ROWS: u64 = 20_000;

const CITIES: [&str; 5] = ["CDMX", "Puebla", "Tijuana", "Monterrey", "Guadalajara"];

/// Write the sample file and return `(rows, bytes)`.
pub fn write_sample(path: &Path) -> io::Result<(u64, u64)> {
    let mut out = BufWriter::with_capacity(1024 * 1024, File::create(path)?);
    out.write_all("id,cust_no,city,amount,note,ts\n".as_bytes())?;

    // Deterministic pseudo-randomness: a xorshift keeps the file stable across
    // runs, which matters because the documentation quotes its output.
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let mut rows = 0u64;
    for i in 0..SAMPLE_ROWS {
        let r = next();
        let cust = format!("C{:06}", r % 50_000);
        let city = CITIES[(r >> 8) as usize % CITIES.len()];
        let amount = 100.0 + (r >> 16) as f64 % 89_900.0;
        let month = 1 + (r >> 24) as u32 % 12;
        let day = 1 + (r >> 32) as u32 % 28;

        // Every 20th row carries a multi-line note; every 25th is empty.
        let note = match i % 25 {
            0 => String::new(),
            1 => "\"first line\nsecond line\"".to_string(),
            2 => "\"amounts: 1,200.00 and 3,400.50\"".to_string(),
            _ => "ok".to_string(),
        };

        let line = format!("{i},{cust},{city},{amount:.2},{note},2026-{month:02}-{day:02}\n");
        out.write_all(line.as_bytes())?;
        rows += 1;

        // One in a thousand rows has an extra unquoted comma: a real-world
        // defect the tour should show the tool catching, not hiding.
        if i % 1000 == 7 {
            out.write_all(
                format!(
                    "{i},C{:06},{city},{amount:.2},stray,comma,2026-{month:02}-{day:02}\n",
                    r % 50_000
                )
                .as_bytes(),
            )?;
            rows += 1;
        }

        // 200 duplicates at the end of the file, so `dedupe` has something to do.
        if i % 100 == 0 && i < SAMPLE_ROWS / 2 {
            out.write_all(line.as_bytes())?;
            rows += 1;
        }
    }
    out.flush()?;
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    Ok((rows, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new() -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_demo_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn sample_is_readable_and_deliberately_imperfect() {
        let dir = Dir::new();
        let path = dir.0.join("orders.csv");
        let (rows, bytes) = write_sample(&path).unwrap();
        assert!(rows >= SAMPLE_ROWS, "expected the duplicates too");
        assert!(bytes > 100_000, "sample should be big enough to be real");

        let (tx, _rx) = std::sync::mpsc::channel();
        let counts = check::count(&path, crate::types::Delimiter::Comma, &tx).unwrap();
        assert_eq!(counts.columns, 6);
        assert_eq!(counts.rows, rows);
        // The stray-comma rows must be visible, not silently repaired.
        assert!(counts.ragged > 0, "sample should contain ragged rows");
    }

    #[test]
    fn sample_is_deterministic() {
        let a = Dir::new();
        let b = Dir::new();
        let (_, bytes_a) = write_sample(&a.0.join("a.csv")).unwrap();
        let (_, bytes_b) = write_sample(&b.0.join("b.csv")).unwrap();
        assert_eq!(bytes_a, bytes_b, "documentation quotes this output");
        assert_eq!(
            std::fs::read(a.0.join("a.csv")).unwrap(),
            std::fs::read(b.0.join("b.csv")).unwrap()
        );
    }
}
