use crate::{human_bytes, print_command_help, report_failure, Parser};
use std::path::PathBuf;

pub(crate) fn cmd_demo(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut out: Option<PathBuf> = None;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "-o" | "--out" => match p.value("--out") {
                Ok(v) => out = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("demo");
                return 0;
            }
            other => out = Some(PathBuf::from(other)),
        }
    }

    let dir = out.unwrap_or_else(|| PathBuf::from("csvx-demo"));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("error: cannot create {}: {e}", dir.display());
        return 1;
    }
    let path = dir.join("orders.csv");
    let bytes = csvx_core::demo::write_sample(&path);
    match bytes {
        Ok((rows, bytes)) => {
            println!(
                "✓ wrote {} ({rows} data rows, {})",
                path.display(),
                human_bytes(bytes)
            );
            println!();
            println!("  Try the 10-minute tour:");
            println!("    csvx preview {}", path.display());
            println!("    csvx analyze {}", path.display());
            println!(
                "    csvx filter {} --where \"city=CDMX\" --where \"amount>1000\" -o cdmx.csv",
                path.display()
            );
            println!("    csvx groupby {} --by city --agg \"amount:sum,amount:count\" --order sum_amount --desc", path.display());
            println!();
            println!("  Or serve it to a front-end:");
            println!("    csvx serve --root {}", dir.display());
            0
        }
        Err(e) => report_failure("pivot", e), // engine returned the error itself
    }
}
