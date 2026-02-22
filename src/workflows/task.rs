//! Workflow Task — tracks workflow execution state.
//!
//! Maps to the MCP Task spec: each workflow execution is a task with
//! an ID, status lifecycle, progress tracking, and output.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use cloudevents::Event;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::info;

use crate::workflows::error::WorkflowError;
use crate::workflows::executor;
use crate::workflows::model::WorkflowDef;

/// Task status lifecycle: pending → running → completed/failed/cancelled.
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
}

impl TaskStore {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            max_retained: 1000,
        }
    }

    /// Spawn a workflow as a tracked task. Returns the task ID.
    ///
    /// The workflow runs to completion (or failure) and the task state
    /// is updated throughout. The caller gets the task ID immediately
    /// and can query status via `get` or `list`.
    pub async fn spawn(
        &self,
        workflow_name: &str,
        def: &WorkflowDef,
        event: &Event,
        client: &reqwest::Client,
    ) -> Result<(String, Value), WorkflowError> {
        let task_id = Self::generate_id();

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
        };

        {
            let mut tasks = self.tasks.lock().await;
            tasks.insert(task_id.clone(), task);
        }

        info!(task_id = %task_id, workflow = %workflow_name, "task: started");

        // Execute workflow
        let result = executor::run(def, event, client).await;

        // Update task with outcome
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.get_mut(&task_id) {
            let elapsed = task.started_at.map(|s| s.elapsed().as_millis() as u64);
            task.duration_ms = elapsed;

            match &result {
                Ok(output) => {
                    task.status = TaskStatus::Completed;
                    task.output = Some(output.clone());
                    info!(task_id = %task_id, duration_ms = ?elapsed, "task: completed");
                }
                Err(err) => {
                    task.status = TaskStatus::Failed;
                    task.error = Some(err.to_string());
                    info!(task_id = %task_id, error = %err, "task: failed");
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

    /// Cancel a running task (sets status only — actual cancellation
    /// requires cooperative checking in the executor, which is future work).
    pub async fn cancel(&self, id: &str) -> bool {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.get_mut(id) {
            if task.status == TaskStatus::Running {
                task.status = TaskStatus::Cancelled;
                task.duration_ms = task.started_at.map(|s| s.elapsed().as_millis() as u64);
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

    #[tokio::test]
    async fn task_lifecycle_success() {
        let store = TaskStore::new();
        let client = reqwest::Client::new();
        let yaml = r#"
do:
  - init:
      set:
        result: "done"
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));

        let (task_id, output) = store.spawn("test-wf", &def, &event, &client).await.unwrap();

        assert_eq!(output["result"], json!("done"));

        let task = store.get(&task_id).await.unwrap();
        assert_eq!(task.status, TaskStatus::Completed);
        assert!(task.duration_ms.is_some());
        assert!(task.error.is_none());
    }

    #[tokio::test]
    async fn task_lifecycle_failure() {
        let store = TaskStore::new();
        let client = reqwest::Client::new();
        let yaml = r#"
do:
  - boom:
      set:
        x: "${ .nonexistent | error }"
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));

        let result = store.spawn("failing-wf", &def, &event, &client).await;
        assert!(result.is_err());

        let tasks = store.list().await;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, TaskStatus::Failed);
    }

    #[tokio::test]
    async fn task_list() {
        let store = TaskStore::new();
        let client = reqwest::Client::new();
        let yaml = r#"
do:
  - step:
      set:
        x: 1
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();

        store
            .spawn("wf-1", &def, &test_event(json!({})), &client)
            .await
            .unwrap();
        store
            .spawn("wf-2", &def, &test_event(json!({})), &client)
            .await
            .unwrap();

        let tasks = store.list().await;
        assert_eq!(tasks.len(), 2);
    }
}
