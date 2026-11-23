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

/// An exclusive writer lock. Dropping it removes the lock file.
pub struct JournalLock {
    path: PathBuf,
}

impl JournalLock {
    /// Wait up to five seconds for a writer, treating a lock older than ten seconds as
    /// left behind by a killed process. The same policy as `update_locked` always had.
    pub fn acquire(sidecar: &Path) -> io::Result<Self> {
        let path = sidecar.with_extension("journal.lock");
        let started = std::time::Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if let Ok(meta) = std::fs::metadata(&path) {
                        if let Ok(modified) = meta.modified() {
                            if modified.elapsed().map(|d| d.as_secs()).unwrap_or(0) > 10 {
                                let _ = std::fs::remove_file(&path);
                                continue;
                            }
                        }
                    }
                    if started.elapsed() > std::time::Duration::from_secs(5) {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!("another writer holds {}; retry in a moment", path.display()),
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for JournalLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
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

    /// Where a transaction's group boundary is remembered: one line per batch, each
    /// holding the journal length after that batch.
    pub fn blocks_sidecar(base: &Path) -> PathBuf {
        PathBuf::from(format!("{}.blocks", Self::sidecar(base).display()))
    }

    /// Load the journal, or an empty one when the sidecar does not exist.
    ///
    /// The file is CSV so it can be read, diffed and hand-edited; a corrupt line
    /// is skipped with its number reported, because losing an undo step is better
    /// than refusing to open the sheet.
    pub fn load(path: &Path) -> io::Result<Self> {
        let ops = Self::read_ops(path)?;
        // The redo sidecar is derived from the journal's path, so callers keep
        // passing the same one path they always did.
        let redo = Self::read_ops(&path.with_extension("journal.redo"))?;
        Ok(Self::reconcile(ops, redo))
    }

    /// A save writes redo before the journal, so a crash in between leaves the undone
    /// operation in both files. Dropping it from the redo stack restores the state the
    /// save meant to reach; without this, the next redo would apply it twice.
    fn reconcile(ops: Vec<Op>, mut redo: Vec<Op>) -> Self {
        while let (Some(last_op), Some(last_redo)) = (ops.last(), redo.last()) {
            if format!("{last_op:?}") != format!("{last_redo:?}") {
                break;
            }
            redo.pop();
        }
        Self { ops, redo }
    }

    /// Load, and say how many records could not be read.
    ///
    /// A journal cut off mid-write loses its last operation — that is unavoidable — but
    /// it should not be lost *silently*: the caller can then tell the user that a step
    /// was dropped, which is the difference between "my edit vanished" and "the tool
    /// said the journal ended early".
    pub fn load_reporting(path: &Path) -> io::Result<(Self, usize, usize)> {
        let (ops, skipped_ops) = Self::read_ops_reporting(path)?;
        let (redo, skipped_redo) = Self::read_ops_reporting(&path.with_extension("journal.redo"))?;
        Ok((Self::reconcile(ops, redo), skipped_ops, skipped_redo))
    }

    fn read_ops_reporting(path: &Path) -> io::Result<(Vec<Op>, usize)> {
        if !path.exists() {
            return Ok((Vec::new(), 0));
        }
        let mut reader = RecordReader::new(
            BufReader::with_capacity(1024 * 1024, File::open(path)?),
            1024 * 1024,
        );
        let mut record = Vec::with_capacity(4096);
        let mut ops = Vec::new();
        let mut skipped = 0usize;
        while reader.next_record(&mut record)? {
            // Every operation is written with a trailing newline. A record without one was
            // cut off mid-write, and it can still *parse*: "set,1,city,CDM" is a valid
            // set-cell op, so applying it would write a truncated value instead of
            // reporting the lost edit. Requiring the terminator turns that into a drop.
            if !record.ends_with(b"\n") {
                skipped += 1;
                continue;
            }
            match parse_op(&record) {
                Some(op) => ops.push(op),
                None => skipped += 1,
            }
        }
        Ok((ops, skipped))
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
        let redo = path.with_extension("journal.redo");
        if self.redo.is_empty() {
            // Delete the redo file *before* writing the journal. In the other order a
            // crash between the two steps leaves a stale redo stack next to a journal that
            // already took a new edit, and the next redo would resurrect an operation the
            // user's new edit was supposed to discard.
            if redo.exists() {
                std::fs::remove_file(&redo)?;
            }
            return Self::write_ops(path, &self.ops);
        }
        // Write the redo first, then the journal: if the crash lands between them the
        // undone operation is in both files, and load() recognises that duplicate instead
        // of offering it back twice.
        Self::write_ops(&redo, &self.redo)?;
        Self::write_ops(path, &self.ops)
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

    /// Read-modify-write the journal while holding an exclusive lock.
    ///
    /// Two `edit` processes on the same file is a normal thing to do (a script and a
    /// person, or two tabs of a front-end). Without a lock both load the same ops,
    /// both append their own, and the last rename wins — one edit disappears with no
    /// error anywhere. The lock makes the read and the write one step, so a concurrent
    /// writer either waits or fails loudly.
    pub fn update_locked<F>(sidecar: &Path, apply: F) -> io::Result<Self>
    where
        F: FnOnce(&mut Journal) -> io::Result<()>,
    {
        let _lock = JournalLock::acquire(sidecar)?;
        let mut journal = Self::load(sidecar)?;
        apply(&mut journal)?;
        journal.save(sidecar)?;
        Ok(journal)
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

    /// The redo stack, for callers that render "undo this change" buttons.
    pub fn redo_ops(&self) -> &[Op] {
        &self.redo
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
        Op::FillColumns {
            columns,
            from,
            to,
            value,
        } => join_csv_line(
            &[
                op.tag(),
                &columns.join("|"),
                &from.to_string(),
                &to.to_string(),
                value,
            ],
            b',',
        ),
        Op::SetColumnFormula { column, formula } => {
            join_csv_line(&[op.tag(), column, formula], b',')
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
        "fillcols" if fields.len() >= 5 => Some(Op::FillColumns {
            columns: fields[1]
                .split('|')
                .map(str::to_string)
                .filter(|c| !c.is_empty())
                .collect(),
            from: fields[2].trim().parse().ok()?,
            to: fields[3].trim().parse().ok()?,
            value: fields[4].clone(),
        }),
        "setformula" if fields.len() >= 3 => Some(Op::SetColumnFormula {
            column: fields[1].clone(),
            formula: fields[2].clone(),
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

/// What a journal repair did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepairSummary {
    /// Records that could be read and were written back.
    pub kept: usize,
    /// Records that could not be read and were dropped.
    pub dropped: usize,
    /// Copy of the journal as it was before the rewrite.
    pub backup: Option<std::path::PathBuf>,
    /// True when a dry run only counted; nothing was written.
    pub dry_run: bool,
}

/// Rewrite a damaged journal down to its readable records, keeping a copy first.
///
/// Reading already skips unreadable records, so this is not about making the sheet
/// readable — it is about making the file *tidy* again, on purpose, with a backup. A
/// journal that keeps a truncated tail forever will keep reporting damage forever.
pub fn repair(sidecar: &Path, dry_run: bool) -> io::Result<RepairSummary> {
    if !sidecar.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no journal at {}", sidecar.display()),
        ));
    }
    let (journal, dropped, _) = {
        let (ops, skipped) = Journal::read_ops_reporting(sidecar)?;
        (ops, skipped, 0)
    };
    let mut summary = RepairSummary {
        kept: journal.len(),
        dropped,
        backup: None,
        dry_run,
    };
    if dry_run || dropped == 0 {
        return Ok(summary);
    }

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = sidecar.with_extension(format!("journal.bak-{stamp}"));
    std::fs::copy(sidecar, &backup)?;

    let mut body = String::new();
    for op in &journal {
        // `render_op` already ends with the record's newline; adding one more would
        // double-space the file and make the next load skip every other record.
        body.push_str(&render_op(op));
    }
    let temporary = sidecar.with_extension(format!("journal.tmp-{stamp}"));
    std::fs::write(&temporary, body)?;
    std::fs::rename(&temporary, sidecar)?;
    summary.backup = Some(backup);
    Ok(summary)
}

/// Remember that a transaction of `size` operations ends at journal length `end`.
///
/// Both numbers are stored: `end` says whether the marker still describes this journal,
/// and `size` says how many operations the transaction added, which is what one undo
/// must take back together.
pub fn record_block(sidecar: &Path, end: usize, size: usize) -> io::Result<()> {
    record_marker(&undo_marker(sidecar), end, size)
}

/// Symmetry for redo: after a block undo the redo stack holds the whole batch, and one
/// redo should put it all back.
pub fn record_redo_block(sidecar: &Path, end: usize, size: usize) -> io::Result<()> {
    record_marker(&redo_marker(sidecar), end, size)
}

/// A new single edit makes any remembered transaction boundary meaningless; it also
/// clears the redo stack, so both markers go.
pub fn clear_blocks(sidecar: &Path) {
    let _ = std::fs::remove_file(undo_marker(sidecar));
    let _ = std::fs::remove_file(redo_marker(sidecar));
}

/// If the journal still ends on a transaction boundary, consume it and report how many
/// operations to undo together. A stale marker (the journal grew or shrank since) is
/// dropped rather than confused with a real one.
pub fn take_block(sidecar: &Path, current_len: usize) -> io::Result<Option<usize>> {
    take_marker(&undo_marker(sidecar), current_len)
}

pub fn take_redo_block(sidecar: &Path, current_len: usize) -> io::Result<Option<usize>> {
    take_marker(&redo_marker(sidecar), current_len)
}

fn undo_marker(sidecar: &Path) -> std::path::PathBuf {
    Journal::blocks_sidecar(sidecar)
}

fn redo_marker(sidecar: &Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!(
        "{}.redo.blocks",
        Journal::sidecar(sidecar).display()
    ))
}

fn record_marker(path: &Path, end: usize, size: usize) -> io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{end},{size}")
}

fn take_marker(path: &Path, current_len: usize) -> io::Result<Option<usize>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let mut marks: Vec<(usize, usize)> = text
        .lines()
        .filter_map(|line| {
            let (end, size) = line.trim().split_once(',')?;
            Some((end.parse().ok()?, size.parse().ok()?))
        })
        .collect();
    match marks.pop() {
        Some((end, size)) if end == current_len && size > 0 => {
            if marks.is_empty() {
                let _ = std::fs::remove_file(path);
            } else {
                std::fs::write(
                    path,
                    marks
                        .iter()
                        .map(|(e, s)| format!("{e},{s}\n"))
                        .collect::<String>(),
                )?;
            }
            Ok(Some(size))
        }
        _ => {
            let _ = std::fs::remove_file(path);
            Ok(None)
        }
    }
}

fn crash_point(name: &str) {
    if std::env::var("CSVX_COMPACT_CRASH_AT").ok().as_deref() == Some(name) {
        std::process::abort();
    }
}

/// Summary of one explicit journal compaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactSummary {
    pub operations: usize,
    pub bytes: u64,
    pub base_archive: PathBuf,
}

fn archive_path(base: &Path) -> PathBuf {
    PathBuf::from(format!("{}.base", base.display()))
}
fn history_path(sidecar: &Path) -> PathBuf {
    PathBuf::from(format!("{}.history", sidecar.display()))
}
fn history_blocks_path(sidecar: &Path) -> PathBuf {
    PathBuf::from(format!("{}.blocks.history", sidecar.display()))
}
fn prepared_marker(base: &Path) -> PathBuf {
    PathBuf::from(format!("{}.compact.prepared", base.display()))
}
fn installed_marker(base: &Path) -> PathBuf {
    PathBuf::from(format!("{}.compact.installed", base.display()))
}
fn snapshot_tmp(base: &Path) -> PathBuf {
    PathBuf::from(format!("{}.compact.tmp", base.display()))
}

/// Fold the current journal into a snapshot at `base`, keeping the original bytes in
/// `base.base` and the old journal in `base.journal.history` so undo can cross the
/// boundary. One level only; refuses when a compaction or redo branch is already there.
pub fn compact(
    base: &Path,
    delimiter: Delimiter,
    tx: &ProgressSender,
) -> io::Result<CompactSummary> {
    let sidecar = Journal::sidecar(base);
    let _lock = JournalLock::acquire(&sidecar)?;
    let journal = Journal::load(&sidecar)?;
    if journal.redo_len() > 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cannot compact while a redo branch exists; clear it first",
        ));
    }
    if journal.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the journal is already empty",
        ));
    }
    let archive = archive_path(base);
    let history = history_path(&sidecar);
    if archive.exists() || history.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "this file already has a compacted snapshot",
        ));
    }
    let tmp = snapshot_tmp(base);
    let _ = std::fs::remove_file(&tmp);
    let summary = super::export(base, &journal.ops, &tmp, delimiter, tx)?;
    std::fs::copy(base, &archive)?;
    std::fs::copy(&sidecar, &history)?;
    crash_point("after_copies");
    let blocks = Journal::blocks_sidecar(base);
    let blocks_history = history_blocks_path(&sidecar);
    if blocks.exists() {
        std::fs::copy(&blocks, &blocks_history)?;
    }
    std::fs::write(prepared_marker(base), b"prepared\n")?;
    crash_point("after_prepared");
    std::fs::rename(&tmp, base)?;
    crash_point("after_rename");
    std::fs::write(installed_marker(base), b"installed\n")?;
    crash_point("after_install");
    Journal::new().save(&sidecar)?;
    if blocks.exists() {
        std::fs::remove_file(&blocks)?;
    }
    let _ = std::fs::remove_file(prepared_marker(base));
    let _ = std::fs::remove_file(installed_marker(base));
    Ok(CompactSummary {
        operations: journal.len(),
        bytes: summary.bytes,
        base_archive: archive,
    })
}

/// Finish or roll back a compaction interrupted between its file renames. Called
/// before a journal is loaded, so a crash cannot apply the old journal to a snapshot.
pub fn recover_compaction(base: &Path) -> io::Result<()> {
    let sidecar = Journal::sidecar(base);
    let prepared = prepared_marker(base);
    let installed = installed_marker(base);
    let tmp = snapshot_tmp(base);
    if installed.exists() {
        // The snapshot is in place; the old journal must not replay on top of it.
        Journal::new().save(&sidecar)?;
        let blocks = Journal::blocks_sidecar(base);
        let _ = std::fs::remove_file(blocks);
        let _ = std::fs::remove_file(&prepared);
        let _ = std::fs::remove_file(&installed);
        return Ok(());
    }
    if prepared.exists() {
        if tmp.exists() {
            // The snapshot was never installed: drop the copies, keep the sheet.
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_file(archive_path(base));
            let _ = std::fs::remove_file(history_path(&sidecar));
            let _ = std::fs::remove_file(history_blocks_path(&sidecar));
            let _ = std::fs::remove_file(&prepared);
        } else {
            // Renamed but the installed marker never landed: finish the compaction.
            Journal::new().save(&sidecar)?;
            let blocks = Journal::blocks_sidecar(base);
            let _ = std::fs::remove_file(blocks);
            let _ = std::fs::remove_file(&prepared);
        }
        return Ok(());
    }
    // No marker at all: copies left by a crash before the marker are stale while the
    // journal still has operations. An empty journal means a finished compaction.
    let archive = archive_path(base);
    let history = history_path(&sidecar);
    if archive.exists() && history.exists() {
        let journal = Journal::load(&sidecar)?;
        if !journal.is_empty() {
            let _ = std::fs::remove_file(&archive);
            let _ = std::fs::remove_file(&history);
            let _ = std::fs::remove_file(history_blocks_path(&sidecar));
            let _ = std::fs::remove_file(snapshot_tmp(base));
        }
    }
    Ok(())
}

/// Put the pre-compaction base and journal back so undo can cross the boundary.
/// Returns false when there is nothing to restore, and refuses while a redo branch
/// exists (that branch belongs to the compacted state).
pub fn restore_compacted(base: &Path) -> io::Result<bool> {
    let sidecar = Journal::sidecar(base);
    let archive = archive_path(base);
    let history = history_path(&sidecar);
    if !archive.exists() || !history.exists() {
        return Ok(false);
    }
    let journal = Journal::load(&sidecar)?;
    if !journal.is_empty() {
        return Ok(false);
    }
    if journal.redo_len() > 0 {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "cannot undo past a compacted snapshot while a redo branch exists",
        ));
    }
    std::fs::remove_file(base)?;
    std::fs::rename(&archive, base)?;
    std::fs::rename(&history, &sidecar)?;
    let blocks = Journal::blocks_sidecar(base);
    let blocks_history = history_blocks_path(&sidecar);
    if blocks_history.exists() {
        let _ = std::fs::remove_file(&blocks);
        std::fs::rename(&blocks_history, &blocks)?;
    }
    let _ = std::fs::remove_file(prepared_marker(base));
    let _ = std::fs::remove_file(installed_marker(base));
    let _ = std::fs::remove_file(snapshot_tmp(base));
    Ok(true)
}

#[cfg(test)]
mod damaged_tests {
    use super::Journal;

    /// A journal cut off mid-write keeps the edits that were complete, drops the broken
    /// record, and says how many it dropped. The file itself is never rewritten here:
    /// reporting is the contract, repair is a separate decision.
    #[test]
    fn a_truncated_record_is_counted_not_silently_dropped() {
        let dir = std::env::temp_dir().join(format!("csvs_journal_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.csv.journal");
        std::fs::write(&path, "set,1,city,CDMX\nset,2,amount,10\nset,3,note").unwrap();
        std::fs::write(dir.join("data.csv.journal.redo"), "set,4,city,Puebla\nset").unwrap();

        let (journal, skipped, skipped_redo) = Journal::load_reporting(&path).unwrap();
        assert_eq!(journal.len(), 2, "both complete records must survive");
        assert_eq!(skipped, 1, "the truncated record must be reported");
        assert_eq!(skipped_redo, 1, "the redo sidecar gets the same treatment");

        // Loading without reporting still applies the readable operations, so a damaged
        // journal degrades the sheet instead of hiding every edit.
        let plain = Journal::load(&path).unwrap();
        assert_eq!(plain.len(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Repair keeps the readable records, drops the truncated one, and never touches the
    /// original without leaving a copy behind.
    #[test]
    fn repair_rewrites_only_the_damaged_tail() {
        let dir = std::env::temp_dir().join(format!("csvs_journal_repair_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.csv.journal");
        let original = "set,1,city,CDMX\nset,2,amount,10\nset,3,note";
        std::fs::write(&path, original).unwrap();

        let summary = super::repair(&path, true).unwrap();
        assert_eq!(summary.kept, 2);
        assert_eq!(summary.dropped, 1);
        assert!(summary.dry_run);
        assert!(summary.backup.is_none(), "a dry run writes nothing");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        let summary = super::repair(&path, false).unwrap();
        assert_eq!(summary.kept, 2);
        let backup = summary.backup.expect("a real repair leaves a copy");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
        let repaired = std::fs::read_to_string(&path).unwrap();
        assert_eq!(repaired.lines().count(), 2);
        assert!(repaired.starts_with("set,1,city,CDMX\n"));

        // A repaired journal loads with nothing left to report.
        let (journal, skipped, _) = super::Journal::load_reporting(&path).unwrap();
        assert_eq!(journal.len(), 2);
        assert_eq!(skipped, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A cut can land anywhere, including inside a value where the fragment still parses as
    /// a complete operation ("set,1,city,CDM"). Every cut must lose exactly the unfinished
    /// record, never apply half of it.
    #[test]
    fn any_cut_point_loses_only_the_unfinished_record() {
        let dir = std::env::temp_dir().join(format!("csvs_journal_cuts_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.csv.journal");
        let head = "set,1,city,CDMX\nset,2,amount,10\n";
        let tail = "set,3,note,refund issued\n";

        for cut in 0..=tail.len() {
            let content = format!("{head}{}", &tail[..cut]);
            std::fs::write(&path, &content).unwrap();
            let (journal, skipped, _) = super::Journal::load_reporting(&path).unwrap();
            // A cut exactly at the record's terminator leaves a complete record, not a tear.
            let complete = cut == tail.len();
            let expected_ops = if complete { 3 } else { 2 };
            assert_eq!(
                journal.len(),
                expected_ops,
                "cut at {cut}: readable ops changed"
            );
            let expected = if cut == 0 || complete { 0 } else { 1 };
            assert_eq!(skipped, expected, "cut at {cut}: skipped count");
        }

        // A torn value must not survive as a shorter value.
        let torn = format!("{head}set,3,note,CDM");
        std::fs::write(&path, &torn).unwrap();
        let (journal, skipped, _) = super::Journal::load_reporting(&path).unwrap();
        assert_eq!((journal.len(), skipped), (2, 1));
        assert!(
            !journal
                .ops()
                .iter()
                .any(|op| format!("{op:?}").contains("CDM\"")),
            "a truncated value must not be applied: {journal:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two files cannot be renamed atomically together, so the save order defines what a
    /// crash can leave behind. Crash *between* redo and journal: the undone op is in both.
    /// Loading must reconcile it, not hand it back twice.
    #[test]
    fn a_crash_between_the_two_files_does_not_double_the_undo() {
        let dir = std::env::temp_dir().join(format!("csvs_journal_pair_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.csv.journal");
        let redo = dir.join("data.csv.journal.redo");
        let head = "set,1,city,CDMX\n";
        let undone = "set,2,amount,10\n";
        // State a crash can produce: journal and redo both hold the undone operation.
        std::fs::write(&path, format!("{head}{undone}")).unwrap();
        std::fs::write(&redo, undone).unwrap();

        let journal = super::Journal::load(&path).unwrap();
        assert_eq!(journal.len(), 2, "the journal is untouched");
        assert!(
            journal.redo_ops().is_empty(),
            "the duplicate must not come back as a redo: {:?}",
            journal.redo_ops()
        );

        // A genuinely undone operation (not in the journal) still offers itself back.
        std::fs::write(&path, head).unwrap();
        std::fs::write(&redo, undone).unwrap();
        let journal = super::Journal::load(&path).unwrap();
        assert_eq!(journal.redo_len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The lock exists because two writers appending without it lose an edit silently.
    /// This runs eight writers against one journal and asserts every edit survives.
    #[test]
    fn concurrent_writers_do_not_lose_an_edit() {
        let dir = std::env::temp_dir().join(format!("csvs_journal_lock_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sidecar = dir.join("data.csv.journal");

        let mut handles = Vec::new();
        for worker in 0..8u64 {
            let sidecar = sidecar.clone();
            handles.push(std::thread::spawn(move || {
                super::Journal::update_locked(&sidecar, |journal| {
                    journal.push(super::Op::SetCell {
                        row: worker,
                        column: "city".to_string(),
                        value: format!("C{worker}"),
                    });
                    Ok(())
                })
                .map(|_| ())
            }));
        }
        for handle in handles {
            handle.join().unwrap().expect("a writer failed");
        }

        let journal = super::Journal::load(&sidecar).unwrap();
        assert_eq!(journal.len(), 8, "an edit was lost: {journal:?}");
        for worker in 0..8u64 {
            let found = journal.ops().iter().any(|op| match op {
                super::Op::SetCell { row, value, .. } => {
                    *row == worker && value == &format!("C{worker}")
                }
                _ => false,
            });
            assert!(found, "edit {worker} is missing: {journal:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod compact_tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "csvs_compact_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn edited_journal() -> Journal {
        Journal {
            ops: vec![
                Op::SetCell {
                    row: 0,
                    column: "city".into(),
                    value: "Tijuana".into(),
                },
                Op::SetCell {
                    row: 1,
                    column: "amount".into(),
                    value: "99".into(),
                },
            ],
            redo: Vec::new(),
        }
    }

    #[test]
    fn compact_then_restore_preserves_the_snapshot_and_history() {
        let d = dir("round");
        let base = d.join("b.csv");
        std::fs::write(&base, "id,city,amount\n1,CDMX,10\n2,Puebla,20\n").unwrap();
        let sidecar = Journal::sidecar(&base);
        edited_journal().save(&sidecar).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let summary = compact(&base, Delimiter::Comma, &tx).unwrap();
        assert_eq!(summary.operations, 2);
        let snapshot = std::fs::read_to_string(&base).unwrap();
        assert!(snapshot.contains("1,Tijuana,10"), "{snapshot}");
        assert!(snapshot.contains("2,Puebla,99"), "{snapshot}");
        assert!(Journal::load(&sidecar).unwrap().is_empty());
        assert!(archive_path(&base).exists());
        assert!(history_path(&sidecar).exists());
        assert!(restore_compacted(&base).unwrap());
        assert_eq!(
            std::fs::read_to_string(&base).unwrap(),
            "id,city,amount\n1,CDMX,10\n2,Puebla,20\n"
        );
        assert_eq!(Journal::load(&sidecar).unwrap().len(), 2);
        assert!(!archive_path(&base).exists());
        assert!(!history_path(&sidecar).exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn compact_refuses_a_second_level_and_leaves_the_sheet_untouched() {
        let d = dir("occupied");
        let base = d.join("b.csv");
        std::fs::write(&base, "id,city,amount\n1,CDMX,10\n2,Puebla,20\n").unwrap();
        let sidecar = Journal::sidecar(&base);
        edited_journal().save(&sidecar).unwrap();
        std::fs::create_dir(archive_path(&base)).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let error = compact(&base, Delimiter::Comma, &tx).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(&base).unwrap(),
            "id,city,amount\n1,CDMX,10\n2,Puebla,20\n"
        );
        assert_eq!(Journal::load(&sidecar).unwrap().len(), 2);
        assert!(!snapshot_tmp(&base).exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_writer_waits_for_the_journal_lock() {
        let d = dir("lock");
        let base = d.join("b.csv");
        std::fs::write(&base, "id\n1\n").unwrap();
        let sidecar = Journal::sidecar(&base);
        let held = JournalLock::acquire(&sidecar).unwrap();
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        let path = sidecar.clone();
        let handle = std::thread::spawn(move || {
            Journal::update_locked(&path, |journal| {
                journal.push(Op::SetCell {
                    row: 0,
                    column: "id".into(),
                    value: "2".into(),
                });
                Ok(())
            })
            .unwrap();
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(
            !done.load(std::sync::atomic::Ordering::SeqCst),
            "a second writer must wait for the lock"
        );
        drop(held);
        handle.join().unwrap();
        assert_eq!(Journal::load(&sidecar).unwrap().len(), 1);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn recovery_rolls_back_when_the_snapshot_was_never_installed() {
        let d = dir("rollback");
        let base = d.join("b.csv");
        std::fs::write(&base, "id,city\n1,CDMX\n2,Puebla\n").unwrap();
        let sidecar = Journal::sidecar(&base);
        edited_journal().save(&sidecar).unwrap();
        std::fs::copy(&base, archive_path(&base)).unwrap();
        std::fs::copy(&sidecar, history_path(&sidecar)).unwrap();
        std::fs::write(snapshot_tmp(&base), "id,city\n1,Tijuana\n").unwrap();
        std::fs::write(prepared_marker(&base), b"prepared\n").unwrap();
        recover_compaction(&base).unwrap();
        assert_eq!(Journal::load(&sidecar).unwrap().len(), 2);
        assert!(!snapshot_tmp(&base).exists());
        assert!(!archive_path(&base).exists());
        assert!(!history_path(&sidecar).exists());
        assert!(!prepared_marker(&base).exists());
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn recovery_finishes_when_the_snapshot_was_installed() {
        let d = dir("installed");
        let base = d.join("b.csv");
        std::fs::write(&base, "id,city\n1,Tijuana\n2,Puebla\n").unwrap();
        let sidecar = Journal::sidecar(&base);
        edited_journal().save(&sidecar).unwrap();
        std::fs::copy(&base, archive_path(&base)).unwrap();
        std::fs::copy(&sidecar, history_path(&sidecar)).unwrap();
        std::fs::write(prepared_marker(&base), b"prepared\n").unwrap();
        recover_compaction(&base).unwrap();
        assert!(Journal::load(&sidecar).unwrap().is_empty());
        assert!(!prepared_marker(&base).exists());
        assert!(!installed_marker(&base).exists());
        std::fs::remove_dir_all(&d).ok();
    }
}
