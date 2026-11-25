use crate::{print_command_help, Parser};
use std::path::PathBuf;

pub(crate) fn cmd_mcp(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = p.next() {
        match arg.as_str() {
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
                eprintln!("error: unknown option: {other}");
                return 2;
            }
        }
    }
    let root = root.unwrap_or_else(|| PathBuf::from("."));
    match csvx_mcp::run_mcp_stdio(&root) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}
