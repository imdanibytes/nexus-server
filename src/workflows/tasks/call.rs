//! `call` task handler — HTTP calls and custom action delegation.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tracing::info;

use crate::sandbox::{self, Sandbox, SandboxConfig};
use crate::server::SharedState;
use crate::workflows::context::WorkflowContext;
use crate::workflows::error::WorkflowError;
use crate::workflows::expressions;
use crate::workflows::handle::ExecutionHandle;

/// Execute a `call` task.
///
/// - `call: http` -> direct HTTP request via reqwest
/// - `call: custom:sandbox` -> create/exec/destroy coding sandboxes
/// - `call: custom:agent` -> run the coding agent with optional sandbox
pub async fn execute(
    target: &str,
    with: Option<&Value>,
    ctx: &WorkflowContext,
    shared: &Arc<SharedState>,
    handle: &ExecutionHandle,
) -> Result<Value, WorkflowError> {
    match target {
        "http" => execute_http(with, ctx, &shared.http_client, handle).await,
        other if other.starts_with("custom:") => {
            let action_name = &other[7..];
            execute_custom(action_name, with, ctx, shared, handle).await
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
    handle: &ExecutionHandle,
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

    // Race HTTP request against cancellation
    let resp = tokio::select! {
        result = req.send() => {
            result.map_err(|e| WorkflowError::Task(format!("HTTP request failed: {e}")))?
        }
        _ = handle.cancel.cancelled() => {
            return Err(WorkflowError::Cancelled);
        }
    };

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
    with: Option<&Value>,
    ctx: &WorkflowContext,
    shared: &Arc<SharedState>,
    handle: &ExecutionHandle,
) -> Result<Value, WorkflowError> {
    match action_name {
        "sandbox" => execute_sandbox(with, ctx, shared).await,
        "agent" => execute_agent(with, ctx, shared, handle).await,
        other => Err(WorkflowError::Task(format!(
            "unknown custom action '{other}'"
        ))),
    }
}

/// `call: custom:sandbox` — manage coding sandboxes.
///
/// Actions:
/// - `create`: Clone repo, build container, register in sandbox registry.
///   Requires: `repo_url`, `git_ref` (optional, defaults to "main").
///   Returns: `{ "sandbox_id": "<container_name>" }`
/// - `exec`: Run a command in an existing sandbox.
///   Requires: `sandbox_id`, `command`.
///   Returns: `{ "exit_code": N, "stdout": "...", "stderr": "..." }`
/// - `destroy`: Destroy a sandbox and clean up.
///   Requires: `sandbox_id`.
async fn execute_sandbox(
    with: Option<&Value>,
    ctx: &WorkflowContext,
    shared: &Arc<SharedState>,
) -> Result<Value, WorkflowError> {
    let args = match with {
        Some(v) => expressions::resolve_object(v, &ctx.input, &ctx.vars())?,
        None => return Err(WorkflowError::Task("custom:sandbox requires 'with' arguments".into())),
    };

    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| WorkflowError::Task("custom:sandbox requires 'action' field".into()))?;

    match action {
        "create" => {
            let repo_url = args
                .get("repo_url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| WorkflowError::Task("sandbox create requires 'repo_url'".into()))?;
            let git_ref = args
                .get("git_ref")
                .and_then(|v| v.as_str())
                .unwrap_or("main");

            let github_token = shared
                .github_token()
                .await
                .map_err(|e| WorkflowError::Task(format!("github token: {e}")))?;

            let container_name = format!("nexus-sandbox-{}", uuid::Uuid::new_v4().simple());

            info!(repo = %repo_url, git_ref = %git_ref, container = %container_name, "workflow: creating sandbox");

            // Clone repo
            let temp_dir = sandbox::clone_repo(repo_url, git_ref, &github_token)
                .await
                .map_err(|e| WorkflowError::Task(format!("clone failed: {e}")))?;

            // Build config from cloned repo
            let config = SandboxConfig::from_repo(
                &container_name,
                std::path::Path::new(&temp_dir),
                HashMap::new(),
            );

            // Create container
            let sb = Sandbox::create(&config)
                .await
                .map_err(|e| {
                    sandbox::cleanup_temp_dir(&temp_dir);
                    WorkflowError::Task(format!("sandbox create failed: {e}"))
                })?;

            // Clean up host temp dir (repo is already copied into container)
            sandbox::cleanup_temp_dir(&temp_dir);

            // Register in shared registry
            let sandbox_id = shared.sandbox_registry.insert(Arc::new(sb)).await;

            Ok(serde_json::json!({ "sandbox_id": sandbox_id }))
        }
        "exec" => {
            let sandbox_id = args
                .get("sandbox_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| WorkflowError::Task("sandbox exec requires 'sandbox_id'".into()))?;
            let command = args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| WorkflowError::Task("sandbox exec requires 'command'".into()))?;

            let sb = shared
                .sandbox_registry
                .get(sandbox_id)
                .await
                .ok_or_else(|| WorkflowError::Task(format!("sandbox '{sandbox_id}' not found")))?;

            let result = sb
                .exec_shell(command)
                .await
                .map_err(|e| WorkflowError::Task(format!("sandbox exec failed: {e}")))?;

            Ok(serde_json::json!({
                "exit_code": result.exit_code,
                "stdout": result.stdout,
                "stderr": result.stderr,
            }))
        }
        "destroy" => {
            let sandbox_id = args
                .get("sandbox_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| WorkflowError::Task("sandbox destroy requires 'sandbox_id'".into()))?;

            let sb = shared
                .sandbox_registry
                .remove(sandbox_id)
                .await
                .ok_or_else(|| WorkflowError::Task(format!("sandbox '{sandbox_id}' not found")))?;

            sb.destroy()
                .await
                .map_err(|e| WorkflowError::Task(format!("sandbox destroy failed: {e}")))?;

            Ok(serde_json::json!({ "destroyed": sandbox_id }))
        }
        other => Err(WorkflowError::Task(format!(
            "unknown sandbox action '{other}'"
        ))),
    }
}

/// `call: custom:agent` — run the coding agent.
///
/// Requires: `prompt`.
/// Optional: `sandbox_id` — attach a sandbox for filesystem tools.
/// Returns: `{ "output": "<agent final text>" }`
async fn execute_agent(
    with: Option<&Value>,
    ctx: &WorkflowContext,
    shared: &Arc<SharedState>,
    handle: &ExecutionHandle,
) -> Result<Value, WorkflowError> {
    let args = match with {
        Some(v) => expressions::resolve_object(v, &ctx.input, &ctx.vars())?,
        None => return Err(WorkflowError::Task("custom:agent requires 'with' arguments".into())),
    };

    let prompt = args
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| WorkflowError::Task("custom:agent requires 'prompt'".into()))?;

    let claude_config = shared
        .claude
        .as_ref()
        .ok_or_else(|| WorkflowError::Task("[claude] config section required for agent".into()))?;

    let github_token = shared
        .github_token()
        .await
        .map_err(|e| WorkflowError::Task(format!("github token: {e}")))?;

    let mut agent = crate::agent::AgentLoop::new(
        claude_config.clone(),
        github_token,
        shared.http_client.clone(),
    )
    .with_cancel(handle.cancel.clone());

    // Attach sandbox if specified
    if let Some(sandbox_id) = args.get("sandbox_id").and_then(|v| v.as_str()) {
        let sb = shared
            .sandbox_registry
            .get(sandbox_id)
            .await
            .ok_or_else(|| WorkflowError::Task(format!("sandbox '{sandbox_id}' not found")))?;
        agent = agent.with_sandbox(sb);
    }

    info!(prompt_len = prompt.len(), "workflow: starting agent");

    let output = agent
        .run(prompt)
        .await
        .map_err(|e| WorkflowError::Task(format!("agent failed: {e}")))?;

    Ok(serde_json::json!({ "output": output }))
}
