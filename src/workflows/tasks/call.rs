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
/// Optional: `system_prompt` — custom system prompt for the agent.
/// Returns: `{ "output": "<agent final text>" }`
async fn execute_agent(
    with: Option<&Value>,
    ctx: &WorkflowContext,
    shared: &Arc<SharedState>,
    handle: &ExecutionHandle,
) -> Result<Value, WorkflowError> {
    let args = match with {
        Some(v) => expressions::resolve_object(v, &ctx.input, &ctx.vars())?,
        None => {
            return Err(WorkflowError::Task(
                "custom:agent requires 'with' arguments".into(),
            ))
        }
    };

    let prompt = args
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| WorkflowError::Task("custom:agent requires 'prompt'".into()))?;

    let claude_config = shared
        .claude
        .as_ref()
        .ok_or_else(|| WorkflowError::Task("[claude] config section required for agent".into()))?;

    let api_key = std::env::var(&claude_config.api_key_env).unwrap_or_default();

    let github_token = shared
        .github_token()
        .await
        .map_err(|e| WorkflowError::Task(format!("github token: {e}")))?;

    // Build provider
    let provider =
        nexus_agent::AnthropicProvider::with_client(shared.http_client.clone(), api_key);

    // Build tools: GitHub tools + optional sandbox tools
    let mut registry =
        crate::agent::github_toolset(shared.http_client.clone(), github_token);

    if let Some(sandbox_id) = args.get("sandbox_id").and_then(|v| v.as_str()) {
        let sb = shared
            .sandbox_registry
            .get(sandbox_id)
            .await
            .ok_or_else(|| WorkflowError::Task(format!("sandbox '{sandbox_id}' not found")))?;
        registry = crate::agent::add_sandbox_tools(registry, sb);
    }

    // Build context manager
    let system_prompt = args
        .get("system_prompt")
        .and_then(|v| v.as_str())
        .unwrap_or(
            "You are an autonomous coding agent. Use the available tools to accomplish the task. Be concise and focused.",
        );

    let context = nexus_agent::ManagedContextManager::new(
        &claude_config.model,
        claude_config.max_tokens,
        200_000, // Claude context window
    )
    .with_system(system_prompt)
    .with_tools(registry.schemas());

    let tools = nexus_agent::ToolPipeline::new(registry);

    let config = nexus_agent::AgentConfig {
        model: claude_config.model.clone(),
        max_tokens: claude_config.max_tokens,
        max_turns: 20,
        session_id: None,
    };

    let mut agent = nexus_agent::Agent::new(provider, context, tools, config);

    info!(prompt_len = prompt.len(), "workflow: starting agent");

    // Stream agent events into the handle's log buffer for observability
    let (tx, mut rx) = tokio::sync::mpsc::channel::<nexus_agent::AgentEvent>(64);
    let log_buf = handle.log_buffer.clone();
    let drain_task = tokio::spawn(async move {
        use crate::workflows::handle::push_log;
        while let Some(event) = rx.recv().await {
            let entry = match &event {
                nexus_agent::AgentEvent::TurnStart { turn } => {
                    format!("[turn {turn}] started")
                }
                nexus_agent::AgentEvent::Thinking { content } => {
                    let preview = if content.len() > 120 {
                        format!("{}...", &content[..120])
                    } else {
                        content.clone()
                    };
                    format!("[thinking] {preview}")
                }
                nexus_agent::AgentEvent::Text { content } => {
                    let preview = if content.len() > 200 {
                        format!("{}...", &content[..200])
                    } else {
                        content.clone()
                    };
                    format!("[text] {preview}")
                }
                nexus_agent::AgentEvent::ToolCall { name, input } => {
                    let input_str = input.to_string();
                    let preview = if input_str.len() > 150 {
                        format!("{}...", &input_str[..150])
                    } else {
                        input_str
                    };
                    format!("[tool_call] {name}({preview})")
                }
                nexus_agent::AgentEvent::ToolResult { name, output, is_error } => {
                    let status = if *is_error { "ERR" } else { "ok" };
                    let preview = if output.len() > 200 {
                        format!("{}...", &output[..200])
                    } else {
                        output.clone()
                    };
                    format!("[tool_result] {name} [{status}] {preview}")
                }
                nexus_agent::AgentEvent::Compacted { pre_tokens, post_tokens } => {
                    format!("[compacted] {pre_tokens} -> {post_tokens} tokens")
                }
                nexus_agent::AgentEvent::Finished { turns } => {
                    format!("[finished] {turns} turns")
                }
                nexus_agent::AgentEvent::Error { message } => {
                    format!("[error] {message}")
                }
            };
            push_log(&log_buf, entry);
        }
    });

    let result = agent
        .invoke_streaming_with_cancel(prompt, handle.cancel.clone(), tx)
        .await
        .map_err(|e| match e {
            nexus_agent::AgentError::Cancelled => WorkflowError::Cancelled,
            other => WorkflowError::Task(format!("agent failed: {other}")),
        })?;

    // Wait for drain task to finish processing remaining events
    let _ = drain_task.await;

    Ok(serde_json::json!({ "output": result.text }))
}
