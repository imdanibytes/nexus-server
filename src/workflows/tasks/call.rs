//! `call` task handler — HTTP calls and custom action delegation.

use serde_json::Value;
use tracing::info;

use crate::workflows::context::WorkflowContext;
use crate::workflows::error::WorkflowError;
use crate::workflows::expressions;

/// Execute a `call` task.
///
/// - `call: http` → direct HTTP request via reqwest
/// - `call: custom:<name>` → delegates to a registered action (future)
pub async fn execute(
    target: &str,
    with: Option<&Value>,
    ctx: &WorkflowContext,
    client: &reqwest::Client,
) -> Result<Value, WorkflowError> {
    match target {
        "http" => execute_http(with, ctx, client).await,
        other if other.starts_with("custom:") => {
            let action_name = &other[7..];
            execute_custom(action_name, with, ctx).await
        }
        other => Err(WorkflowError::Task(format!(
            "unsupported call target: '{other}'"
        ))),
    }
}

async fn execute_http(
    with: Option<&Value>,
    ctx: &WorkflowContext,
    client: &reqwest::Client,
) -> Result<Value, WorkflowError> {
    let args = match with {
        Some(v) => expressions::resolve_object(v, &ctx.input, &ctx.vars())?,
        None => return Err(WorkflowError::Task("call:http requires 'with' arguments".into())),
    };

    let method = args
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("get");
    let endpoint = args
        .get("endpoint")
        .and_then(|v| v.as_str())
        .ok_or_else(|| WorkflowError::Task("call:http requires 'endpoint'".into()))?;

    info!(method, endpoint, "workflow: call http");

    let mut req = match method.to_lowercase().as_str() {
        "get" => client.get(endpoint),
        "post" => client.post(endpoint),
        "put" => client.put(endpoint),
        "delete" => client.delete(endpoint),
        "patch" => client.patch(endpoint),
        "head" => client.head(endpoint),
        other => {
            return Err(WorkflowError::Task(format!(
                "unsupported HTTP method: '{other}'"
            )))
        }
    };

    // Set headers
    if let Some(headers) = args.get("headers").and_then(|v| v.as_object()) {
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                req = req.header(k.as_str(), s);
            }
        }
    }

    // Set body
    if let Some(body) = args.get("body") {
        req = req.header("content-type", "application/json").json(body);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| WorkflowError::Task(format!("HTTP request failed: {e}")))?;

    let status = resp.status().as_u16();
    let resp_body: Value = resp.json().await.unwrap_or(Value::Null);

    info!(status, "workflow: call http response");

    Ok(serde_json::json!({
        "status": status,
        "body": resp_body,
    }))
}

async fn execute_custom(
    action_name: &str,
    _with: Option<&Value>,
    _ctx: &WorkflowContext,
) -> Result<Value, WorkflowError> {
    // TODO: Phase 2 — look up action in ActionRegistry and execute
    Err(WorkflowError::Task(format!(
        "custom action '{action_name}' not yet implemented in workflow executor"
    )))
}
