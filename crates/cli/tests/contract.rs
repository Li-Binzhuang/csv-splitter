//! The advertised capability list must be the dispatch, not a copy of it.
//!
//! `EDIT_ACTIONS` is what `/capabilities` tells a client it can call. It is a hand-written
//! constant while the actions are actually handled by a `match` in `serve.rs`, so the two
//! can drift: add an arm and forget the constant, and a client will never learn the action
//! exists. This test reads the source of both and compares them, which is cheap and exact.

use std::collections::BTreeSet;

fn serve_source() -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/serve.rs"))
        .expect("serve.rs should be readable")
}

/// The `EDIT_ACTIONS` list, as the strings a client sees.
fn advertised_actions(source: &str) -> BTreeSet<String> {
    let start = source
        .find("const EDIT_ACTIONS: &[&str] = &[")
        .expect("EDIT_ACTIONS");
    let rest = &source[start..];
    let end = rest.find("];").expect("end of EDIT_ACTIONS");
    let block = &rest[..end];
    let mut actions = BTreeSet::new();
    for piece in block.split('"').skip(1).step_by(2) {
        let name = piece.trim();
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
            actions.insert(name.to_string());
        }
    }
    actions
}

/// The arm labels of the `match action.as_str()` block.
///
/// An alias arm (`"insert-column" | "insertcolumn"`) dispatches *both* spellings, so both
/// count. Three actions (`export`, `undo`, `redo`) are handled by branches that return
/// before this `match`; they are dispatched, just not here. The list is explicit rather
/// than inferred so that adding one requires thinking about whether it belongs there.
fn dispatched_actions(source: &str) -> BTreeSet<String> {
    let start = source
        .find("let op = match action.as_str() {")
        .expect("the action match");
    let rest = &source[start..];
    let mut actions = BTreeSet::new();
    for line in rest.lines().skip(1) {
        let trimmed = line.trim_start();
        // The block ends at the catch-all arm.
        if trimmed.starts_with("other =>") {
            break;
        }
        if !trimmed.starts_with('"') {
            continue;
        }
        let Some(label) = trimmed.split("=>").next() else {
            continue;
        };
        for piece in label.split('|') {
            let name = piece.trim().trim_matches('"').trim();
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                actions.insert(name.to_string());
            }
        }
    }
    actions
}

/// Actions served by the early branches in `edit_apply`, before the `match`.
const HANDLED_BEFORE_THE_MATCH: &[&str] = &["export", "undo", "redo"];

/// Squashed spellings accepted for convenience. They are real aliases, not capabilities a
/// client should be told about: `/capabilities` advertises the canonical name.
const ALIASES: &[&str] = &["deletewhere", "insertcolumn", "splitcolumn", "setformula"];

#[test]
fn every_advertised_edit_action_is_dispatched() {
    let source = serve_source();
    let advertised = advertised_actions(&source);
    let dispatched = dispatched_actions(&source);
    assert!(!advertised.is_empty() && !dispatched.is_empty());

    let missing: Vec<&String> = advertised
        .difference(&dispatched)
        .filter(|a| !HANDLED_BEFORE_THE_MATCH.contains(&a.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "/capabilities advertises actions that serve.rs does not handle: {missing:?}"
    );

    let unadvertised: Vec<&String> = dispatched
        .difference(&advertised)
        .filter(|a| !HANDLED_BEFORE_THE_MATCH.contains(&a.as_str()))
        .filter(|a| !ALIASES.contains(&a.as_str()))
        .collect();
    assert!(
        unadvertised.is_empty(),
        "serve.rs handles actions that /capabilities never mentions: {unadvertised:?}"
    );
}
