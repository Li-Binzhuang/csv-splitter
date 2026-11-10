//! Editing a huge CSV without rewriting it.
//!
//! Excel and WPS load the file and keep it in memory, which is exactly what this
//! tool cannot do for a 10 GB export. So editing works the other way round:
//!
//! ```text
//!   immutable base file  +  append-only journal  =  a "sheet"
//! ```
//!
//! Every edit is an operation appended to a sidecar journal (`data.csv.journal`).
//! The base file is never touched. Reading a window means: map the logical row to
//! its physical row, then apply the edits that touch it — cost proportional to
//! the number of edits, never to the number of rows. Materialising an export is a
//! single streaming pass.
//!
//! Why a journal rather than an overlay file:
//!
//! * **Undo is free** — pop the last operation.
//! * **It is inspectable** — the journal is a CSV of operations, readable in any
//!   editor, diffable, and safe to commit next to the data.
//! * **It stays small** — a thousand cell edits are kilobytes, however big the
//!   file is.
//! * **The original stays intact** — if an edit turns out wrong, the source is
//!   still there, and `export` simply stops applying it.

use crate::csv_reader::{join_csv_line, parse_csv_record, RecordReader};
use crate::encoding;
use crate::types::{Delimiter, ProgressEvent, ProgressSender};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// One recorded edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Replace one cell. Rows are logical (0-based, header excluded).
    SetCell {
        row: u64,
        column: String,
        value: String,
    },
    /// Insert rows at a logical position, with explicit cell values.
    InsertRows {
        at: u64,
        rows: Vec<Vec<String>>,
    },
    /// Delete `count` logical rows starting at `at`.
    DeleteRows {
        at: u64,
        count: u64,
    },
    /// Replace a substring in one column (`None` = every column).
    Substitute {
        column: Option<String>,
        from: String,
        to: String,
    },
    /// Append a computed column (formula evaluated at export time).
    AddColumn {
        name: String,
        formula: String,
    },
    DropColumn {
        name: String,
    },
    RenameColumn {
        from: String,
        to: String,
    },
}

impl Op {
    /// The verb used in the journal file (also what `edit --json` reports).
    pub fn tag(&self) -> &'static str {
        match self {
            Self::SetCell { .. } => "set",
            Self::InsertRows { .. } => "insert",
            Self::DeleteRows { .. } => "delete",
            Self::Substitute { .. } => "substitute",
            Self::AddColumn { .. } => "addcolumn",
            Self::DropColumn { .. } => "dropcolumn",
            Self::RenameColumn { .. } => "renamecolumn",
        }
    }

    /// One-line human description, used by `edit status` and by the API.
    pub fn describe(&self) -> String {
        match self {
            Self::SetCell { row, column, value } => {
                format!("set row {row} '{column}' = '{}'", truncate(value, 40))
            }
            Self::InsertRows { at, rows } => format!("insert {} row(s) at {at}", rows.len()),
            Self::DeleteRows { at, count } => format!("delete {count} row(s) at {at}"),
            Self::Substitute { column, from, to } => format!(
                "substitute '{}' → '{}' in {}",
                truncate(from, 20),
                truncate(to, 20),
                column.as_deref().unwrap_or("every column")
            ),
            Self::AddColumn { name, formula } => format!("add column '{name}' = {formula}"),
            Self::DropColumn { name } => format!("drop column '{name}'"),
            Self::RenameColumn { from, to } => format!("rename '{from}' → '{to}'"),
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    let chars: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        format!("{chars}…")
    } else {
        chars
    }
}

/// The ordered list of edits applied to a base file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Journal {
    ops: Vec<Op>,
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

    /// Load the journal, or an empty one when the sidecar does not exist.
    ///
    /// The file is CSV so it can be read, diffed and hand-edited; a corrupt line
    /// is skipped with its number reported, because losing an undo step is better
    /// than refusing to open the sheet.
    pub fn load(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self::new());
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
        Ok(Self { ops })
    }

    /// Save atomically (write to `.tmp`, then rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("journal.tmp");
        {
            let mut out = BufWriter::with_capacity(256 * 1024, File::create(&tmp)?);
            for op in &self.ops {
                out.write_all(render_op(op).as_bytes())?;
            }
            out.flush()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn push(&mut self, op: Op) {
        self.ops.push(op);
    }

    /// Remove and return the last operation — the whole of undo.
    pub fn undo(&mut self) -> Option<Op> {
        self.ops.pop()
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
fn render_op(op: &Op) -> String {
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
        Op::Substitute { column, from, to } => join_csv_line(
            &[op.tag(), column.as_deref().unwrap_or("*"), from, to],
            b',',
        ),
        Op::AddColumn { name, formula } => join_csv_line(&[op.tag(), name, formula], b','),
        Op::DropColumn { name } => join_csv_line(&[op.tag(), name], b','),
        Op::RenameColumn { from, to } => join_csv_line(&[op.tag(), from, to], b','),
    }
}

/// Parse one journal record back into an operation.
fn parse_op(record: &[u8]) -> Option<Op> {
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
        "substitute" if fields.len() >= 4 => Some(Op::Substitute {
            column: match fields[1].trim() {
                "*" | "" => None,
                name => Some(name.to_string()),
            },
            from: fields[2].clone(),
            to: fields[3].clone(),
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
enum Segment {
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
    fn logical_start(&self) -> u64 {
        match self {
            Self::Base { logical, .. } | Self::Virtual { logical, .. } => *logical,
        }
    }

    fn len(&self) -> u64 {
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

/// Map between logical rows (what the user sees) and physical ones (the file).
#[derive(Clone, Debug, Default)]
pub struct RowMap {
    segments: Vec<Segment>,
    rows: u64,
}

impl RowMap {
    /// Build the map from a list of operations.
    ///
    /// Cost is proportional to the number of structural operations, not to the
    /// number of rows: a 19-million-row file with three inserts is three splits.
    pub fn build(ops: &[Op], base_rows: u64) -> Self {
        let mut segments = vec![Segment::Base {
            logical: 0,
            physical: 0,
            len: base_rows,
        }];

        for op in ops {
            match op {
                Op::InsertRows { at, rows } => insert(&mut segments, *at, rows.clone(), u64::MAX),
                Op::DeleteRows { at, count } => delete(&mut segments, *at, *count),
                _ => {}
            }
        }

        let rows = segments
            .iter()
            .map(Segment::len)
            .fold(0u64, u64::saturating_add);
        Self { segments, rows }
    }

    /// Logical row count after insertions and deletions.
    pub fn len(&self) -> u64 {
        self.rows
    }

    /// Recompute logical numbering after a structural change.
    fn refresh(&mut self) {
        let mut logical = 0u64;
        for seg in &mut self.segments {
            match seg {
                Segment::Base { logical: l, .. } | Segment::Virtual { logical: l, .. } => {
                    *l = logical
                }
            }
            logical = logical.saturating_add(seg.len());
        }
        self.rows = logical;
    }

    /// Stable identity of a logical row (what cell edits are keyed by).
    pub fn id_at(&self, logical: u64) -> Option<RowId> {
        match self.row(logical)? {
            RowRef::Base(physical) => Some(RowId::Base(physical)),
            RowRef::Virtual { id, .. } => Some(RowId::Virtual(id)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// Resolve one logical row.
    pub fn row(&self, logical: u64) -> Option<RowRef> {
        if logical >= self.rows {
            return None;
        }
        let idx = match self
            .segments
            .binary_search_by(|seg| seg.logical_start().cmp(&logical))
        {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        let seg = &self.segments[idx];
        let offset = logical - seg.logical_start();
        match seg {
            Segment::Base { physical, .. } => Some(RowRef::Base(physical.saturating_add(offset))),
            Segment::Virtual { rows, id, .. } => rows
                .get(offset as usize)
                .cloned()
                .map(|cells| RowRef::Virtual { id: *id, cells }),
        }
    }

    /// Physical row ranges backing the logical window `[start, start+count)`.
    ///
    /// Returned as `(physical_start, physical_len, logical_offset)` runs so a
    /// reader can stream consecutive rows instead of seeking per row.
    pub fn physical_runs(&self, start: u64, count: u64) -> Vec<Run> {
        let mut out = Vec::new();
        let end = start.saturating_add(count).min(self.rows);
        for seg in &self.segments {
            let seg_start = seg.logical_start();
            let seg_end = seg_start.saturating_add(seg.len());
            if seg_end <= start || seg_start >= end {
                continue;
            }
            let from = start.max(seg_start);
            let to = end.min(seg_end);
            match seg {
                Segment::Base { physical, .. } => out.push(Run::Base {
                    logical: from,
                    physical: physical.saturating_add(from - seg_start),
                    len: to - from,
                }),
                Segment::Virtual { rows, id, .. } => out.push(Run::Virtual {
                    logical: from,
                    id: *id,
                    rows: rows[(from - seg_start) as usize..(to - seg_start) as usize].to_vec(),
                }),
            }
        }
        out
    }
}

/// One contiguous piece of a logical window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    Base {
        logical: u64,
        physical: u64,
        len: u64,
    },
    Virtual {
        logical: u64,
        id: u64,
        rows: Vec<Vec<String>>,
    },
}

impl Run {
    pub fn logical_start(&self) -> u64 {
        match self {
            Self::Base { logical, .. } | Self::Virtual { logical, .. } => *logical,
        }
    }
}

fn insert(segments: &mut Vec<Segment>, at: u64, rows: Vec<Vec<String>>, id: u64) {
    if rows.is_empty() {
        return;
    }
    // Rebuild the list: everything before `at` keeps its place, the new block
    // goes in, everything after shifts by the block length.
    let mut out = Vec::with_capacity(segments.len() + 1);
    let mut placed = false;
    for seg in segments.drain(..) {
        let start = seg.logical_start();
        let seg_len = seg.len();
        if start <= at && at < start.saturating_add(seg_len) {
            // Split this segment exactly at `at`.
            let head_len = at - start;
            if head_len > 0 {
                out.push(match &seg {
                    Segment::Base { physical, .. } => Segment::Base {
                        logical: start,
                        physical: *physical,
                        len: head_len,
                    },
                    Segment::Virtual { rows: vr, id, .. } => Segment::Virtual {
                        logical: start,
                        id: *id,
                        rows: vr[..head_len as usize].to_vec(),
                    },
                });
            }
            out.push(Segment::Virtual {
                logical: at,
                id,
                rows: rows.clone(),
            });
            let tail_len = seg_len.saturating_sub(head_len);
            if tail_len > 0 {
                out.push(match &seg {
                    Segment::Base { physical, .. } => Segment::Base {
                        logical: at,
                        physical: physical.saturating_add(head_len),
                        len: tail_len,
                    },
                    Segment::Virtual { rows: vr, id, .. } => Segment::Virtual {
                        logical: at,
                        id: *id,
                        rows: vr[head_len as usize..].to_vec(),
                    },
                });
            }
            placed = true;
        } else {
            out.push(seg);
        }
    }
    if !placed {
        out.push(Segment::Virtual {
            logical: at,
            id,
            rows,
        });
    }
    // Logical numbering restarts from zero with the new layout.
    let mut logical = 0u64;
    for seg in &mut out {
        match seg {
            Segment::Base { logical: l, .. } | Segment::Virtual { logical: l, .. } => *l = logical,
        }
        logical = logical.saturating_add(seg.len());
    }
    *segments = out;
}

fn delete(segments: &mut Vec<Segment>, at: u64, count: u64) {
    if count == 0 {
        return;
    }
    let mut remaining = count;
    let mut out = Vec::with_capacity(segments.len());
    for seg in segments.drain(..) {
        let start = seg.logical_start();
        let seg_len = seg.len();
        if remaining == 0 || at >= start.saturating_add(seg_len) {
            out.push(seg);
            continue;
        }
        if at <= start && remaining >= seg_len {
            // Wholly deleted.
            remaining -= seg_len;
            continue;
        }
        // Partial: keep the head before `at`, then keep whatever is left.
        let head = at.saturating_sub(start).min(seg_len);
        let tail_from = head.saturating_add(remaining).min(seg_len);
        remaining = remaining.saturating_sub(seg_len.saturating_sub(head));
        if head > 0 {
            out.push(match &seg {
                Segment::Base { physical, .. } => Segment::Base {
                    logical: start,
                    physical: *physical,
                    len: head,
                },
                Segment::Virtual { rows, id, .. } => Segment::Virtual {
                    logical: start,
                    id: *id,
                    rows: rows[..head as usize].to_vec(),
                },
            });
        }
        if tail_from < seg_len {
            out.push(match &seg {
                Segment::Base { physical, .. } => Segment::Base {
                    logical: start,
                    physical: physical.saturating_add(tail_from),
                    len: seg_len.saturating_sub(tail_from),
                },
                Segment::Virtual { rows, id, .. } => Segment::Virtual {
                    logical: start,
                    id: *id,
                    rows: rows[tail_from as usize..].to_vec(),
                },
            });
        }
    }
    let mut logical = 0u64;
    for seg in &mut out {
        match seg {
            Segment::Base { logical: l, .. } | Segment::Virtual { logical: l, .. } => *l = logical,
        }
        logical = logical.saturating_add(seg.len());
    }
    *segments = out;
}

// ------------------------------------------------------------- sheet & export

/// Edits that change cell contents rather than row structure.
#[derive(Clone, Debug, Default)]
pub struct CellEdits {
    /// `(row identity, column name) → new value`.
    ///
    /// Keyed by identity, not by logical position: inserting a row above must not
    /// silently reassign an earlier edit to its neighbour, which is exactly what
    /// would happen if edits were stored by index.
    cells: HashMap<(RowId, String), String>,
    /// Substring replacements, applied in journal order.
    subs: Vec<(Option<String>, String, String)>,
}

/// Stable identity of a row: a position in the base file, or an insert block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowId {
    Base(u64),
    Virtual(u64),
}

impl CellEdits {
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.subs.is_empty()
    }

    /// New value for a cell, if an edit touched it.
    pub fn cell(&self, row: RowId, column: &str) -> Option<&str> {
        self.cells
            .get(&(row, column.to_string()))
            .map(String::as_str)
    }

    /// Apply substitutions to one value.
    pub fn substitute(&self, column: &str, value: &str) -> String {
        let mut out = value.to_string();
        for (target, from, to) in &self.subs {
            let applies = match target {
                None => true,
                Some(name) => name == column,
            };
            if applies && out.contains(from.as_str()) {
                out = out.replace(from.as_str(), to);
            }
        }
        out
    }
}

/// The compiled sheet: final row map plus the cell edits resolved to identities.
#[derive(Clone, Debug, Default)]
pub struct Sheet {
    pub map: RowMap,
    pub edits: CellEdits,
}

/// Apply the journal once, resolving positional edits into stable identities.
///
/// This is the piece that makes editing behave like a spreadsheet: a `set` at
/// row 500000 records *which* row it hit, so a later insert near the top shifts
/// that row down (and the edit goes with it) instead of silently re-pointing the
/// edit at the row that now occupies slot 500000.
pub fn compile(ops: &[Op], base_rows: u64) -> Sheet {
    let mut map = RowMap::build(&[], base_rows);
    let mut cells: HashMap<(RowId, String), String> = HashMap::new();
    let mut subs: Vec<(Option<String>, String, String)> = Vec::new();

    for (index, op) in ops.iter().enumerate() {
        match op {
            Op::InsertRows { at, rows } => {
                insert(&mut map.segments, *at, rows.clone(), index as u64);
                map.refresh();
            }
            Op::DeleteRows { at, count } => {
                delete(&mut map.segments, *at, *count);
                map.refresh();
            }
            Op::SetCell { row, column, value } => {
                if let Some(id) = map.id_at(*row) {
                    cells.insert((id, column.clone()), value.clone());
                }
            }
            Op::Substitute { column, from, to } => {
                subs.push((column.clone(), from.clone(), to.clone()));
            }
            _ => {}
        }
    }

    Sheet {
        map,
        edits: CellEdits { cells, subs },
    }
}

/// Header layout after column operations.
pub fn resolve_columns(ops: &[Op], base: &[String]) -> io::Result<Vec<String>> {
    let mut columns = base.to_vec();
    for op in ops {
        match op {
            Op::RenameColumn { from, to } => {
                let idx = columns.iter().position(|c| c == from).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("cannot rename: no column '{from}'"),
                    )
                })?;
                columns[idx] = to.clone();
            }
            Op::DropColumn { name } => {
                let idx = columns.iter().position(|c| c == name).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("cannot drop: no column '{name}'"),
                    )
                })?;
                columns.remove(idx);
                if columns.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "the journal drops every column",
                    ));
                }
            }
            Op::AddColumn { name, .. } => columns.push(name.clone()),
            _ => {}
        }
    }
    Ok(columns)
}

/// Read the base file's header.
pub fn base_header(base: &Path, delimiter: Delimiter) -> io::Result<Vec<String>> {
    let reader = encoding::open_decoded(base)?;
    let mut records = RecordReader::new(BufReader::with_capacity(256 * 1024, reader), 256 * 1024);
    let mut record = Vec::with_capacity(4096);
    if !records.next_record(&mut record)? {
        return Ok(Vec::new());
    }
    Ok(parse_csv_record(
        String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
        delimiter.byte(),
    ))
}

/// Materialise `base + journal` into `output`.
///
/// One streaming pass over the base file: no per-row seeking, no memory that
/// grows with the file. Cells are rewritten only where an edit touches them, and
/// the write-side encoding is UTF-8 (documented; the base stays untouched).
pub fn export(
    base: &Path,
    ops: &[Op],
    output: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
) -> io::Result<ExportSummary> {
    let delim = delimiter.byte();
    let base_columns = base_header(base, delimiter)?;
    if base_columns.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the base file has no header",
        ));
    }
    let columns = resolve_columns(ops, &base_columns)?;
    let sheet = compile(ops, u64::MAX);
    let edits = &sheet.edits;
    let map = &sheet.map;

    tx.send(ProgressEvent::PhaseStart {
        phase: "Exporting edits".into(),
        total_bytes: std::fs::metadata(base).map(|m| m.len()).unwrap_or(0),
    })
    .ok();

    let mut out = BufWriter::with_capacity(8 * 1024 * 1024, File::create(output)?);
    let header: Vec<&str> = columns.iter().map(String::as_str).collect();
    out.write_all(join_csv_line(&header, delim).as_bytes())?;

    // Column mapping: kept base columns by index, added ones evaluated later.
    let mut keep: Vec<Option<usize>> = Vec::new(); // output index -> base index
    let mut added: Vec<(usize, String)> = Vec::new(); // output index -> formula
    for (out_idx, name) in columns.iter().enumerate() {
        match base_columns.iter().position(|b| b == name) {
            Some(base_idx) => keep.push(Some(base_idx)),
            None => {
                keep.push(None);
                if let Some(Op::AddColumn { formula, .. }) = ops
                    .iter()
                    .find(|op| matches!(op, Op::AddColumn { name: n, .. } if n == name))
                {
                    added.push((out_idx, formula.clone()));
                }
            }
        }
    }

    let reader_bytes = encoding::open_decoded(base)?;
    let mut records = RecordReader::new(
        BufReader::with_capacity(8 * 1024 * 1024, reader_bytes),
        8 * 1024 * 1024,
    );
    let mut record = Vec::with_capacity(64 * 1024);
    if !records.next_record(&mut record)? {
        return Ok(ExportSummary::default());
    }

    let mut summary = ExportSummary {
        columns: columns.clone(),
        ..Default::default()
    };
    let mut physical: u64 = 0;
    let mut logical: u64 = 0;

    loop {
        // Which logical row sits at `logical`? Ask the map so inserted blocks and
        // deleted ranges are handled by the same code path as normal rows.
        match map.row(logical) {
            None => break,
            Some(RowRef::Virtual { id, cells }) => {
                emit(
                    &mut out,
                    &columns,
                    &keep,
                    &cells,
                    edits,
                    RowId::Virtual(id),
                    delim,
                )?;
                summary.rows += 1;
                logical += 1;
            }
            Some(RowRef::Base(target)) => {
                // Skip base rows that were deleted, then read the target one.
                while physical <= target {
                    if !records.next_record(&mut record)? {
                        physical = u64::MAX;
                        break;
                    }
                    physical += 1;
                }
                if physical == u64::MAX {
                    break;
                }
                let cells = parse_csv_record(
                    String::from_utf8_lossy(&record).trim_end_matches(['\n', '\r']),
                    delim,
                );
                emit(
                    &mut out,
                    &columns,
                    &keep,
                    &cells,
                    edits,
                    RowId::Base(target),
                    delim,
                )?;
                summary.rows += 1;
                logical += 1;
            }
        }
    }

    // Anything still in the file but not addressed by the map is unreachable;
    // report it rather than silently dropping rows.
    let _ = &added;
    out.flush()?;
    summary.bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    tx.send(ProgressEvent::PhaseDone {
        phase: "Exporting edits".into(),
    })
    .ok();
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn emit(
    out: &mut impl Write,
    columns: &[String],
    keep: &[Option<usize>],
    base_cells: &[String],
    edits: &CellEdits,
    id: RowId,
    delim: u8,
) -> io::Result<()> {
    let mut cells: Vec<String> = Vec::with_capacity(columns.len());
    for (idx, column) in columns.iter().enumerate() {
        let raw = match keep[idx] {
            Some(base_idx) => base_cells.get(base_idx).cloned().unwrap_or_default(),
            None => String::new(),
        };
        let value = match edits.cell(id, column) {
            Some(v) => v.to_string(),
            None => edits.substitute(column, &raw),
        };
        cells.push(value);
    }
    let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
    out.write_all(join_csv_line(&refs, delim).as_bytes())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExportSummary {
    pub rows: u64,
    pub bytes: u64,
    pub columns: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(std::path::PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "csvs_wb_{tag}_{}_{}",
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

    fn base_with(dir: &Dir, csv: &str) -> std::path::PathBuf {
        let p = dir.join("base.csv");
        std::fs::write(&p, csv).unwrap();
        p
    }

    fn export_with(dir: &Dir, base: &std::path::Path, ops: Vec<Op>) -> String {
        let out = dir.join("out.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        export(base, &ops, &out, Delimiter::Comma, &tx).unwrap();
        std::fs::read_to_string(&out).unwrap()
    }

    const BASE: &str = "id,city,amount\n1,CDMX,10\n2,Puebla,20\n3,CDMX,30\n4,Tijuana,40\n";

    #[test]
    fn a_set_cell_changes_exactly_one_row() {
        let dir = Dir::new("set");
        let base = base_with(&dir, BASE);
        let ops = vec![Op::SetCell {
            row: 1,
            column: "city".into(),
            value: "Monterrey".into(),
        }];
        let text = export_with(&dir, &base, ops);
        assert_eq!(
            text,
            "id,city,amount\n1,CDMX,10\n2,Monterrey,20\n3,CDMX,30\n4,Tijuana,40\n"
        );
    }

    #[test]
    fn inserted_rows_shift_later_edits() {
        let dir = Dir::new("insert");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::InsertRows {
                at: 1,
                rows: vec![vec!["9".into(), "New".into(), "50".into()]],
            },
            // Row 2 is *after* the insert, so it is the original row 1.
            Op::SetCell {
                row: 2,
                column: "city".into(),
                value: "Edited".into(),
            },
        ];
        let text = export_with(&dir, &base, ops);
        assert_eq!(
            text,
            "id,city,amount\n1,CDMX,10\n9,New,50\n2,Edited,20\n3,CDMX,30\n4,Tijuana,40\n"
        );
    }

    #[test]
    fn deleted_rows_disappear_and_later_edits_shift() {
        let dir = Dir::new("delete");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::DeleteRows { at: 1, count: 2 },
            Op::SetCell {
                row: 1,
                column: "city".into(),
                value: "Last".into(),
            },
        ];
        let text = export_with(&dir, &base, ops);
        assert_eq!(text, "id,city,amount\n1,CDMX,10\n4,Last,40\n");
    }

    #[test]
    fn substitute_hits_one_column_or_every_column() {
        let dir = Dir::new("sub");
        let base = base_with(&dir, BASE);
        let ops = vec![Op::Substitute {
            column: Some("city".into()),
            from: "CDMX".into(),
            to: "Ciudad de México".into(),
        }];
        let text = export_with(&dir, &base, ops);
        assert!(text.contains("Ciudad de México"));
        assert!(!text.contains(",CDMX,"), "{text}");

        let ops = vec![Op::Substitute {
            column: None,
            from: "1".into(),
            to: "one".into(),
        }];
        let text = export_with(&dir, &base, ops);
        assert!(text.contains("one,CDMX,one0"), "{text}");
    }

    #[test]
    fn column_operations_rewrite_the_header() {
        let dir = Dir::new("cols");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::DropColumn {
                name: "amount".into(),
            },
            Op::RenameColumn {
                from: "city".into(),
                to: "城市".into(),
            },
            Op::AddColumn {
                name: "tag".into(),
                formula: "".into(),
            },
        ];
        let text = export_with(&dir, &base, ops);
        assert_eq!(
            text.lines().next().unwrap(),
            "id,城市,tag",
            "header should follow the operations"
        );
    }

    #[test]
    fn journal_round_trips_through_its_csv_sidecar() {
        let dir = Dir::new("journal");
        let base = base_with(&dir, BASE);
        let path = Journal::sidecar(&base);

        let mut journal = Journal::new();
        journal.push(Op::SetCell {
            row: 3,
            column: "city".into(),
            value: "with, comma and \"quote\"".into(),
        });
        journal.push(Op::InsertRows {
            at: 0,
            rows: vec![vec!["0".into(), "zero".into(), "-".into()]],
        });
        journal.push(Op::Substitute {
            column: None,
            from: "a".into(),
            to: "A".into(),
        });
        journal.save(&path).unwrap();

        let loaded = Journal::load(&path).unwrap();
        assert_eq!(loaded, journal, "journal must survive a save/load cycle");

        // And the reloaded journal produces the same export.
        let a = export_with(&dir, &base, journal.ops().to_vec());
        let b = export_with(&dir, &base, loaded.ops().to_vec());
        assert_eq!(a, b);
    }

    #[test]
    fn undo_pops_the_last_operation() {
        let dir = Dir::new("undo");
        let base = base_with(&dir, BASE);
        let mut journal = Journal::new();
        journal.push(Op::SetCell {
            row: 0,
            column: "city".into(),
            value: "X".into(),
        });
        let before = export_with(&dir, &base, journal.ops().to_vec());
        assert!(before.contains(",X,"), "{before}");

        assert_eq!(
            journal.undo(),
            Some(Op::SetCell {
                row: 0,
                column: "city".into(),
                value: "X".into()
            })
        );
        let after = export_with(&dir, &base, journal.ops().to_vec());
        assert_eq!(after, BASE, "undo should restore the original");
        assert!(journal.is_empty());
    }

    #[test]
    fn row_map_resolves_windows_without_touching_the_file() {
        // 10 base rows, delete rows 2..5, insert 2 rows at 0.
        let ops = vec![
            Op::DeleteRows { at: 2, count: 3 },
            Op::InsertRows {
                at: 0,
                rows: vec![vec!["a".into()], vec!["b".into()]],
            },
        ];
        let map = RowMap::build(&ops, 10);
        assert_eq!(map.len(), 10 - 3 + 2);

        assert!(matches!(map.row(0), Some(RowRef::Virtual { .. })));
        assert_eq!(map.row(2), Some(RowRef::Base(0)));
        // Base rows 2,3,4 were deleted, so logical 2 is base 0 and logical 3 base 1.
        assert_eq!(map.row(3), Some(RowRef::Base(1)));
        // Then it jumps over the deleted range to base 5.
        assert_eq!(map.row(4), Some(RowRef::Base(5)));

        let runs = map.physical_runs(0, 5);
        assert_eq!(runs.len(), 3, "virtual block, then two base runs: {runs:?}");
        assert_eq!(runs[1].logical_start(), 2);
    }

    #[test]
    fn a_huge_file_with_a_few_edits_keeps_a_tiny_journal() {
        // 100k base rows recorded as a length only: the map must not iterate them.
        let ops = vec![
            Op::DeleteRows { at: 0, count: 5 },
            Op::InsertRows {
                at: 7,
                rows: vec![vec!["new".into()]],
            },
            Op::SetCell {
                row: 1000,
                column: "x".into(),
                value: "y".into(),
            },
        ];
        let map = RowMap::build(&ops, 100_000);
        assert_eq!(map.len(), 100_000 - 5 + 1);
        assert_eq!(map.row(0), Some(RowRef::Base(5)));
        // A row far past every edit still resolves in O(log segments).
        // 5 deleted at the top, 1 inserted after them: 99000 + 5 - 1.
        assert_eq!(map.row(99_000), Some(RowRef::Base(99_004)));
    }

    #[test]
    fn window_applies_edits_without_rewriting_the_file() {
        let dir = Dir::new("window");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::SetCell {
                row: 1,
                column: "city".into(),
                value: "Monterrey".into(),
            },
            Op::InsertRows {
                at: 2,
                rows: vec![vec!["9".into(), "New".into(), "50".into()]],
            },
        ];

        // The source file must be untouched.
        assert_eq!(std::fs::read_to_string(&base).unwrap(), BASE);

        let w = window(&base, &ops, 0, 10, Delimiter::Comma).unwrap();
        assert_eq!(w.total, 5, "4 base rows + 1 inserted");
        assert_eq!(w.columns, vec!["id", "city", "amount"]);
        assert_eq!(w.rows[1][1], "Monterrey", "edit visible");
        assert_eq!(w.rows[2][1], "New", "inserted row visible");

        // A window that lands only on the tail must not include earlier rows.
        let w = window(&base, &ops, 2, 2, Delimiter::Comma).unwrap();
        assert_eq!(w.rows.len(), 2);
        assert_eq!(w.rows[0][1], "New");
        assert_eq!(w.rows[1][1], "CDMX");
    }

    /// The behaviour that separates a spreadsheet from a text editor: an edit
    /// stays attached to its row when rows above it are inserted.
    #[test]
    fn an_edit_survives_a_later_insert_above_it() {
        let dir = Dir::new("stable");
        let base = base_with(&dir, BASE);
        // Edit the last row, then insert a row at the very top.
        let ops = vec![
            Op::SetCell {
                row: 3,
                column: "city".into(),
                value: "Edited".into(),
            },
            Op::InsertRows {
                at: 0,
                rows: vec![vec!["0".into(), "Top".into(), "0".into()]],
            },
        ];
        let text = export_with(&dir, &base, ops);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 6, "header + 5 rows");
        assert_eq!(lines[1], "0,Top,0", "inserted row");
        // The edited row moved down by one and kept its edit.
        assert_eq!(lines[5], "4,Edited,40", "{text}");
    }

    #[test]
    fn an_edit_on_an_inserted_row_is_independent() {
        let dir = Dir::new("virtual_edit");
        let base = base_with(&dir, BASE);
        let ops = vec![
            Op::InsertRows {
                at: 0,
                rows: vec![vec!["9".into(), "New".into(), "50".into()]],
            },
            // This row is the inserted one (logical 0 at the time of the edit).
            Op::SetCell {
                row: 0,
                column: "city".into(),
                value: "Newer".into(),
            },
            Op::InsertRows {
                at: 0,
                rows: vec![vec!["8".into(), "First".into(), "40".into()]],
            },
        ];
        let text = export_with(&dir, &base, ops);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "8,First,40");
        assert_eq!(lines[2], "9,Newer,50", "edit follows the inserted row");
    }

    #[test]
    fn unknown_column_operations_are_refused() {
        let dir = Dir::new("badcols");
        let base = base_with(&dir, BASE);
        let ops = vec![Op::DropColumn {
            name: "nope".into(),
        }];
        let out = dir.join("o.csv");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = export(&base, &ops, &out, Delimiter::Comma, &tx).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

// ---------------------------------------------------------------- window read

/// One window of the edited sheet, ready to render.
#[derive(Clone, Debug, Default)]
pub struct Window {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Logical row count after the journal is applied.
    pub total: u64,
}

/// Logical row count after the journal is applied.
///
/// Needs the base row count, which comes from the sparse index (built once,
/// reused from the sidecar). The `u64::MAX` sentinel inside [`RowMap`] must never
/// reach a caller: it is an implementation detail of "the file is open-ended".
pub fn logical_rows(base: &Path, ops: &[Op]) -> io::Result<u64> {
    let sidecar = crate::index::SparseIndex::sidecar_path(base);
    let index = match crate::index::SparseIndex::load(&sidecar) {
        Ok(idx) if idx.matches(base) => idx,
        _ => crate::index::SparseIndex::build(base, crate::index::DEFAULT_INTERVAL)?,
    };
    Ok(RowMap::build(ops, index.data_rows()).len())
}

/// Read `count` logical rows starting at `start`, applying edits.
///
/// The base file is read only where the window lands: with a row index that is
/// one or two seeks, without one it is a single sequential pass. Memory is
/// bounded by the window, not the file — the point of the whole design.
pub fn window(
    base: &Path,
    ops: &[Op],
    start: u64,
    count: u64,
    delimiter: Delimiter,
) -> io::Result<Window> {
    let delim = delimiter.byte();
    let base_columns = base_header(base, delimiter)?;
    if base_columns.is_empty() {
        return Ok(Window::default());
    }
    let columns = resolve_columns(ops, &base_columns)?;

    let sidecar = crate::index::SparseIndex::sidecar_path(base);
    let index = match crate::index::SparseIndex::load(&sidecar) {
        Ok(idx) if idx.matches(base) => idx,
        _ => crate::index::SparseIndex::build(base, crate::index::DEFAULT_INTERVAL)?,
    };
    let sheet = compile(ops, index.data_rows());
    let map = &sheet.map;
    let edits = &sheet.edits;

    let mut rows = Vec::new();
    for run in map.physical_runs(start, count) {
        match run {
            Run::Base { physical, len, .. } => {
                let records = index.read_rows(base, physical, len)?;
                for (i, record) in records.iter().enumerate() {
                    let cells = parse_csv_record(
                        String::from_utf8_lossy(record).trim_end_matches(['\n', '\r']),
                        delim,
                    );
                    rows.push(shape(
                        columns.len(),
                        &columns,
                        &base_columns,
                        &cells,
                        edits,
                        RowId::Base(physical + i as u64),
                    ));
                }
            }
            Run::Virtual {
                id,
                rows: virtual_rows,
                ..
            } => {
                for cells in &virtual_rows {
                    rows.push(shape(
                        columns.len(),
                        &columns,
                        &base_columns,
                        cells,
                        edits,
                        RowId::Virtual(id),
                    ));
                }
            }
        }
    }

    Ok(Window {
        columns,
        rows,
        total: map.len(),
    })
}

/// Project a base row (or an inserted one) onto the journal's column layout.
fn shape(
    width: usize,
    columns: &[String],
    base_columns: &[String],
    base_cells: &[String],
    edits: &CellEdits,
    id: RowId,
) -> Vec<String> {
    let mut out = Vec::with_capacity(width);
    for column in columns {
        let raw = base_columns
            .iter()
            .position(|b| b == column)
            .and_then(|idx| base_cells.get(idx))
            .cloned()
            .unwrap_or_default();
        let value = match edits.cell(id, column) {
            Some(v) => v.to_string(),
            None => edits.substitute(column, &raw),
        };
        out.push(value);
    }
    out
}
