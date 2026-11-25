use crate::{print_command_help, Parser};
use csvx_agent as agent;
use std::path::PathBuf;

pub(crate) fn cmd_agent(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut root: Option<PathBuf> = None;
    let mut addr = "127.0.0.1:8788".to_string();
    let mut action = "serve".to_string();
    let mut endpoint: Option<String> = None;
    let mut model: Option<String> = None;
    let mut prompt: Option<String> = None;
    let mut max_steps = 4usize;
    let mut allow_apply = false;
    let mut skill: Option<PathBuf> = None;
    let mut trace: Option<PathBuf> = None;
    let mut replay_trace: Option<PathBuf> = None;
    if let Some(first) = args.first().filter(|arg| !arg.starts_with('-')) {
        action = first.clone();
    }
    while let Some(arg) = p.next() {
        match arg.as_str() {
            "serve" | "manifest" | "run" | "replay" => action = arg,
            "--trace" => match p.value("--trace") {
                Ok(value) => trace = Some(PathBuf::from(value)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--replay" => match p.value("--replay") {
                Ok(value) => replay_trace = Some(PathBuf::from(value)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--endpoint" => match p.value("--endpoint") {
                Ok(value) => endpoint = Some(value),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--model" => match p.value("--model") {
                Ok(value) => model = Some(value),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--prompt" => match p.value("--prompt") {
                Ok(value) => prompt = Some(value),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--max-steps" => match p.value("--max-steps").and_then(|value| {
                value
                    .parse::<usize>()
                    .map_err(|_| format!("--max-steps takes a number, not {value:?}"))
            }) {
                Ok(value) => max_steps = value,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--allow-apply" => allow_apply = true,
            "--skill" => match p.value("--skill") {
                Ok(value) => skill = Some(PathBuf::from(value)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--root" => match p.value("--root") {
                Ok(value) => root = Some(PathBuf::from(value)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--addr" => match p.value("--addr") {
                Ok(value) => addr = value,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--port" => match p.value("--port").and_then(|value| {
                value
                    .parse::<u16>()
                    .map(|port| format!("127.0.0.1:{port}"))
                    .map_err(|_| format!("--port takes a number, not {value:?}"))
            }) {
                Ok(value) => addr = value,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                print_command_help("agent");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => root = Some(PathBuf::from(other)),
        }
    }
    if action == "manifest" {
        println!("{}", agent::manifest());
        return 0;
    }
    if action == "replay" {
        let (Some(root), Some(trace)) = (root, replay_trace) else {
            eprintln!("error: agent replay needs --root and --replay");
            return 2;
        };
        return match agent::replay(&trace, &root) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("error: {error}");
                1
            }
        };
    }
    if action == "run" {
        let (Some(endpoint), Some(model), Some(prompt), Some(root)) =
            (endpoint, model, prompt, root)
        else {
            eprintln!("error: agent run needs --root, --endpoint, --model and --prompt");
            return 2;
        };
        return match agent::run(
            &root,
            &endpoint,
            &model,
            &prompt,
            skill.as_deref(),
            max_steps.clamp(1, 12),
            allow_apply,
            trace.as_deref(),
        ) {
            Ok(answer) => {
                println!("{answer}");
                0
            }
            Err(error) => {
                eprintln!("error: {error}");
                1
            }
        };
    }
    let Some(root) = root else {
        eprintln!("error: agent serve needs --root <dir>");
        return 2;
    };
    match agent::serve(&root, &addr) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
