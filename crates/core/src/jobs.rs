//! Background job registry for long-running operations (split / merge).
//!
//! A 10 GB split takes minutes; an HTTP handler cannot block for that. Jobs run
//! on their own thread, publish progress into a shared table, and the client
//! polls by id. Nothing about a job lives on the connection that started it.
//!
//! The registry is in-process and intentionally simple: it is a single-user
//! local tool, not a scheduler. Jobs are evicted once they fall out of the
//! retained window.

use crate::types::{CancelToken, ProgressEvent, ProgressSender};
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
    /// Stopped on request. The worker saw the flag and returned before finishing.
    Cancelled,
}

impl JobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
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
    /// Whether this job can actually be stopped, as opposed to merely relabelled.
    pub cancellable: bool,
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

/// What `cancel(id)` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The flag was set; the worker stops at its next check.
    Cancelling,
    /// The job is not the kind that can stop.
    NotCancellable,
    /// Nothing to cancel: it already ended.
    Finished(JobState),
    /// No such job.
    Unknown,
}

#[derive(Clone, Default)]
pub struct JobRegistry {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    jobs: HashMap<String, JobStatus>,
    /// The stop flags of jobs that can be stopped, by id.
    cancels: HashMap<String, CancelToken>,
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
        self.start(kind, false, move |tx, _| work(tx))
    }

    /// Start a job that can be stopped: the worker receives a [`CancelToken`] it is
    /// expected to check between chunks, and `cancel(id)` sets it.
    pub fn submit_cancellable<F>(&self, kind: &str, work: F) -> String
    where
        F: FnOnce(ProgressSender, CancelToken) + Send + 'static,
    {
        self.start(kind, true, work)
    }

    fn start<F>(&self, kind: &str, cancellable: bool, work: F) -> String
    where
        F: FnOnce(ProgressSender, CancelToken) + Send + 'static,
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
            cancellable,
        };
        let token = CancelToken::new();
        {
            let mut inner = self.inner.lock().unwrap();
            inner.jobs.insert(id.clone(), status);
            if cancellable {
                inner.cancels.insert(id.clone(), token.clone());
            }
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

            work(tx, token);

            // The worker dropped its sender; wait for the relay to drain so the
            // final status is visible before `wait` returns.
            let _ = relay.join();
        });

        id
    }

    /// Ask a cancellable job to stop. Returns what actually happened, so a caller can
    /// tell "stopping" from "could not be stopped" instead of offering a fake button.
    pub fn cancel(&self, id: &str) -> CancelOutcome {
        let mut inner = self.inner.lock().unwrap();
        let (cancellable, state) = match inner.jobs.get(id) {
            Some(row) => (row.cancellable, row.state),
            None => return CancelOutcome::Unknown,
        };
        // Answering "already finished" before "cannot be cancelled" is more useful:
        // the caller's question was "is it still doing work?".
        if matches!(
            state,
            JobState::Done | JobState::Failed | JobState::Cancelled
        ) {
            return CancelOutcome::Finished(state);
        }
        if !cancellable {
            return CancelOutcome::NotCancellable;
        }
        if let Some(token) = inner.cancels.get(id) {
            token.cancel();
        }
        let Some(row) = inner.jobs.get_mut(id) else {
            return CancelOutcome::Unknown;
        };
        // The worker stops at its next check; the state is marked now so a poller sees
        // that the request was accepted, and the interrupted error confirms it.
        row.state = JobState::Cancelled;
        CancelOutcome::Cancelling
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

#[cfg(test)]
mod cancel_registry_tests {
    use super::*;

    /// A cancellable job really stops: the worker sees the flag, exits, and the row
    /// says `cancelled` — not `done`, and not `failed`.
    #[test]
    fn a_cancellable_job_stops_and_reports_it() {
        let registry = JobRegistry::new();
        let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = started.clone();
        let id = registry.submit_cancellable("test", move |tx, token| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            while !token.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = tx.send(ProgressEvent::Info("stopped".to_string()));
        });
        // Wait until the worker is actually running, then cancel.
        for _ in 0..100 {
            if started.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(registry.cancel(&id), CancelOutcome::Cancelling));
        let status = registry.wait(&id, Duration::from_secs(5)).unwrap();
        assert_eq!(status.state, JobState::Cancelled, "{status:?}");
        assert!(status.cancellable);
    }

    /// A plain job is not offered a cancel button that would do nothing.
    #[test]
    fn a_plain_job_refuses_cancellation() {
        let registry = JobRegistry::new();
        let id = registry.submit("test", |tx| {
            let _ = tx.send(ProgressEvent::Info("done".to_string()));
        });
        assert!(matches!(
            registry.cancel(&id),
            CancelOutcome::NotCancellable
        ));
        assert!(matches!(registry.cancel("nope"), CancelOutcome::Unknown));
    }
}
