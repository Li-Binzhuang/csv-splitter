//! Documentation drift checks.
//!
//! README and the getting-started guide quote commands and endpoints. Both are
//! easy to edit and impossible to compile, so the only way they stay true is a
//! test that reads them and compares against the program.
//!
//! Two things are checked, both cheap and both have already drifted once:
//!   * every `csv-splitter <command>` mentioned in a code block is a real
//!     sub-command;
//!   * every `GET /x` / `POST /x` mentioned is advertised by the router.

use std::io::{BufRead, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

fn unique_seq() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}

const README: &str = include_str!("../../../README.md");
const GUIDE: &str = include_str!("../../../docs/GETTING_STARTED.md");
const ROUTER: &str = include_str!("../src/serve.rs");

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "csvs_docs_{tag}_{}_{}",
            std::process::id(),
            unique_seq()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Sub-commands the binary actually has, taken from its own help.
fn real_commands() -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
        .arg("--help")
        .output()
        .expect("run --help");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Every `csv-splitter <word>` inside a fenced code block in the docs.
fn documented_commands(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_block = !in_block;
            continue;
        }
        if !in_block {
            continue;
        }
        // "$ csv-splitter demo", "csv-splitter preview …"
        let body = trimmed.trim_start_matches("$ ").trim();
        if let Some(rest) = body.strip_prefix("csv-splitter ") {
            let word = rest.split_whitespace().next().unwrap_or("");
            // Program *output* also begins with the name ("csv-splitter API on
            // http://…"); a sub-command is always a lowercase word.
            if !word.is_empty() && word.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                out.push(word.trim_end_matches('\\').to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn every_documented_command_exists() {
    let help = real_commands();
    let mut problems = Vec::new();
    for doc in [("README", README), ("GETTING_STARTED", GUIDE)] {
        for command in documented_commands(doc.1) {
            // `--help` lists sub-commands as "  <name>  <args…>".
            let listed = help
                .lines()
                .any(|line| line.trim_start().starts_with(&format!("{command} ")));
            if !listed {
                problems.push(format!(
                    "{}: `csv-splitter {command}` is not a sub-command",
                    doc.0
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Endpoints mentioned in the docs, e.g. "GET /rows?path=…".
fn documented_endpoints(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in text.split('`') {
        let token = token.trim();
        for verb in ["GET ", "POST "] {
            if let Some(rest) = token.strip_prefix(verb) {
                let path = rest.split(['?', ' ', '`']).next().unwrap_or("");
                if path.starts_with('/') {
                    out.push(format!("{} {path}", verb.trim()));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

#[test]
fn every_documented_endpoint_is_advertised_by_the_router() {
    // The router's 404 body lists what it serves; that is the source of truth.
    let advertised = ROUTER;
    let mut problems = Vec::new();
    for endpoint in documented_endpoints(README) {
        let (verb, path) = endpoint.split_once(' ').unwrap();
        // GET routes live in the read match; POST routes in the write match.
        // Both are plain `"<path>" => handler(..)`, so the path itself is the
        // thing to find — the handler name is not part of the contract.
        let needle = format!("\"{path}\" =>");
        if !advertised.contains(&needle) {
            problems.push(format!(
                "README documents {verb} {path}, the router does not serve it"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn the_documented_serve_port_actually_works() {
    // The docs tell people to open http://127.0.0.1:8787/; make sure the default
    // is that port and that GET / answers with the viewer.
    let dir = TempDir::new("port");
    std::fs::write(dir.join("a.csv"), "id\n1\n").unwrap();

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
        .args(["serve", "--root", dir.0.to_str().unwrap()])
        .args(["--addr", "127.0.0.1:0"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn serve");
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .split("http://")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .expect("banner address")
        .to_string();

    let mut stream = TcpStream::connect(&addr).unwrap();
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let ok = raw.starts_with("HTTP/1.1 200") && raw.contains("text/html");
    let _ = child.kill();
    let _ = child.wait();
    assert!(ok, "GET / did not serve the viewer: {raw}");

    assert!(
        README.contains("127.0.0.1:8787"),
        "the default port changed"
    );
}

/// The API reference must not describe endpoints the server does not serve.
const API_DOC: &str = include_str!("../../../docs/API.md");
const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");
const JOBS: &str = include_str!("../../../crates/core/src/jobs.rs");

#[test]
fn api_reference_matches_the_router() {
    let mut documented: Vec<String> = Vec::new();
    for token in API_DOC.split('`') {
        let token = token.trim();
        for verb in ["GET ", "POST "] {
            if let Some(rest) = token.strip_prefix(verb) {
                let path = rest.split(['?', ' ', '`']).next().unwrap_or("");
                // The root route and the "these are the writes" summary line are
                // prose, not endpoint references.
                if path.starts_with('/') && !path.contains('|') {
                    documented.push(format!("{} {path}", verb.trim()));
                }
            }
        }
    }
    documented.sort();
    documented.dedup();

    let mut problems = Vec::new();
    for entry in &documented {
        let (_verb, path) = entry.split_once(' ').unwrap();
        // `GET /` is served by a dedicated arm (`"/" => page(...)`), not by the
        // route table, so check the source for the literal path.
        let needle = format!("\"{path}\"");
        if !ROUTER.contains(&needle) {
            problems.push(format!(
                "docs/API.md documents {entry}, the router does not serve it"
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(
        !documented.is_empty(),
        "the API doc should mention endpoints"
    );
}

#[test]
fn api_reference_names_only_parameters_the_server_reads() {
    // Only the parameter column of the endpoint tables is checked: prose mentions
    // commands and functions freely, and that is not what this guards.
    let mut missing = Vec::new();
    for line in API_DOC.lines() {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if cells.len() < 3 || !line.starts_with("| `") {
            continue;
        }
        let params = cells[2];
        for token in params.split('`') {
            let word = token.trim();
            if word.len() < 2
                || !word.chars().all(|c| c.is_ascii_alphanumeric())
                || word.chars().next().is_some_and(|c| c.is_ascii_digit())
            {
                continue;
            }
            let needle = format!("\"{word}\"");
            if !ROUTER.contains(&needle) && !JOBS.contains(&needle) {
                missing.push(word.to_string());
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "docs name parameters nobody reads: {missing:?}"
    );
}

/// The help's task index may only name real sub-commands.
#[test]
fn the_task_index_names_real_commands() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout).to_string();
    let block = match help.split_once("WHAT DO YOU WANT TO DO?") {
        Some((_, rest)) => rest.split("\nCOMMANDS").next().unwrap_or("").to_string(),
        None => panic!("the help lost its task index"),
    };
    let known = crate::real_commands();
    let mut unknown = Vec::new();
    // Only the right-hand column names commands; the left is prose.
    for line in block.lines() {
        let Some((_, commands)) = line.rsplit_once("  ") else {
            continue;
        };
        for word in commands.split(|c: char| c == ',' || c.is_whitespace() || c == '/') {
            let word = word.trim();
            if word.is_empty() || !word.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
                continue;
            }
            let listed = known
                .lines()
                .any(|line| line.trim_start().starts_with(&format!("{word} ")));
            if !listed && !["set", "insert-column", "rename", "drop", "where", "-o"].contains(&word)
            {
                unknown.push(word.to_string());
            }
        }
    }
    assert!(
        unknown.is_empty(),
        "task index names unknown commands: {unknown:?}"
    );
}

/// The manifest version and the newest changelog entry must agree.
///
/// They drifted once without anyone noticing: the release bumps were written with
/// `replace("## [0.6N] …")` anchors that did not exist yet, so six entries silently
/// never landed while the code kept moving.
#[test]
fn the_changelog_leads_with_the_current_version() {
    let manifest = include_str!("../../../Cargo.toml");
    let version = manifest
        .lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.split('"').next())
        .expect("workspace version");
    let first_entry = CHANGELOG
        .lines()
        .find_map(|line| line.strip_prefix("## ["))
        .and_then(|rest| rest.split(']').next())
        .expect("a changelog entry");
    assert_eq!(
        first_entry, version,
        "the newest changelog entry is {first_entry}, the manifest says {version}"
    );
}

/// Every entry must have a date, so the changelog stays a timeline rather than a
/// pile of headings.
#[test]
fn changelog_entries_are_dated() {
    let undated: Vec<&str> = CHANGELOG
        .lines()
        .filter(|line| line.starts_with("## ["))
        .filter(|line| !line.contains("— "))
        .collect();
    assert!(undated.is_empty(), "entries without a date: {undated:?}");
}

/// The quote-style claim names real commands: each one must reject a bad value,
/// which proves it parsed the flag at all.
#[test]
fn the_quote_style_claim_names_commands_that_accept_it() {
    let accepting = [
        "preview", "filter", "view", "groupby", "pivot", "chart", "dedupe", "stats", "analyze",
        "lookup",
    ];
    let dir = std::env::temp_dir().join(format!("csvs_quotes_doc_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("t.csv");
    std::fs::write(&file, "a,b\n1,2\n").unwrap();

    let mut missing = Vec::new();
    for command in accepting {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
            .args([command, "--quotes", "nonsense", file.to_str().unwrap()])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Accepting the flag is what this checks: either the bad value is rejected, or
        // the command's own required-argument check fires first. "unknown option"
        // would mean the flag is not parsed at all.
        if stderr.contains("unknown option") {
            missing.push(format!("{command}: {stderr}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        missing.is_empty(),
        "these do not accept --quotes: {missing:?}"
    );
}

/// `command --help` must mention every flag that command actually parses.
///
/// The source is parsed rather than a list of flags being maintained by hand: a flag
/// that is added to a command and forgotten here fails the test, which is exactly how
/// sixty-one options ended up undocumented before this existed.
#[test]
fn every_command_documents_the_flags_it_parses() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
    )
    .expect("main.rs should be readable");

    let mut problems = Vec::new();
    for (name, _, body) in command_bodies(&source) {
        let mut flags: Vec<String> = Vec::new();
        for quoted in body.split(r#""--"#).skip(1) {
            let flag: String = quoted
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                .collect();
            if !flag.is_empty() {
                flags.push(format!("--{flag}"));
            }
        }
        // Ask the binary what it documents, so the check is end to end.
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
            .args([name, "--help"])
            .output()
            .unwrap();
        let help = String::from_utf8_lossy(&out.stdout);
        for flag in flags {
            if flag == "--help" {
                continue;
            }
            if !help.contains(flag.as_str()) {
                problems.push(format!("{name} parses {flag} but does not list it"));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// Split `main.rs` into `(command name, function body)` for the `cmd_*` functions.
fn command_bodies(source: &str) -> Vec<(&str, usize, &str)> {
    let mut out = Vec::new();
    let mut rest = source;
    let mut offset = 0;
    while let Some(at) = rest.find("\nfn cmd_") {
        let start = at + 4;
        let head = &rest[start..];
        let name_end = head.find('(').unwrap_or(0);
        let name = &head[4..name_end];
        let body_start = start;
        let next = rest[start..].find("\nfn cmd_").map(|i| start + i);
        let body_end = match next {
            Some(next) => next,
            None => rest.len(),
        };
        // Stop at the next top-level function of any kind, not only another cmd_.
        let stop = rest[body_start..]
            .match_indices("\nfn ")
            .find(|(_, _)| true)
            .map(|(i, _)| body_start + i)
            .unwrap_or(body_end);
        out.push((name, offset, &rest[body_start..stop]));
        rest = &rest[stop..];
        offset += stop;
        if rest.len() <= 1 {
            break;
        }
    }
    out
}

/// `docs/API.md` and `serve.rs` must describe the same set of endpoints.
///
/// The route table is the authority: the doc is derived from it by a test rather than
/// by memory. `docs/API.md` was silently overwritten with changelog text once, and
/// nothing noticed for several rounds — a test like this is cheaper than that.
#[test]
fn the_api_reference_lists_every_route() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let server = std::fs::read_to_string(manifest.join("src/serve.rs")).unwrap();
    let api = std::fs::read_to_string(manifest.join("../../docs/API.md")).unwrap();

    assert!(
        api.starts_with("# HTTP API"),
        "docs/API.md should start with its own title, not: {:?}",
        api.lines().next().unwrap_or("")
    );

    // Routes are matched arms of the form `"/name" =>` or `"/a" | "/b" =>`.
    let mut routes: Vec<String> = Vec::new();
    for line in server.lines() {
        let trimmed = line.trim();
        if !(trimmed.contains("\" =>") || trimmed.contains("\" |")) {
            continue;
        }
        for piece in trimmed.split("=>").next().unwrap_or("").split('|') {
            let name = piece.trim().trim_matches('"');
            if name.starts_with('/') && !name.contains(' ') {
                routes.push(name.to_string());
            }
        }
    }
    routes.sort();
    routes.dedup();

    let mut missing = Vec::new();
    for route in &routes {
        // `/` and `/index.html` serve the built-in page; the doc mentions it once.
        if route == "/index.html" {
            continue;
        }
        if !api.contains(route.as_str()) {
            missing.push(route.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "routes absent from docs/API.md: {missing:?}"
    );
}

/// The README must name every command the binary dispatches.
///
/// The command list is derived from `main.rs`, not from a table someone maintains: a
/// command added without a line in the README fails here. `view`, `validate` and `book`
/// were all in that state at some point — implemented, documented nowhere.
#[test]
fn the_readme_names_every_command() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest.join("src/main.rs")).unwrap();
    let readme = std::fs::read_to_string(manifest.join("../../README.md")).unwrap();

    let mut commands: Vec<String> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(r#"Some(""#) {
            if rest.contains(") => cmd_") {
                if let Some(name) = rest.split('"').next() {
                    if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase()) {
                        commands.push(name.to_string());
                    }
                }
            }
        }
    }
    commands.sort();
    commands.dedup();
    assert!(
        commands.len() > 20,
        "command extraction broke: {commands:?}"
    );

    let mut missing = Vec::new();
    for command in &commands {
        // A word-boundary check, so `view` is not satisfied by `previewer`.
        let occurrences = readme.match_indices(command.as_str()).any(|(at, _)| {
            let before = readme[..at].chars().next_back();
            let after = readme[at + command.len()..].chars().next();
            let word = |c: Option<char>| {
                !c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            };
            word(before) && word(after)
        });
        if !occurrences {
            missing.push(command.clone());
        }
    }
    assert!(missing.is_empty(), "README never mentions: {missing:?}");
}

/// Every command named in CAPABILITIES.md must exist, and every command must be named.
///
/// Both directions matter: a doc that names a removed command sends people to a dead
/// end, and one that omits a command leaves it undiscoverable. The command list comes
/// from `main.rs`, so this cannot be satisfied by editing the doc alone.
#[test]
fn capabilities_names_real_commands_and_all_of_them() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest.join("src/main.rs")).unwrap();
    let doc = std::fs::read_to_string(manifest.join("../../docs/CAPABILITIES.md")).unwrap();

    let mut commands: Vec<String> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(r#"Some(""#) {
            if rest.contains(") => cmd_") {
                if let Some(name) = rest.split('"').next() {
                    if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase()) {
                        commands.push(name.to_string());
                    }
                }
            }
        }
    }
    commands.sort();
    assert!(commands.len() > 20, "{commands:?}");

    let word_at = |text: &str, at: usize, len: usize| {
        let before = text[..at].chars().next_back();
        let after = text[at + len..].chars().next();
        let edge =
            |c: Option<char>| !c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        edge(before) && edge(after)
    };

    let mut missing = Vec::new();
    for command in &commands {
        let found = doc
            .match_indices(command.as_str())
            .any(|(at, _)| word_at(&doc, at, command.len()));
        if !found {
            missing.push(command.clone());
        }
    }

    // Commands mentioned as `cmd` in a table must be real. Only the left-hand column of
    // a table row is checked, since that is where this doc names commands.
    let mut phantom = Vec::new();
    for line in doc.lines() {
        let Some(row) = line.strip_prefix('|') else {
            continue;
        };
        for cell in row.split('|') {
            let cell = cell.trim();
            // A cell like "`split f.csv -n N` / `--by-size`" names `split` first.
            if let Some(rest) = cell.strip_prefix('`') {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                    .collect();
                // A command cell reads `cmd …` — a bare word followed by a space or a
                // tick. `crates/core`, `lag(...)`, `where=…` are not commands.
                let rest_after = &rest[name.len()..];
                let looks_like_a_command = !name.is_empty()
                    && !name.starts_with("--")
                    && (rest_after.starts_with(' ') || rest_after.starts_with('`'));
                if looks_like_a_command && !commands.iter().any(|c| c == &name) {
                    phantom.push(name);
                }
            }
        }
    }
    phantom.sort();
    phantom.dedup();

    assert!(
        missing.is_empty() && phantom.is_empty(),
        "not documented: {missing:?}; documented but not a command: {phantom:?}"
    );
}

/// ARCHITECTURE.md must name every core module, so the map cannot go stale.
///
/// A module added to the engine but absent from the map is invisible to whoever opens
/// the doc to find out what exists — which is the whole reason to have a map.
#[test]
fn the_architecture_map_names_every_core_module() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let core = manifest.join("../core/src");
    let doc = std::fs::read_to_string(manifest.join("../../docs/ARCHITECTURE.md")).unwrap();
    assert!(
        doc.starts_with("# 架构 / Architecture"),
        "wrong file? first line: {:?}",
        doc.lines().next()
    );

    let mut modules: Vec<String> = Vec::new();
    let mut stack = vec![core.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let stem = path.file_stem().unwrap().to_string_lossy().to_string();
                // `mod.rs` and `lib.rs` are structure, not capabilities.
                if !matches!(stem.as_str(), "mod" | "lib") {
                    modules.push(stem);
                }
            }
        }
    }
    modules.sort();
    assert!(modules.len() > 30, "{modules:?}");

    let mut missing = Vec::new();
    for module in &modules {
        // The map lists modules as `name` in backticks or inside a path like
        // `workbook/rows`; both count, but the name must appear.
        if !doc.contains(module.as_str()) {
            missing.push(module.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "modules absent from the map: {missing:?}"
    );
}

/// The troubleshooting guide quotes real behaviour: reproduce each case and compare.
///
/// A troubleshooting doc that has drifted is worse than none: it teaches the reader to
/// look for a sentence the tool no longer prints. These cases are cheap to run, so the
/// guide is checked against the binary that shipped with it.
#[test]
fn troubleshooting_quotes_are_real() {
    let dir = std::env::temp_dir().join(format!("csvs_trouble_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("t.csv");
    std::fs::write(&csv, "id,city,amount\n1,CDMX,10\n2,Puebla,20\n").unwrap();
    let path = csv.to_str().unwrap();

    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
            .args(args)
            .output()
            .unwrap();
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };

    // Each case: how to trigger it, and the sentence the guide promises.
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec!["filter", path, "--where", "amounts>5"],
            "not found; did you mean",
        ),
        (
            vec!["edit", "formula", path, "--name", "x", "--value", "nope*2"],
            "unknown column",
        ),
        (
            vec![
                "edit",
                "formula",
                path,
                "--name",
                "x",
                "--value",
                r#"ref("nope.csv","id","amount")"#,
            ],
            "not found next to",
        ),
        (
            vec!["preview", "/definitely/not/here.csv"],
            "No such file or directory",
        ),
        (
            vec![
                "edit", "set", path, "--row", "0", "--col", "city", "--value", "X",
            ],
            "rows are 1-based",
        ),
        (
            vec!["dedupe", path, "--by", "id", "--max-keys", "1"],
            "raise --max-keys",
        ),
        (
            vec![
                "chart",
                path,
                "--x",
                "id",
                "--y",
                "amount:sum",
                "--max-points",
                "1",
            ],
            "raise --max-points",
        ),
    ];

    let mut problems = Vec::new();
    for (args, expected) in cases {
        let text = run(&args);
        if !text.contains(expected) {
            problems.push(format!("{args:?} did not say {expected:?}: {text}"));
        }
    }

    // Refusing to overwrite the input.
    let text = run(&["filter", path, "--where", "city=CDMX", "-o", path]);
    if !text.contains("refusing to write over the input file") {
        problems.push(format!("overwrite guard: {text}"));
    }

    // A journal cut off mid-write.
    std::fs::write(dir.join("t.csv.journal"), "set,0,city,Z\nset,1,ci").unwrap();
    let text = run(&["view", path, "--count", "2"]);
    if !text.contains("could not be read") {
        problems.push(format!("journal damage warning: {text}"));
    }

    let _ = std::fs::remove_dir_all(&dir);
    assert!(problems.is_empty(), "{problems:#?}");
}

/// The handover document must not promise a command that does not exist.
///
/// It is the first file a new maintainer reads, so a stale command name there costs
/// more than the same mistake in a comment.
#[test]
fn the_handover_names_only_real_commands() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc = std::fs::read_to_string(manifest.join("../../docs/HANDOVER.md")).unwrap();
    let help = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&help.stdout).to_string();
    let commands: Vec<&str> = help
        .lines()
        .skip_while(|l| !l.starts_with("COMMANDS"))
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .map(|l| l.split_whitespace().next().unwrap_or(""))
        .filter(|c| !c.is_empty())
        .collect();
    assert!(commands.len() >= 25, "{commands:?}");

    // Every backticked word in the doc that looks like a command must be one.
    let mut problems = Vec::new();
    for word in doc.split('`').skip(1).step_by(2) {
        let first = word.split_whitespace().next().unwrap_or("");
        if first.contains(' ') || first.starts_with('-') || first.is_empty() {
            continue;
        }
        let candidate = first.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
        // Only judge words that are in the command vocabulary.
        if commands.contains(&candidate) {
            continue;
        }
        if matches!(
            candidate,
            "edit" | "get" | "post" | "csv-splitter" | "docs" | "scripts" | "crates"
        ) {
            continue;
        }
        if candidate.len() > 3
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && !doc.contains(&format!("--{candidate}"))
            && candidate.parse::<f64>().is_err()
        {
            // Words like `commands`, `journal`, `rows` are prose; only flag names that
            // appear nowhere else in the help text.
            if !help.contains(candidate)
                && !doc.contains(&format!("/{candidate}"))
                && looks_like_command(candidate)
            {
                problems.push(candidate.to_string());
            }
        }
    }
    assert!(
        problems.is_empty(),
        "handover names unknown commands: {problems:?}"
    );
}

fn looks_like_command(word: &str) -> bool {
    matches!(
        word,
        "split"
            | "merge"
            | "preview"
            | "view"
            | "index"
            | "serve"
            | "convert"
            | "filter"
            | "sort"
            | "analyze"
            | "dedupe"
            | "columns"
            | "partition"
            | "concat"
            | "groupby"
            | "pivot"
            | "format"
            | "chart"
            | "layout"
            | "lookup"
            | "book"
            | "demo"
            | "search"
            | "files"
            | "sql"
            | "stats"
            | "extract"
            | "verify"
    )
}

/// `--freeze-first` takes a count, and the help must say so.
#[test]
fn freeze_first_says_it_takes_a_count() {
    let help = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
        .args(["layout", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(
        help.contains("first N columns"),
        "help must not imply a column list: {help}"
    );

    let dir = std::env::temp_dir().join(format!("csvs_freeze_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("t.csv");
    std::fs::write(&csv, "id,city,amount\n1,CDMX,10\n").unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_csv-splitter"))
        .args(["layout", csv.to_str().unwrap(), "--freeze-first", "id,city"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("takes a count"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every route the code serves is listed in API.md, and nothing is listed that is gone.
///
/// A front-end reads API.md, not `serve.rs`; a route that exists but is undocumented is
/// invisible, and one documented but removed is a trap.
#[test]
fn api_md_lists_every_route() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest.join("src/serve.rs")).unwrap();
    let doc = std::fs::read_to_string(manifest.join("../../docs/API.md")).unwrap();

    // Routes appear in the dispatch as `"/name" =>`.
    let mut routes: Vec<String> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('"') {
            if let Some(name) = rest.split('"').next() {
                if name.starts_with('/') && rest.contains("=>") {
                    routes.push(name.to_string());
                }
            }
        }
    }
    routes.sort();
    routes.dedup();
    assert!(routes.len() >= 10, "{routes:?}");

    let missing: Vec<&String> = routes
        .iter()
        .filter(|route| route.as_str() != "/" && !doc.contains(route.as_str()))
        .collect();
    assert!(missing.is_empty(), "routes not in API.md: {missing:?}");
}

/// The integration guide's endpoint list must match the code, and its claims must be
/// among the ones the tests prove.
#[test]
fn integration_guide_names_real_parameters() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let doc = std::fs::read_to_string(manifest.join("../../docs/INTEGRATION.md")).unwrap();
    let serve = std::fs::read_to_string(manifest.join("src/serve.rs")).unwrap();

    // Every `…/name` or `http://…/name` the guide uses is a real route.
    let mut problems = Vec::new();
    for token in doc
        .split('"')
        .filter(|t| t.contains('/') && t.contains('?'))
    {
        let route = token
            .split('?')
            .next()
            .unwrap_or("")
            .rsplit('/')
            .next()
            .unwrap_or("");
        if route.is_empty() {
            continue;
        }
        let quoted = format!("\"/{route}\"");
        if !serve.contains(&quoted) {
            problems.push(format!(
                "guide names /{route}, which the server does not serve"
            ));
        }
    }

    // The parameters the guide promises are the ones the handlers read.
    for param in [
        "columns",
        "start-col",
        "count-cols",
        "expect",
        "sample",
        "name",
    ] {
        assert!(
            serve.contains(&format!("\"{param}\"")) || serve.contains(param),
            "parameter {param} is not read anywhere: the guide would be wrong"
        );
        assert!(doc.contains(param), "the guide does not mention {param}");
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// The capabilities response, the 404 fallback and the dispatcher agree on the route set.
///
/// Three lists of "what this server serves" is how an endpoint ends up working but not
/// advertised (insert-column was exactly that). They come from one constant now, and this
/// test walks the dispatcher's own arms to prove nothing was added without it.
#[test]
fn every_served_route_is_advertised() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest.join("src/serve.rs")).unwrap();

    let list = |name: &str| -> Vec<String> {
        let start = source
            .find(&format!("const {name}: &[&str] = &["))
            .unwrap_or_else(|| panic!("missing {name}"));
        let end = source[start..].find("];").unwrap() + start;
        source[start..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(|s| s.to_string())
            .collect()
    };
    let reads = list("READ_ROUTES");
    let writes = list("WRITE_ROUTES");
    assert!(reads.len() >= 20, "{reads:?}");
    assert!(writes.len() >= 6, "{writes:?}");

    // GET arms in the dispatch (`"/name" => handler`).
    let mut served: Vec<String> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('"') {
            if let Some(name) = rest.split('"').next() {
                if name.starts_with('/') && rest.contains("=>") {
                    served.push(name.to_string());
                }
            }
        }
    }
    let mut missing: Vec<String> = served
        .iter()
        .filter(|route| route.as_str() != "/index.html")
        .filter(|route| {
            !reads.contains(route)
                && !writes
                    .iter()
                    .any(|w| w.strip_prefix("POST ") == Some(route.as_str()))
        })
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "served but not in READ_ROUTES: {missing:?}"
    );

    // And the write arms are POST routes in their own list.
    for route in [
        "/split",
        "/merge",
        "/sort",
        "/dedupe",
        "/partition",
        "/warm",
        "/edit",
        "/index",
    ] {
        assert!(
            writes
                .iter()
                .any(|w| w.strip_prefix("POST ") == Some(route)),
            "{route} is served as a POST but not listed"
        );
    }
}

/// API.md's endpoint index names every advertised route.
#[test]
fn the_endpoint_index_covers_the_capabilities() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest.join("src/serve.rs")).unwrap();
    let doc = std::fs::read_to_string(manifest.join("../../docs/API.md")).unwrap();

    let start = source
        .find("const READ_ROUTES: &[&str] = &[")
        .expect("READ_ROUTES");
    let end = source[start..].find("];").unwrap() + start;
    let routes: Vec<String> = source[start..end]
        .split('"')
        .skip(1)
        .step_by(2)
        .filter(|r| *r != "/")
        .map(|r| r.to_string())
        .collect();

    let missing: Vec<&String> = routes
        .iter()
        .filter(|r| !doc.contains(r.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "routes missing from API.md: {missing:?}"
    );
}

/// Every path-requiring read is a real route, and the list is a subset of the routes.
#[test]
fn path_requiring_reads_are_real_routes() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = std::fs::read_to_string(manifest.join("src/serve.rs")).unwrap();
    let list = |name: &str| -> Vec<String> {
        let start = source
            .find(&format!("const {name}: &[&str] = &["))
            .unwrap_or_else(|| panic!("missing {name}"));
        let end = source[start..].find("];").unwrap() + start;
        source[start..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(|s| s.to_string())
            .collect()
    };
    let reads = list("READ_ROUTES");
    let needs = list("PATH_REQUIRED");
    assert!(!needs.is_empty());
    for route in &needs {
        assert!(
            reads.contains(route),
            "{route} requires a path but is not a served read route"
        );
    }
}

/// The reverse of `every_command_documents_the_flags_it_parses`: every flag the docs
/// tell a reader to type is a flag the tool accepts.
///
/// The forward test cannot catch this — a documented flag that does not exist produces
/// no warning anywhere, and the reader gets `unknown option`. That is exactly what
/// happened to `serve --port`, which INTEGRATION.md recommended and nothing implemented.
#[test]
fn every_documented_flag_exists() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    let source = std::fs::read_to_string(manifest.join("src/main.rs")).unwrap();
    let accepted: Vec<String> = source
        .split('"')
        .filter(|s| s.starts_with("--") && s.len() > 2 && !s.contains(' '))
        .map(|s| s.to_string())
        .collect();

    // Flags belonging to other tools in the examples (cargo, git) are not ours to check.
    let foreign = [
        "--features",
        "--all",
        "--all-targets",
        "--diff-filter",
        "--lib",
        "--no-deps",
        "--path",
        "--release",
        "--test",
        "--workspace",
        "--help",
        "--version",
        "--quiet",
        "--benches",
    ];

    let mut docs: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path
                    .file_name()
                    .is_some_and(|n| n != "target" && n != ".git")
                {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "md") {
                // The changelog is history: it legitimately names flags that have been
                // removed or renamed, so it is not a statement about the current CLI.
                if path.file_name().is_some_and(|n| n != "CHANGELOG.md") {
                    docs.push(path);
                }
            }
        }
    }
    assert!(docs.len() >= 8, "{docs:?}");

    let mut unknown: Vec<String> = Vec::new();
    for doc in docs {
        let text = std::fs::read_to_string(&doc).unwrap();
        for word in text.split([' ', '\n', '"', '`', '(', ')', ',', '|']) {
            let word = word.trim();
            let flag = word.trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
            if !flag.starts_with("--") || flag.len() < 3 {
                continue;
            }
            // Markdown table rules (`|---|`) are dashes, not options.
            if flag.chars().all(|c| c == '-') {
                continue;
            }
            // `--keep/--drop` is prose listing three flags, not one flag name.
            let name = flag.split('=').next().unwrap().split('/').next().unwrap();
            if foreign.contains(&name) || accepted.iter().any(|a| a.as_str() == name) {
                continue;
            }
            unknown.push(format!("{}: {name}", doc.display()));
        }
    }
    unknown.sort();
    unknown.dedup();
    assert!(
        unknown.is_empty(),
        "documented flags the tool does not accept: {unknown:#?}"
    );
}

/// Every `/route` a doc tells a reader to call is a route the server serves.
///
/// The existing route test goes one way (served routes must be documented); this one
/// catches the opposite: a guide inventing an endpoint (or naming a removed one) that no
/// handler will answer.
#[test]
fn every_documented_route_is_served() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    let serve = std::fs::read_to_string(manifest.join("src/serve.rs")).unwrap();

    let list = |name: &str| -> Vec<String> {
        let start = serve
            .find(&format!("const {name}: &[&str] = &["))
            .unwrap_or_else(|| panic!("missing {name}"));
        let end = serve[start..].find("];").unwrap() + start;
        serve[start..end]
            .split('"')
            .skip(1)
            .step_by(2)
            .map(|s| s.to_string())
            .collect()
    };
    let reads = list("READ_ROUTES");
    let writes: Vec<String> = list("WRITE_ROUTES")
        .iter()
        .filter_map(|w| w.strip_prefix("POST ").map(|s| s.to_string()))
        .collect();

    let mut docs: Vec<std::path::PathBuf> = Vec::new();
    for name in [
        "README.md",
        "docs/API.md",
        "docs/INTEGRATION.md",
        "docs/CAPABILITIES.md",
        "docs/TROUBLESHOOTING.md",
        "docs/HANDOFF.md",
        "docs/ARCHITECTURE.md",
    ] {
        let path = root.join(name);
        if path.exists() {
            docs.push(path);
        }
    }
    assert!(docs.len() >= 4, "{docs:?}");

    let mut unknown: Vec<String> = Vec::new();
    for doc in docs {
        let text = std::fs::read_to_string(&doc).unwrap();
        // Route mentions look like `/name?` or `/name"` inside curl lines; require a
        // known-route character set to avoid matching file paths and markdown.
        for token in text.split('"') {
            let Some(route) = token.split('?').next() else {
                continue;
            };
            let route = route.trim();
            if !route.starts_with('/') || route.len() < 2 {
                continue;
            }
            let name: String = route
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '/' || *c == '-')
                .collect();
            if name.len() < 2 || name == "/" {
                continue;
            }
            // A curl URL ends at the path; only bare `/x` tokens are endpoint claims.
            if name.contains("//") || name.contains(".md") {
                continue;
            }
            if reads.iter().any(|r| r == &name) || writes.iter().any(|w| w == &name) {
                continue;
            }
            // Shell/yaml examples carry absolute paths; those start with a directory.
            if name.starts_with("/Users") || name.starts_with("/tmp") || name.starts_with("/data") {
                continue;
            }
            unknown.push(format!("{}: {name}", doc.display()));
        }
    }
    unknown.sort();
    unknown.dedup();
    assert!(
        unknown.is_empty(),
        "documented routes not served: {unknown:#?}"
    );
}
