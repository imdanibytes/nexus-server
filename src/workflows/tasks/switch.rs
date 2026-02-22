//! `switch` task handler — conditional branching.

use std::collections::BTreeMap;

use crate::workflows::context::WorkflowContext;
use crate::workflows::error::WorkflowError;
use crate::workflows::expressions;
use crate::workflows::model::CaseDef;

/// Flow control directive returned by switch evaluation.
pub enum FlowDirective {
    /// Continue to next task in sequence.
    Continue,
    /// Jump to a named task.
    GoTo(String),
    /// Exit the workflow.
    Exit,
}

/// Evaluate switch cases and return a flow directive.
pub fn execute(
    cases: &[BTreeMap<String, CaseDef>],
    ctx: &WorkflowContext,
) -> Result<FlowDirective, WorkflowError> {
    for case_map in cases {
        for (_case_name, case_def) in case_map {
            let matches = match &case_def.when {
                Some(expr) => expressions::evaluate_bool(expr, &ctx.input, &ctx.vars())?,
                None => true, // default case
            };

            if matches {
                return match case_def.then.as_deref() {
                    Some("exit") => Ok(FlowDirective::Exit),
                    Some("continue") | None => Ok(FlowDirective::Continue),
                    Some(target) => Ok(FlowDirective::GoTo(target.to_string())),
                };
            }
        }
    }

    Ok(FlowDirective::Continue)
}
