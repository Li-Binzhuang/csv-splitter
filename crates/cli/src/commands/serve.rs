use crate::{print_command_help, serve, Parser};
use std::path::PathBuf;

pub(crate) fn cmd_serve(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    let mut addr = "127.0.0.1:8787".to_string();
    let mut check = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--root" => match p.value("--root") {
                Ok(v) => root = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            // `--port` is the shorthand people type; `--addr` is the full form.
            "--port" => match p.value("--port").and_then(|v| {
                v.parse::<u16>()
                    .map(|port| format!("127.0.0.1:{port}"))
                    .map_err(|_| format!("--port takes a number, not {v:?}"))
            }) {
                Ok(v) => addr = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--addr" => match p.value("--addr") {
                Ok(v) => addr = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--check" => check = true,
            "-h" | "--help" => {
                print_command_help("serve");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => root = Some(PathBuf::from(other)),
        }
    }

    let Some(root) = root else {
        eprintln!("error: serve needs --root <dir>");
        return 2;
    };
    if check {
        return serve::check(&addr, &root);
    }
    match serve::run(serve::ServerConfig { addr, root }) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
