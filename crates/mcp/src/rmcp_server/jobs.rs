use super::{tool_result, value_to_mcp, CsvxServer};
use rmcp::model::CallToolResponse;
use serde_json::{json, Value};
use std::sync::Arc;

impl CsvxServer {
    pub(super) async fn job_tool(&self, name: &str, value: &Value) -> Option<CallToolResponse> {
        match name {
            "csvx_job_submit" => {
                let tool = value.get("tool").and_then(Value::as_str)?.to_string();
                if matches!(tool.as_str(), "csvx_cache_stats" | "csvx_cache_clear") {
                    return Some(tool_result(
                        format!("{tool} is a server tool, not a CLI job"),
                        true,
                        "job_start_error",
                    ));
                }
                let args = value.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let args = value_to_mcp(&args);
                let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                Some(match jobs.submit(&self.root, &tool, &args) {
                    Ok(id) => tool_result(
                        format!(r#"{{"job":{},"state":"running"}}"#, json!(id)),
                        false,
                        "ok",
                    ),
                    Err(error) => tool_result(error, true, "job_start_error"),
                })
            }
            "csvx_job_status" => {
                let id = value.get("id").and_then(Value::as_str)?.to_string();
                let wait = match value.get("wait") {
                    None => 0,
                    Some(Value::Number(number)) => match number.as_u64() {
                        Some(value) if value <= 30 => value,
                        _ => {
                            return Some(tool_result(
                                "wait must be an integer between 0 and 30".to_string(),
                                true,
                                "wait_range",
                            ));
                        }
                    },
                    Some(_) => {
                        return Some(tool_result(
                            "wait must be an integer between 0 and 30".to_string(),
                            true,
                            "wait_range",
                        ));
                    }
                };
                let jobs = Arc::clone(&self.jobs);
                let result = tokio::task::spawn_blocking(move || {
                    jobs.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .status_text(&id, wait)
                })
                .await
                .ok()
                .flatten();
                Some(match result {
                    Some(text) => tool_result(text, false, "ok"),
                    None => tool_result(
                        format!(
                            "unknown job {}",
                            value.get("id").and_then(Value::as_str).unwrap_or("")
                        ),
                        true,
                        "unknown_job",
                    ),
                })
            }
            "csvx_job_cancel" => {
                let id = value.get("id").and_then(Value::as_str)?.to_string();
                let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                Some(match jobs.cancel_text(&id) {
                    Some(text) => tool_result(text, false, "ok"),
                    None => tool_result(format!("unknown job {id}"), true, "unknown_job"),
                })
            }
            "csvx_job_list" => {
                let full = value.get("full").and_then(Value::as_bool).unwrap_or(false);
                let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                Some(tool_result(jobs.list_text(full), false, "ok"))
            }
            "csvx_cache_clear" => {
                let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                Some(tool_result(jobs.cache_clear_text(), false, "ok"))
            }
            "csvx_cache_stats" => {
                let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
                Some(tool_result(jobs.cache_stats_text(), false, "ok"))
            }
            _ => None,
        }
    }
}
