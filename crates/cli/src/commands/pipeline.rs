use crate::commands::pipeline_history::{self, HistoryStep};
use crate::commands::shared::{json_escape, json_quote};
use crate::{print_command_help, Parser};
use std::io::{Read, Write};
use std::thread;
use std::time::{Duration, Instant};

/// Topological scheduler: ready steps (all dependencies done) are freshness-checked;
/// fresh ones reuse their prior rows and stale ones run as one concurrent wave, bounded
/// by `jobs`. Unlike the linear path, ordering follows the dependency graph, not the
/// manifest row order.
#[allow(clippy::too_many_arguments)]
fn dag_output_backup(output: &std::path::Path, stamp: &str, index: usize) -> std::path::PathBuf {
    output.with_file_name(format!(
        ".{}.dag-backup-{stamp}-{index}",
        output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("out")
    ))
}

fn restore_dag_outputs(touched: &[(std::path::PathBuf, Option<std::path::PathBuf>)]) {
    for (output, backup) in touched {
        let _ = std::fs::remove_file(output);
        if let Some(backup) = backup {
            let _ = std::fs::rename(backup, output);
        }
    }
}

fn print_step_marker(name: &str, fresh: bool, index: usize, total: usize) {
    println!(
        "CSVX_STEP {} {name} {}/{}",
        if fresh { "skip" } else { "done" },
        index + 1,
        total
    );
}

#[allow(clippy::too_many_arguments)]
fn run_pipeline_dag(
    steps: &[PipelineStep],
    root: &std::path::Path,
    force: bool,
    step_timeout_ms: u64,
    retries: usize,
    retry_delay_ms: u64,
    jobs: usize,
    previous: &[PipelineStateRow],
    state_path: &std::path::Path,
    progress_markers: bool,
    content_check: bool,
) -> std::io::Result<PipelineResults> {
    use std::collections::HashSet;
    let dependencies: Vec<Vec<usize>> = (0..steps.len())
        .map(|index| {
            pipeline_dependencies(steps, index)
                .into_iter()
                .filter_map(|name| steps.iter().position(|step| step.name == name))
                .collect()
        })
        .collect();
    let mut done = HashSet::new();
    let mut results = Vec::with_capacity(steps.len());
    let mut state_rows = Vec::with_capacity(steps.len());
    let stamp = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    );
    let mut touched: Vec<(std::path::PathBuf, Option<std::path::PathBuf>)> = Vec::new();
    while done.len() < steps.len() {
        let ready: Vec<usize> = (0..steps.len())
            .filter(|index| {
                !done.contains(index)
                    && dependencies[*index]
                        .iter()
                        .all(|dependency| done.contains(dependency))
            })
            .collect();
        if ready.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "pipeline dependency cycle",
            ));
        }
        let mut stale = Vec::new();
        for &index in &ready {
            let input_path = pipeline_resolve_input(&steps[index], steps, root)?;
            match pipeline_fresh_row(&steps[index], &input_path, previous, force, content_check)? {
                Some(row) => {
                    results.push((
                        steps[index].name.clone(),
                        true,
                        0,
                        steps[index].output.display().to_string(),
                    ));
                    state_rows.push(row);
                    done.insert(index);
                }
                None => stale.push(index),
            }
        }
        if stale.is_empty() {
            continue;
        }
        let wave: Vec<usize> = stale.into_iter().take(jobs).collect();
        for &index in &wave {
            let output = &steps[index].output;
            if output.exists() {
                let backup = dag_output_backup(output, &stamp, index);
                let _ = std::fs::remove_file(&backup);
                if let Err(error) = std::fs::rename(output, &backup) {
                    restore_dag_outputs(&touched);
                    return Err(error);
                }
                touched.push((output.clone(), Some(backup)));
            } else {
                touched.push((output.clone(), None));
            }
        }
        let outcomes = match run_branch_group(
            steps,
            &wave,
            root,
            force,
            step_timeout_ms,
            retries,
            retry_delay_ms,
        ) {
            Ok(outcomes) => outcomes,
            Err(error) => {
                restore_dag_outputs(&touched);
                return Err(error);
            }
        };
        for (offset, (name, fresh, ms, output, state_row)) in outcomes.into_iter().enumerate() {
            if progress_markers {
                print_step_marker(&name, fresh, wave[offset], steps.len());
            }
            results.push((name, fresh, ms, output));
            state_rows.push(state_row);
            done.insert(wave[offset]);
        }
    }
    for (_, backup) in &touched {
        if let Some(backup) = backup {
            let _ = std::fs::remove_file(backup);
        }
    }
    save_pipeline_state(state_path, &state_rows)?;
    Ok(results)
}

fn normalize_lexical_path(path: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    out
}

fn normalize_plan_path(path: &std::path::Path) -> std::path::PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| normalize_lexical_path(path))
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn read_watch_round(path: &std::path::Path) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let Some(rest) = text.split(r#""round":"#).nth(1) else {
        return 0;
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<usize>().unwrap_or(0)
}

fn read_watch_failures(path: &std::path::Path) -> usize {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let Some(rest) = text.split(r#""consecutiveFailures":"#).nth(1) else {
        return 0;
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<usize>().unwrap_or(0)
}

fn read_watch_last_error(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let rest = text.split(r#""lastError":""#).nth(1)?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('u') => {
                    let _ = chars.by_ref().take(4).count();
                    out.push('?');
                }
                Some(other) => out.push(other),
                None => break,
            },
            other => out.push(other),
        }
    }
    (!out.is_empty()).then_some(out)
}

fn read_watch_next_delay(path: &std::path::Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let rest = text.split(r#""nextDelayMs":"#).nth(1)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn read_watch_previous_failure(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text.contains(r#""success":false"#))
        .unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
fn write_watch_state(
    path: &std::path::Path,
    round: usize,
    ran: usize,
    total: usize,
    success: bool,
    failures: usize,
    last_success_ms: u128,
    last_failure_ms: u128,
    next_delay_ms: u64,
    last_error: &str,
) -> std::io::Result<()> {
    let body = format!(
        r#"{{"round":{},"ran":{},"total":{},"success":{},"consecutiveFailures":{},"lastSuccessAtMs":{},"lastFailureAtMs":{},"nextDelayMs":{},"lastError":"{}","updatedAtMs":{}}}"#,
        round,
        ran,
        total,
        success,
        failures,
        last_success_ms,
        last_failure_ms,
        next_delay_ms,
        json_escape(last_error),
        now_millis()
    );
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, body)?;
    std::fs::rename(temp, path)
}

#[allow(clippy::too_many_arguments)]
fn watch_pipeline(
    plan: &std::path::Path,
    root: Option<&std::path::Path>,
    force: bool,
    dry_run: bool,
    step_timeout_ms: u64,
    retries: usize,
    retry_delay_ms: u64,
    jobs: usize,
    dag: bool,
    clean: bool,
    unlock: bool,
    progress_markers: bool,
    content_check: bool,
    json: bool,
    interval_ms: u64,
    backoff_max_ms: u64,
    iterations: usize,
    max_failures: usize,
    state_path: Option<&std::path::Path>,
    verbose: bool,
) -> std::io::Result<()> {
    let interval_ms = interval_ms.max(10);
    let backoff_max_ms = backoff_max_ms.max(interval_ms);
    let mut delay_ms = interval_ms;
    let mut failures = state_path.map(read_watch_failures).unwrap_or(0);
    let mut last_success_ms = 0u128;
    let mut last_failure_ms = 0u128;
    let mut round = state_path.map(read_watch_round).unwrap_or(0);
    if let Some(path) = state_path {
        if failures > 0 {
            eprintln!("watch: previous state has {failures} consecutive failure(s)");
            if let Some(error) = read_watch_last_error(path) {
                eprintln!("watch: previous failure: {error}");
            }
            if max_failures > 0 && failures >= max_failures {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("watch: previous state already reached the failure cap ({failures}); remove {} to restart", path.display()),
                ));
            }
            if let Some(saved) = read_watch_next_delay(path) {
                delay_ms = saved.clamp(interval_ms, backoff_max_ms);
            }
        } else if read_watch_previous_failure(path) {
            eprintln!("watch: previous state ended in failure");
        }
    }
    loop {
        round += 1;
        match run_pipeline_plan(
            plan,
            root,
            force,
            dry_run,
            step_timeout_ms,
            retries,
            retry_delay_ms,
            jobs,
            dag,
            clean,
            unlock,
            progress_markers,
            content_check,
        ) {
            Ok((results, _)) => {
                let ran = results.iter().filter(|(_, fresh, _, _)| !*fresh).count();
                if json {
                    let steps = results
                        .iter()
                        .map(|(name, fresh, _, detail)| {
                            format!(
                                r#"{{"step":{},"fresh":{},"detail":{}}}"#,
                                json_quote(name),
                                fresh,
                                json_quote(detail)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    println!(
                        r#"{{"watch":true,"round":{},"success":true,"ran":{},"total":{},"consecutiveFailures":0,"nextDelayMs":{},"steps":[{}]}}"#,
                        round,
                        ran,
                        results.len(),
                        interval_ms,
                        steps
                    );
                } else {
                    if ran > 0 || verbose {
                        println!("watch: round {round} ran {ran}/{} step(s)", results.len());
                    }
                    for (name, fresh, _, detail) in &results {
                        if !*fresh {
                            println!("→ {name} ({detail})");
                        }
                    }
                }
                delay_ms = interval_ms;
                failures = 0;
                last_success_ms = now_millis();
                if let Some(path) = state_path {
                    if let Err(error) = write_watch_state(
                        path,
                        round,
                        ran,
                        results.len(),
                        true,
                        0,
                        last_success_ms,
                        last_failure_ms,
                        interval_ms,
                        "",
                    ) {
                        eprintln!("watch: state write failed: {error}");
                    }
                }
            }
            Err(error) => {
                eprintln!("watch: {error}");
                failures += 1;
                last_failure_ms = now_millis();
                let next_delay_ms = delay_ms.saturating_mul(2).min(backoff_max_ms);
                if json {
                    println!(
                        r#"{{"watch":true,"round":{},"success":false,"consecutiveFailures":{},"nextDelayMs":{},"error":{}}}"#,
                        round,
                        failures,
                        next_delay_ms,
                        json_quote(&error.to_string())
                    );
                }
                if let Some(path) = state_path {
                    if let Err(write_error) = write_watch_state(
                        path,
                        round,
                        0,
                        0,
                        false,
                        failures,
                        last_success_ms,
                        last_failure_ms,
                        next_delay_ms,
                        &error.to_string(),
                    ) {
                        eprintln!("watch: state write failed: {write_error}");
                    }
                }
                if iterations > 0 {
                    return Err(error);
                }
                if max_failures > 0 && failures >= max_failures {
                    eprintln!("watch: stopping after {failures} consecutive failure(s)");
                    return Err(error);
                }
                delay_ms = next_delay_ms;
            }
        }
        if iterations > 0 && round >= iterations {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    }
}

#[allow(clippy::too_many_arguments)]
fn run_pipeline_plan(
    plan: &std::path::Path,
    root: Option<&std::path::Path>,
    force: bool,
    dry_run: bool,
    step_timeout_ms: u64,
    retries: usize,
    retry_delay_ms: u64,
    jobs: usize,
    dag: bool,
    clean: bool,
    unlock: bool,
    progress_markers: bool,
    content_check: bool,
) -> std::io::Result<(PipelineResults, PipelineLineage)> {
    let root = root
        .map(std::path::Path::to_path_buf)
        .or_else(|| plan.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let plan_hash = pipeline_hash_file(plan).unwrap_or_default();
    let reader = csvx_core::encoding::open_decoded(plan)?;
    let mut records = csvx_core::csv_reader::RecordReader::new(
        std::io::BufReader::with_capacity(256 * 1024, reader),
        256 * 1024,
    );
    let mut record = Vec::new();
    if !records.next_record(&mut record)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the pipeline plan is empty",
        ));
    }
    let header: Vec<String> = csvx_core::csv_reader::parse_csv_record_with(
        &String::from_utf8_lossy(&record),
        b',',
        csvx_core::csv_reader::QuoteStyle::Rfc4180,
    );
    let expected = ["step", "input", "command", "args", "output"];
    let header_ok = (header.len() == 5 || header.len() == 6)
        && header
            .iter()
            .take(5)
            .zip(expected)
            .all(|(cell, want)| cell.trim() == want)
        && header.get(5).is_none_or(|cell| cell.trim() == "group");
    if !header_ok {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the pipeline plan header must be step,input,command,args,output[,group]",
        ));
    }
    let mut steps: Vec<PipelineStep> = Vec::new();
    while records.next_record(&mut record)? {
        let cells: Vec<String> = csvx_core::csv_reader::parse_csv_record_with(
            &String::from_utf8_lossy(&record),
            b',',
            csvx_core::csv_reader::QuoteStyle::Rfc4180,
        );
        if cells.iter().all(|cell| cell.trim().is_empty()) {
            continue;
        }
        if cells.len() != 5 && cells.len() != 6 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a pipeline step needs 5 or 6 columns",
            ));
        }
        let name = cells[0].trim().to_string();
        if name.is_empty() || cells[2].trim().is_empty() || cells[4].trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a pipeline step needs a name, command and output",
            ));
        }
        if steps.iter().any(|step| step.name == name) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("duplicate pipeline step {name}"),
            ));
        }
        steps.push(PipelineStep {
            name,
            input: if cells[1].trim().is_empty() {
                None
            } else {
                Some(cells[1].trim().to_string())
            },
            command: cells[2].trim().to_string(),
            args: cells[3].to_string(),
            output: root.join(cells[4].trim()),
            group: cells
                .get(5)
                .map(|cell| cell.trim().to_string())
                .unwrap_or_default(),
        });
    }
    if steps.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the pipeline plan has no steps",
        ));
    }
    for (index, step) in steps.iter().enumerate() {
        if let Some(input) = &step.input {
            if let Some(position) = steps.iter().position(|candidate| &candidate.name == input) {
                if position >= index {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "step {} refers to {} which is not an earlier step",
                            step.name, input
                        ),
                    ));
                }
            }
        }
    }
    for (index, step) in steps.iter().enumerate() {
        if let Some(other) = steps[index + 1..].iter().find(|candidate| {
            normalize_lexical_path(&candidate.output) == normalize_lexical_path(&step.output)
        }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "steps {} and {} declare the same output {}",
                    step.name,
                    other.name,
                    step.output.display()
                ),
            ));
        }
    }
    for step in &steps {
        for candidate in &steps {
            let Some(input) = candidate.input.as_deref() else {
                continue;
            };
            if steps.iter().any(|known| known.name == input) {
                continue;
            }
            if normalize_plan_path(&root.join(input)) == normalize_plan_path(&step.output) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "step {} writes the source input {} in place",
                        step.name,
                        step.output.display()
                    ),
                ));
            }
        }
    }
    if unlock {
        let _ = std::fs::remove_file(format!("{}.pipeline.lock", plan.display()));
    }
    let _lock = acquire_pipeline_lock(plan)?;
    let recovered = recover_dag_backups(&steps)?;
    if recovered > 0 {
        eprintln!("recovered {recovered} interrupted DAG output(s)");
    }
    let cleaned = cleanup_stale_staging(&steps, clean)?;
    if cleaned > 0 {
        eprintln!("cleaned {cleaned} stale pipeline staging file(s)");
    }
    let state_path = pipeline_state_path(plan);
    let previous = load_pipeline_state(&state_path);
    let bin = std::env::current_exe()?;
    let mut state_rows = Vec::with_capacity(steps.len());
    let mut results: Vec<(String, bool, u64, String)> = Vec::with_capacity(steps.len());
    let mut previous_output: Option<std::path::PathBuf> = None;
    if dag && jobs > 1 && !dry_run {
        let results = run_pipeline_dag(
            &steps,
            &root,
            force,
            step_timeout_ms,
            retries,
            retry_delay_ms,
            jobs,
            &previous,
            &state_path,
            progress_markers,
            content_check,
        )?;
        let lineage = steps
            .iter()
            .enumerate()
            .map(|(index, step)| (step.name.clone(), pipeline_dependencies(&steps, index)))
            .collect();
        save_pipeline_lineage(plan, &lineage)?;
        return Ok((results, lineage));
    }
    let mut skip = 0usize;
    for (index, step) in steps.iter().enumerate() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if jobs > 1 && !step.group.is_empty() && !dry_run {
            let mut end = index + 1;
            while end < steps.len() && steps[end].group == step.group {
                end += 1;
            }
            let mut fresh_rows = Vec::with_capacity(end - index);
            let mut stale = Vec::new();
            for (offset, candidate) in steps[index..end].iter().enumerate() {
                let input_path = pipeline_resolve_input(candidate, &steps, &root)?;
                if let Some(row) =
                    pipeline_fresh_row(candidate, &input_path, &previous, force, content_check)?
                {
                    fresh_rows.push((offset, Some(row)));
                } else {
                    stale.push(index + offset);
                    fresh_rows.push((offset, None));
                }
            }
            let outcomes = if stale.is_empty() {
                Vec::new()
            } else {
                run_branch_group(
                    &steps,
                    &stale,
                    &root,
                    force,
                    step_timeout_ms,
                    retries,
                    retry_delay_ms,
                )?
            };
            let mut outcomes = outcomes.into_iter();
            for (offset, (_, fresh_row)) in fresh_rows.into_iter().enumerate() {
                let step = &steps[index + offset];
                if let Some(row) = fresh_row {
                    if !dry_run && progress_markers {
                        print_step_marker(step.name.as_str(), true, index + offset, steps.len());
                    }
                    results.push((
                        step.name.clone(),
                        true,
                        0,
                        step.output.display().to_string(),
                    ));
                    state_rows.push(row);
                } else {
                    let (name, fresh, ms, output, state_row) =
                        outcomes.next().expect("one outcome per stale step");
                    if !dry_run && progress_markers {
                        print_step_marker(&name, fresh, index + offset, steps.len());
                    }
                    results.push((name, fresh, ms, output));
                    state_rows.push(state_row);
                }
            }
            previous_output = Some(steps[end - 1].output.clone());
            skip = end - index - 1;
            continue;
        }
        let input_path = match &step.input {
            None => previous_output.clone().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the first pipeline step needs an input",
                )
            })?,
            Some(name) => steps
                .iter()
                .find(|candidate| &candidate.name == name)
                .map(|candidate| candidate.output.clone())
                .unwrap_or_else(|| root.join(name)),
        };
        if !input_path.exists() {
            if dry_run {
                results.push((
                    step.name.clone(),
                    false,
                    0,
                    format!("missing input {}", input_path.display()),
                ));
                previous_output = Some(step.output.clone());
                continue;
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "step {}: input {} not found",
                    step.name,
                    input_path.display()
                ),
            ));
        }
        let input_string = input_path.display().to_string();
        let args_hash = pipeline_hash_text(&step.args);
        let input_stamp = pipeline_file_stamp(&input_path)?;
        let output_exists = step.output.exists();
        let output_stamp = if output_exists {
            pipeline_file_stamp(&step.output)?
        } else {
            String::new()
        };
        let hash_content = content_check || pipeline_auto_content_check(&input_path, &step.output);
        let prior = previous.iter().find(|row| {
            row.name == step.name
                && row.command == step.command
                && row.args == step.args
                && row.input == input_string
        });
        let fresh = !force
            && match prior {
                Some(row) => {
                    if hash_content && !row.input_hash.is_empty() && !row.output_hash.is_empty() {
                        output_exists
                            && pipeline_hash_file(&input_path)? == row.input_hash
                            && pipeline_hash_file(&step.output)
                                .map(|hash| hash == row.output_hash)
                                .unwrap_or(false)
                    } else if !row.input_stamp.is_empty() && !row.output_stamp.is_empty() {
                        // Fast path: metadata alone proves the input and output unchanged.
                        output_exists
                            && row.input_stamp == input_stamp
                            && row.output_stamp == output_stamp
                    } else {
                        // Legacy state without stamps: fall back to content hashes.
                        let input_hash = pipeline_hash_file(&input_path)?;
                        output_exists
                            && input_hash == row.input_hash
                            && pipeline_hash_file(&step.output)
                                .map(|hash| hash == row.output_hash)
                                .unwrap_or(false)
                    }
                }
                None => false,
            };
        if fresh {
            let row = prior.expect("fresh requires a prior state row");
            let output_string = step.output.display().to_string();
            if !dry_run && progress_markers {
                print_step_marker(&step.name, true, index, steps.len());
            }
            results.push((step.name.clone(), true, 0, output_string.clone()));
            state_rows.push(PipelineStateRow {
                name: step.name.clone(),
                command: step.command.clone(),
                args: step.args.clone(),
                input: input_string,
                output: output_string,
                input_hash: row.input_hash.clone(),
                args_hash,
                output_hash: row.output_hash.clone(),
                elapsed_ms: 0,
                input_stamp,
                output_stamp,
            });
            previous_output = Some(step.output.clone());
            continue;
        }
        if dry_run {
            results.push((step.name.clone(), false, 0, "would run".to_string()));
            previous_output = Some(step.output.clone());
            continue;
        }
        let input_hash = pipeline_hash_file(&input_path)?;
        if let Some(parent) = step.output.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let temp = step.output.with_file_name(format!(
            ".{}.pipeline.tmp",
            step.output
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("out")
        ));
        let _ = std::fs::remove_file(&temp);
        let out_string = temp.display().to_string();
        let args = split_pipeline_args(&step.args)
            .into_iter()
            .map(|arg| {
                arg.replace("{in}", &input_string)
                    .replace("{out}", &out_string)
            })
            .collect::<Vec<_>>();
        let uses_out = step.args.contains("{out}");
        let command = split_pipeline_args(&step.command);
        let mut output = None;
        let mut elapsed_ms = 0u64;
        for attempt in 0..=retries {
            let _ = std::fs::remove_file(&temp);
            let mut process = std::process::Command::new(&bin);
            process.arg(command.first().map(String::as_str).unwrap_or(""));
            process.arg(&input_path);
            if command.len() > 1 {
                process.args(&command[1..]);
            }
            process.args(&args);
            let started = Instant::now();
            match run_step_command(process, step_timeout_ms) {
                Ok(result) => {
                    elapsed_ms = started.elapsed().as_millis() as u64;
                    if result.status.success() {
                        let _ = pipeline_history::append_attempt(
                            &root,
                            plan,
                            &plan_hash,
                            &step.name,
                            attempt + 1,
                            elapsed_ms,
                            true,
                            "ok",
                            "",
                            None,
                        );
                        output = Some(result);
                        break;
                    }
                    let last = format!(
                        "step {} failed: {}",
                        step.name,
                        String::from_utf8_lossy(&result.stderr).trim()
                    );
                    let _ = pipeline_history::append_attempt(
                        &root,
                        plan,
                        &plan_hash,
                        &step.name,
                        attempt + 1,
                        elapsed_ms,
                        false,
                        "step_failed",
                        &last,
                        Some(retry_delay_ms),
                    );
                    if attempt < retries {
                        eprintln!("{last}; retrying ({}/{})", attempt + 1, retries);
                        thread::sleep(Duration::from_millis(retry_delay_ms));
                        continue;
                    }
                    let _ = std::fs::remove_file(&temp);
                    return Err(std::io::Error::other(format!(
                        "{last} (after {} attempts)",
                        attempt + 1
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    let last = format!("step {} timed out after {} ms", step.name, step_timeout_ms);
                    let _ = pipeline_history::append_attempt(
                        &root,
                        plan,
                        &plan_hash,
                        &step.name,
                        attempt + 1,
                        step_timeout_ms,
                        false,
                        "timeout",
                        &last,
                        Some(retry_delay_ms),
                    );
                    if attempt < retries {
                        eprintln!("{last}; retrying ({}/{})", attempt + 1, retries);
                        thread::sleep(Duration::from_millis(retry_delay_ms));
                        continue;
                    }
                    let _ = std::fs::remove_file(&temp);
                    return Err(std::io::Error::other(format!(
                        "{last} (after {} attempts)",
                        attempt + 1
                    )));
                }
                Err(error) => {
                    let _ = pipeline_history::append_attempt(
                        &root,
                        plan,
                        &plan_hash,
                        &step.name,
                        attempt + 1,
                        started.elapsed().as_millis() as u64,
                        false,
                        "spawn_error",
                        &error.to_string(),
                        None,
                    );
                    let _ = std::fs::remove_file(&temp);
                    return Err(error);
                }
            }
        }
        let output = output.expect("successful attempt sets output");
        let _ = &output;
        if uses_out {
            if !temp.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("step {} did not write its output", step.name),
                ));
            }
        } else {
            use std::io::Write;
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(&output.stdout)?;
            file.flush()?;
        }
        std::fs::rename(&temp, &step.output)?;
        let output_hash = pipeline_hash_file(&step.output)?;
        state_rows.push(PipelineStateRow {
            name: step.name.clone(),
            command: step.command.clone(),
            args: step.args.clone(),
            input: input_string,
            output: step.output.display().to_string(),
            input_hash,
            args_hash,
            output_hash,
            elapsed_ms,
            input_stamp,
            output_stamp: pipeline_file_stamp(&step.output)?,
        });
        if !dry_run && progress_markers {
            print_step_marker(&step.name, false, index, steps.len());
        }
        results.push((
            step.name.clone(),
            false,
            elapsed_ms,
            step.output.display().to_string(),
        ));
        previous_output = Some(step.output.clone());
        if index + 1 == steps.len() {
            break;
        }
    }
    if !dry_run {
        save_pipeline_state(&state_path, &state_rows)?;
    }
    let lineage = steps
        .iter()
        .enumerate()
        .map(|(index, step)| (step.name.clone(), pipeline_dependencies(&steps, index)))
        .collect();
    if !dry_run {
        save_pipeline_lineage(plan, &lineage)?;
    }
    Ok((results, lineage))
}

fn pipeline_history_root(
    plan: &std::path::Path,
    root: Option<&std::path::Path>,
) -> std::path::PathBuf {
    root.map(std::path::Path::to_path_buf)
        .or_else(|| plan.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

pub(crate) fn cmd_pipeline(args: &[String]) -> i32 {
    let mut p = Parser::new(args);
    let mut action: Option<String> = None;
    let mut plan: Option<std::path::PathBuf> = None;
    let mut root: Option<std::path::PathBuf> = None;
    let mut force = false;
    let mut dry_run = false;
    let mut step_timeout_ms = 0u64;
    let mut retries = 0usize;
    let mut retry_delay_ms = 250u64;
    let mut jobs = 1usize;
    let mut dag = false;
    let mut clean = false;
    let mut unlock = false;
    let mut verify = false;
    let mut fail_on_stale = false;
    let mut progress_markers = false;
    let mut content_check = false;
    let mut json = false;
    let mut watch_interval_ms = 5_000u64;
    let mut watch_backoff_max_ms = 60_000u64;
    let mut watch_iterations = 0usize;
    let mut watch_max_failures = 0usize;
    let mut watch_state: Option<std::path::PathBuf> = None;
    let mut verbose = false;
    while let Some(arg) = p.next() {
        match arg.as_str() {
            "--root" => match p.value("--root") {
                Ok(value) => root = Some(std::path::PathBuf::from(value)),
                Err(e) => {
                    eprintln!("error: {e}");
                    return 2;
                }
            },
            "--force" => force = true,
            "--dry-run" => dry_run = true,
            "--progress-markers" => progress_markers = true,
            "--content-check" => content_check = true,
            "--fail-on-stale" => fail_on_stale = true,
            "--verify" => verify = true,
            "--unlock" => unlock = true,
            "--clean" => clean = true,
            "--dag" => dag = true,
            "--jobs" => match p
                .value("--jobs")
                .and_then(|value| value.parse::<usize>().map_err(|_| "bad --jobs".to_string()))
            {
                Ok(value) => jobs = value.max(1),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--retries" => match p.value("--retries").and_then(|value| {
                value
                    .parse::<usize>()
                    .map_err(|_| "bad --retries".to_string())
            }) {
                Ok(value) => retries = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--retry-delay" => match p.value("--retry-delay").and_then(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| "bad --retry-delay (milliseconds)".to_string())
            }) {
                Ok(value) => retry_delay_ms = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--step-timeout" => match p.value("--step-timeout").and_then(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| "bad --step-timeout (milliseconds)".to_string())
            }) {
                Ok(value) => step_timeout_ms = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--json" => json = true,
            "--interval-ms" => match p.value("--interval-ms").and_then(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| "bad --interval-ms".to_string())
            }) {
                Ok(value) => watch_interval_ms = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--iterations" => match p.value("--iterations").and_then(|value| {
                value
                    .parse::<usize>()
                    .map_err(|_| "bad --iterations".to_string())
            }) {
                Ok(value) => watch_iterations = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--backoff-max-ms" => match p.value("--backoff-max-ms").and_then(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| "bad --backoff-max-ms".to_string())
            }) {
                Ok(value) => watch_backoff_max_ms = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--max-failures" => match p.value("--max-failures").and_then(|value| {
                value
                    .parse::<usize>()
                    .map_err(|_| "bad --max-failures".to_string())
            }) {
                Ok(value) => watch_max_failures = value,
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--state" => match p.value("--state") {
                Ok(value) => watch_state = Some(std::path::PathBuf::from(value)),
                Err(error) => {
                    eprintln!("error: {error}");
                    return 2;
                }
            },
            "--verbose" => verbose = true,
            "-h" | "--help" => {
                print_command_help("pipeline");
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option: {other}");
                return 2;
            }
            other => {
                if action.is_none() {
                    action = Some(other.to_string());
                } else if plan.is_none() {
                    plan = Some(std::path::PathBuf::from(other));
                } else {
                    eprintln!("error: unexpected argument: {other}");
                    return 2;
                }
            }
        }
    }
    let Some(action) = action else {
        eprintln!("error: pipeline needs run|status|watch <plan>");
        return 2;
    };
    if action == "history" {
        let plan = plan.unwrap_or_else(|| std::path::PathBuf::from("."));
        let root = pipeline_history_root(&plan, root.as_deref());
        match pipeline_history::read(&root) {
            Ok(text) if text.trim().is_empty() => println!("(no pipeline history yet)"),
            Ok(text) => print!("{text}"),
            Err(error) => {
                eprintln!("error: {error}");
                return 1;
            }
        }
        return 0;
    }
    let Some(plan) = plan else {
        eprintln!("error: pipeline needs a plan file");
        return 2;
    };
    match action.as_str() {
        "watch" => match watch_pipeline(
            &plan,
            root.as_deref(),
            force,
            dry_run,
            step_timeout_ms,
            retries,
            retry_delay_ms,
            jobs,
            dag,
            clean,
            unlock,
            progress_markers,
            content_check,
            json,
            watch_interval_ms,
            watch_backoff_max_ms,
            watch_iterations,
            watch_max_failures,
            watch_state.as_deref(),
            verbose,
        ) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("error: {error}");
                1
            }
        },
        "run" => {
            let started_at = now_millis();
            let history_root = pipeline_history_root(&plan, root.as_deref());
            let plan_hash = pipeline_hash_file(&plan).unwrap_or_default();
            match run_pipeline_plan(
                &plan,
                root.as_deref(),
                force,
                dry_run,
                step_timeout_ms,
                retries,
                retry_delay_ms,
                jobs,
                dag,
                clean,
                unlock,
                progress_markers,
                content_check,
            ) {
                Ok((results, lineage)) => {
                    if !dry_run {
                        let steps: Vec<HistoryStep> = results
                            .iter()
                            .map(|(name, fresh, ms, _)| HistoryStep {
                                name: name.clone(),
                                fresh: *fresh,
                                ms: *ms,
                            })
                            .collect();
                        if let Err(error) = pipeline_history::append_run(
                            &history_root,
                            &plan,
                            &plan_hash,
                            started_at,
                            now_millis(),
                            true,
                            None,
                            None,
                            &steps,
                        ) {
                            eprintln!("warning: pipeline history write failed: {error}");
                        }
                    }
                    if dry_run {
                        if json {
                            let items = results
                                .iter()
                                .enumerate()
                                .map(|(index, (name, fresh, _, detail))| {
                                    let deps = lineage
                                        .get(index)
                                        .map(|(_, deps)| deps)
                                        .unwrap_or(&Vec::new())
                                        .iter()
                                        .map(|dep| json_quote(dep))
                                        .collect::<Vec<_>>()
                                        .join(",");
                                    format!(
                                        r#"{{"step":{},"fresh":{},"detail":{},"deps":[{}]}}"#,
                                        json_quote(name),
                                        fresh,
                                        json_quote(detail),
                                        deps
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join(",");
                            println!(r#"{{"dryRun":true,"steps":[{items}]}}"#);
                        } else {
                            for (index, (name, fresh, _, detail)) in results.iter().enumerate() {
                                let deps = lineage
                                    .get(index)
                                    .map(|(_, deps)| deps.clone())
                                    .unwrap_or_default();
                                if progress_markers {
                                    println!(
                                        "CSVX_STEP {} {name} {}/{}",
                                        if *fresh { "skip" } else { "done" },
                                        index + 1,
                                        results.len()
                                    );
                                }
                                if *fresh {
                                    println!("↷ {name}");
                                } else if deps.is_empty() {
                                    println!("→ {name} ({detail})");
                                } else {
                                    println!("→ {name} ({detail}; deps: {})", deps.join(","));
                                }
                            }
                        }
                        return 0;
                    }
                    if json {
                        let items: Vec<String> = results
                            .iter()
                            .map(|(name, fresh, ms, output)| {
                                format!(
                                    r#"{{"step":"{}","fresh":{},"ms":{},"output":"{}"}}"#,
                                    name,
                                    fresh,
                                    ms,
                                    output.replace('\\', "\\\\")
                                )
                            })
                            .collect();
                        println!(r#"{{"steps":[{}]}}"#, items.join(","));
                    } else {
                        for (name, fresh, ms, _) in results.iter() {
                            if *fresh {
                                println!("↷ {name}");
                            } else {
                                println!("✓ {name} ({ms} ms)");
                            }
                        }
                    }
                    0
                }
                Err(e) => {
                    if !dry_run {
                        if let Err(error) = pipeline_history::append_run(
                            &history_root,
                            &plan,
                            &plan_hash,
                            started_at,
                            now_millis(),
                            false,
                            Some("pipeline_error"),
                            Some(&e.to_string()),
                            &[],
                        ) {
                            eprintln!("warning: pipeline history write failed: {error}");
                        }
                    }
                    eprintln!("error: {e}");
                    1
                }
            }
        }
        "status" => {
            let rows = load_pipeline_state(&pipeline_state_path(&plan));
            if rows.is_empty() {
                println!("(no pipeline state yet)");
                return 0;
            }
            let stale: Vec<Option<String>> = if verify {
                rows.iter()
                    .map(|row| pipeline_state_stale_reason(row, content_check))
                    .collect()
            } else {
                vec![None; rows.len()]
            };
            let fresh: Vec<bool> = stale.iter().map(|reason| reason.is_none()).collect();
            let items: Vec<String> = rows
                .iter()
                .zip(stale.iter())
                .map(|(row, reason)| {
                    let verify_field = if verify {
                        match reason {
                            Some(reason) => format!(
                                r#","fresh":false,"staleReason":{}"#,
                                json_quote(reason)
                            ),
                            None => r#","fresh":true"#.to_string(),
                        }
                    } else {
                        String::new()
                    };
                    format!(
                        r#"{{"step":"{}","output":"{}","inputHash":"{}","outputHash":"{}","ms":{}{} }}"#,
                        row.name,
                        row.output.replace('\\', "\\\\"),
                        row.input_hash,
                        row.output_hash,
                        row.elapsed_ms,
                        verify_field
                    )
                })
                .collect();
            if json {
                println!(r#"{{"steps":[{}]}}"#, items.join(","));
                if verify && fail_on_stale && fresh.iter().any(|is_fresh| !*is_fresh) {
                    return 3;
                }
                return 0;
            }
            if verify {
                println!("{:<20} {:<32} {:>8} status", "step", "output", "ms");
                for (row, reason) in rows.iter().zip(stale.iter()) {
                    println!(
                        "{:<20} {:<32} {:>8} {}",
                        row.name,
                        row.output,
                        row.elapsed_ms,
                        match reason {
                            Some(reason) => format!("stale ({reason})"),
                            None => "fresh".to_string(),
                        }
                    );
                }
            } else {
                println!(
                    "{:<20} {:<32} {:<10} {:>8} state",
                    "step", "output", "input", "ms"
                );
                for row in &rows {
                    println!(
                        "{:<20} {:<32} {:<10} {:>8} {}",
                        row.name, row.output, row.input_hash, row.elapsed_ms, row.output_hash
                    );
                }
            }
            if verify && fail_on_stale && fresh.iter().any(|is_fresh| !*is_fresh) {
                return 3;
            }
            0
        }
        other => {
            eprintln!("error: unknown pipeline action {other}; use run|status");
            2
        }
    }
}

#[derive(Clone, Debug)]
struct PipelineStep {
    name: String,
    input: Option<String>,
    command: String,
    args: String,
    output: std::path::PathBuf,
    group: String,
}

type PipelineResults = Vec<(String, bool, u64, String)>;
type BranchOutcome = (String, bool, u64, String, PipelineStateRow);
type PipelineLineage = Vec<(String, Vec<String>)>;

#[derive(Clone, Debug, Default)]
struct PipelineStateRow {
    name: String,
    command: String,
    args: String,
    input: String,
    output: String,
    input_hash: String,
    args_hash: String,
    output_hash: String,
    elapsed_ms: u64,
    input_stamp: String,
    output_stamp: String,
}

fn pipeline_state_path(plan: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.pipeline.csv", plan.display()))
}

fn pipeline_lineage_path(plan: &std::path::Path) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{}.lineage.csv", plan.display()))
}

fn save_pipeline_lineage(plan: &std::path::Path, lineage: &PipelineLineage) -> std::io::Result<()> {
    let mut text = String::from("step,deps\n");
    for (name, deps) in lineage {
        text.push_str(&pipeline_csv_cell(name));
        text.push(',');
        text.push_str(&pipeline_csv_cell(&deps.join(";")));
        text.push('\n');
    }
    std::fs::write(pipeline_lineage_path(plan), text)
}

fn fnv_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn pipeline_hash_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        for byte in &buffer[..read] {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    Ok(format!("{hash:016x}"))
}

fn pipeline_file_stamp(path: &std::path::Path) -> std::io::Result<String> {
    let meta = std::fs::metadata(path)?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    Ok(format!("{}:{modified}", meta.len()))
}

fn pipeline_hash_text(text: &str) -> String {
    format!("{:016x}", fnv_hash(text.as_bytes()))
}

fn split_pipeline_args(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in text.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn load_pipeline_state(path: &std::path::Path) -> Vec<PipelineStateRow> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut reader = csvx_core::csv_reader::RecordReader::new(
        std::io::BufReader::with_capacity(256 * 1024, file),
        256 * 1024,
    );
    let mut record = Vec::new();
    let mut rows = Vec::new();
    let mut header: Option<Vec<String>> = None;
    while reader.next_record(&mut record).unwrap_or(false) {
        let cells: Vec<String> = csvx_core::csv_reader::parse_csv_record_with(
            &String::from_utf8_lossy(&record),
            b',',
            csvx_core::csv_reader::QuoteStyle::Rfc4180,
        );
        if header.is_none() {
            header = Some(cells);
            continue;
        }
        if cells.len() < 8 {
            continue;
        }
        rows.push(PipelineStateRow {
            name: cells[0].clone(),
            command: cells[1].clone(),
            args: cells[2].clone(),
            input: cells[3].clone(),
            output: cells[4].clone(),
            input_hash: cells[5].clone(),
            args_hash: cells[6].clone(),
            output_hash: cells[7].clone(),
            elapsed_ms: cells
                .get(8)
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            input_stamp: cells.get(9).cloned().unwrap_or_default(),
            output_stamp: cells.get(10).cloned().unwrap_or_default(),
        });
    }
    rows
}

fn save_pipeline_state(path: &std::path::Path, rows: &[PipelineStateRow]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = std::path::PathBuf::from(format!("{}.tmp", path.display()));
    let mut file = std::fs::File::create(&tmp)?;
    writeln!(
        file,
        "step,command,args,input,output,input_hash,args_hash,output_hash,elapsed_ms,input_stamp,output_stamp"
    )?;
    for row in rows {
        writeln!(
            file,
            "{},{},{},{},{},{},{},{},{},{},{}",
            row.name,
            row.command,
            row.args,
            row.input,
            row.output,
            row.input_hash,
            row.args_hash,
            row.output_hash,
            row.elapsed_ms,
            row.input_stamp,
            row.output_stamp
        )?;
    }
    file.flush()?;
    std::fs::rename(&tmp, path)
}

/// Run one pipeline child with an optional wall-clock timeout.
///
/// `Command::output()` waits forever; pipeline steps are external processes that
/// can hang on a bad command. The two reader threads drain stdout/stderr while
/// `try_wait` polls, so a verbose step cannot fill a pipe and deadlock the parent.
fn run_step_command(
    mut process: std::process::Command,
    timeout_ms: u64,
) -> std::io::Result<std::process::Output> {
    process
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = process.spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let stdout_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let stderr_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if timeout_ms > 0 && started.elapsed() >= Duration::from_millis(timeout_ms) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_thread.join();
                    let _ = stderr_thread.join();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!("step exceeded {timeout_ms} ms"),
                    ));
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
    };
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Remove remnants of an interrupted parallel run. Only files that exactly match a
/// declared output's staging/legacy temp prefix are touched; a normal run only deletes
/// them after an hour, `--clean` forces it.
/// Recover outputs left mid-wave by a crashed run. A backup exists only after the
/// previous output was moved aside, so restoring it is the safe state: any current
/// output belongs to an uncommitted run and state was never written.
struct PipelineLock {
    path: std::path::PathBuf,
}

impl Drop for PipelineLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(crate) fn process_alive(pid: u32) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output();
    match output {
        Ok(output) => output.status.success() && !output.stdout.is_empty(),
        Err(_) => true,
    }
}

fn lock_owner_alive(path: &std::path::Path) -> Option<bool> {
    let text = std::fs::read_to_string(path).ok()?;
    let pid = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid="))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u32>().ok())?;
    Some(process_alive(pid))
}

fn acquire_pipeline_lock(plan: &std::path::Path) -> std::io::Result<PipelineLock> {
    let path = std::path::PathBuf::from(format!("{}.pipeline.lock", plan.display()));
    if path.exists() {
        let age = path
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .unwrap_or_default();
        let owner = lock_owner_alive(&path);
        let stale_by_age = owner.is_none() && age >= Duration::from_secs(24 * 3600);
        let stale_by_owner = matches!(owner, Some(false));
        if !stale_by_age && !stale_by_owner {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "pipeline is already running (lock {}); remove it if no process is active",
                    path.display()
                ),
            ));
        }
        let _ = std::fs::remove_file(&path);
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            let _ = writeln!(
                file,
                "pid={} started={:?}",
                std::process::id(),
                std::time::SystemTime::now()
            );
            Ok(PipelineLock { path })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "pipeline is already running (lock {}); remove it if no process is active",
                    path.display()
                ),
            ))
        }
        Err(error) => Err(error),
    }
}

fn recover_dag_backups(steps: &[PipelineStep]) -> std::io::Result<usize> {
    let mut recovered = 0usize;
    for step in steps {
        let Some(parent) = step.output.parent() else {
            continue;
        };
        if !parent.exists() {
            continue;
        }
        let name = step
            .output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("out");
        let prefix = format!(".{name}.dag-backup-");
        let mut backups: Vec<std::path::PathBuf> = Vec::new();
        for entry in std::fs::read_dir(parent)?.flatten() {
            let file = entry.file_name();
            if file.to_string_lossy().starts_with(&prefix) {
                backups.push(entry.path());
            }
        }
        if backups.is_empty() {
            continue;
        }
        let newest = backups
            .iter()
            .max_by_key(|path| {
                path.metadata()
                    .and_then(|meta| meta.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            })
            .cloned()
            .expect("non-empty backups");
        let _ = std::fs::remove_file(&step.output);
        std::fs::rename(&newest, &step.output)?;
        for backup in &backups {
            if backup != &newest {
                let _ = std::fs::remove_file(backup);
            }
        }
        recovered += 1;
    }
    Ok(recovered)
}

fn cleanup_stale_staging(steps: &[PipelineStep], force: bool) -> std::io::Result<usize> {
    let mut removed = 0usize;
    for step in steps {
        let Some(parent) = step.output.parent() else {
            continue;
        };
        if !parent.exists() {
            continue;
        }
        let name = step
            .output
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("out");
        let branch_prefix = format!(".{name}.branch-");
        let legacy = format!(".{name}.pipeline.tmp");
        for entry in std::fs::read_dir(parent)?.flatten() {
            let file = entry.file_name();
            let file = file.to_string_lossy();
            let matches =
                file == legacy || (file.starts_with(&branch_prefix) && file.ends_with(".tmp"));
            if !matches {
                continue;
            }
            if !force {
                let age = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .unwrap_or_default();
                if age < Duration::from_secs(3600) {
                    continue;
                }
            }
            if std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

const AUTO_CONTENT_CHECK_LIMIT: u64 = 1 << 20;

fn auto_content_check_limit() -> u64 {
    std::env::var("CSVX_CONTENT_CHECK_LIMIT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(AUTO_CONTENT_CHECK_LIMIT)
}

fn pipeline_auto_content_check(input: &std::path::Path, output: &std::path::Path) -> bool {
    let limit = auto_content_check_limit();
    let size = |path: &std::path::Path| {
        std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(u64::MAX)
    };
    size(input) <= limit && size(output) <= limit
}

fn pipeline_state_stale_reason(row: &PipelineStateRow, content_check: bool) -> Option<String> {
    let input = std::path::Path::new(&row.input);
    let output = std::path::Path::new(&row.output);
    if !input.exists() {
        return Some("input missing".to_string());
    }
    if !output.exists() {
        return Some("output missing".to_string());
    }
    let hash_content = content_check || pipeline_auto_content_check(input, output);
    if hash_content && !row.input_hash.is_empty() && !row.output_hash.is_empty() {
        let input_hash = match pipeline_hash_file(input) {
            Ok(hash) => hash,
            Err(_) => return Some("input hash unavailable".to_string()),
        };
        if input_hash != row.input_hash {
            return Some("input hash changed".to_string());
        }
        let output_hash = match pipeline_hash_file(output) {
            Ok(hash) => hash,
            Err(_) => return Some("output hash unavailable".to_string()),
        };
        if output_hash != row.output_hash {
            return Some("output hash changed".to_string());
        }
        return None;
    }
    if !row.input_stamp.is_empty() && !row.output_stamp.is_empty() {
        match pipeline_file_stamp(input) {
            Ok(stamp) if stamp == row.input_stamp => {}
            Ok(_) => return Some("input stamp changed".to_string()),
            Err(_) => return Some("input stamp unavailable".to_string()),
        }
        match pipeline_file_stamp(output) {
            Ok(stamp) if stamp == row.output_stamp => {}
            Ok(_) => return Some("output stamp changed".to_string()),
            Err(_) => return Some("output stamp unavailable".to_string()),
        }
        return None;
    }
    let input_hash = match pipeline_hash_file(input) {
        Ok(hash) => hash,
        Err(_) => return Some("input hash unavailable".to_string()),
    };
    if input_hash != row.input_hash {
        return Some("input hash changed".to_string());
    }
    let output_hash = match pipeline_hash_file(output) {
        Ok(hash) => hash,
        Err(_) => return Some("output hash unavailable".to_string()),
    };
    if output_hash != row.output_hash {
        return Some("output hash changed".to_string());
    }
    None
}

fn pipeline_dependencies(steps: &[PipelineStep], index: usize) -> Vec<String> {
    match &steps[index].input {
        Some(name) if steps[..index].iter().any(|step| &step.name == name) => vec![name.clone()],
        Some(_) => Vec::new(),
        None if index > 0 => vec![steps[index - 1].name.clone()],
        None => Vec::new(),
    }
}

fn pipeline_csv_cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// Run one `group` of independent steps as concurrent child pipelines. Each child
/// gets a one-step plan so it can reuse the normal command/temp/rename path. Grouped
/// steps must name an explicit input (a source file or an earlier step's output);
/// we do not try to infer freshness here, so they rerun when the group is reached.
fn pipeline_resolve_input(
    step: &PipelineStep,
    steps: &[PipelineStep],
    root: &std::path::Path,
) -> std::io::Result<std::path::PathBuf> {
    let position = steps
        .iter()
        .position(|candidate| candidate.name == step.name)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "unknown pipeline step")
        })?;
    let name = match &step.input {
        Some(name) => name,
        None if position > 0 => &steps[position - 1].name,
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("step {} has no input", step.name),
            ))
        }
    };
    let path = if let Some(position) = steps.iter().position(|candidate| &candidate.name == name) {
        steps[position].output.clone()
    } else if std::path::Path::new(name).is_absolute() {
        std::path::PathBuf::from(name)
    } else {
        root.join(name)
    };
    if !path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("step {}: input {} not found", step.name, path.display()),
        ));
    }
    Ok(path)
}

fn pipeline_fresh_row(
    step: &PipelineStep,
    input_path: &std::path::Path,
    previous: &[PipelineStateRow],
    force: bool,
    content_check: bool,
) -> std::io::Result<Option<PipelineStateRow>> {
    if force {
        return Ok(None);
    }
    let input_string = input_path.display().to_string();
    let output_exists = step.output.exists();
    let input_stamp = pipeline_file_stamp(input_path)?;
    let output_stamp = if output_exists {
        pipeline_file_stamp(&step.output)?
    } else {
        String::new()
    };
    let hash_content = content_check || pipeline_auto_content_check(input_path, &step.output);
    let Some(row) = previous.iter().find(|row| {
        row.name == step.name
            && row.command == step.command
            && row.args == step.args
            && row.input == input_string
    }) else {
        return Ok(None);
    };
    let fresh = if hash_content && !row.input_hash.is_empty() && !row.output_hash.is_empty() {
        output_exists
            && pipeline_hash_file(input_path)? == row.input_hash
            && pipeline_hash_file(&step.output)
                .map(|hash| hash == row.output_hash)
                .unwrap_or(false)
    } else if !row.input_stamp.is_empty() && !row.output_stamp.is_empty() {
        output_exists && row.input_stamp == input_stamp && row.output_stamp == output_stamp
    } else {
        let input_hash = pipeline_hash_file(input_path)?;
        output_exists
            && input_hash == row.input_hash
            && pipeline_hash_file(&step.output)
                .map(|hash| hash == row.output_hash)
                .unwrap_or(false)
    };
    if !fresh {
        return Ok(None);
    }
    Ok(Some(PipelineStateRow {
        name: step.name.clone(),
        command: step.command.clone(),
        args: step.args.clone(),
        input: input_string,
        output: step.output.display().to_string(),
        input_hash: row.input_hash.clone(),
        args_hash: row.args_hash.clone(),
        output_hash: row.output_hash.clone(),
        elapsed_ms: 0,
        input_stamp,
        output_stamp,
    }))
}

fn run_branch_group(
    all: &[PipelineStep],
    group_indices: &[usize],
    root: &std::path::Path,
    force: bool,
    step_timeout_ms: u64,
    retries: usize,
    retry_delay_ms: u64,
) -> std::io::Result<Vec<BranchOutcome>> {
    let bin = std::env::current_exe()?;
    let first = all[group_indices[0]].name.as_bytes();
    let stamp = format!("{}-{}", std::process::id(), fnv_hash(first));
    let mut plans = Vec::with_capacity(group_indices.len());
    let mut inputs = Vec::with_capacity(group_indices.len());
    let mut staged = Vec::with_capacity(group_indices.len());
    for (index, &step_index) in group_indices.iter().enumerate() {
        let step = &all[step_index];
        let name = step.input.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("grouped step {} needs an explicit input", step.name),
            )
        })?;
        let input_path =
            if let Some(position) = all.iter().position(|candidate| &candidate.name == name) {
                if group_indices.contains(&position) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "grouped step {} depends on {} in the same group",
                            step.name, name
                        ),
                    ));
                }
                all[position].output.clone()
            } else if std::path::Path::new(name).is_absolute() {
                std::path::PathBuf::from(name)
            } else {
                root.join(name)
            };
        if !input_path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "step {}: input {} not found",
                    step.name,
                    input_path.display()
                ),
            ));
        }
        inputs.push((step, input_path.clone()));
        let stage = step.output.with_file_name(format!(
            ".{}.branch-{stamp}-{index}.tmp",
            step.output
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("out")
        ));
        staged.push(stage.clone());
        let plan = std::env::temp_dir().join(format!("csvx-branch-{stamp}-{index}.csv"));
        let row = format!(
            "{},{},{},{},{}\n",
            pipeline_csv_cell(&step.name),
            pipeline_csv_cell(&input_path.display().to_string()),
            pipeline_csv_cell(&step.command),
            pipeline_csv_cell(&step.args),
            pipeline_csv_cell(&stage.display().to_string())
        );
        std::fs::write(&plan, format!("step,input,command,args,output\n{row}"))?;
        plans.push(plan);
    }
    struct BranchChild {
        index: usize,
        child: std::process::Child,
        stdout: std::path::PathBuf,
        stderr: std::path::PathBuf,
    }
    let mut children = Vec::with_capacity(plans.len());
    for (index, plan) in plans.iter().enumerate() {
        let out_path = std::env::temp_dir().join(format!("csvx-branch-{stamp}-{index}.out"));
        let err_path = std::env::temp_dir().join(format!("csvx-branch-{stamp}-{index}.err"));
        let out = std::fs::File::create(&out_path)?;
        let err = std::fs::File::create(&err_path)?;
        let mut process = std::process::Command::new(&bin);
        process.args([
            "pipeline",
            "run",
            plan.to_str().unwrap_or(""),
            "--root",
            root.to_str().unwrap_or(""),
            "--json",
        ]);
        if force {
            process.arg("--force");
        }
        if step_timeout_ms > 0 {
            process.args(["--step-timeout", &step_timeout_ms.to_string()]);
        }
        if retries > 0 {
            process.args([
                "--retries",
                &retries.to_string(),
                "--retry-delay",
                &retry_delay_ms.to_string(),
            ]);
        }
        let child = process
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(out))
            .stderr(std::process::Stdio::from(err))
            .spawn()?;
        children.push(BranchChild {
            index,
            child,
            stdout: out_path,
            stderr: err_path,
        });
    }
    let started = Instant::now();
    let mut statuses: Vec<Option<std::process::ExitStatus>> = vec![None; children.len()];
    let mut failure: Option<String> = None;
    loop {
        let mut running = 0usize;
        for branch in children.iter_mut() {
            if statuses[branch.index].is_some() {
                continue;
            }
            match branch.child.try_wait() {
                Ok(Some(status)) => {
                    statuses[branch.index] = Some(status);
                    if !status.success() && failure.is_none() {
                        let error = std::fs::read_to_string(&branch.stderr)
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        failure = Some(format!(
                            "step {} failed: {error}",
                            inputs[branch.index].0.name
                        ));
                    }
                }
                Ok(None) => running += 1,
                Err(error) => {
                    if failure.is_none() {
                        failure = Some(format!(
                            "step {} failed: {error}",
                            inputs[branch.index].0.name
                        ));
                    }
                }
            }
        }
        if failure.is_some() || running == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    for branch in children.iter_mut() {
        if failure.is_some() {
            let _ = branch.child.kill();
        }
        let _ = branch.child.wait();
        let _ = std::fs::remove_file(&branch.stdout);
        let _ = std::fs::remove_file(&branch.stderr);
    }
    for plan in &plans {
        let _ = std::fs::remove_file(plan);
        let _ = std::fs::remove_file(pipeline_state_path(plan));
    }
    let elapsed_ms = started.elapsed().as_millis() as u64;
    if let Some(error) = failure {
        for stage in &staged {
            let _ = std::fs::remove_file(stage);
        }
        return Err(std::io::Error::other(error));
    }
    // Commit the whole wave only after every child succeeded. Existing outputs are
    // replaced, but a failed wave never touches them.
    for ((step, _), stage) in inputs.iter().zip(staged.iter()) {
        if step.output.exists() {
            let _ = std::fs::remove_file(&step.output);
        }
        if let Err(error) = std::fs::rename(stage, &step.output) {
            for stage in &staged {
                let _ = std::fs::remove_file(stage);
            }
            return Err(error);
        }
    }
    let mut results = Vec::with_capacity(group_indices.len());
    for (step, input_path) in inputs.iter() {
        let input_hash = pipeline_hash_file(input_path)?;
        let output_hash = pipeline_hash_file(&step.output)?;
        let input_stamp = pipeline_file_stamp(input_path)?;
        let output_stamp = pipeline_file_stamp(&step.output)?;
        let args_hash = pipeline_hash_text(&step.args);
        let output_string = step.output.display().to_string();
        results.push((
            step.name.clone(),
            false,
            elapsed_ms,
            output_string.clone(),
            PipelineStateRow {
                name: step.name.clone(),
                command: step.command.clone(),
                args: step.args.clone(),
                input: input_path.display().to_string(),
                output: output_string,
                input_hash,
                args_hash,
                output_hash,
                elapsed_ms,
                input_stamp,
                output_stamp,
            },
        ));
    }
    Ok(results)
}
