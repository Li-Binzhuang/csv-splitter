//! Row identity, the logical→physical map, and cell edits.

use super::*;
use crate::workbook::journal::{RowRef, Segment};

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
    /// Regex replacements: (column, pattern, replacement).
    regex_subs: Vec<(Option<String>, String, String)>,
    /// Bulk-edit rules: (filter expression, column, value).
    rules: Vec<(String, String, String)>,
    /// Pasted blocks: (row identity, anchor column, one row of values).
    blocks: Vec<(RowId, String, Vec<String>)>,
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

    /// Write a pasted block's values into this row, if this row is part of one.
    pub fn apply_blocks(&self, columns: &[String], id: RowId, cells: &mut [String]) {
        for (target, anchor, values) in &self.blocks {
            if *target != id {
                continue;
            }
            let Some(anchor_idx) = columns.iter().position(|c| c == anchor) else {
                continue;
            };
            for (offset, value) in values.iter().enumerate() {
                if let Some(cell) = cells.get_mut(anchor_idx + offset) {
                    *cell = value.clone();
                }
            }
        }
    }

    /// Apply bulk-edit rules to a whole row.
    ///
    /// Called after the row is assembled and before formulas run, so a rule can
    /// reference values from other columns and a formula can read the result.
    pub fn apply_rules(&self, columns: &[String], cells: &mut [String]) {
        for (filter, column, value) in &self.rules {
            let Ok(spec) = crate::filter::FilterSpec::parse(filter) else {
                continue;
            };
            let Some(target) = columns.iter().position(|c| c == column) else {
                continue;
            };
            let Some(source) = columns.iter().position(|c| c == &spec.column) else {
                continue;
            };
            let matches = cells
                .get(source)
                .map(|cell| crate::filter::matches(cell, &spec))
                .unwrap_or(false);
            if matches {
                cells[target] = value.clone();
            }
        }
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
        for (target, pattern, to) in &self.regex_subs {
            let applies = match target {
                None => true,
                Some(name) => name == column,
            };
            if applies {
                out = regex_replace(pattern, &out, to);
            }
        }
        out
    }
}

/// Replace every match of `pattern` in `value` with `to`.
///
/// The pattern uses the same subset as conditional formatting, which means no
/// capture groups: `to` is inserted verbatim. A pattern that does not compile
/// leaves the value untouched rather than failing a whole export.
pub fn regex_replace(pattern: &str, value: &str, to: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let chars: Vec<char> = value.chars().collect();
    let mut cursor = 0usize;
    while cursor <= chars.len() {
        match crate::format::find_span(pattern, value, cursor) {
            Some((start, end)) => {
                out.extend(chars[cursor..start].iter());
                out.push_str(to);
                cursor = end;
            }
            None => {
                out.extend(chars[cursor..].iter());
                break;
            }
        }
    }
    out
}

/// The output columns, each pointing at its base column (or `None` if computed).
///
/// This is the single place that understands renames and drops: matching an
/// output name against the *original* header is wrong as soon as a column is
/// renamed, which silently blanked every renamed column before this existed.
pub fn column_layout(
    ops: &[Op],
    base_columns: &[String],
) -> io::Result<Vec<(String, Option<usize>)>> {
    let mut layout: Vec<(String, Option<usize>)> = base_columns
        .iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), Some(i)))
        .collect();
    for op in ops {
        match op {
            Op::RenameColumn { from, to } => {
                let slot = layout
                    .iter_mut()
                    .find(|(name, _)| name == from)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("cannot rename: no column '{from}'"),
                        )
                    })?;
                slot.0 = to.clone();
            }
            Op::DropColumn { name } => {
                let idx = layout.iter().position(|(n, _)| n == name).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("cannot drop: no column '{name}'"),
                    )
                })?;
                layout.remove(idx);
                if layout.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "the journal drops every column",
                    ));
                }
            }
            Op::SplitColumn { column, into, .. } => {
                let at = layout
                    .iter()
                    .position(|(n, _)| n == column)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("cannot split: no column '{column}'"),
                        )
                    })?;
                // The pieces occupy the original slot, in order, each reading from
                // the same base column; the split itself happens per row.
                let base = layout[at].1;
                layout.splice(at..at + 1, into.iter().map(|name| (name.clone(), base)));
            }
            Op::AddColumn { name, .. } => layout.push((name.clone(), None)),
            Op::AddColumnAt { after, name, .. } => {
                let position = match after {
                    None => layout.len(),
                    Some(reference) => {
                        let index =
                            layout
                                .iter()
                                .position(|(n, _)| n == reference)
                                .ok_or_else(|| {
                                    io::Error::new(
                                        io::ErrorKind::InvalidInput,
                                        format!(
                                            "cannot insert after '{reference}': no such column"
                                        ),
                                    )
                                })?;
                        index + 1
                    }
                };
                layout.insert(position, (name.clone(), None));
            }
            _ => {}
        }
    }
    Ok(layout)
}

/// Every `AddColumn` operation, with its parsed formula.
///
/// Parsed once per sheet, not per row: a formula that fails to parse or reads a
/// column that does not exist is a configuration error, and should be reported
/// before a million rows are written.
pub fn formulas(ops: &[Op], columns: &[String]) -> io::Result<Vec<(String, Formula)>> {
    Ok(compile_formulas(ops, columns)?.items)
}

/// Parsed formulas plus every referenced sheet they need, loaded once.
pub struct CompiledFormulas {
    pub items: Vec<(String, Formula)>,
    pub refs: crate::formula::Refs,
}

/// Parse formulas and validate the columns they read.
///
/// Cross-sheet references are *not* loaded here: `formulas()` is called by code
/// that has no idea which directory the sheet lives in. Use
/// [`compile_formulas_at`] when a base path is available (export and window reads).
/// Rewrite a rename through formula text, so a formula keeps pointing at its column.
///
/// `rename qty → amount` happens after `formula total = qty*2` was written; without
/// this the formula keeps naming a column that no longer exists and the export fails.
/// Strings are left alone: only bare identifiers and `[name]` references are rewritten.
fn rename_identifiers(formula: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(formula.len());
    let mut rest = formula;
    let mut in_quotes = false;
    while !rest.is_empty() {
        let ch = rest.chars().next().expect("non-empty");
        if ch == '"' {
            in_quotes = !in_quotes;
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
            continue;
        }
        if !in_quotes {
            // `[name]` form.
            if ch == '[' {
                if let Some(end) = rest.find(']') {
                    let inner = &rest[1..end];
                    if inner == from {
                        out.push('[');
                        out.push_str(to);
                        out.push(']');
                        rest = &rest[end + 1..];
                        continue;
                    }
                }
            }
            // Bare identifier: a maximal run of name characters.
            if ch.is_alphanumeric() || ch == '_' {
                let token: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if token == from {
                    out.push_str(to);
                } else {
                    out.push_str(&token);
                }
                rest = &rest[token.len()..];
                continue;
            }
        }
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

pub fn compile_formulas(ops: &[Op], columns: &[String]) -> io::Result<CompiledFormulas> {
    let mut items = Vec::new();
    let mut wanted: Vec<(String, String, String)> = Vec::new();
    // Renames are column-level facts, so every formula sees the final names.
    let renames: Vec<(String, String)> = ops
        .iter()
        .filter_map(|op| match op {
            Op::RenameColumn { from, to } => Some((from.clone(), to.clone())),
            _ => None,
        })
        .collect();
    for op in ops {
        let (name, source) = match op {
            Op::AddColumn { name, formula } => (name, ColumnSource::Formula(formula.clone())),
            Op::AddColumnAt { name, source, .. } => (name, source.clone()),
            _ => continue,
        };
        if let ColumnSource::Formula(formula) = &source {
            let mut formula = formula.clone();
            for (from, to) in &renames {
                formula = rename_identifiers(&formula, from, to);
            }
            let formula = &formula;
            let parsed = Formula::parse(formula).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("column '{name}': {e}"))
            })?;
            for reference in parsed.references() {
                if !columns.iter().any(|c| c == &reference) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("column '{name}': formula reads unknown column '{reference}'"),
                    ));
                }
            }
            wanted.extend(parsed.sheet_references());
            items.push((name.clone(), parsed));
        }
    }
    let _ = wanted;
    Ok(CompiledFormulas {
        items,
        refs: crate::formula::Refs::default(),
    })
}

/// Same as [`compile_formulas`], and loads every referenced sheet from `base`'s
/// directory so evaluation never has to touch the disk mid-scan.
pub fn compile_formulas_at(
    base: &Path,
    ops: &[Op],
    columns: &[String],
) -> io::Result<CompiledFormulas> {
    let mut compiled = compile_formulas(ops, columns)?;
    let mut wanted: Vec<(String, String, String)> = Vec::new();
    for (_, formula) in &compiled.items {
        wanted.extend(formula.sheet_references());
    }
    if !wanted.is_empty() {
        let directory = base.parent().unwrap_or_else(|| Path::new("."));
        compiled.refs =
            crate::formula::Refs::load_for(directory, &wanted, crate::formula::DEFAULT_MAX_KEYS)?;
    }
    Ok(compiled)
}

/// Evaluate a computed column for one row.
fn eval_formula(
    formula: &Formula,
    columns: &[String],
    cells: &[String],
    refs: &crate::formula::Refs,
    row_number: u64,
    state: Option<&crate::formula::RowState>,
) -> String {
    let row: HashMap<String, String> = columns
        .iter()
        .zip(cells.iter())
        .map(|(c, v)| (c.clone(), v.clone()))
        .collect();
    formula
        .eval_stateful(&row, columns, refs, row_number, state)
        .render()
}

/// Parse an A1-style reference: `B3`, `AA10`.
///
/// Rows are 1-based and columns are letters, which is how people write them; the
/// result is 0-based on both axes so it can index the sheet directly.
pub fn parse_a1(cell: &str, columns: &[String]) -> Result<(u64, usize), String> {
    let text = cell.trim();
    let letters: String = text
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let digits: String = text
        .chars()
        .skip_while(|c| c.is_ascii_alphabetic())
        .collect();
    if letters.is_empty() || digits.is_empty() {
        return Err(format!("'{text}' is not a cell like B3"));
    }
    let mut column = 0usize;
    for ch in letters.to_ascii_uppercase().chars() {
        column = column * 26 + (ch as usize - 'A' as usize + 1);
    }
    let column = column.saturating_sub(1);
    if column >= columns.len() {
        return Err(format!(
            "'{text}' is beyond the last column ({} columns)",
            columns.len()
        ));
    }
    let row: u64 = digits
        .parse()
        .map_err(|_| format!("'{text}' has no row number"))?;
    if row == 0 {
        return Err("rows are 1-based; row 0 does not exist".to_string());
    }
    Ok((row - 1, column))
}

/// Parse a range `A2:C5` (or a single cell) into a rectangle.
pub fn parse_a1_range(range: &str, columns: &[String]) -> Result<(u64, usize, u64, usize), String> {
    let (start, end) = match range.split_once(':') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => {
            let (row, column) = parse_a1(range, columns)?;
            return Ok((row, column, row, column));
        }
    };
    let (start_row, start_col) = parse_a1(start, columns)?;
    let (end_row, end_col) = parse_a1(end, columns)?;
    if end_row < start_row || end_col < start_col {
        return Err(format!("'{range}' ends before it starts"));
    }
    Ok((start_row, start_col, end_row, end_col))
}

/// How each output column gets its value.
///
/// Computed once per export or window, never per row: a formula is parsed one time
/// and a literal is cloned one time, however many rows follow.
#[derive(Clone, Debug, Default)]
pub struct ColumnPlan {
    pub formulas: Vec<Option<Formula>>,
    pub literals: Vec<Option<String>>,
    /// Split pieces: (output index to fill, separator, piece number, source index).
    ///
    /// The source index is resolved in `build`, after the layout is known, so a
    /// split column can sit anywhere and be renamed or moved afterwards.
    pub splits: Vec<(usize, String, usize, usize)>,
}

impl ColumnPlan {
    /// `base_columns` is the file's original header and `columns` the output header
    /// after the layout ops. Splits need both: the pieces sit where the source used
    /// to be, and the source's name no longer exists among the outputs.
    /// Which piece of which base cell this output column is, if it is a split piece.
    pub fn split_piece(&self, target: usize) -> Option<(&str, usize, usize)> {
        self.splits
            .iter()
            .find(|(index, _, _, _)| *index == target)
            .map(|(_, by, piece, base)| (by.as_str(), *piece, *base))
    }

    pub fn build(
        ops: &[Op],
        base_columns: &[String],
        layout: &[(String, Option<usize>)],
        compiled: &CompiledFormulas,
    ) -> Self {
        let columns: Vec<String> = layout.iter().map(|(name, _)| name.clone()).collect();
        let formulas = columns
            .iter()
            .map(|c| {
                compiled
                    .items
                    .iter()
                    .find(|(n, _)| n == c)
                    .map(|(_, f)| f.clone())
            })
            .collect();
        let mut literals: Vec<Option<String>> = vec![None; columns.len()];
        for op in ops {
            if let Op::AddColumnAt {
                name,
                source: ColumnSource::Literal(value),
                ..
            } = op
            {
                if let Some(index) = columns.iter().position(|c| c == name) {
                    literals[index] = Some(value.clone());
                }
            }
        }
        // Which output column is the source of each split piece? Look the source
        // column up in the *final* layout: rows are projected onto it in order.
        let mut splits = Vec::new();
        for op in ops {
            let Op::SplitColumn { column, by, into } = op else {
                continue;
            };
            let Some(base_index) = base_columns.iter().position(|c| c == column) else {
                continue;
            };
            // Every output column reading from that base column is one of the pieces,
            // in layout order.
            let pieces: Vec<usize> = layout
                .iter()
                .enumerate()
                .filter(|(_, (_, base))| *base == Some(base_index))
                .map(|(index, _)| index)
                .collect();
            if pieces.is_empty() {
                continue;
            }
            let source = pieces[0];
            for (piece, target) in pieces.iter().enumerate().take(into.len()) {
                splits.push((*target, by.clone(), piece, source));
            }
        }
        Self {
            formulas,
            literals,
            splits,
        }
    }

    /// Columns the formulas need history for.
    pub fn row_needs(&self) -> crate::formula::RowNeeds {
        let mut needs = crate::formula::RowNeeds::default();
        for formula in self.formulas.iter().flatten() {
            needs.absorb(&formula.row_needs());
        }
        needs
    }

    /// Fill derived columns into a finished row.
    pub fn apply(
        &self,
        columns: &[String],
        cells: &mut [String],
        refs: &crate::formula::Refs,
        row_number: u64,
        state: Option<&crate::formula::RowState>,
    ) {
        // Pieces first: a formula may refer to a piece by name, and the formula run
        // below reads the cells in their current state. The last piece keeps the rest
        // of the value, so `a,b,c,d` split into three gives "a", "b", "c,d".
        // Split pieces are not applied here: they are derived from the *base* value and
        // an explicit edit to a piece has to survive, so `RowPipeline::row` computes
        // them while projecting. See `split_piece`.
        for (index, literal) in self.literals.iter().enumerate() {
            if let Some(value) = literal {
                if let Some(cell) = cells.get_mut(index) {
                    *cell = value.clone();
                }
            }
        }
        for (index, formula) in self.formulas.iter().enumerate() {
            if let Some(f) = formula {
                let rendered = eval_formula(f, columns, cells, refs, row_number, state);
                if let Some(cell) = cells.get_mut(index) {
                    *cell = rendered;
                }
            }
        }
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
    let mut regex_subs: Vec<(Option<String>, String, String)> = Vec::new();
    let mut rules: Vec<(String, String, String)> = Vec::new();
    let mut blocks: Vec<(RowId, String, Vec<String>)> = Vec::new();

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
            // A rename is metadata about a column, so edits that already name it move
            // with it. Without this, `set qty` followed by `rename qty → amount` drops
            // the edit silently — the value simply stops being there.
            Op::RenameColumn { from, to } => {
                let renamed: Vec<((RowId, String), String)> = cells
                    .iter()
                    .filter(|((_, column), _)| column == from)
                    .map(|(key, value)| ((key.0, to.clone()), value.clone()))
                    .collect();
                for (key, value) in renamed {
                    cells.insert(key, value);
                }
                cells.retain(|(_, column), _| column != from);
                for entry in &mut subs {
                    if entry.0.as_deref() == Some(from.as_str()) {
                        entry.0 = Some(to.clone());
                    }
                }
                for entry in &mut regex_subs {
                    if entry.0.as_deref() == Some(from.as_str()) {
                        entry.0 = Some(to.clone());
                    }
                }
                for entry in &mut rules {
                    if entry.1 == *from {
                        entry.1 = to.clone();
                    }
                }
            }
            Op::Substitute { column, from, to } => {
                subs.push((column.clone(), from.clone(), to.clone()));
            }
            Op::SubstituteRegex {
                column,
                pattern,
                to,
            } => {
                regex_subs.push((column.clone(), pattern.clone(), to.clone()));
            }
            Op::SetWhere {
                filter,
                column,
                value,
            } => rules.push((filter.clone(), column.clone(), value.clone())),
            Op::PasteBlock { row, column, rows } => {
                for (offset, values) in rows.iter().enumerate() {
                    // Resolve to a row identity now, so a later insert elsewhere
                    // moves the paste with the row it landed on.
                    if let Some(id) = map.id_at(row.saturating_add(offset as u64)) {
                        blocks.push((id, column.clone(), values.clone()));
                    }
                }
            }
            _ => {}
        }
    }

    Sheet {
        map,
        edits: CellEdits {
            cells,
            subs,
            regex_subs,
            rules,
            blocks,
        },
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
            Op::AddColumnAt { after, name, .. } => {
                let position = match after {
                    None => columns.len(),
                    Some(reference) => {
                        let index =
                            columns.iter().position(|c| c == reference).ok_or_else(|| {
                                io::Error::new(
                                    io::ErrorKind::InvalidInput,
                                    format!("cannot insert after '{reference}': no such column"),
                                )
                            })?;
                        index + 1
                    }
                };
                columns.insert(position, name.clone());
            }
            _ => {}
        }
    }
    Ok(columns)
}

#[cfg(test)]
mod split_tests {
    use super::*;

    #[test]
    fn a_column_can_be_split_into_several() {
        let base: Vec<String> = vec!["id".into(), "full".into()];
        let ops = vec![Op::SplitColumn {
            column: "full".to_string(),
            by: ",".to_string(),
            into: vec!["first".into(), "last".into(), "addr".into()],
        }];
        let layout = column_layout(&ops, &base).unwrap();
        let names: Vec<String> = layout.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, vec!["id", "first", "last", "addr"]);

        let compiled = compile_formulas(&ops, &names).unwrap();
        let plan = ColumnPlan::build(&ops, &base, &layout, &compiled);
        assert_eq!(plan.splits.len(), 3, "{:?}", plan.splits);

        // Pieces are rendered by the export pipeline (so an explicit edit can win); this
        // test pins the mapping it consults.
        assert_eq!(plan.split_piece(1), Some((",", 0, 1)));
        assert_eq!(plan.split_piece(2), Some((",", 1, 1)));
        assert_eq!(plan.split_piece(3), Some((",", 2, 1)));
        assert_eq!(plan.split_piece(0), None);
        let _ = names;
    }
}

#[cfg(test)]
mod rename_tests {
    use super::*;

    #[test]
    fn a_rename_rewrites_the_formula_but_not_the_strings() {
        assert_eq!(rename_identifiers("qty*2", "qty", "amount"), "amount*2");
        assert_eq!(rename_identifiers("[qty]+1", "qty", "amount"), "[amount]+1");
        // Text is text: a literal that happens to say `qty` stays as written.
        assert_eq!(
            rename_identifiers(r#"if(qty>5,"qty","no")"#, "qty", "amount"),
            r#"if(amount>5,"qty","no")"#
        );
        // Partial matches are not identifiers.
        assert_eq!(
            rename_identifiers("qty_total", "qty", "amount"),
            "qty_total"
        );
    }

    #[test]
    fn an_edit_survives_a_rename_of_its_column() {
        let ops = vec![
            Op::SetCell {
                row: 1,
                column: "qty".to_string(),
                value: "99".to_string(),
            },
            Op::RenameColumn {
                from: "qty".to_string(),
                to: "amount".to_string(),
            },
        ];
        let sheet = compile(&ops, 2);
        // The edit is keyed by column name, so the rename has to move it.
        assert_eq!(
            sheet
                .edits
                .cells
                .get(&(RowId::Base(1), "amount".to_string())),
            Some(&"99".to_string()),
            "{:?}",
            sheet.edits.cells
        );
        assert!(!sheet
            .edits
            .cells
            .contains_key(&(RowId::Base(1), "qty".to_string())));
    }
}
