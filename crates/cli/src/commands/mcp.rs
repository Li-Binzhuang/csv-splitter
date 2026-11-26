use crate::commands::mcp_config::{self, CLIENTS};
use crate::{print_command_help, Parser};
use std::path::PathBuf;

pub(crate) fn cmd_mcp(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    let mut action: Option<String> = None;
    let mut client: Option<String> = None;
    let mut binary: Option<PathBuf> = None;
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
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let binary = mcp_config::absolute(&binary, &cwd);
        let root = mcp_config::absolute(&root.unwrap_or_else(|| PathBuf::from(".")), &cwd);
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
