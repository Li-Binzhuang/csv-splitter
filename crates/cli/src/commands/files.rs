use crate::{human_bytes, print_command_help, report_failure, Parser};
use std::path::PathBuf;

pub(crate) fn cmd_files(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    let mut exts = "csv,tsv,txt,gz,zst,zip,bz2,xz".to_string();
    let mut json = false;

    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--root" => match p.value("--root") {
                Ok(v) => root = Some(PathBuf::from(v)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--ext" => match p.value("--ext") {
                Ok(v) => exts = v,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--json" => json = true,
            "-h" | "--help" => {
                print_command_help("files");
                return 0;
            }
            other => root = Some(PathBuf::from(other)),
        }
    }

    let Some(root) = root else {
        eprintln!("error: files needs --root <dir>");
        return 2;
    };
    let listing = match csvx_core::listing::list(&root, &exts, 200) {
        Ok(l) => l,
        Err(e) => {
            return report_failure("files", e);
        }
    };

    if json {
        let items: Vec<String> = listing
            .files
            .iter()
            .map(|f| {
                format!(
                    r#"{{"path":"{}","bytes":{},"modified":{}}}"#,
                    f.relative.replace('"', "\\\""),
                    f.bytes,
                    f.modified_secs
                )
            })
            .collect();
        println!(
            r#"{{"root":"{}","truncated":{},"files":[{}]}}"#,
            listing.root.display(),
            listing.truncated,
            items.join(",")
        );
        return 0;
    }

    println!(
        "{} file(s) under {}{}",
        listing.files.len(),
        listing.root.display(),
        if listing.truncated {
            " (truncated)"
        } else {
            ""
        }
    );
    for f in listing.files {
        println!("{:>12}  {}", human_bytes(f.bytes), f.relative);
    }
    0
}
