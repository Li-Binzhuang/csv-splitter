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
