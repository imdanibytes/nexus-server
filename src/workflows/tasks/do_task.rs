//! `do` task handler — nested sequential execution.

use std::collections::BTreeMap;

use crate::workflows::context::WorkflowContext;
use crate::workflows::error::WorkflowError;
use crate::workflows::executor::Outcome;
use crate::workflows::model::TaskDef;

/// Execute a nested `do` block by delegating to the executor.
pub async fn execute(
    tasks: &[BTreeMap<String, TaskDef>],
    ctx: &mut WorkflowContext,
    client: &reqwest::Client,
    steps: &mut usize,
    max_steps: usize,
) -> Result<Outcome, WorkflowError> {
    crate::workflows::executor::execute_task_list(tasks, ctx, client, steps, max_steps).await
}
