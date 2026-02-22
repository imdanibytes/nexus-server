//! Execution handle — shared state threaded through the workflow executor.
//!
//! Replaces the old `steps: &mut usize` + `max_steps: usize` pair with a
//! single `Clone + Send + Sync` struct that carries step counting,
//! cancellation, and (later) timeouts, checkpointing, and auditing.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::workflows::audit::AuditSender;
use crate::workflows::checkpoint::FileCheckpointWriter;
use crate::workflows::error::WorkflowError;

/// Max log entries retained per task.
const MAX_LOG_ENTRIES: usize = 50;

/// Shared log buffer for agent execution events.
///
/// Uses `std::sync::Mutex` (not tokio) because locks are very short-lived
/// (append a string) and we need blocking access for serialization.
pub type LogBuffer = Arc<std::sync::Mutex<VecDeque<String>>>;

/// Create a new empty log buffer.
pub fn new_log_buffer() -> LogBuffer {
    Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES)))
}

/// Append a log entry, evicting the oldest if at capacity.
pub fn push_log(buf: &LogBuffer, entry: String) {
    if let Ok(mut logs) = buf.lock() {
        if logs.len() >= MAX_LOG_ENTRIES {
            logs.pop_front();
        }
        logs.push_back(entry);
    }
}

/// Snapshot the log buffer as a Vec for serialization.
pub fn snapshot_logs(buf: &LogBuffer) -> Vec<String> {
    buf.lock()
        .map(|logs| logs.iter().cloned().collect())
        .unwrap_or_default()
}

/// Shared execution state for a single workflow run.
///
/// Cloneable — the same handle is shared by the top-level executor,
/// nested `do` blocks, and try/catch/finally branches.
#[derive(Clone)]
pub struct ExecutionHandle {
    /// Atomic step counter shared across all branches.
    pub steps: Arc<AtomicUsize>,
    /// Maximum steps before the workflow is killed.
    pub max_steps: usize,
    /// Cooperative cancellation token.
    pub cancel: CancellationToken,
    /// Default timeout per step (overridden by per-task `timeout` field).
    pub default_step_timeout: Option<Duration>,
    /// Checkpoint writer for crash recovery.
    pub checkpoint: Option<Arc<FileCheckpointWriter>>,
    /// Task ID for checkpoint filename.
    pub task_id: Option<String>,
    /// Workflow name for checkpoint metadata.
    pub workflow_name: Option<String>,
    /// Audit event broadcast sender.
    pub audit: Option<AuditSender>,
    /// Shared log buffer for agent execution events.
    pub log_buffer: LogBuffer,
}

impl ExecutionHandle {
    pub fn new(max_steps: usize) -> Self {
        Self {
            steps: Arc::new(AtomicUsize::new(0)),
            max_steps,
            cancel: CancellationToken::new(),
            default_step_timeout: None,
            checkpoint: None,
            task_id: None,
            workflow_name: None,
            audit: None,
            log_buffer: new_log_buffer(),
        }
    }

    /// Create a handle with an existing cancellation token (from TaskStore).
    pub fn with_cancel(max_steps: usize, cancel: CancellationToken) -> Self {
        Self {
            steps: Arc::new(AtomicUsize::new(0)),
            max_steps,
            cancel,
            default_step_timeout: None,
            checkpoint: None,
            task_id: None,
            workflow_name: None,
            audit: None,
            log_buffer: new_log_buffer(),
        }
    }

    /// Create a handle with a shared log buffer (from TaskStore).
    pub fn with_log_buffer(mut self, buf: LogBuffer) -> Self {
        self.log_buffer = buf;
        self
    }

    /// Set checkpoint writer and task metadata for crash recovery.
    pub fn with_checkpoint(
        mut self,
        writer: Arc<FileCheckpointWriter>,
        task_id: String,
        workflow_name: String,
    ) -> Self {
        self.checkpoint = Some(writer);
        self.task_id = Some(task_id);
        self.workflow_name = Some(workflow_name);
        self
    }

    /// Set the audit event sender.
    pub fn with_audit(mut self, sender: AuditSender) -> Self {
        self.audit = Some(sender);
        self
    }

    /// Set the default step timeout.
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_step_timeout = Some(timeout);
        self
    }

    /// Resolve the effective timeout for a step.
    /// Per-step timeout overrides the handle default.
    pub fn resolve_timeout(&self, step_timeout_ms: Option<u64>) -> Option<Duration> {
        step_timeout_ms
            .map(Duration::from_millis)
            .or(self.default_step_timeout)
    }

    /// Increment the step counter. Returns `Err` if max steps exceeded or cancelled.
    pub fn increment_steps(&self) -> Result<(), WorkflowError> {
        self.check_cancel()?;
        let current = self.steps.load(Ordering::Relaxed);
        if current >= self.max_steps {
            return Err(WorkflowError::MaxStepsExceeded(self.max_steps));
        }
        self.steps.store(current + 1, Ordering::Relaxed);
        Ok(())
    }

    /// Check if the workflow has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Return `Err(Cancelled)` if the token has been fired.
    pub fn check_cancel(&self) -> Result<(), WorkflowError> {
        if self.cancel.is_cancelled() {
            return Err(WorkflowError::Cancelled);
        }
        Ok(())
    }

    /// Current step count.
    pub fn step_count(&self) -> usize {
        self.steps.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increment_respects_max() {
        let handle = ExecutionHandle::new(3);
        assert!(handle.increment_steps().is_ok());
        assert!(handle.increment_steps().is_ok());
        assert!(handle.increment_steps().is_ok());
        assert!(matches!(
            handle.increment_steps(),
            Err(WorkflowError::MaxStepsExceeded(3))
        ));
        assert_eq!(handle.step_count(), 3);
    }

    #[test]
    fn check_cancel_before_fire() {
        let handle = ExecutionHandle::new(100);
        assert!(handle.check_cancel().is_ok());
        assert!(!handle.is_cancelled());
    }

    #[test]
    fn check_cancel_after_fire() {
        let handle = ExecutionHandle::new(100);
        handle.cancel.cancel();
        assert!(handle.is_cancelled());
        assert!(matches!(
            handle.check_cancel(),
            Err(WorkflowError::Cancelled)
        ));
    }

    #[test]
    fn increment_checks_cancel() {
        let handle = ExecutionHandle::new(100);
        handle.cancel.cancel();
        assert!(matches!(
            handle.increment_steps(),
            Err(WorkflowError::Cancelled)
        ));
        // Step counter should NOT have incremented
        assert_eq!(handle.step_count(), 0);
    }

    #[test]
    fn with_cancel_shares_token() {
        let token = CancellationToken::new();
        let handle = ExecutionHandle::with_cancel(100, token.clone());
        assert!(!handle.is_cancelled());
        token.cancel();
        assert!(handle.is_cancelled());
    }
}
