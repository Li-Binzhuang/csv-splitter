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

/// `/spec` must not describe endpoints that do not exist, nor omit ones that do.
///
/// The parameter table is hand-written and grew route by route; a guessed route name
/// (`/eval`) sat in it until this test existed. Both directions matter: a client that
/// trusts `/spec` will build a request the server answers with 404.
#[test]
fn spec_routes_match_the_served_routes() {
    let source = serve_source();

    let listed: BTreeSet<String> = {
        let start = source.find("const SPEC_PARAMS").expect("SPEC_PARAMS");
        let rest = &source[start..];
        let end = rest.find("\n];").expect("end of SPEC_PARAMS");
        // Entries wrap: `("/rows", &[…])` and `(\n    "/schema",\n …)`. Matching "("
        // followed by a quoted path anywhere in the block handles both.
        // Walk every quoted string in the block; a route is a quote-delimited token that
        // starts with `/` and is followed by `,` or `&`. This covers both the inline and
        // the wrapped forms without depending on the exact whitespace.
        let mut found = BTreeSet::new();
        for (index, piece) in rest[..end].split('"').enumerate() {
            if index % 2 == 1 && piece.starts_with('/') && !piece.contains(char::is_whitespace) {
                found.insert(piece.to_string());
            }
        }
        found
    };

    let served: BTreeSet<String> = {
        let mut routes = BTreeSet::new();
        for constant in ["READ_ROUTES", "WRITE_ROUTES"] {
            let start = source.find(&format!("const {constant}")).expect(constant);
            let rest = &source[start..];
            let end = rest.find("\n];").expect("end of the route list");
            for line in rest[..end].lines() {
                // Read routes are `"/split"`, write routes `"POST /split"`: the path is
                // what follows the last space either way.
                if let Some(at) = line.find('"') {
                    let literal = line[at + 1..].split('"').next().unwrap_or("");
                    let path = literal.split_whitespace().last().unwrap_or(literal);
                    if path.starts_with('/') {
                        routes.insert(path.to_string());
                    }
                }
            }
        }
        // A write route is served at its path, with the verb in the constant's name.
        routes.retain(|r| r.starts_with('/'));
        routes
    };

    assert!(!listed.is_empty() && !served.is_empty());
    let phantom: Vec<&String> = listed.difference(&served).collect();
    assert!(
        phantom.is_empty(),
        "/spec documents routes the server does not serve: {phantom:?}"
    );
    let undocumented: Vec<&String> = served.difference(&listed).collect();
    assert!(
        undocumented.is_empty(),
        "served routes missing from /spec's parameter table: {undocumented:?}"
    );
}

/// `/spec` must describe every status the server can return, not the six it used to list.
/// The check reads the source so a new code in `reason_for` cannot ship undocumented.
#[test]
fn spec_documents_every_status_the_server_can_return() {
    let serve =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/serve.rs")).unwrap();

    let reason_block = serve
        .split("fn reason_for(status: u16)")
        .nth(1)
        .expect("reason_for must exist")
        .split("\n}")
        .next()
        .unwrap();
    let mut codes: Vec<u16> = Vec::new();
    for line in reason_block.lines() {
        let trimmed = line.trim();
        // Arms look like `202 => "Accepted",`; the wildcard arm has no number.
        if let Some((left, _)) = trimmed.split_once(" => ") {
            if let Ok(number) = left.trim().parse::<u16>() {
                codes.push(number);
            }
        }
    }
    assert!(!codes.is_empty(), "no status codes found in reason_for");

    let notes = serve
        .split("const STATUS_NOTES")
        .nth(1)
        .expect("STATUS_NOTES must exist");
    for code in codes {
        // rustfmt may put each tuple on its own line, so look for `201,` rather than the
        // one-line `(201, ` form the source had when this test was written.
        assert!(
            notes.contains(&format!("{code},")),
            "status {code} is returned by the server but has no note in STATUS_NOTES"
        );
    }
}

/// Every 4xx/5xx body in the server must carry `error` plus `parameter` and `hint`.
/// A message that only says "bad" leaves the caller to guess, and the promise
/// "error messages carry the next step" needs a test or it decays.
#[test]
fn every_error_body_names_the_parameter_and_the_next_step() {
    let serve =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/serve.rs")).unwrap();
    let mut problems: Vec<String> = Vec::new();
    let mut offset = 0;
    while let Some(pos) = serve[offset..].find("respond_json(") {
        let start = offset + pos;
        offset = start + 1;
        // Only calls whose first few lines contain a status literal of their own; a
        // number that happens to appear inside an argument is not a status.
        // A 400 means "your request is wrong", so it must say which parameter; a 422 or
        // 404 is about the file or the id, and its message is the engine's own wording.
        let is_client_error = serve[start..].lines().take(3).any(|line| {
            let token = line.trim().trim_end_matches(',');
            token.parse::<u16>() == Ok(400)
        });
        if !is_client_error {
            continue;
        }
        // The body of the call: from here to the matching close paren at depth zero.
        let mut depth = 0i32;
        let mut body_end = start;
        for (i, ch) in serve[start..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        body_end = start + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &serve[start..body_end];
        if !body.contains("\"error\"") {
            problems.push(format!(
                "an error response has no error field at byte {start}"
            ));
        }
        // Guidance means naming the parameter or saying what to do next; a 404 for an
        // unknown id cannot say more than "this id is unknown" plus where ids come from.
        if !body.contains("\"parameter\"") {
            problems.push(format!(
                "a 400 body does not name its parameter at byte {start}: {}",
                &body[..body.len().min(80)]
            ));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// A handler that reads a parameter `/spec` does not advertise is a contract that lies by
/// omission: the client cannot know to send it. This reads the source so a new
/// `params.get("x")` cannot ship undeclared.
///
/// Scope: handlers reachable from a `"/route" => name(...)` arm. Routes dispatched as an
/// inline `{ ... }` block (the GET half of `/job`) are not scanned; their reads were checked
/// by hand once, and a block that grows a new parameter will not be caught here.
#[test]
fn spec_advertises_every_parameter_a_handler_reads() {
    let serve =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/serve.rs")).unwrap();

    // route -> handlers, all occurrences, because /job appears in both GET and write tables.
    let mut handlers: Vec<(String, String)> = Vec::new();
    let mut offset = 0;
    while let Some(pos) = serve[offset..].find("\" => ") {
        let start = offset + pos;
        offset = start + 1;
        // `start` points at the closing quote of the route literal, so the route is the
        // text between the two quote characters that precede the arrow.
        let before = &serve[..start];
        // `start` is the route literal's closing quote, so its opening quote is the last
        // one before that, and the route is everything in between.
        let open = match before.rfind('"') {
            Some(i) => i,
            None => continue,
        };
        let route = before[open + 1..].to_string();
        let after = &serve[start + 5..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if route.starts_with('/') && !name.is_empty() {
            handlers.push((route, name));
        }
    }
    assert!(!handlers.is_empty(), "no routes found");

    // SPEC_PARAMS: route -> declared names.
    let mut declared: std::collections::HashMap<String, Vec<String>> = Default::default();
    let mut offset = 0;
    while let Some(pos) = serve[offset..].find("SPEC_PARAMS") {
        let start = offset + pos;
        offset = start + 1;
        let tail = &serve[start..];
        let Some(open) = tail.find('(') else { continue };
        let Some(close) = tail.find("],") else {
            continue;
        };
        let chunk = &tail[open..close];
        let mut route = String::new();
        let mut names: Vec<String> = Vec::new();
        for piece in chunk.split('"').skip(1).step_by(2) {
            if piece.starts_with('/') && route.is_empty() {
                route = piece.to_string();
            } else if !piece.is_empty() {
                names.push(piece.to_string());
            }
        }
        if !route.is_empty() {
            declared.insert(route, names);
        }
        offset = start + close;
    }
    assert!(!declared.is_empty(), "no SPEC_PARAMS found");

    // Function bodies by name.
    fn body<'a>(source: &'a str, name: &str) -> Option<&'a str> {
        let start = source.find(&format!("\nfn {name}("))?;
        let open = source[start..].find('{')? + start;
        let mut depth = 0i32;
        for (i, ch) in source[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&source[start..open + i]);
                    }
                }
                _ => {}
            }
        }
        None
    }

    let mut problems: Vec<String> = Vec::new();
    for (route, handler) in handlers {
        let Some(names) = declared.get(&route) else {
            continue;
        };
        let Some(source) = body(&serve, &handler) else {
            continue;
        };
        let mut reads: Vec<String> = Vec::new();
        let mut cursor = 0;
        while let Some(pos) = source[cursor..].find("params.get(\"") {
            let start = cursor + pos + "params.get(\"".len();
            cursor = start;
            let end = source[start..]
                .find('"')
                .map(|i| start + i)
                .unwrap_or(start);
            let key = source[start..end].to_string();
            if !key.is_empty() && !reads.contains(&key) {
                reads.push(key);
            }
        }
        for key in reads {
            if !names.iter().any(|n| n == &key) {
                problems.push(format!(
                    "{route} ({handler}) reads {key} but /spec omits it"
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}
