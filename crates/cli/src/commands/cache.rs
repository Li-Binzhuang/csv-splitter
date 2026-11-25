use crate::{print_command_help, Parser};
use std::path::PathBuf;

pub(crate) fn cmd_cache(args: &[String]) -> i32 {
    let mut input: Option<PathBuf> = None;
    let mut build = false;
    let mut drop_it = false;
    let mut status = false;
    let mut p = Parser::new(args);
    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--build" => build = true,
            "--drop" => drop_it = true,
            "--status" => status = true,
            "-h" | "--help" => {
                print_command_help("cache");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option for cache: {other}");
                return 2;
            }
            other => input = Some(PathBuf::from(other)),
        }
    }
    let Some(path) = input else {
        eprintln!("error: cache needs an input file");
        return 2;
    };
    let sidecar = csvx_core::sql_cache::sidecar(&path);
    let fresh = csvx_core::sql_cache::is_fresh(&path);
    if drop_it {
        match csvx_core::sql_cache::drop_cache(&path) {
            Ok(true) => {
                println!("removed {}", sidecar.display());
                return 0;
            }
            Ok(false) => {
                println!("no cache for {}", path.display());
                return 0;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }
    if build {
        let options = csvx_core::sheet::ReadOptions::default();
        let columns =
            match csvx_core::sheet::Sheet::open(&path, csvx_core::sheet::ReadOptions::default()) {
                Ok(sheet) => match sheet.schema(64) {
                    Ok(columns) => columns,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return 1;
                    }
                },
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
        match csvx_core::sql_cache::build(
            &path,
            options.delimiter,
            &columns,
            csvx_core::sql::DEFAULT_TIMEOUT_MS * 20,
        ) {
            Ok(out) => {
                let bytes = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                println!("built {} ({bytes} bytes)", out.display());
                return 0;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    }
    if status || !build && !drop_it {
        let bytes = std::fs::metadata(&sidecar).map(|m| m.len()).unwrap_or(0);
        println!(
            "{} — {}",
            sidecar.display(),
            if fresh {
                format!("fresh ({bytes} bytes)")
            } else if sidecar.exists() {
                "stale (the sheet changed); rebuild with --build".to_string()
            } else {
                "missing; build with --build".to_string()
            }
        );
        return 0;
    }
    0
}
