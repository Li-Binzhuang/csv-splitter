//! The append-only journal, and the text format it is stored in.

use super::*;

/// The ordered list of edits applied to a base file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Journal {
    ops: Vec<Op>,
    /// Operations taken back by `undo` and available to `redo`.
    ///
    /// Kept in its own sidecar so the journal itself stays append-only for readers:
    /// a file being read never has to know that someone pressed undo.
    redo: Vec<Op>,
}

impl Journal {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sidecar path for a base file: `orders.csv` → `orders.csv.journal`.
    pub fn sidecar(base: &Path) -> PathBuf {
        let mut name = base.as_os_str().to_string_lossy().to_string();
        name.push_str(".journal");
        PathBuf::from(name)
    }

    /// Where undone operations wait for a redo.
    pub fn redo_sidecar(base: &Path) -> PathBuf {
        PathBuf::from(format!("{}.redo", Self::sidecar(base).display()))
    }

    /// Load the journal, or an empty one when the sidecar does not exist.
    ///
    /// The file is CSV so it can be read, diffed and hand-edited; a corrupt line
    /// is skipped with its number reported, because losing an undo step is better
    /// than refusing to open the sheet.
    pub fn load(path: &Path) -> io::Result<Self> {
        Ok(Self {
            ops: Self::read_ops(path)?,
            // The redo sidecar is derived from the journal's path, so callers keep
            // passing the same one path they always did.
            redo: Self::read_ops(&path.with_extension("journal.redo"))?,
        })
    }

    fn read_ops(path: &Path) -> io::Result<Vec<Op>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut reader = RecordReader::new(
            BufReader::with_capacity(1024 * 1024, File::open(path)?),
            1024 * 1024,
        );
        let mut record = Vec::with_capacity(4096);
        let mut ops = Vec::new();
        while reader.next_record(&mut record)? {
            if let Some(op) = parse_op(&record) {
                ops.push(op);
            }
        }
        Ok(ops)
    }

    /// Save atomically (write to `.tmp`, then rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        Self::write_ops(path, &self.ops)?;
        let redo = path.with_extension("journal.redo");
        if self.redo.is_empty() {
            // No redo stack means no file: an empty sidecar would be noise.
            if redo.exists() {
                std::fs::remove_file(&redo)?;
            }
            return Ok(());
        }
        Self::write_ops(&redo, &self.redo)
    }

    fn write_ops(path: &Path, ops: &[Op]) -> io::Result<()> {
        let tmp = path.with_extension("tmp");
        {
            let mut out = BufWriter::with_capacity(256 * 1024, File::create(&tmp)?);
            for op in ops {
                out.write_all(render_op(op).as_bytes())?;
            }
            out.flush()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// Record a new operation.
    ///
    /// Any new edit discards the redo stack, the way a spreadsheet does: once you
    /// change something after undoing, the branch you undid is gone.
    pub fn push(&mut self, op: Op) {
        self.ops.push(op);
        self.redo.clear();
    }

    /// Remove and return the last operation, making it available to `redo`.
    pub fn undo(&mut self) -> Option<Op> {
        let op = self.ops.pop()?;
        self.redo.push(op.clone());
        Some(op)
    }

    /// Put the most recently undone operation back, returning it.
    pub fn redo(&mut self) -> Option<Op> {
        let op = self.redo.pop()?;
        self.ops.push(op.clone());
        Some(op)
    }

    pub fn undo_len(&self) -> usize {
        self.ops.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Render one operation as a single CSV record.
pub(crate) fn render_op(op: &Op) -> String {
    match op {
        Op::SetCell { row, column, value } => {
            join_csv_line(&[op.tag(), &row.to_string(), column, value], b',')
        }
        Op::InsertRows { at, rows } => {
            // The whole inserted block rides in one quoted field: "[a,b],[c,d]".
            // `join_csv_line` terminates with a newline; inside this compound
            // field that would split the record, so trim it off.
            let cell = rows
                .iter()
                .map(|r| {
                    join_csv_line(&r.iter().map(String::as_str).collect::<Vec<_>>(), b',')
                        .trim_end_matches('\n')
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\u{1e}");
            join_csv_line(&[op.tag(), &at.to_string(), &cell], b',')
        }
        Op::DeleteRows { at, count } => {
            join_csv_line(&[op.tag(), &at.to_string(), &count.to_string()], b',')
        }
        Op::SetWhere {
            filter,
            column,
            value,
        } => join_csv_line(&[op.tag(), filter, column, value], b','),
        Op::DeleteRanges { runs } => join_csv_line(
            &[
                op.tag(),
                &runs
                    .iter()
                    .map(|(at, count)| format!("{at}:{count}"))
                    .collect::<Vec<_>>()
                    .join(";"),
            ],
            b',',
        ),
        Op::FillRange {
            column,
            from,
            to,
            value,
        } => join_csv_line(
            &[op.tag(), &from.to_string(), &to.to_string(), column, value],
            b',',
        ),
        Op::Substitute { column, from, to } => join_csv_line(
            &[op.tag(), column.as_deref().unwrap_or("*"), from, to],
            b',',
        ),
        Op::SplitColumn { column, by, into } => {
            join_csv_line(&[op.tag(), column, by, &into.join("|")], b',')
        }
        Op::SubstituteRegex {
            column,
            pattern,
            to,
        } => join_csv_line(
            &[op.tag(), column.as_deref().unwrap_or("*"), pattern, to],
            b',',
        ),
        Op::AddColumn { name, formula } => join_csv_line(&[op.tag(), name, formula], b','),
        Op::PasteBlock { row, column, rows } => {
            let block = rows
                .iter()
                .map(|r| {
                    join_csv_line(&r.iter().map(String::as_str).collect::<Vec<_>>(), b',')
                        .trim_end_matches('\n')
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\u{1e}");
            join_csv_line(&[op.tag(), &row.to_string(), column, &block], b',')
        }
        Op::AddColumnAt {
            after,
            name,
            source,
        } => {
            let (kind, value) = match source {
                ColumnSource::Literal(v) => ("literal", v.clone()),
                ColumnSource::Formula(f) => ("formula", f.clone()),
            };
            join_csv_line(
                &[op.tag(), after.as_deref().unwrap_or(""), name, kind, &value],
                b',',
            )
        }
        Op::DropColumn { name } => join_csv_line(&[op.tag(), name], b','),
        Op::RenameColumn { from, to } => join_csv_line(&[op.tag(), from, to], b','),
    }
}

/// Parse one journal record back into an operation.
pub(crate) fn parse_op(record: &[u8]) -> Option<Op> {
    let text = String::from_utf8_lossy(record);
    let trimmed = text.trim_end_matches(['\n', '\r']);
    let fields = parse_csv_record(trimmed, b',');
    let tag = fields.first()?.trim();
    match tag {
        "set" if fields.len() >= 4 => Some(Op::SetCell {
            row: fields[1].trim().parse().ok()?,
            column: fields[2].clone(),
            value: fields[3].clone(),
        }),
        "insert" if fields.len() >= 3 => {
            let at = fields[1].trim().parse().ok()?;
            let block = &fields[2];
            let mut rows = Vec::new();
            for line in block.split('\u{1e}') {
                let line = line.trim_end_matches(['\n', '\r']);
                if line.is_empty() {
                    continue;
                }
                rows.push(parse_csv_record(line, b','));
            }
            Some(Op::InsertRows { at, rows })
        }
        "delete" if fields.len() >= 3 => Some(Op::DeleteRows {
            at: fields[1].trim().parse().ok()?,
            count: fields[2].trim().parse().ok()?,
        }),
        "setwhere" if fields.len() >= 4 => Some(Op::SetWhere {
            filter: fields[1].clone(),
            column: fields[2].clone(),
            value: fields[3].clone(),
        }),
        "dels" if fields.len() >= 2 => Some(Op::DeleteRanges {
            runs: fields[1]
                .split(';')
                .filter_map(|pair| {
                    let (at, count) = pair.split_once(':')?;
                    Some((at.trim().parse().ok()?, count.trim().parse().ok()?))
                })
                .collect(),
        }),
        "fill" if fields.len() >= 5 => Some(Op::FillRange {
            from: fields[1].parse().ok()?,
            to: fields[2].parse().ok()?,
            column: fields[3].clone(),
            value: fields[4].clone(),
        }),
        "split-col" if fields.len() >= 4 => Some(Op::SplitColumn {
            column: fields[1].clone(),
            by: fields[2].clone(),
            into: fields[3]
                .split('|')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }),
        "subst-rx" if fields.len() >= 4 => Some(Op::SubstituteRegex {
            column: match fields[1].trim() {
                "*" | "" => None,
                name => Some(name.to_string()),
            },
            pattern: fields[2].clone(),
            to: fields[3].clone(),
        }),
        "substitute" if fields.len() >= 4 => Some(Op::Substitute {
            column: match fields[1].trim() {
                "*" | "" => None,
                name => Some(name.to_string()),
            },
            from: fields[2].clone(),
            to: fields[3].clone(),
        }),
        "pasteblock" if fields.len() >= 4 => {
            let mut rows = Vec::new();
            for line in fields[3].split('\u{1e}') {
                let line = line.trim_end_matches(['\n', '\r']);
                if line.is_empty() {
                    continue;
                }
                rows.push(parse_csv_record(line, b','));
            }
            Some(Op::PasteBlock {
                row: fields[1].trim().parse().ok()?,
                column: fields[2].clone(),
                rows,
            })
        }
        "addcolumnat" if fields.len() >= 5 => Some(Op::AddColumnAt {
            after: if fields[1].trim().is_empty() {
                None
            } else {
                Some(fields[1].trim().to_string())
            },
            name: fields[2].clone(),
            source: if fields[3].trim() == "literal" {
                ColumnSource::Literal(fields[4].clone())
            } else {
                ColumnSource::Formula(fields[4].clone())
            },
        }),
        "addcolumn" if fields.len() >= 3 => Some(Op::AddColumn {
            name: fields[1].clone(),
            formula: fields[2].clone(),
        }),
        "dropcolumn" if fields.len() >= 2 => Some(Op::DropColumn {
            name: fields[1].clone(),
        }),
        "renamecolumn" if fields.len() >= 3 => Some(Op::RenameColumn {
            from: fields[1].clone(),
            to: fields[2].clone(),
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------- row mapping

/// A contiguous run of logical rows that come from one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Segment {
    /// Rows `physical..physical+len` of the base file, starting at `logical`.
    Base {
        logical: u64,
        physical: u64,
        len: u64,
    },
    /// Rows typed in by the user; they exist only in the journal. `id` is the
    /// journal position of the insert operation, which is stable across reloads
    /// and is what cell edits on inserted rows are keyed by.
    Virtual {
        logical: u64,
        id: u64,
        rows: Vec<Vec<String>>,
    },
}

impl Segment {
    pub(crate) fn logical_start(&self) -> u64 {
        match self {
            Self::Base { logical, .. } | Self::Virtual { logical, .. } => *logical,
        }
    }

    pub(crate) fn len(&self) -> u64 {
        match self {
            Self::Base { len, .. } => *len,
            Self::Virtual { rows, .. } => rows.len() as u64,
        }
    }
}

/// Where a logical row actually lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowRef {
    /// Read record `physical` from the base file.
    Base(u64),
    /// This row was typed in; `id` identifies the insert operation it came from.
    Virtual { id: u64, cells: Vec<String> },
}
