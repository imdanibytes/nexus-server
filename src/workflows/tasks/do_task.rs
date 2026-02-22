//! `do` task handler — nested sequential execution.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::server::SharedState;
use crate::workflows::context::WorkflowContext;
use crate::workflows::error::WorkflowError;
use crate::workflows::executor::Outcome;
use crate::workflows::handle::ExecutionHandle;
use crate::workflows::model::TaskDef;

/// Execute a nested `do` block by delegating to the executor.
pub async fn execute(
    tasks: &[BTreeMap<String, TaskDef>],
    ctx: &mut WorkflowContext,
    shared: &Arc<SharedState>,
    handle: &ExecutionHandle,
) -> Result<Outcome, WorkflowError> {
    crate::workflows::executor::execute_task_list(tasks, ctx, shared, handle).await
}
