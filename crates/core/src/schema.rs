//! Column-set comparison: what a second file adds, loses, or moved.
//!
//! This lives in the engine because two entry points print it — `csvx schema --diff`
//! and `GET /schema?diff=` — and two copies of "which column moved" eventually disagree
//! about what "same" means. The JSON produced here is the same document both callers
//! serve, so a client cannot tell which door it came through.

/// One column that exists in both files but at a different position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Moved {
    pub name: String,
    /// 1-based position here.
    pub here: usize,
    /// 1-based position in the other file.
    pub there: usize,
}

/// The difference between two column lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    /// No column is missing from either side.
    pub same: bool,
    /// Present here, absent there.
    pub removed: Vec<String>,
    /// Absent here, present there.
    pub added: Vec<String>,
    /// Present on both sides, at a different position.
    pub moved: Vec<Moved>,
}

impl Diff {
    pub fn to_json(&self) -> String {
        let list = |items: &[String]| {
            items
                .iter()
                .map(|c| crate::json::quote(c))
                .collect::<Vec<_>>()
                .join(",")
        };
        let moved: Vec<String> = self
            .moved
            .iter()
            .map(|m| {
                format!(
                    r#"{{"name":{},"here":{},"there":{}}}"#,
                    crate::json::quote(&m.name),
                    m.here,
                    m.there
                )
            })
            .collect();
        format!(
            r#"{{"same":{},"removed":[{}],"added":[{}],"moved":[{}]}}"#,
            self.same,
            list(&self.removed),
            list(&self.added),
            moved.join(",")
        )
    }
}

/// Compare two column lists.
///
/// `same` means "the set matches", not "the order matches": a file whose columns were
/// reordered has the same columns, and reporting it as different would make a routine
/// reorder look like a breaking schema change. Reordering is reported separately in
/// `moved`, which is the useful thing to show.
pub fn diff(mine: &[String], theirs: &[String]) -> Diff {
    let removed: Vec<String> = mine
        .iter()
        .filter(|c| !theirs.contains(c))
        .cloned()
        .collect();
    let added: Vec<String> = theirs
        .iter()
        .filter(|c| !mine.contains(c))
        .cloned()
        .collect();
    let moved: Vec<Moved> = mine
        .iter()
        .filter_map(|name| {
            let here = mine.iter().position(|c| c == name)? + 1;
            let there = theirs.iter().position(|c| c == name)? + 1;
            (here != there).then_some(Moved {
                name: name.clone(),
                here,
                there,
            })
        })
        .collect();
    Diff {
        same: removed.is_empty() && added.is_empty(),
        removed,
        added,
        moved,
    }
}
