use std::sync::atomic::Ordering;

use serde_json::{json, Value};

use crate::config::Config;
use crate::server::SharedState;

/// MCP tool definitions.
pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "list_rules",
            "description": "List all routing rules with their enabled state.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "list_webhooks",
            "description": "List all registered webhook endpoints.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "server_stats",
            "description": "Get server uptime and event processing statistics.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "recent_events",
            "description": "Get the most recently processed webhook events.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Max events to return (default: 20)"
                    }
                }
            }
        }),
        json!({
            "name": "enable_rule",
            "description": "Enable a routing rule by name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Rule name" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "disable_rule",
            "description": "Disable a routing rule by name. Disabled rules are skipped during event matching.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Rule name" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "reload_config",
            "description": "Reload rules from config.toml on disk. Replaces all rules with the new config.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "refresh_github_token",
            "description": "Force-refresh the cached GitHub App installation token.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ]
}

/// Dispatch a tool call and return the result as a string.
pub async fn execute(name: &str, args: &Value, state: &SharedState) -> String {
    match name {
        "list_rules" => list_rules(state).await,
        "list_webhooks" => list_webhooks(state),
        "server_stats" => server_stats(state),
        "recent_events" => recent_events(args, state).await,
        "enable_rule" => set_rule_enabled(args, state, true).await,
        "disable_rule" => set_rule_enabled(args, state, false).await,
        "reload_config" => reload_config(state).await,
        "refresh_github_token" => refresh_github_token(state).await,
        _ => format!("Unknown tool: {name}"),
    }
}

async fn list_rules(state: &SharedState) -> String {
    let rules = state.rules.read().await;
    let list: Vec<Value> = rules
        .iter()
        .map(|r| {
            json!({
                "name": r.name,
                "filter": r.filter.type_prefix,
                "action": r.action,
                "enabled": r.enabled,
            })
        })
        .collect();
    serde_json::to_string_pretty(&list).unwrap_or_default()
}

fn list_webhooks(state: &SharedState) -> String {
    // Webhook configs aren't stored on SharedState (consumed during router build).
    // Return the count — full webhook info would require storing them.
    json!({
        "webhook_count": state.webhook_count,
        "note": "Webhook details are consumed during router initialization. Use /status for counts."
    })
    .to_string()
}

fn server_stats(state: &SharedState) -> String {
    let stats = &state.stats;
    let uptime = chrono::Utc::now() - stats.started_at;

    json!({
        "started_at": stats.started_at.to_rfc3339(),
        "uptime_seconds": uptime.num_seconds(),
        "events_received": stats.events_received.load(Ordering::Relaxed),
        "events_matched": stats.events_matched.load(Ordering::Relaxed),
        "actions_succeeded": stats.actions_succeeded.load(Ordering::Relaxed),
        "actions_failed": stats.actions_failed.load(Ordering::Relaxed),
        "webhooks": state.webhook_count,
    })
    .to_string()
}

async fn recent_events(args: &Value, state: &SharedState) -> String {
    let limit = args["limit"].as_u64().unwrap_or(20) as usize;
    let events = state.recent_events.lock().await;
    let recent: Vec<&crate::server::RecentEvent> = events.iter().rev().take(limit).collect();
    serde_json::to_string_pretty(&recent).unwrap_or_default()
}

async fn set_rule_enabled(args: &Value, state: &SharedState, enabled: bool) -> String {
    let name = match args["name"].as_str() {
        Some(n) => n,
        None => return "Error: missing 'name' argument".to_string(),
    };

    let mut rules = state.rules.write().await;
    if let Some(rule) = rules.iter_mut().find(|r| r.name == name) {
        rule.enabled = enabled;
        let action = if enabled { "enabled" } else { "disabled" };
        format!("Rule '{}' {action}", rule.name)
    } else {
        let available: Vec<&str> = rules.iter().map(|r| r.name.as_str()).collect();
        format!("Rule '{name}' not found. Available: {available:?}")
    }
}

async fn reload_config(state: &SharedState) -> String {
    let path = state.config_path.to_string_lossy().to_string();
    // Convert the non-Send Box<dyn Error> to String before any .await
    match Config::load(&path).map_err(|e| e.to_string()) {
        Ok(new_config) => {
            let count = new_config.rules.len();
            let mut rules = state.rules.write().await;
            *rules = new_config.rules;
            format!("Config reloaded from {path}: {count} rules")
        }
        Err(e) => format!("Error reloading config: {e}"),
    }
}

async fn refresh_github_token(state: &SharedState) -> String {
    match &state.github_app {
        Some(app) => match app.force_refresh().await {
            Ok(_) => "GitHub App token refreshed".to_string(),
            Err(e) => format!("Error refreshing token: {e}"),
        },
        None => "No GitHub App auth configured — using static PAT from env".to_string(),
    }
}
