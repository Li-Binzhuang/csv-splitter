//! The `transaction` batch: one text syntax, one atomic journal write.
//!
//! The CLI and the HTTP endpoint both accept `set:row:col:value;delete:row:count;
//! insert:row:a,b,c`. That parsing used to live twice, which meant a syntax change had
//! to be made twice or the two surfaces would disagree. It lives here now, together with
//! the all-or-nothing append: validation happens first, so a bad entry never reaches the
//! journal, and the surviving entries land in a single locked save.

use super::{Journal, Op};
use crate::csv_reader::parse_csv_record;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionErrorKind {
    /// The entry does not parse (`set` without a value, an unknown tag, …).
    Syntax,
    /// The entry parses but points outside the sheet.
    OutOfRange,
    /// The entry names a column the sheet does not have.
    UnknownColumn,
}

#[derive(Debug, Clone)]
pub struct TransactionError {
    pub kind: TransactionErrorKind,
    pub message: String,
}

impl std::fmt::Display for TransactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransactionError {}

fn error(kind: TransactionErrorKind, message: String) -> TransactionError {
    TransactionError { kind, message }
}

/// Parse and validate a batch against the sheet's real shape.
///
/// Rows are 1-based on the wire, like every other row number; `Op` stores 0-based rows.
pub fn parse_ops(text: &str, total: u64, columns: &[String]) -> Result<Vec<Op>, TransactionError> {
    let mut ops = Vec::new();
    for part in text.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mut pieces = part.splitn(4, ':');
        let kind = pieces.next().unwrap_or_default().trim();
        let syntax = || {
            error(
                TransactionErrorKind::Syntax,
                format!("transaction entry '{part}' is not valid"),
            )
        };
        match kind {
            "set" => {
                let (Some(row_text), Some(column), Some(value)) =
                    (pieces.next(), pieces.next(), pieces.next())
                else {
                    return Err(syntax());
                };
                let Ok(row) = row_text.trim().parse::<u64>() else {
                    return Err(syntax());
                };
                if row < 1 || row > total {
                    return Err(error(
                        TransactionErrorKind::OutOfRange,
                        format!("row {row} is outside 1..={total}"),
                    ));
                }
                let column = column.trim();
                if !columns.iter().any(|c| c == column) {
                    return Err(error(
                        TransactionErrorKind::UnknownColumn,
                        format!(
                            "transaction entry '{part}' names unknown column '{column}' (available: {})",
                            columns.join(", ")
                        ),
                    ));
                }
                ops.push(Op::SetCell {
                    row: row - 1,
                    column: column.to_string(),
                    value: value.to_string(),
                });
            }
            "delete" => {
                let (Some(row_text), Some(count_text)) = (pieces.next(), pieces.next()) else {
                    return Err(syntax());
                };
                let (Ok(row), Ok(count)) = (
                    row_text.trim().parse::<u64>(),
                    count_text.trim().parse::<u64>(),
                ) else {
                    return Err(syntax());
                };
                if count == 0 || row < 1 || row.saturating_add(count - 1) > total {
                    return Err(error(
                        TransactionErrorKind::OutOfRange,
                        format!("delete {row}:{count} is outside 1..={total} rows"),
                    ));
                }
                ops.push(Op::DeleteRows { at: row - 1, count });
            }
            "insert" => {
                let (Some(row_text), Some(value)) = (pieces.next(), pieces.next()) else {
                    return Err(syntax());
                };
                let Ok(row) = row_text.trim().parse::<u64>() else {
                    return Err(syntax());
                };
                if row < 1 || row > total + 1 {
                    return Err(error(
                        TransactionErrorKind::OutOfRange,
                        format!("insert row {row} is outside 1..={}", total + 1),
                    ));
                }
                let cells = if value.trim().is_empty() {
                    Vec::new()
                } else {
                    parse_csv_record(value, b',')
                };
                ops.push(Op::InsertRows {
                    at: row - 1,
                    rows: vec![cells],
                });
            }
            _ => return Err(syntax()),
        }
    }
    if ops.is_empty() {
        return Err(error(
            TransactionErrorKind::Syntax,
            "ops parsed to no operations".to_string(),
        ));
    }
    Ok(ops)
}

/// Append every operation in one locked, atomic journal update.
///
/// Returns the journal length after the write (the number of pending operations), which
/// is what both surfaces report.
pub fn append_atomically(sidecar: &Path, ops: &[Op]) -> io::Result<usize> {
    let mut applied = 0usize;
    Journal::update_locked(sidecar, |fresh| {
        for op in ops {
            fresh.push(op.clone());
        }
        applied = fresh.len();
        Ok(())
    })?;
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns() -> Vec<String> {
        vec!["city".to_string(), "amount".to_string()]
    }

    #[test]
    fn a_batch_parses_to_the_expected_operations() {
        let ops = parse_ops("set:1:city:X;delete:2:1;insert:4:d,4", 3, &columns()).unwrap();
        assert_eq!(ops.len(), 3);
        assert!(matches!(ops[0], Op::SetCell { row: 0, .. }));
        assert!(matches!(ops[1], Op::DeleteRows { at: 1, count: 1 }));
        assert!(matches!(ops[2], Op::InsertRows { at: 3, .. }));
    }

    #[test]
    fn a_bad_entry_rejects_the_whole_batch() {
        for (text, kind) in [
            ("set:1:city:Q;delete:99:1", TransactionErrorKind::OutOfRange),
            ("set:1:nope:Q", TransactionErrorKind::UnknownColumn),
            ("set:1:city", TransactionErrorKind::Syntax),
        ] {
            let err = parse_ops(text, 3, &columns()).unwrap_err();
            assert_eq!(err.kind, kind, "{}", err.message);
        }
        // The good half must not be returned after the bad half fails.
        assert!(parse_ops("set:1:city:Q;delete:99:1", 3, &columns()).is_err());
    }
}
