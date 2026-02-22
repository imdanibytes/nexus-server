//! Workflow Task — tracks workflow execution state.
//!
//! Maps to the MCP Task spec: each workflow execution is a task with
//! an ID, status lifecycle, progress tracking, and output.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use cloudevents::Event;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::server::SharedState;
use crate::workflows::audit::{self, AuditEvent, AuditSender};
use crate::workflows::checkpoint::FileCheckpointWriter;
use crate::workflows::error::WorkflowError;
use crate::workflows::executor;
use crate::workflows::handle::ExecutionHandle;
use crate::workflows::model::WorkflowDef;

/// Task status lifecycle: pending -> running -> completed/failed/cancelled.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// A workflow execution wrapped in a trackable task.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowTask {
    pub id: String,
    pub workflow_name: String,
    pub status: TaskStatus,
    pub steps_executed: usize,
    #[serde(skip)]
    pub started_at: Option<Instant>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
    pub output: Option<Value>,
    /// Cancellation token — fire this to cooperatively cancel the workflow.
    #[serde(skip)]
    pub cancel: CancellationToken,
}

/// Summary view of a task for listing.
#[derive(Debug, Clone, Serialize)]
pub struct TaskSummary {
    pub id: String,
    pub workflow_name: String,
    pub status: TaskStatus,
    pub steps_executed: usize,
    pub duration_ms: Option<u64>,
}

impl From<&WorkflowTask> for TaskSummary {
    fn from(task: &WorkflowTask) -> Self {
        Self {
            id: task.id.clone(),
            workflow_name: task.workflow_name.clone(),
            status: task.status.clone(),
            steps_executed: task.steps_executed,
            duration_ms: task.duration_ms,
        }
    }
}

/// Shared registry of workflow task executions.
///
/// Thread-safe store for tracking active and completed workflow runs.
/// Completed tasks are retained for observability (capped to prevent unbounded growth).
#[derive(Clone)]
pub struct TaskStore {
    tasks: Arc<Mutex<HashMap<String, WorkflowTask>>>,
    max_retained: usize,
    checkpoint_writer: Option<Arc<FileCheckpointWriter>>,
    audit_sender: Option<AuditSender>,
}

impl TaskStore {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            max_retained: 1000,
            checkpoint_writer: None,
            audit_sender: None,
        }
    }

    /// Create a task store with checkpoint persistence.
    pub fn with_checkpoint_dir(dir: impl Into<PathBuf>) -> Result<Self, std::io::Error> {
        let writer = FileCheckpointWriter::new(dir)?;
        Ok(Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            max_retained: 1000,
            checkpoint_writer: Some(Arc::new(writer)),
            audit_sender: None,
        })
    }

    /// Set the audit event sender for workflow execution telemetry.
    pub fn with_audit(mut self, sender: AuditSender) -> Self {
        self.audit_sender = Some(sender);
        self
    }

    /// Spawn a workflow as a tracked task. Returns the task ID and output.
    ///
    /// Creates a `CancellationToken` on the task and threads it through
    /// the executor via `ExecutionHandle`. Call `cancel()` to fire the token.
    pub async fn spawn(
        &self,
        workflow_name: &str,
        def: &WorkflowDef,
        event: &Event,
        shared: &Arc<SharedState>,
    ) -> Result<(String, Value), WorkflowError> {
        let task_id = Self::generate_id();
        let cancel = CancellationToken::new();

        // Build the execution handle with this task's token
        let mut handle = ExecutionHandle::with_cancel(1000, cancel.clone());
        if let Some(ref writer) = self.checkpoint_writer {
            handle = handle.with_checkpoint(
                Arc::clone(writer),
                task_id.clone(),
                workflow_name.to_string(),
            );
        }
        if let Some(ref sender) = self.audit_sender {
            handle = handle.with_audit(sender.clone());
        }

        // Register task as running
        let task = WorkflowTask {
            id: task_id.clone(),
            workflow_name: workflow_name.to_string(),
            status: TaskStatus::Running,
            steps_executed: 0,
            started_at: Some(Instant::now()),
            duration_ms: None,
            error: None,
            output: None,
            cancel,
        };

        {
            let mut tasks = self.tasks.lock().await;
            tasks.insert(task_id.clone(), task);
        }

        info!(task_id = %task_id, workflow = %workflow_name, "task: started");

        // Emit WorkflowStarted audit event
        if let Some(ref sender) = self.audit_sender {
            audit::emit(sender, AuditEvent::WorkflowStarted {
                task_id: task_id.clone(),
                workflow_name: workflow_name.to_string(),
            });
        }

        // Execute workflow with the handle
        let result = executor::run_with_handle(def, event, shared, &handle).await;

        // Remove checkpoint on completion (success, failure, or cancel)
        if let Some(ref writer) = self.checkpoint_writer {
            if let Err(e) = writer.remove(&task_id).await {
                warn!(task_id = %task_id, error = %e, "failed to remove checkpoint");
            }
        }

        // Update task with outcome
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.get_mut(&task_id) {
            let elapsed = task.started_at.map(|s| s.elapsed().as_millis() as u64);
            task.duration_ms = elapsed;
            task.steps_executed = handle.step_count();

            match &result {
                Ok(output) => {
                    task.status = TaskStatus::Completed;
                    task.output = Some(output.clone());
                    info!(task_id = %task_id, duration_ms = ?elapsed, "task: completed");
                    if let Some(ref sender) = self.audit_sender {
                        audit::emit(sender, AuditEvent::WorkflowCompleted {
                            task_id: task_id.clone(),
                            duration_ms: elapsed.unwrap_or(0),
                            steps_executed: task.steps_executed,
                        });
                    }
                }
                Err(WorkflowError::Cancelled) => {
                    task.status = TaskStatus::Cancelled;
                    task.error = Some("cancelled".into());
                    info!(task_id = %task_id, "task: cancelled");
                    if let Some(ref sender) = self.audit_sender {
                        audit::emit(sender, AuditEvent::WorkflowCancelled {
                            task_id: task_id.clone(),
                            steps_executed: task.steps_executed,
                        });
                    }
                }
                Err(err) => {
                    task.status = TaskStatus::Failed;
                    task.error = Some(err.to_string());
                    info!(task_id = %task_id, error = %err, "task: failed");
                    if let Some(ref sender) = self.audit_sender {
                        audit::emit(sender, AuditEvent::WorkflowFailed {
                            task_id: task_id.clone(),
                            error: err.to_string(),
                            steps_executed: task.steps_executed,
                        });
                    }
                }
            }
        }

        // Evict old completed tasks if over capacity
        self.evict_if_needed(&mut tasks);

        result.map(|output| (task_id, output))
    }

    /// Get a task by ID.
    pub async fn get(&self, id: &str) -> Option<WorkflowTask> {
        self.tasks.lock().await.get(id).cloned()
    }

    /// List all tasks as summaries.
    pub async fn list(&self) -> Vec<TaskSummary> {
        self.tasks
            .lock()
            .await
            .values()
            .map(TaskSummary::from)
            .collect()
    }

    /// Cancel a running task by firing its cancellation token.
    ///
    /// The executor will observe the cancellation at its next yield point
    /// and return `WorkflowError::Cancelled`.
    pub async fn cancel(&self, id: &str) -> bool {
        let tasks = self.tasks.lock().await;
        if let Some(task) = tasks.get(id) {
            if task.status == TaskStatus::Running {
                task.cancel.cancel();
                return true;
            }
        }
        false
    }

    fn generate_id() -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        use std::time::{SystemTime, UNIX_EPOCH};
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        format!("wt-{ts:x}-{seq:x}")
    }

    /// Resume incomplete workflows from checkpoints (crash recovery).
    ///
    /// Called on server startup. Scans the checkpoint directory, looks up
    /// each workflow definition, and resumes execution from the last
    /// saved step.
    pub async fn resume_incomplete(&self, shared: &Arc<SharedState>) -> usize {
        let writer = match &self.checkpoint_writer {
            Some(w) => w,
            None => return 0,
        };

        let checkpoints = match writer.list_incomplete().await {
            Ok(cps) => cps,
            Err(e) => {
                warn!(error = %e, "failed to scan checkpoints");
                return 0;
            }
        };

        if checkpoints.is_empty() {
            return 0;
        }

        info!(count = checkpoints.len(), "resuming incomplete workflows");
        let mut resumed = 0;

        for cp in &checkpoints {
            let def = match shared.workflow_store.read().await.get(&cp.workflow_name).cloned() {
                Some(d) => d,
                None => {
                    warn!(
                        task_id = %cp.task_id,
                        workflow = %cp.workflow_name,
                        "cannot resume: workflow definition not found"
                    );
                    // Remove orphaned checkpoint
                    let _ = writer.remove(&cp.task_id).await;
                    continue;
                }
            };

            let cancel = CancellationToken::new();
            let mut handle = ExecutionHandle::with_cancel(1000, cancel.clone())
                .with_checkpoint(
                    Arc::clone(writer),
                    cp.task_id.clone(),
                    cp.workflow_name.clone(),
                );
            if let Some(ref sender) = self.audit_sender {
                handle = handle.with_audit(sender.clone());
            }

            // Register as running
            let task = WorkflowTask {
                id: cp.task_id.clone(),
                workflow_name: cp.workflow_name.clone(),
                status: TaskStatus::Running,
                steps_executed: cp.global_steps,
                started_at: Some(Instant::now()),
                duration_ms: None,
                error: None,
                output: None,
                cancel,
            };

            {
                let mut tasks = self.tasks.lock().await;
                tasks.insert(cp.task_id.clone(), task);
            }

            info!(
                task_id = %cp.task_id,
                workflow = %cp.workflow_name,
                step_index = cp.step_index,
                "task: resuming from checkpoint"
            );

            let result = executor::resume_from_checkpoint(&def, cp, shared, &handle).await;

            // Remove checkpoint on completion
            if let Err(e) = writer.remove(&cp.task_id).await {
                warn!(task_id = %cp.task_id, error = %e, "failed to remove checkpoint");
            }

            // Update task
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.get_mut(&cp.task_id) {
                let elapsed = task.started_at.map(|s| s.elapsed().as_millis() as u64);
                task.duration_ms = elapsed;
                task.steps_executed = handle.step_count();

                match &result {
                    Ok(output) => {
                        task.status = TaskStatus::Completed;
                        task.output = Some(output.clone());
                        info!(task_id = %cp.task_id, "task: resume completed");
                        if let Some(ref sender) = self.audit_sender {
                            audit::emit(sender, AuditEvent::WorkflowCompleted {
                                task_id: cp.task_id.clone(),
                                duration_ms: elapsed.unwrap_or(0),
                                steps_executed: task.steps_executed,
                            });
                        }
                    }
                    Err(WorkflowError::Cancelled) => {
                        task.status = TaskStatus::Cancelled;
                        task.error = Some("cancelled".into());
                        if let Some(ref sender) = self.audit_sender {
                            audit::emit(sender, AuditEvent::WorkflowCancelled {
                                task_id: cp.task_id.clone(),
                                steps_executed: task.steps_executed,
                            });
                        }
                    }
                    Err(err) => {
                        task.status = TaskStatus::Failed;
                        task.error = Some(err.to_string());
                        warn!(task_id = %cp.task_id, error = %err, "task: resume failed");
                        if let Some(ref sender) = self.audit_sender {
                            audit::emit(sender, AuditEvent::WorkflowFailed {
                                task_id: cp.task_id.clone(),
                                error: err.to_string(),
                                steps_executed: task.steps_executed,
                            });
                        }
                    }
                }
            }

            resumed += 1;
        }

        resumed
    }

    fn evict_if_needed(&self, tasks: &mut HashMap<String, WorkflowTask>) {
        if tasks.len() <= self.max_retained {
            return;
        }

        // Remove oldest completed/failed tasks
        let mut completed: Vec<_> = tasks
            .iter()
            .filter(|(_, t)| matches!(t.status, TaskStatus::Completed | TaskStatus::Failed))
            .map(|(id, _)| id.clone())
            .collect();

        // Sort by ID (which embeds timestamp) — oldest first
        completed.sort();

        let to_remove = tasks.len() - self.max_retained;
        for id in completed.into_iter().take(to_remove) {
            tasks.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxRegistry;
    use crate::workflows::WorkflowStore;
    use cloudevents::{EventBuilder, EventBuilderV10};
    use serde_json::json;

    fn test_event(data: Value) -> Event {
        EventBuilderV10::new()
            .id("test-1")
            .ty("com.test.event")
            .source("test")
            .data("application/json", data)
            .build()
            .unwrap()
    }

    fn test_shared() -> Arc<SharedState> {
        Arc::new(SharedState {
            claude: None,
            github_token_env: "GITHUB_TOKEN".into(),
            github_app: None,
            http_client: reqwest::Client::new(),
            source_count: 0,
            stats: crate::server::ServerStats::new(),
            recent_events: tokio::sync::Mutex::new(std::collections::VecDeque::new()),
            seen_deliveries: tokio::sync::Mutex::new(crate::server::DeliveryTracker::new()),
            workflow_store: tokio::sync::RwLock::new(WorkflowStore::new()),
            task_store: TaskStore::new(),
            sandbox_registry: SandboxRegistry::new(),
        })
    }

    #[tokio::test]
    async fn task_lifecycle_success() {
        let store = TaskStore::new();
        let shared = test_shared();
        let yaml = r#"
do:
  - init:
      set:
        result: "done"
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));

        let (task_id, output) = store.spawn("test-wf", &def, &event, &shared).await.unwrap();

        assert_eq!(output["result"], json!("done"));

        let task = store.get(&task_id).await.unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
        assert!(task.duration_ms.is_some());
        assert!(task.error.is_none());
    }

    #[tokio::test]
    async fn task_lifecycle_failure() {
        let store = TaskStore::new();
        let shared = test_shared();
        let yaml = r#"
do:
  - boom:
      set:
        x: "${ .nonexistent | error }"
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));

        let result = store.spawn("failing-wf", &def, &event, &shared).await;
        assert!(result.is_err());

        let tasks = store.list().await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, TaskStatus::Failed);
    }

    #[tokio::test]
    async fn task_cancel_fires_token() {
        // Verify cancel() fires the token on the WorkflowTask.
        let store = TaskStore::new();
        let cancel = CancellationToken::new();

        // Manually insert a running task with a known token.
        let task = WorkflowTask {
            id: "test-cancel".into(),
            workflow_name: "wf".into(),
            status: TaskStatus::Running,
            steps_executed: 0,
            started_at: Some(Instant::now()),
            duration_ms: None,
            error: None,
            output: None,
            cancel: cancel.clone(),
        };
        store.tasks.lock().await.insert("test-cancel".into(), task);

        assert!(!cancel.is_cancelled());
        assert!(store.cancel("test-cancel").await);
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn task_cancel_noop_on_completed() {
        let store = TaskStore::new();
        let cancel = CancellationToken::new();

        let task = WorkflowTask {
            id: "done-task".into(),
            workflow_name: "wf".into(),
            status: TaskStatus::Completed,
            steps_executed: 5,
            started_at: None,
            duration_ms: Some(100),
            error: None,
            output: Some(serde_json::json!({})),
            cancel: cancel.clone(),
        };
        store.tasks.lock().await.insert("done-task".into(), task);

        // cancel() should return false for non-running tasks
        assert!(!store.cancel("done-task").await);
        assert!(!cancel.is_cancelled());
    }

    #[tokio::test]
    async fn task_list() {
        let store = TaskStore::new();
        let shared = test_shared();
        let yaml = r#"
do:
  - step:
      set:
        x: 1
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();

        store
            .spawn("wf-1", &def, &test_event(json!({})), &shared)
            .await
            .unwrap();
        store
            .spawn("wf-2", &def, &test_event(json!({})), &shared)
            .await
            .unwrap();

        let tasks = store.list().await;
        assert_eq!(tasks.len(), 2);
    }

    #[tokio::test]
    async fn task_store_with_checkpoint_removes_on_completion() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = TaskStore::with_checkpoint_dir(tmp.path()).unwrap();
        let shared = test_shared();

        let yaml = r#"
do:
  - step1:
      set:
        a: 1
  - step2:
      set:
        b: 2
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));

        let (task_id, output) = store.spawn("ck-wf", &def, &event, &shared).await.unwrap();
        assert_eq!(output["b"], json!(2));

        // Checkpoint should have been removed after successful completion
        let writer = store.checkpoint_writer.as_ref().unwrap();
        let cp = writer.load(&task_id).await.unwrap();
        assert!(cp.is_none(), "checkpoint should be cleaned up on success");
    }

    #[tokio::test]
    async fn task_store_checkpoint_removed_on_failure() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = TaskStore::with_checkpoint_dir(tmp.path()).unwrap();
        let shared = test_shared();

        let yaml = r#"
do:
  - step1:
      set:
        a: 1
  - boom:
      set:
        x: "${ .nonexistent | error }"
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));

        let result = store.spawn("fail-wf", &def, &event, &shared).await;
        assert!(result.is_err());

        // Checkpoint should be removed even on failure
        let writer = store.checkpoint_writer.as_ref().unwrap();
        let incomplete = writer.list_incomplete().await.unwrap();
        assert!(incomplete.is_empty(), "checkpoint should be cleaned up on failure");
    }

    #[tokio::test]
    async fn audit_events_emitted_on_success() {
        let (tx, mut rx) = crate::workflows::audit::channel(64);
        let store = TaskStore::new().with_audit(tx);
        let shared = test_shared();

        let yaml = r#"
do:
  - step1:
      set:
        a: 1
  - step2:
      set:
        b: 2
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));

        let (_task_id, _output) = store.spawn("audit-wf", &def, &event, &shared).await.unwrap();

        // Drop store to close the sender so rx drains
        drop(store);

        let mut events = Vec::new();
        while let Ok(ev) = rx.recv().await {
            events.push(ev);
        }

        // Expect: WorkflowStarted, StepStarted×2, StepCompleted×2, WorkflowCompleted
        assert!(events.len() >= 6, "expected at least 6 audit events, got {}", events.len());
        assert!(matches!(&events[0], AuditEvent::WorkflowStarted { .. }));
        assert!(matches!(events.last().unwrap(), AuditEvent::WorkflowCompleted { .. }));
    }

    #[tokio::test]
    async fn audit_events_emitted_on_failure() {
        let (tx, mut rx) = crate::workflows::audit::channel(64);
        let store = TaskStore::new().with_audit(tx);
        let shared = test_shared();

        let yaml = r#"
do:
  - boom:
      set:
        x: "${ .nonexistent | error }"
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));

        let _ = store.spawn("fail-audit", &def, &event, &shared).await;
        drop(store);

        let mut events = Vec::new();
        while let Ok(ev) = rx.recv().await {
            events.push(ev);
        }

        // Should contain WorkflowStarted, StepStarted, StepFailed, WorkflowFailed
        assert!(events.len() >= 4, "expected at least 4 audit events, got {}", events.len());
        assert!(matches!(&events[0], AuditEvent::WorkflowStarted { .. }));
        assert!(matches!(events.last().unwrap(), AuditEvent::WorkflowFailed { .. }));
    }
}
