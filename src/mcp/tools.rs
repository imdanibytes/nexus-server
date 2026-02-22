use std::path::Path;
use std::sync::atomic::Ordering;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::config::Config;
use crate::server::SharedState;

const REPO: &str = "imdanibytes/nexus-server";

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
        json!({
            "name": "check_update",
            "description": "Check if a newer version of nexus-server is available on GitHub Releases.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "apply_update",
            "description": "Download the latest release, verify its SHA256 checksum, replace the running binary, and restart. The server will briefly disconnect while restarting.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "version": {
                        "type": "string",
                        "description": "Target version tag (e.g. 'v0.2.0'). If omitted, updates to latest."
                    }
                }
            }
        }),
        json!({
            "name": "list_workflows",
            "description": "List all loaded workflow definitions.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "list_tasks",
            "description": "List all workflow task executions with their status.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "get_task",
            "description": "Get details of a specific workflow task execution.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Task ID" }
                },
                "required": ["id"]
            }
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
        "check_update" => check_update(state).await,
        "apply_update" => apply_update(args, state).await,
        "list_workflows" => list_workflows(state),
        "list_tasks" => list_tasks(state).await,
        "get_task" => get_task(args, state).await,
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
    json!({
        "source_count": state.source_count,
        "note": "Source details are consumed during router initialization. Use /status for counts."
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
        "sources": state.source_count,
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

// ---------------------------------------------------------------------------
// Self-update tools
// ---------------------------------------------------------------------------

/// Returns the Rust target triple baked in at compile time.
fn current_target() -> &'static str {
    env!("TARGET")
}

async fn check_update(state: &SharedState) -> String {
    let current = env!("CARGO_PKG_VERSION");
    let current_ver = match semver::Version::parse(current) {
        Ok(v) => v,
        Err(e) => return format!("Error parsing current version: {e}"),
    };

    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = match state
        .http_client
        .get(&url)
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github+json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return format!("Error fetching latest release: {e}"),
    };

    if resp.status().as_u16() == 404 {
        return json!({
            "current_version": current,
            "update_available": false,
            "message": "No releases found"
        })
        .to_string();
    }

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return format!("GitHub API error {status}: {body}");
    }

    let release: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return format!("Error parsing release: {e}"),
    };

    let tag = release["tag_name"].as_str().unwrap_or("");
    let latest_str = tag.strip_prefix('v').unwrap_or(tag);
    let latest_ver = match semver::Version::parse(latest_str) {
        Ok(v) => v,
        Err(e) => return format!("Error parsing release version '{tag}': {e}"),
    };

    let update_available = latest_ver > current_ver;
    let target = current_target();
    let asset_name = format!("nexus-server-{target}");

    let asset = release["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["name"].as_str() == Some(&asset_name)));

    let mut result = json!({
        "current_version": current,
        "latest_version": latest_str,
        "update_available": update_available,
        "tag": tag,
        "published_at": release["published_at"],
    });

    if let Some(asset) = asset {
        result["download_url"] = asset["browser_download_url"].clone();
        result["asset_size"] = asset["size"].clone();
    } else if update_available {
        result["warning"] = json!(format!("No binary for target '{target}'"));
    }

    serde_json::to_string_pretty(&result).unwrap_or_default()
}

async fn apply_update(args: &Value, state: &SharedState) -> String {
    let current = env!("CARGO_PKG_VERSION");
    let current_ver = match semver::Version::parse(current) {
        Ok(v) => v,
        Err(e) => return format!("Error parsing current version: {e}"),
    };

    // Fetch release metadata
    let release_url = match args["version"].as_str() {
        Some(tag) => format!("https://api.github.com/repos/{REPO}/releases/tags/{tag}"),
        None => format!("https://api.github.com/repos/{REPO}/releases/latest"),
    };

    let release: Value = match state
        .http_client
        .get(&release_url)
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github+json")
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => match resp.json().await {
            Ok(v) => v,
            Err(e) => return format!("Error parsing release: {e}"),
        },
        Ok(resp) => return format!("Release not found (HTTP {})", resp.status().as_u16()),
        Err(e) => return format!("Error fetching release: {e}"),
    };

    // Parse and validate version (prevent rollback)
    let tag = release["tag_name"].as_str().unwrap_or("");
    let target_str = tag.strip_prefix('v').unwrap_or(tag);
    let target_ver = match semver::Version::parse(target_str) {
        Ok(v) => v,
        Err(e) => return format!("Error parsing target version '{tag}': {e}"),
    };

    if target_ver <= current_ver {
        return json!({
            "error": "version_not_newer",
            "current": current,
            "target": target_str,
            "message": "Target version is not newer than running version. Rollback not supported."
        })
        .to_string();
    }

    // Find binary asset for this platform
    let target = current_target();
    let asset_name = format!("nexus-server-{target}");
    let assets = match release["assets"].as_array() {
        Some(a) => a,
        None => return "No assets found in release".to_string(),
    };

    let download_url = match assets
        .iter()
        .find(|a| a["name"].as_str() == Some(&asset_name))
        .and_then(|a| a["browser_download_url"].as_str())
    {
        Some(u) => u.to_owned(),
        None => return format!("No binary for target '{target}' in release {tag}"),
    };

    // Find expected checksum
    let expected_sha256 = if let Some(cs_url) = assets
        .iter()
        .find(|a| a["name"].as_str() == Some("checksums-sha256.txt"))
        .and_then(|a| a["browser_download_url"].as_str())
    {
        match download_text(&state.http_client, cs_url).await {
            Ok(text) => text
                .lines()
                .find(|l| l.contains(&asset_name))
                .and_then(|l| l.split_whitespace().next())
                .map(|s| s.to_owned()),
            Err(e) => return format!("Error downloading checksums: {e}"),
        }
    } else {
        None
    };

    // Download binary to temp file
    let tmp_path = std::env::temp_dir().join(format!("nexus-server-update-{target_str}"));
    if let Err(e) = download_file(&state.http_client, &download_url, &tmp_path).await {
        return format!("Error downloading binary: {e}");
    }

    // Verify SHA256
    if let Some(ref expected) = expected_sha256 {
        match verify_sha256(&tmp_path, expected) {
            Ok(true) => {}
            Ok(false) => {
                let _ = std::fs::remove_file(&tmp_path);
                return "SHA256 checksum mismatch — update aborted.".to_string();
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_path);
                return format!("Error computing checksum: {e}");
            }
        }
    } else {
        warn!("no checksums file in release, skipping verification");
    }

    // Set executable permission
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755));
    }

    // Atomic binary swap
    if let Err(e) = self_replace::self_replace(&tmp_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return format!("Error replacing binary: {e}");
    }
    let _ = std::fs::remove_file(&tmp_path);

    tracing::info!(from = current, to = target_str, "binary replaced, re-executing");

    // Delay re-exec so the MCP response can flush
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        do_reexec();
    });

    json!({
        "success": true,
        "previous_version": current,
        "new_version": target_str,
        "message": "Binary replaced. Server is restarting..."
    })
    .to_string()
}

async fn download_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let resp = client
        .get(url)
        .header("user-agent", "nexus-server")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    resp.text().await.map_err(|e| e.to_string())
}

async fn download_file(client: &reqwest::Client, url: &str, dest: &Path) -> Result<(), String> {
    let resp = client
        .get(url)
        .header("user-agent", "nexus-server")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    std::fs::write(dest, &bytes).map_err(|e| e.to_string())
}

fn verify_sha256(path: &Path, expected: &str) -> Result<bool, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let hash = hex::encode(Sha256::digest(&data));
    Ok(hash == expected.to_lowercase())
}

// ---------------------------------------------------------------------------
// Workflow tools
// ---------------------------------------------------------------------------

fn list_workflows(state: &SharedState) -> String {
    let names = state.workflow_store.names();
    let list: Vec<Value> = names
        .iter()
        .map(|name| json!({ "name": name }))
        .collect();
    serde_json::to_string_pretty(&list).unwrap_or_default()
}

async fn list_tasks(state: &SharedState) -> String {
    let tasks = state.task_store.list().await;
    serde_json::to_string_pretty(&tasks).unwrap_or_default()
}

async fn get_task(args: &Value, state: &SharedState) -> String {
    let id = match args["id"].as_str() {
        Some(id) => id,
        None => return "Error: missing 'id' argument".to_string(),
    };

    match state.task_store.get(id).await {
        Some(task) => serde_json::to_string_pretty(&task).unwrap_or_default(),
        None => format!("Task '{id}' not found"),
    }
}

// ---------------------------------------------------------------------------
// Self-update internals
// ---------------------------------------------------------------------------

fn do_reexec() -> ! {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().expect("failed to get current exe path");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let err = std::process::Command::new(exe).args(&args).exec();
    eprintln!("FATAL: re-exec failed: {err}");
    std::process::exit(1);
}
