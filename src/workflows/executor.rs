//! Workflow executor — walks task lists and dispatches to handlers.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use cloudevents::Event;
use serde_json::Value;
use tracing::{info, warn};

use crate::server::SharedState;
use crate::workflows::audit::{self, AuditEvent};
use crate::workflows::checkpoint::Checkpoint;
use crate::workflows::context::WorkflowContext;
use crate::workflows::error::WorkflowError;
use crate::workflows::expressions;
use crate::workflows::handle::ExecutionHandle;
use crate::workflows::model::{TaskDef, TaskKind, WorkflowDef};
use crate::workflows::tasks::switch::FlowDirective;
use crate::workflows::tasks::{call, do_task, set, switch, try_task};

const DEFAULT_MAX_STEPS: usize = 1000;

/// Outcome of executing a task list.
#[derive(Debug)]
pub enum Outcome {
    /// All tasks completed normally.
    Completed,
    /// A task requested workflow exit (`then: exit`).
    Exited,
}

/// Run a workflow definition against a triggering event.
pub async fn run(
    def: &WorkflowDef,
    event: &Event,
    shared: &Arc<SharedState>,
) -> Result<Value, WorkflowError> {
    let handle = ExecutionHandle::new(DEFAULT_MAX_STEPS);
    run_with_handle(def, event, shared, &handle).await
}

/// Run a workflow with an externally-provided handle (used by TaskStore).
pub async fn run_with_handle(
    def: &WorkflowDef,
    event: &Event,
    shared: &Arc<SharedState>,
    handle: &ExecutionHandle,
) -> Result<Value, WorkflowError> {
    let mut ctx = WorkflowContext::from_event(event);

    info!("workflow: starting execution");
    let checkpoint_enabled = handle.checkpoint.is_some();
    let outcome = execute_task_list_impl(
        &def.do_, &mut ctx, shared, handle, 0, checkpoint_enabled,
    ).await?;
    info!(steps = handle.step_count(), outcome = ?outcome, "workflow: execution complete");

    Ok(ctx.output)
}

/// Resume a workflow from a checkpoint (crash recovery).
///
/// Restores context from the checkpoint and starts execution from the
/// saved step index, skipping steps that already completed.
pub async fn resume_from_checkpoint(
    def: &WorkflowDef,
    checkpoint: &Checkpoint,
    shared: &Arc<SharedState>,
    handle: &ExecutionHandle,
) -> Result<Value, WorkflowError> {
    let mut ctx = WorkflowContext::from_checkpoint(&checkpoint.context);

    info!(
        step_index = checkpoint.step_index,
        global_steps = checkpoint.global_steps,
        "workflow: resuming from checkpoint"
    );

    // Fast-forward the global step counter to match the checkpoint
    handle.steps.store(checkpoint.global_steps, std::sync::atomic::Ordering::Relaxed);

    let checkpoint_enabled = handle.checkpoint.is_some();
    let outcome = execute_task_list_impl(
        &def.do_, &mut ctx, shared, handle, checkpoint.step_index, checkpoint_enabled,
    ).await?;
    info!(steps = handle.step_count(), outcome = ?outcome, "workflow: resume complete");

    Ok(ctx.output)
}

/// Execute a sequential list of tasks with flow control.
///
/// Core loop used by top-level `do`, nested `do`, and try/catch/finally blocks.
/// Returns a boxed future to break the async recursion cycle
/// (do_task and try_task call back into this function).
pub(crate) fn execute_task_list<'a>(
    tasks: &'a [BTreeMap<String, TaskDef>],
    ctx: &'a mut WorkflowContext,
    shared: &'a Arc<SharedState>,
    handle: &'a ExecutionHandle,
) -> Pin<Box<dyn Future<Output = Result<Outcome, WorkflowError>> + Send + 'a>> {
    Box::pin(execute_task_list_impl(tasks, ctx, shared, handle, 0, false))
}

/// Inner implementation of the task list executor.
///
/// `start_idx` allows resuming from a checkpoint (skip completed steps).
/// `checkpoint_steps` enables checkpoint saves after each top-level step.
async fn execute_task_list_impl(
    tasks: &[BTreeMap<String, TaskDef>],
    ctx: &mut WorkflowContext,
    shared: &Arc<SharedState>,
    handle: &ExecutionHandle,
    start_idx: usize,
    checkpoint_steps: bool,
) -> Result<Outcome, WorkflowError> {
    let mut idx = start_idx;

    'outer: while idx < tasks.len() {
        let task_map = &tasks[idx];

        for (task_name, task_def) in task_map {
            // Check cancellation + increment step counter
            handle.increment_steps()?;

            // Conditional execution
            if let Some(ref cond) = task_def.if_ {
                if !expressions::evaluate_bool(cond, &ctx.input, &ctx.vars())? {
                    info!(task = %task_name, "workflow: skipping (condition false)");
                    continue;
                }
            }

            info!(task = %task_name, "workflow: executing");

            // Emit StepStarted audit event
            emit_audit(handle, AuditEvent::StepStarted {
                task_id: handle.task_id.clone().unwrap_or_default(),
                step_name: task_name.to_string(),
                step_index: idx,
            });

            let step_start = std::time::Instant::now();

            // Input transform
            if let Some(ref input_def) = task_def.input {
                if let Some(ref from) = input_def.from {
                    ctx.apply_input_transform(from)?;
                }
            }

            // Dispatch task and get flow directive (with optional timeout)
            let dispatch_result = match handle.resolve_timeout(task_def.timeout) {
                Some(duration) => {
                    let duration_ms = duration.as_millis() as u64;
                    tokio::time::timeout(duration, dispatch_task(task_name, task_def, ctx, shared, handle))
                        .await
                        .map_err(|_| WorkflowError::StepTimeout {
                            step_name: task_name.to_string(),
                            duration_ms,
                        })?
                }
                None => dispatch_task(task_name, task_def, ctx, shared, handle).await,
            };

            let directive = match dispatch_result {
                Ok(d) => d,
                Err(e) => {
                    emit_audit(handle, AuditEvent::StepFailed {
                        task_id: handle.task_id.clone().unwrap_or_default(),
                        step_name: task_name.to_string(),
                        step_index: idx,
                        error: e.to_string(),
                    });
                    return Err(e);
                }
            };

            emit_audit(handle, AuditEvent::StepCompleted {
                task_id: handle.task_id.clone().unwrap_or_default(),
                step_name: task_name.to_string(),
                step_index: idx,
                duration_ms: step_start.elapsed().as_millis() as u64,
            });

            // Output transform
            if let Some(ref output_def) = task_def.output {
                if let Some(ref as_) = output_def.as_ {
                    ctx.apply_output_transform(as_)?;
                }
            }

            // Export to context
            if let Some(ref export_def) = task_def.export {
                if let Some(ref as_) = export_def.as_ {
                    ctx.apply_export(as_)?;
                }
            }

            // Resolve final flow: task_def.then overrides task's own directive
            let final_directive = match &task_def.then {
                Some(then) => match then.as_str() {
                    "continue" => FlowDirective::Continue,
                    "exit" => FlowDirective::Exit,
                    target => FlowDirective::GoTo(target.to_string()),
                },
                None => directive,
            };

            match final_directive {
                FlowDirective::Continue => {
                    ctx.advance();
                    if checkpoint_steps {
                        save_checkpoint(handle, ctx, idx + 1, task_name).await;
                    }
                }
                FlowDirective::Exit => return Ok(Outcome::Exited),
                FlowDirective::GoTo(ref target) => {
                    ctx.advance();
                    let target_idx = find_task_index(tasks, target).ok_or_else(|| {
                        WorkflowError::Task(format!("goto target '{target}' not found"))
                    })?;
                    if checkpoint_steps {
                        save_checkpoint(handle, ctx, target_idx, task_name).await;
                    }
                    idx = target_idx;
                    continue 'outer;
                }
            }
        }

        idx += 1;
    }

    Ok(Outcome::Completed)
}

/// Dispatch a single task by kind, returning a flow directive.
async fn dispatch_task(
    task_name: &str,
    task_def: &TaskDef,
    ctx: &mut WorkflowContext,
    shared: &Arc<SharedState>,
    handle: &ExecutionHandle,
) -> Result<FlowDirective, WorkflowError> {
    match task_def.kind() {
        TaskKind::Call { target, with } => {
            let result = call::execute(target, with, ctx, shared, handle).await?;
            ctx.output = result;
            Ok(FlowDirective::Continue)
        }
        TaskKind::Set(values) => {
            let result = set::execute(values, ctx)?;
            ctx.output = result;
            Ok(FlowDirective::Continue)
        }
        TaskKind::Switch(cases) => switch::execute(cases, ctx),
        TaskKind::Do(subtasks) => {
            match do_task::execute(subtasks, ctx, shared, handle).await? {
                Outcome::Exited => Ok(FlowDirective::Exit),
                Outcome::Completed => Ok(FlowDirective::Continue),
            }
        }
        TaskKind::Try => {
            match try_task::execute(task_def, ctx, shared, handle).await? {
                Outcome::Exited => Ok(FlowDirective::Exit),
                Outcome::Completed => Ok(FlowDirective::Continue),
            }
        }
        TaskKind::Unknown => {
            warn!(task = %task_name, "workflow: unknown task type, skipping");
            Ok(FlowDirective::Continue)
        }
    }
}

/// Emit an audit event if the handle has a sender.
fn emit_audit(handle: &ExecutionHandle, event: AuditEvent) {
    if let Some(ref sender) = handle.audit {
        audit::emit(sender, event);
    }
}

/// Save a checkpoint after a step completes (best-effort, won't fail the workflow).
async fn save_checkpoint(
    handle: &ExecutionHandle,
    ctx: &WorkflowContext,
    next_step_index: usize,
    last_step_name: &str,
) {
    let (writer, task_id) = match (&handle.checkpoint, &handle.task_id) {
        (Some(w), Some(id)) => (w, id),
        _ => return,
    };

    use std::time::{SystemTime, UNIX_EPOCH};
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let cp = Checkpoint {
        task_id: task_id.clone(),
        workflow_name: handle.workflow_name.clone().unwrap_or_default(),
        step_index: next_step_index,
        global_steps: handle.step_count(),
        context: ctx.to_checkpoint_context(),
        started_at_epoch_ms: now_ms,
        last_step_name: Some(last_step_name.to_string()),
    };

    if let Err(e) = writer.save(&cp).await {
        warn!(error = %e, task_id = %task_id, "failed to save checkpoint");
    } else {
        emit_audit(handle, AuditEvent::CheckpointSaved {
            task_id: task_id.clone(),
            step_index: next_step_index,
        });
    }
}

fn find_task_index(tasks: &[BTreeMap<String, TaskDef>], name: &str) -> Option<usize> {
    tasks.iter().position(|map| map.contains_key(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudevents::{EventBuilder, EventBuilderV10};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

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
        use crate::sandbox::SandboxRegistry;
        use crate::workflows::task::TaskStore;
        use crate::workflows::WorkflowStore;

        Arc::new(SharedState {
            rules: tokio::sync::RwLock::new(vec![]),
            claude: None,
            github_token_env: "GITHUB_TOKEN".into(),
            github_app: None,
            http_client: reqwest::Client::new(),
            source_count: 0,
            config_path: std::path::PathBuf::new(),
            stats: crate::server::ServerStats::new(),
            recent_events: tokio::sync::Mutex::new(std::collections::VecDeque::new()),
            seen_deliveries: tokio::sync::Mutex::new(crate::server::DeliveryTracker::new()),
            workflow_store: WorkflowStore::new(),
            task_store: TaskStore::new(),
            sandbox_registry: SandboxRegistry::new(),
        })
    }

    #[tokio::test]
    async fn set_then_read() {
        let yaml = r#"
do:
  - init:
      set:
        greeting: "hello"
        count: 42
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));
        let shared = test_shared();
        let result = run(&def, &event, &shared).await.unwrap();
        assert_eq!(result["greeting"], json!("hello"));
        assert_eq!(result["count"], json!(42));
    }

    #[tokio::test]
    async fn set_with_expression() {
        let yaml = r#"
do:
  - init:
      set:
        doubled: "${ .value * 2 }"
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({"value": 21}));
        let shared = test_shared();
        let result = run(&def, &event, &shared).await.unwrap();
        assert_eq!(result["doubled"], json!(42));
    }

    #[tokio::test]
    async fn switch_exit() {
        let yaml = r#"
do:
  - check:
      switch:
        - done:
            when: ".status == \"complete\""
            then: exit
        - default:
            then: continue
  - unreachable:
      set:
        reached: true
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({"status": "complete"}));
        let shared = test_shared();
        let result = run(&def, &event, &shared).await.unwrap();
        // The second task should not execute
        assert_eq!(result, Value::Null);
    }

    #[tokio::test]
    async fn switch_goto() {
        let yaml = r#"
do:
  - route:
      switch:
        - big:
            when: ".size > 100"
            then: handleBig
        - default:
            then: handleSmall
  - handleSmall:
      set:
        handler: "small"
      then: exit
  - handleBig:
      set:
        handler: "big"
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({"size": 200}));
        let shared = test_shared();
        let result = run(&def, &event, &shared).await.unwrap();
        assert_eq!(result["handler"], json!("big"));
    }

    #[tokio::test]
    async fn conditional_if_skip() {
        let yaml = r#"
do:
  - maybeRun:
      if: ".enabled"
      set:
        ran: true
  - always:
      set:
        done: true
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({"enabled": false}));
        let shared = test_shared();
        let result = run(&def, &event, &shared).await.unwrap();
        assert_eq!(result["done"], json!(true));
        assert_eq!(result.get("ran"), None);
    }

    #[tokio::test]
    async fn nested_do() {
        let yaml = r#"
do:
  - outer:
      do:
        - inner1:
            set:
              x: 1
        - inner2:
            set:
              y: 2
  - final:
      set:
        z: 3
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));
        let shared = test_shared();
        let result = run(&def, &event, &shared).await.unwrap();
        assert_eq!(result["z"], json!(3));
    }

    #[tokio::test]
    async fn max_steps_exceeded() {
        let yaml = r#"
do:
  - loop:
      set:
        i: 1
      then: loop
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));
        let shared = test_shared();
        let err = run(&def, &event, &shared).await.unwrap_err();
        assert!(matches!(err, WorkflowError::MaxStepsExceeded(_)));
    }

    #[tokio::test]
    async fn cancellation_stops_execution() {
        let yaml = r#"
do:
  - step1:
      set:
        a: 1
  - step2:
      set:
        b: 2
  - step3:
      set:
        c: 3
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));
        let shared = test_shared();

        // Create a pre-cancelled handle
        let handle = ExecutionHandle::with_cancel(1000, CancellationToken::new());
        handle.cancel.cancel();

        let result = run_with_handle(&def, &event, &shared, &handle).await;
        assert!(matches!(result, Err(WorkflowError::Cancelled)));
    }

    #[tokio::test]
    async fn step_completes_within_timeout() {
        let yaml = r#"
do:
  - fast:
      set:
        done: true
      timeout: 5000
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));
        let shared = test_shared();
        let result = run(&def, &event, &shared).await.unwrap();
        assert_eq!(result["done"], json!(true));
    }

    #[tokio::test]
    async fn default_timeout_does_not_interfere() {
        let yaml = r#"
do:
  - fast:
      set:
        done: true
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));
        let shared = test_shared();
        let handle = ExecutionHandle::new(1000)
            .with_default_timeout(std::time::Duration::from_secs(5));
        let result = run_with_handle(&def, &event, &shared, &handle).await.unwrap();
        assert_eq!(result["done"], json!(true));
    }

    #[tokio::test]
    async fn per_step_timeout_overrides_default() {
        // Default is 1ms (would timeout if used), per-step is 5s (generous)
        let yaml = r#"
do:
  - fast:
      set:
        done: true
      timeout: 5000
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));
        let shared = test_shared();
        let handle = ExecutionHandle::new(1000)
            .with_default_timeout(std::time::Duration::from_millis(1));
        let result = run_with_handle(&def, &event, &shared, &handle).await.unwrap();
        // Completed — per-step 5s overrode the 1ms default
        assert_eq!(result["done"], json!(true));
    }

    #[tokio::test]
    async fn try_catch_finally_success() {
        let yaml = r#"
do:
  - protected:
      try:
        do:
          - step1:
              set:
                result: "ok"
      catch:
        as: err
        do:
          - handle:
              set:
                caught: true
      finally:
        do:
          - cleanup:
              set:
                cleaned: true
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));
        let shared = test_shared();
        let result = run(&def, &event, &shared).await.unwrap();
        assert_eq!(result["cleaned"], json!(true));
    }

    #[tokio::test]
    async fn checkpoint_saved_during_execution() {
        use crate::workflows::checkpoint::FileCheckpointWriter;

        let tmp = tempfile::TempDir::new().unwrap();
        let writer = Arc::new(FileCheckpointWriter::new(tmp.path()).unwrap());

        let yaml = r#"
do:
  - step1:
      set:
        x: 1
  - step2:
      set:
        y: 2
  - step3:
      set:
        z: 3
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let event = test_event(json!({}));
        let shared = test_shared();

        let handle = ExecutionHandle::new(1000).with_checkpoint(
            Arc::clone(&writer),
            "cp-test-1".into(),
            "test-wf".into(),
        );

        let result = run_with_handle(&def, &event, &shared, &handle).await.unwrap();
        assert_eq!(result["z"], json!(3));

        // After completion, the last checkpoint should exist
        // (TaskStore removes it, but executor alone doesn't)
        let cp = writer.load("cp-test-1").await.unwrap().unwrap();
        assert_eq!(cp.step_index, 3); // past all 3 steps
        assert_eq!(cp.workflow_name, "test-wf");
        assert_eq!(cp.context.context["z"], json!(3));
    }

    #[tokio::test]
    async fn resume_skips_completed_steps() {
        use crate::workflows::checkpoint::{Checkpoint, CheckpointContext, FileCheckpointWriter};

        let tmp = tempfile::TempDir::new().unwrap();
        let writer = Arc::new(FileCheckpointWriter::new(tmp.path()).unwrap());

        // Workflow: 3 steps. We'll pretend step 1 already ran.
        let yaml = r#"
do:
  - step1:
      set:
        x: 1
  - step2:
      set:
        y: 2
  - step3:
      set:
        z: 3
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let shared = test_shared();

        // Simulate a checkpoint after step1 completed
        let cp = Checkpoint {
            task_id: "resume-test".into(),
            workflow_name: "test-wf".into(),
            step_index: 1, // resume from step index 1 (step2)
            global_steps: 1,
            context: CheckpointContext {
                event: json!({}),
                context: json!({"x": 1}),
                input: json!({"x": 1}),
                output: json!({"x": 1}),
            },
            started_at_epoch_ms: 0,
            last_step_name: Some("step1".into()),
        };

        let handle = ExecutionHandle::new(1000).with_checkpoint(
            Arc::clone(&writer),
            "resume-test".into(),
            "test-wf".into(),
        );

        let result = resume_from_checkpoint(&def, &cp, &shared, &handle).await.unwrap();

        // step2 and step3 ran (x=1 from checkpoint, y=2 and z=3 from execution)
        assert_eq!(result["z"], json!(3));

        // Step counter: started at 1 (from checkpoint), ran 2 more steps = 3 total
        assert_eq!(handle.step_count(), 3);
    }
}
