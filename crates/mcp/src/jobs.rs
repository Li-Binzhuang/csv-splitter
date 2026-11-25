use crate::cache::{
    cleanup_stale_mcp_materialized, materialize_cache_limits, MATERIALIZE_CACHE, MATERIALIZE_HITS,
    MATERIALIZE_MISSES,
};
use crate::{mcp_command, mcp_quote, McpJson};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(crate) struct McpJob {
    pub(crate) child: std::process::Child,
    pub(crate) stdout: PathBuf,
    pub(crate) stderr: PathBuf,
    pub(crate) result: Option<(bool, i32, String)>,
    pub(crate) started: std::time::Instant,
}

#[cfg(test)]
pub(crate) fn shutdown_mcp_jobs(jobs: &mut std::collections::HashMap<String, McpJob>) {
    for job in jobs.values_mut() {
        if job.result.is_none() {
            let _ = job.child.kill();
            let _ = job.child.wait();
        }
        drop_mcp_job_files(job);
    }
}

pub(crate) const MCP_JOB_HISTORY: usize = 128;

pub(crate) fn drop_mcp_job_files(job: &McpJob) {
    let _ = std::fs::remove_file(&job.stdout);
    let _ = std::fs::remove_file(&job.stderr);
}

pub(crate) fn prune_mcp_jobs(jobs: &mut std::collections::HashMap<String, McpJob>) {
    while jobs.len() >= MCP_JOB_HISTORY {
        let oldest = jobs
            .iter()
            .filter(|(_, job)| job.result.is_some())
            .min_by_key(|(_, job)| job.started)
            .map(|(id, _)| id.clone());
        let Some(oldest) = oldest else {
            break;
        };
        if let Some(job) = jobs.remove(&oldest) {
            drop_mcp_job_files(&job);
        }
    }
}

fn mcp_job_tool_allowed(tool: &str) -> bool {
    matches!(
        tool,
        "csvx_schema"
            | "csvx_view"
            | "csvx_filter"
            | "csvx_search"
            | "csvx_quality"
            | "csvx_pipeline_status"
            | "csvx_pipeline_run"
            | "csvx_edit_set"
            | "csvx_edit_delete"
            | "csvx_edit_undo"
            | "csvx_edit_redo"
            | "csvx_edit_preview"
            | "csvx_edit_apply"
            | "csvx_report"
            | "csvx_gate"
            | "csvx_concat"
            | "csvx_convert"
    )
}

fn mcp_start_job(
    root: &Path,
    tool: &str,
    arguments: &McpJson,
    job_id: &str,
) -> Result<McpJob, String> {
    if tool.starts_with("csvx_job_") {
        return Err("job tools cannot be submitted as jobs".to_string());
    }
    let mut command = mcp_command(root, tool, arguments)?;
    let stamp = format!("{}-{}", std::process::id(), job_id);
    let stdout = std::env::temp_dir().join(format!("csvx-mcp-{stamp}.out"));
    let stderr = std::env::temp_dir().join(format!("csvx-mcp-{stamp}.err"));
    let out = std::fs::File::create(&stdout).map_err(|e| e.to_string())?;
    let err = std::fs::File::create(&stderr).map_err(|e| e.to_string())?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(out))
        .stderr(std::process::Stdio::from(err));
    let child = command
        .spawn()
        .map_err(|e| format!("cannot start job: {e}"))?;
    Ok(McpJob {
        child,
        stdout,
        stderr,
        result: None,
        started: std::time::Instant::now(),
    })
}

fn mcp_cancel_job(job_id: &str, job: &mut McpJob) -> String {
    if job.result.is_some() {
        return mcp_job_text(job_id, job);
    }
    match job.child.try_wait() {
        Ok(None) => {
            let _ = job.child.kill();
            let _ = job.child.wait();
            let _ = std::fs::remove_file(&job.stdout);
            let _ = std::fs::remove_file(&job.stderr);
            job.result = Some((false, -1, "cancelled".to_string()));
            format!(r#"{{"job":{},"state":"cancelled"}}"#, mcp_quote(job_id))
        }
        _ => mcp_job_text(job_id, job),
    }
}

fn mcp_cache_clear() -> String {
    csvx_core::workbook::clear_window_cache();
    let mut removed = 0usize;
    let mut bytes = 0u64;
    if let Some(cache) = MATERIALIZE_CACHE.get() {
        if let Ok(mut entries) = cache.lock() {
            removed = entries.len();
            bytes = entries.iter().map(|entry| entry.size).sum();
            for entry in entries.drain(..) {
                let _ = std::fs::remove_file(entry.path);
            }
        }
    }
    let stale_removed = cleanup_stale_mcp_materialized();
    format!(r#"{{"removed":{removed},"bytes":{bytes},"staleRemoved":{stale_removed}}}"#)
}

fn mcp_cache_stats() -> String {
    let (max_bytes, max_entries) = materialize_cache_limits();
    let (entries, bytes) = MATERIALIZE_CACHE
        .get()
        .and_then(|cache| {
            cache
                .lock()
                .ok()
                .map(|guard| (guard.len(), guard.iter().map(|entry| entry.size).sum()))
        })
        .unwrap_or((0, 0));
    let (window_hits, window_misses) = csvx_core::workbook::window_cache_stats();
    format!(
        r#"{{"hits":{},"misses":{},"entries":{},"bytes":{},"maxBytes":{},"maxEntries":{},"windowHits":{},"windowMisses":{}}}"#,
        MATERIALIZE_HITS.load(std::sync::atomic::Ordering::Relaxed),
        MATERIALIZE_MISSES.load(std::sync::atomic::Ordering::Relaxed),
        entries,
        bytes,
        max_bytes,
        max_entries,
        window_hits,
        window_misses
    )
}

fn mcp_job_list(jobs: &mut HashMap<String, McpJob>, full: bool) -> String {
    let mut entries = Vec::with_capacity(jobs.len());
    for (job_id, job) in jobs.iter_mut() {
        if full {
            entries.push(mcp_job_text(job_id, job));
            continue;
        }
        // Refresh the cached result first, then summarize without the payload.
        let _ = mcp_job_text(job_id, job);
        let entry = match &job.result {
            Some((success, _, message)) => format!(
                r#"{{"job":{},"state":{},"outputChars":{}}}"#,
                mcp_quote(job_id),
                mcp_quote(if *success { "done" } else { "failed" }),
                message.chars().count()
            ),
            None => format!(
                r#"{{"job":{},"state":"running","elapsedMs":{}}}"#,
                mcp_quote(job_id),
                job.started.elapsed().as_millis()
            ),
        };
        entries.push(entry);
    }
    format!(r#"{{"jobs":[{}]}}"#, entries.join(","))
}

fn mcp_job_text(job_id: &str, job: &mut McpJob) -> String {
    if let Some((success, code, message)) = &job.result {
        return if *success {
            format!(
                r#"{{"job":{},"state":"done","output":{}}}"#,
                mcp_quote(job_id),
                mcp_quote(message)
            )
        } else {
            format!(
                r#"{{"job":{},"state":"failed","exitCode":{},"error":{}}}"#,
                mcp_quote(job_id),
                code,
                mcp_quote(message)
            )
        };
    }
    match job.child.try_wait() {
        Ok(None) => format!(
            r#"{{"job":{},"state":"running","elapsedMs":{}}}"#,
            mcp_quote(job_id),
            job.started.elapsed().as_millis()
        ),
        Ok(Some(status)) => {
            let text = std::fs::read_to_string(&job.stdout)
                .map(|value| value.trim().to_string())
                .unwrap_or_default();
            let error = std::fs::read_to_string(&job.stderr)
                .map(|value| value.trim().to_string())
                .unwrap_or_default();
            let _ = std::fs::remove_file(&job.stdout);
            let _ = std::fs::remove_file(&job.stderr);
            let success = status.success();
            let code = status.code().unwrap_or(-1);
            let message = if success { text } else { error };
            job.result = Some((success, code, message.clone()));
            mcp_job_text(job_id, job)
        }
        Err(error) => format!(
            r#"{{"job":{},"state":"failed","error":{}}}"#,
            mcp_quote(job_id),
            mcp_quote(&error.to_string())
        ),
    }
}

pub struct McpJobRegistry {
    jobs: HashMap<String, McpJob>,
    next: u64,
}

impl Default for McpJobRegistry {
    fn default() -> Self {
        Self {
            jobs: HashMap::new(),
            next: 1,
        }
    }
}

impl McpJobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn submit(
        &mut self,
        root: &Path,
        tool: &str,
        arguments: &McpJson,
    ) -> Result<String, String> {
        if !mcp_job_tool_allowed(tool) {
            return Err(format!("{tool} cannot be submitted as a job"));
        }
        let job_id = format!("job-{}", self.next);
        self.next += 1;
        let job = mcp_start_job(root, tool, arguments, &job_id)?;
        prune_mcp_jobs(&mut self.jobs);
        self.jobs.insert(job_id.clone(), job);
        Ok(job_id)
    }

    pub fn status_text(&mut self, job_id: &str, wait: u64) -> Option<String> {
        let job = self.jobs.get_mut(job_id)?;
        let started = std::time::Instant::now();
        loop {
            let text = mcp_job_text(job_id, job);
            if !text.contains(r#""state":"running""#)
                || started.elapsed() >= std::time::Duration::from_secs(wait)
            {
                return Some(text);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    pub fn cancel_text(&mut self, job_id: &str) -> Option<String> {
        self.jobs
            .get_mut(job_id)
            .map(|job| mcp_cancel_job(job_id, job))
    }

    pub fn list_text(&mut self, full: bool) -> String {
        mcp_job_list(&mut self.jobs, full)
    }

    pub fn cache_clear_text(&self) -> String {
        mcp_cache_clear()
    }

    pub fn cache_stats_text(&self) -> String {
        mcp_cache_stats()
    }
}
