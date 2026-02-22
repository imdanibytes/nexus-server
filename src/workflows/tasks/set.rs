//! `set` task handler — modifies workflow context with evaluated expressions.

use serde_json::Value;
use std::collections::BTreeMap;

use crate::workflows::context::WorkflowContext;
use crate::workflows::error::WorkflowError;
use crate::workflows::expressions;

/// Execute a `set` task: evaluate each value expression and merge into context.
pub fn execute(
    values: &BTreeMap<String, Value>,
    ctx: &mut WorkflowContext,
) -> Result<Value, WorkflowError> {
    let mut result = serde_json::Map::new();

    for (key, value) in values {
        let resolved = expressions::resolve_value(value, &ctx.input, &ctx.vars())?;
        result.insert(key.clone(), resolved);
    }

    let result_value = Value::Object(result);

    // Merge into context
    if let (Value::Object(ref mut ctx_map), Value::Object(ref new_map)) =
        (&mut ctx.context, &result_value)
    {
        for (k, v) in new_map {
            ctx_map.insert(k.clone(), v.clone());
        }
    } else {
        ctx.context = result_value.clone();
    }

    Ok(result_value)
}
