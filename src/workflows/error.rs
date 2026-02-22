//! Workflow-level error type.

use super::expressions::ExprError;

#[derive(Debug, thiserror::Error)]
pub enum WorkflowError {
    #[error("expression error: {0}")]
    Expression(#[from] ExprError),

    #[error("task error: {0}")]
    Task(String),

    #[error("max steps exceeded: limit is {0}")]
    MaxStepsExceeded(usize),

    #[error("workflow not found: {0}")]
    NotFound(String),

    #[error("workflow cancelled")]
    Cancelled,

    #[error("step '{step_name}' timed out after {duration_ms}ms")]
    StepTimeout { step_name: String, duration_ms: u64 },
}
