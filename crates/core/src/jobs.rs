//! Background job registry for long-running operations (split / merge).
//!
//! A 10 GB split takes minutes; an HTTP handler cannot block for that. Jobs run
//! on their own thread, publish progress into a shared table, and the client
//! polls by id. Nothing about a job lives on the connection that started it.
//!
//! The registry is in-process and intentionally simple: it is a single-user
//! local tool, not a scheduler. Jobs are evicted once they fall out of the
//! retained window.

use crate::types::{ProgressEvent, ProgressSender};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How many finished jobs to keep for polling.
const RETAIN: usize = 50;
/// Progress messages kept per job (the UI shows a tail, not a log file).
const KEEP_MESSAGES: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct JobStatus {
    pub id: String,
    pub kind: String,
    pub state: JobState,
    pub phase: String,
    /// Bytes or records, depending on the phase; 0 when unknown.
    pub progress: u64,
    pub total: u64,
    pub messages: Vec<String>,
    pub output: Option<PathBuf>,
    pub error: Option<String>,
    pub started: Instant,
}

impl JobStatus {
    /// 0.0–1.0 when a total is known, else `None`.
    pub fn fraction(&self) -> Option<f64> {
        if self.total == 0 {
            return None;
        }
        Some((self.progress as f64 / self.total as f64).clamp(0.0, 1.0))
    }

    pub fn elapsed_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }
}

#[derive(Clone, Default)]
pub struct JobRegistry {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, JobStatus>,
}

static JOB_SEQ: AtomicU64 = AtomicU64::new(0);

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start `work` on its own thread. `work` receives a sender and must report
    /// through it; the registry turns those events into status updates.
    ///
    /// Returns the job id.
    pub fn submit<F>(&self, kind: &str, work: F) -> String
    where
        F: FnOnce(ProgressSender) + Send + 'static,
    {
        let id = next_id();
        let status = JobStatus {
            id: id.clone(),
            kind: kind.to_string(),
            state: JobState::Queued,
            phase: "queued".to_string(),
            progress: 0,
            total: 0,
            messages: Vec::new(),
            output: None,
            error: None,
            started: Instant::now(),
        };
        {
            let mut inner = self.inner.lock().unwrap();
            inner.jobs.insert(id.clone(), status);
            evict(&mut inner);
        }

        let registry = self.clone();
        let job_id = id.clone();
        thread::spawn(move || {
            let (tx, rx) = std::sync::mpsc::channel::<ProgressEvent>();

            // Relay: every event updates the shared row. The relay owns the row
            // for the duration of the job, so the worker never needs a lock.
            let relay_registry = registry.clone();
            let relay_id = job_id.clone();
            let relay = thread::spawn(move || {
                let mut terminal = None;
                while let Ok(event) = rx.recv() {
                    let mut inner = relay_registry.inner.lock().unwrap();
                    let Some(row) = inner.jobs.get_mut(&relay_id) else {
                        break;
                    };
                    match event {
                        ProgressEvent::PhaseStart { phase, total_bytes } => {
                            row.state = JobState::Running;
                            row.phase = phase;
                            row.total = total_bytes;
                        }
                        ProgressEvent::PhaseDone { .. } => {
                            row.phase = "done".to_string();
                        }
                        ProgressEvent::Progress { bytes } => row.progress = bytes,
                        ProgressEvent::Info(msg) => {
                            if row.messages.len() >= KEEP_MESSAGES {
                                row.messages.remove(0);
                            }
                            row.messages.push(msg);
                        }
                        ProgressEvent::Finished { output } => {
                            row.state = JobState::Done;
                            row.output = Some(output);
                            terminal = Some(());
                        }
                        ProgressEvent::Error(e) => {
                            row.state = JobState::Failed;
                            row.error = Some(e);
                            terminal = Some(());
                        }
                        _ => {}
                    }
                }
                let _ = terminal;
            });

            {
                let mut inner = registry.inner.lock().unwrap();
                if let Some(row) = inner.jobs.get_mut(&job_id) {
                    row.state = JobState::Running;
                }
            }

            work(tx);

            // The worker dropped its sender; wait for the relay to drain so the
            // final status is visible before `wait` returns.
            let _ = relay.join();
        });

        id
    }

    pub fn status(&self, id: &str) -> Option<JobStatus> {
        self.inner.lock().unwrap().jobs.get(id).cloned()
    }

    pub fn list(&self) -> Vec<JobStatus> {
        let inner = self.inner.lock().unwrap();
        let mut jobs: Vec<JobStatus> = inner.jobs.values().cloned().collect();
        // Newest first: the caller almost always wants the job it just started.
        jobs.sort_by_key(|j| std::cmp::Reverse(j.started.elapsed()));
        jobs
    }

    /// Block until the job reaches a terminal state (used by the CLI).
    pub fn wait(&self, id: &str, timeout: Duration) -> Option<JobStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            let status = self.status(id)?;
            if matches!(status.state, JobState::Done | JobState::Failed) {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return Some(status);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn next_id() -> String {
    let seq = JOB_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("job{nanos:x}{seq:x}")
}

fn evict(inner: &mut Inner) {
    if inner.jobs.len() <= RETAIN {
        return;
    }
    // Drop the oldest finished jobs first; running jobs are never evicted.
    let mut finished: Vec<(String, u64)> = inner
        .jobs
        .iter()
        .filter(|(_, j)| matches!(j.state, JobState::Done | JobState::Failed))
        .map(|(id, j)| (id.clone(), j.started.elapsed().as_secs()))
        .collect();
    finished.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    for (id, _) in finished {
        if inner.jobs.len() <= RETAIN {
            break;
        }
        inner.jobs.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_progress_and_completion() {
        let registry = JobRegistry::new();
        let id = registry.submit("demo", |tx| {
            tx.send(ProgressEvent::PhaseStart {
                phase: "work".into(),
                total_bytes: 100,
            })
            .ok();
            tx.send(ProgressEvent::Progress { bytes: 50 }).ok();
            tx.send(ProgressEvent::Finished {
                output: PathBuf::from("/tmp/out"),
            })
            .ok();
        });

        let done = registry.wait(&id, Duration::from_secs(5)).unwrap();
        assert_eq!(done.state, JobState::Done);
        assert_eq!(
            done.output.as_deref(),
            Some(std::path::Path::new("/tmp/out"))
        );
        assert_eq!(done.total, 100);
        assert_eq!(done.progress, 50);
    }

    #[test]
    fn surfaces_errors() {
        let registry = JobRegistry::new();
        let id = registry.submit("boom", |tx| {
            tx.send(ProgressEvent::Error("disk full".into())).ok();
        });
        let done = registry.wait(&id, Duration::from_secs(5)).unwrap();
        assert_eq!(done.state, JobState::Failed);
        assert_eq!(done.error.as_deref(), Some("disk full"));
    }

    #[test]
    fn unknown_id_returns_none() {
        let registry = JobRegistry::new();
        assert!(registry.status("nope").is_none());
        assert!(registry.wait("nope", Duration::from_millis(50)).is_none());
    }

    #[test]
    fn fraction_is_none_without_a_total() {
        let registry = JobRegistry::new();
        let id = registry.submit("quiet", |tx| {
            tx.send(ProgressEvent::Info("no totals".into())).ok();
        });
        let status = registry.wait(&id, Duration::from_secs(5)).unwrap();
        assert!(status.fraction().is_none());
        assert_eq!(status.messages.len(), 1);
    }
}
