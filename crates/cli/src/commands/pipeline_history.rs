//! Persistent pipeline run history: one JSONL line per run, local to the root.
use crate::commands::shared::json_quote;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) struct PipelineHistory {
    path: PathBuf,
    max_bytes: u64,
}

impl PipelineHistory {
    pub(crate) fn new(root: &Path) -> Self {
        let max_bytes = std::env::var("CSVX_PIPELINE_HISTORY_MAX_BYTES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_MAX_BYTES);
        Self {
            path: root.join(".csvx/pipeline-history.jsonl"),
            max_bytes,
        }
    }

    pub(crate) fn append(&self, line: &str) -> std::io::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| std::io::Error::other("history path has no parent"))?;
        fs::create_dir_all(parent)?;
        self.rotate_if_needed()?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")
    }

    pub(crate) fn read(&self) -> std::io::Result<String> {
        let mut text = String::new();
        if !self.path.exists() {
            return Ok(text);
        }
        fs::File::open(&self.path)?.read_to_string(&mut text)?;
        Ok(text)
    }

    fn rotate_if_needed(&self) -> std::io::Result<()> {
        let size = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if size < self.max_bytes {
            return Ok(());
        }
        let rotated = self.path.with_extension("jsonl.1");
        if rotated.exists() {
            fs::remove_file(&rotated)?;
        }
        fs::rename(&self.path, &rotated)
    }
}

pub(crate) struct HistoryStep {
    pub(crate) name: String,
    pub(crate) fresh: bool,
    pub(crate) ms: u64,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn append_run(
    root: &Path,
    plan: &Path,
    plan_hash: &str,
    started_at_ms: u128,
    ended_at_ms: u128,
    ok: bool,
    error_code: Option<&str>,
    error: Option<&str>,
    steps: &[HistoryStep],
) -> std::io::Result<()> {
    let mut line = format!(
        r#"{{"run":"{}","plan":{},"planHash":{},"startedAt":{},"endedAt":{},"ok":{},"errorCode":{},"error":{},"steps":["#,
        plan_hash,
        json_quote(&plan.display().to_string()),
        json_quote(plan_hash),
        started_at_ms,
        ended_at_ms,
        ok,
        json_quote(error_code.unwrap_or("")),
        json_quote(error.unwrap_or("")),
    );
    for (index, step) in steps.iter().enumerate() {
        if index > 0 {
            line.push(',');
        }
        line.push_str(&format!(
            r#"{{"name":{},"status":{},"fresh":{},"ms":{}}}"#,
            json_quote(&step.name),
            if step.fresh { "\"skipped\"" } else { "\"ok\"" },
            step.fresh,
            step.ms
        ));
    }
    line.push_str("]}");
    PipelineHistory::new(root).append(&line)
}

pub(crate) fn read(root: &Path) -> std::io::Result<String> {
    PipelineHistory::new(root).read()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "csvx-pipeline-history-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn appends_and_rotates() {
        let root = dir("rotate");
        let history = PipelineHistory {
            path: root.join(".csvx/pipeline-history.jsonl"),
            max_bytes: 200,
        };
        for index in 0..10 {
            let steps = [HistoryStep {
                name: format!("step-{index}"),
                fresh: index % 2 == 0,
                ms: index as u64,
            }];
            let line = format!(
                r#"{{"run":"r{index}","plan":"p.csv","ok":true,"steps":[{{"name":"s","fresh":{},"ms":{}}}]}}"#,
                index % 2 == 0,
                index
            );
            history.append(&line).unwrap();
            let _ = steps;
        }
        assert!(history.path.exists());
        assert!(history.path.with_extension("jsonl.1").exists());
        assert!(history.read().unwrap().lines().count() >= 1);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reports_unwritable_path() {
        let root = dir("error");
        let blocker = root.join("blocked");
        fs::write(&blocker, b"x").unwrap();
        let history = PipelineHistory {
            path: blocker.join("history.jsonl"),
            max_bytes: 1024,
        };
        assert!(history.append("{}").is_err());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn run_line_has_no_step_values() {
        let root = dir("line");
        let steps = [HistoryStep {
            name: "filtered".to_string(),
            fresh: false,
            ms: 12,
        }];
        append_run(
            &root,
            Path::new("plan.csv"),
            "abc",
            1,
            2,
            true,
            None,
            None,
            &steps,
        )
        .unwrap();
        let text = read(&root).unwrap();
        assert!(text.contains("\"planHash\":\"abc\""));
        assert!(text.contains("\"status\":\"ok\""));
        assert!(text.contains("\"fresh\":false"));
        fs::remove_dir_all(root).ok();
    }
}
