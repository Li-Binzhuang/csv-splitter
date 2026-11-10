//! Cross-sheet references: `ref("customers.csv", "cust_no", "name")`.
//!
//! This is a VLOOKUP living inside a formula column: the current row supplies the
//! key, the referenced sheet supplies the value. The referenced sheet is loaded
//! once and held as a map; the sheet being computed stays streamed.
//!
//! Two decisions that follow from "the referenced sheet is someone else's data":
//!
//! * sheets are resolved **relative to the base file's directory**, never to the
//!   process working directory, so a formula keeps working when the directory moves;
//! * a reference that cannot be loaded is a **formula error at open time**, not a
//!   column of `#N/A` discovered after writing a million rows.

use crate::csv_reader::{parse_csv_record, resolve_column, RecordReader};
use crate::encoding;
use std::collections::HashMap;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

/// Ceiling on keys held per referenced sheet.
pub const DEFAULT_MAX_KEYS: usize = 2_000_000;

/// One loaded referenced sheet, already reduced to key → value.
#[derive(Clone, Debug, Default)]
pub struct SheetRef {
    pub values: HashMap<String, String>,
    pub rows: u64,
}

impl SheetRef {
    /// Load `sheet`, matching `key` and taking `value`.
    pub fn load(
        base_dir: &Path,
        sheet: &str,
        key: &str,
        value: &str,
        max_keys: usize,
    ) -> io::Result<Self> {
        let path: PathBuf = if Path::new(sheet).is_absolute() {
            PathBuf::from(sheet)
        } else {
            base_dir.join(sheet)
        };
        if !path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("referenced sheet {} does not exist", path.display()),
            ));
        }
        let mut records = RecordReader::new(
            BufReader::with_capacity(1024 * 1024, encoding::open_decoded(&path)?),
            1024 * 1024,
        );
        let mut record = Vec::with_capacity(16 * 1024);
        if !records.next_record(&mut record)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("referenced sheet {} has no header", path.display()),
            ));
        }
        let header = parse_csv_record(
            String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
            b',',
        );
        let key_idx = resolve_column(key, &header).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{}: {e}", path.display()),
            )
        })?;
        let value_idx = resolve_column(value, &header).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{}: {e}", path.display()),
            )
        })?;

        let mut values = HashMap::with_capacity(1024);
        let mut rows = 0u64;
        while records.next_record(&mut record)? {
            let fields = parse_csv_record(
                String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
                b',',
            );
            rows += 1;
            let Some(key_value) = fields.get(key_idx) else {
                continue;
            };
            let key_value = key_value.trim();
            if key_value.is_empty() || values.contains_key(key_value) {
                // First occurrence wins, like VLOOKUP.
                continue;
            }
            if values.len() >= max_keys {
                return Err(io::Error::other(format!(
                    "referenced sheet {} has more than {max_keys} keys",
                    path.display()
                )));
            }
            values.insert(
                key_value.to_string(),
                fields.get(value_idx).cloned().unwrap_or_default(),
            );
        }
        Ok(Self { values, rows })
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.values.get(key.trim())
    }
}

/// Every sheet a batch of formulas references, loaded once.
#[derive(Clone, Debug, Default)]
pub struct Refs {
    sheets: HashMap<String, SheetRef>,
}

impl Refs {
    /// Cache key for one (sheet, key, value) triple.
    fn key(sheet: &str, key: &str, value: &str) -> String {
        format!("{sheet}\u{1f}{key}\u{1f}{value}")
    }

    /// Load every reference a formula needs, failing if any is unreadable.
    pub fn load_for(
        base_dir: &Path,
        references: &[(String, String, String)],
        max_keys: usize,
    ) -> io::Result<Self> {
        let mut sheets: HashMap<String, SheetRef> = HashMap::new();
        for (sheet, key, value) in references {
            let cache_key = Self::key(sheet, key, value);
            if sheets.contains_key(&cache_key) {
                continue;
            }
            let loaded = SheetRef::load(base_dir, sheet, key, value, max_keys)?;
            sheets.insert(cache_key, loaded);
        }
        Ok(Self { sheets })
    }

    pub fn get(&self, sheet: &str, key: &str, value: &str) -> Option<&SheetRef> {
        self.sheets.get(&Self::key(sheet, key, value))
    }

    pub fn is_empty(&self) -> bool {
        self.sheets.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sheets.len()
    }
}
