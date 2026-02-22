//! `try` task handler — error handling with try/catch/finally.
//!
//! `finally` is a nexus extension to the CNCF Serverless Workflow spec.
//! It guarantees cleanup runs regardless of success or failure.

use std::sync::Arc;

use serde_json::Value;
use tracing::info;

use crate::server::SharedState;
use crate::workflows::context::WorkflowContext;
use crate::workflows::error::WorkflowError;
use crate::workflows::executor::{self, Outcome};
use crate::workflows::handle::ExecutionHandle;
use crate::workflows::model::TaskDef;

/// Execute a `try`/`catch`/`finally` task.
///
/// Execution order:
/// 1. `try` block runs.
/// 2. If it fails -> `catch` block runs with error in context.
/// 3. `finally` block always runs, regardless of outcome.
/// 4. If `finally` fails, its error takes precedence.
pub async fn execute(
    task: &TaskDef,
    ctx: &mut WorkflowContext,
    shared: &Arc<SharedState>,
    handle: &ExecutionHandle,
) -> Result<Outcome, WorkflowError> {
    let try_block = task
        .try_
        .as_ref()
        .ok_or_else(|| WorkflowError::Task("try task missing try block".into()))?;

    // 1. Run try block
    let try_result =
        executor::execute_task_list(&try_block.do_, ctx, shared, handle).await;

    let mut outcome = Outcome::Completed;
    let mut deferred_error: Option<WorkflowError> = None;

    match try_result {
        Ok(Outcome::Exited) => outcome = Outcome::Exited,
        Ok(Outcome::Completed) => {}
        Err(err) => {
            info!(error = %err, "workflow: try block failed, running catch");

            // 2. Run catch block if present
            if let Some(ref catch) = task.catch {
                let error_var = catch.as_.as_deref().unwrap_or("error");
                let error_value = serde_json::json!({ "message": err.to_string() });

                // Inject error into context
                if let Value::Object(ref mut map) = ctx.context {
                    map.insert(error_var.to_string(), error_value);
                }

                match executor::execute_task_list(&catch.do_, ctx, shared, handle).await
                {
                    Ok(Outcome::Exited) => outcome = Outcome::Exited,
                    Ok(Outcome::Completed) => {}
                    Err(catch_err) => deferred_error = Some(catch_err),
                }
            } else {
                // No catch block — propagate the original error (after finally)
                deferred_error = Some(err);
            }
        }
    }

    // 3. Run finally block (always)
    if let Some(ref finally) = task.finally {
        info!("workflow: running finally block");
        if let Err(finally_err) =
            executor::execute_task_list(&finally.do_, ctx, shared, handle).await
        {
            // Finally error takes precedence
            return Err(finally_err);
        }
    }

    // Propagate deferred error from try/catch
    if let Some(err) = deferred_error {
        return Err(err);
    }

    Ok(outcome)
}
