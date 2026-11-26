use crate::commands::mcp_audit::{self, DEFAULT_LIMIT};
use crate::commands::mcp_config::{self, CLIENTS};
use crate::{print_command_help, Parser};
use std::path::PathBuf;

pub(crate) fn cmd_mcp(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    let mut action: Option<String> = None;
    let mut client: Option<String> = None;
    let mut binary: Option<PathBuf> = None;
    let mut tool: Option<String> = None;
    let mut errors = false;
    let mut limit = DEFAULT_LIMIT;
    let mut json = false;
    while let Some(arg) = p.next() {
        match arg.as_str() {
            "config" if action.is_none() => action = Some("config".to_string()),
            "--client" => match p.value("--client") {
                Ok(value) => client = Some(value),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--binary" => match p.value("--binary") {
                Ok(value) => binary = Some(PathBuf::from(value)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--tool" => match p.value("--tool") {
                Ok(value) => tool = Some(value),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--limit" => match p
                .value("--limit")
                .and_then(|value| mcp_audit::parse_limit(&value))
            {
                Ok(value) => limit = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--errors" => errors = true,
            "--json" => json = true,
            "--root" => match p.value("--root") {
                Ok(value) => root = Some(PathBuf::from(value)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("mcp");
                return 0;
            }
            other => {
                if !other.starts_with('-') && action.is_none() {
                    action = Some(other.to_string());
                } else {
                    eprintln!("error: unknown option: {other}");
                    return 2;
                }
            }
        }
    }
    if let Some(action) = action.as_deref() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let root = mcp_config::absolute(&root.unwrap_or_else(|| PathBuf::from(".")), &cwd);
        if action == "audit" {
            let output = match mcp_audit::run(&root, tool.as_deref(), errors, limit) {
                Ok(output) => output,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 1;
                }
            };
            eprintln!(
                "audit={} bytes={} rotatedBytes={} maxBytes={} total={} shown={}",
                output.path.display(),
                output.bytes,
                output.rotated_bytes,
                output.max_bytes,
                output.total,
                output.lines.len()
            );
            for line in &output.lines {
                if json {
                    println!("{line}");
                } else {
                    println!(
                        "{} {} ok={} {} ms={}",
                        mcp_audit::field(line, "ts"),
                        mcp_audit::field(line, "tool"),
                        mcp_audit::field(line, "ok"),
                        mcp_audit::field(line, "code"),
                        mcp_audit::field(line, "ms")
                    );
                }
            }
            if output.total == 0 && !json {
                println!("(no MCP audit log)");
            }
            return 0;
        }
        if action != "config" {
            eprintln!("error: unknown mcp action: {action}");
            return 2;
        }
        let Some(client) = client else {
            eprintln!("error: mcp config needs --client ({})", CLIENTS.join(", "));
            return 2;
        };
        let binary = binary
            .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| PathBuf::from("csvx")));
        let binary = mcp_config::absolute(&binary, &cwd);
        match mcp_config::render(&client, &binary, &root) {
            Ok(text) => {
                println!("{text}");
                0
            }
            Err(error) => {
                eprintln!("error: {error}");
                2
            }
        }
    } else {
        let root = root.unwrap_or_else(|| PathBuf::from("."));
        match csvx_mcp::run_mcp_stdio(&root) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("error: {error}");
                1
            }
        }
    }
}
