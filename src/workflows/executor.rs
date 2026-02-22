//! Workflow executor — walks task lists and dispatches to handlers.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use cloudevents::Event;
use serde_json::Value;
use tracing::{info, warn};

use crate::workflows::context::WorkflowContext;
use crate::workflows::error::WorkflowError;
use crate::workflows::expressions;
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
    client: &reqwest::Client,
) -> Result<Value, WorkflowError> {
    let mut ctx = WorkflowContext::from_event(event);
    let mut steps = 0usize;

    info!("workflow: starting execution");
    let outcome =
        execute_task_list(&def.do_, &mut ctx, client, &mut steps, DEFAULT_MAX_STEPS).await?;
    info!(steps, outcome = ?outcome, "workflow: execution complete");

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
    client: &'a reqwest::Client,
    steps: &'a mut usize,
    max_steps: usize,
) -> Pin<Box<dyn Future<Output = Result<Outcome, WorkflowError>> + Send + 'a>> {
    Box::pin(execute_task_list_impl(
        tasks, ctx, client, steps, max_steps,
    ))
}

/// Inner implementation of the task list executor.
async fn execute_task_list_impl(
    tasks: &[BTreeMap<String, TaskDef>],
    ctx: &mut WorkflowContext,
    client: &reqwest::Client,
    steps: &mut usize,
    max_steps: usize,
) -> Result<Outcome, WorkflowError> {
    let mut idx = 0;

    'outer: while idx < tasks.len() {
        let task_map = &tasks[idx];

        for (task_name, task_def) in task_map {
            *steps += 1;
            if *steps > max_steps {
                return Err(WorkflowError::MaxStepsExceeded(max_steps));
            }

            // Conditional execution
            if let Some(ref cond) = task_def.if_ {
                if !expressions::evaluate_bool(cond, &ctx.input, &ctx.vars())? {
                    info!(task = %task_name, "workflow: skipping (condition false)");
                    continue;
                }
            }

            info!(task = %task_name, "workflow: executing");

            // Input transform
            if let Some(ref input_def) = task_def.input {
                if let Some(ref from) = input_def.from {
                    ctx.apply_input_transform(from)?;
                }
            }

            // Dispatch task and get flow directive
            let directive =
                dispatch_task(task_name, task_def, ctx, client, steps, max_steps).await?;

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
                FlowDirective::Continue => ctx.advance(),
                FlowDirective::Exit => return Ok(Outcome::Exited),
                FlowDirective::GoTo(ref target) => {
                    ctx.advance();
                    idx = find_task_index(tasks, target).ok_or_else(|| {
                        WorkflowError::Task(format!("goto target '{target}' not found"))
                    })?;
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
    client: &reqwest::Client,
    steps: &mut usize,
    max_steps: usize,
) -> Result<FlowDirective, WorkflowError> {
    match task_def.kind() {
        TaskKind::Call { target, with } => {
            let result = call::execute(target, with, ctx, client).await?;
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
            match do_task::execute(subtasks, ctx, client, steps, max_steps).await? {
                Outcome::Exited => Ok(FlowDirective::Exit),
                Outcome::Completed => Ok(FlowDirective::Continue),
            }
        }
        TaskKind::Try => {
            match try_task::execute(task_def, ctx, client, steps, max_steps).await? {
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

fn find_task_index(tasks: &[BTreeMap<String, TaskDef>], name: &str) -> Option<usize> {
    tasks.iter().position(|map| map.contains_key(name))
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

    fn http_client() -> reqwest::Client {
        reqwest::Client::new()
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
        let result = run(&def, &event, &http_client()).await.unwrap();
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
        let result = run(&def, &event, &http_client()).await.unwrap();
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
        let result = run(&def, &event, &http_client()).await.unwrap();
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
        let result = run(&def, &event, &http_client()).await.unwrap();
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
        let result = run(&def, &event, &http_client()).await.unwrap();
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
        let result = run(&def, &event, &http_client()).await.unwrap();
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
        let err = run(&def, &event, &http_client()).await.unwrap_err();
        assert!(matches!(err, WorkflowError::MaxStepsExceeded(_)));
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
        let result = run(&def, &event, &http_client()).await.unwrap();
        assert_eq!(result["cleaned"], json!(true));
    }
}
