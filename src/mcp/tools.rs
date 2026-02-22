use std::path::Path;
use std::sync::atomic::Ordering;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::server::SharedState;
use crate::workflows::model::WorkflowDef;

const REPO: &str = "imdanibytes/nexus-server";

/// MCP tool definitions.
pub fn definitions() -> Vec<Value> {
    vec![
        // -- Server --
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
            "name": "refresh_github_token",
            "description": "Force-refresh the cached GitHub App installation token.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        // -- Self-update --
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
        // -- Workflows --
        json!({
            "name": "list_workflows",
            "description": "List all loaded workflow definitions.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "create_workflow",
            "description": "Create a new workflow definition from YAML content. Saves to ~/.nexus/workflows/{name}.yaml and loads it into the running server.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Workflow name (used as filename and lookup key)" },
                    "yaml": { "type": "string", "description": "Workflow definition in YAML format (CNCF Serverless Workflow DSL)" }
                },
                "required": ["name", "yaml"]
            }
        }),
        json!({
            "name": "get_workflow",
            "description": "Get the YAML source of a loaded workflow definition.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Workflow name" }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "validate_workflow",
            "description": "Validate a workflow YAML definition without loading it. Returns parse errors if invalid.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "yaml": { "type": "string", "description": "Workflow definition in YAML format" }
                },
                "required": ["yaml"]
            }
        }),
        json!({
            "name": "reload_workflows",
            "description": "Reload all workflow definitions from ~/.nexus/workflows/. Picks up new, modified, and deleted workflow files.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "delete_workflow",
            "description": "Delete a workflow definition. Removes the file from ~/.nexus/workflows/ and unloads it from the running server.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Workflow name to delete" }
                },
                "required": ["name"]
            }
        }),
        // -- Tasks --
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
        "server_stats" => server_stats(state),
        "recent_events" => recent_events(args, state).await,
        "refresh_github_token" => refresh_github_token(state).await,
        "check_update" => check_update(state).await,
        "apply_update" => apply_update(args, state).await,
        "list_workflows" => list_workflows(state).await,
        "create_workflow" => create_workflow(args, state).await,
        "get_workflow" => get_workflow(args, state).await,
        "validate_workflow" => validate_workflow(args),
        "reload_workflows" => reload_workflows(state).await,
        "delete_workflow" => delete_workflow(args, state).await,
        "list_tasks" => list_tasks(state).await,
        "get_task" => get_task(args, state).await,
        _ => format!("Unknown tool: {name}"),
    }
}

// ---------------------------------------------------------------------------
// Server tools
// ---------------------------------------------------------------------------

fn server_stats(state: &SharedState) -> String {
    let stats = &state.stats;
    let uptime = chrono::Utc::now() - stats.started_at;

    json!({
        "started_at": stats.started_at.to_rfc3339(),
        "uptime_seconds": uptime.num_seconds(),
        "events_received": stats.events_received.load(Ordering::Relaxed),
        "events_matched": stats.events_matched.load(Ordering::Relaxed),
        "workflows_succeeded": stats.actions_succeeded.load(Ordering::Relaxed),
        "workflows_failed": stats.actions_failed.load(Ordering::Relaxed),
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

/// Resolve the workflows directory (~/.nexus/workflows/).
fn workflows_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".nexus/workflows"))
}

async fn list_workflows(state: &SharedState) -> String {
    let store = state.workflow_store.read().await;
    let entries = store.list_with_triggers();
    let list: Vec<Value> = entries
        .iter()
        .map(|(name, triggers)| {
            let mut entry = json!({ "name": name });
            if !triggers.is_empty() {
                entry["triggers"] = json!(triggers);
            }
            entry
        })
        .collect();
    serde_json::to_string_pretty(&list).unwrap_or_default()
}

async fn create_workflow(args: &Value, state: &SharedState) -> String {
    let name = match args["name"].as_str() {
        Some(n) => n,
        None => return "Error: missing 'name' argument".to_string(),
    };
    let yaml = match args["yaml"].as_str() {
        Some(y) => y,
        None => return "Error: missing 'yaml' argument".to_string(),
    };

    // Validate YAML parses as a workflow
    if let Err(e) = serde_yaml::from_str::<WorkflowDef>(yaml) {
        return format!("Invalid workflow YAML: {e}");
    }

    // Write to ~/.nexus/workflows/{name}.yaml
    let dir = match workflows_dir() {
        Some(d) => d,
        None => return "Error: cannot determine home directory".to_string(),
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return format!("Error creating workflows directory: {e}");
    }
    let path = dir.join(format!("{name}.yaml"));
    if let Err(e) = std::fs::write(&path, yaml) {
        return format!("Error writing workflow file: {e}");
    }

    // Load into the store
    let mut store = state.workflow_store.write().await;
    match store.load(name, &path) {
        Ok(()) => json!({
            "created": true,
            "name": name,
            "path": path.display().to_string(),
        })
        .to_string(),
        Err(e) => format!("Workflow saved but failed to load: {e}"),
    }
}

async fn get_workflow(args: &Value, state: &SharedState) -> String {
    let name = match args["name"].as_str() {
        Some(n) => n,
        None => return "Error: missing 'name' argument".to_string(),
    };

    // Try to read the YAML source from disk
    if let Some(dir) = workflows_dir() {
        for ext in &["yaml", "yml"] {
            let path = dir.join(format!("{name}.{ext}"));
            if let Ok(content) = std::fs::read_to_string(&path) {
                return json!({
                    "name": name,
                    "path": path.display().to_string(),
                    "yaml": content,
                })
                .to_string();
            }
        }
    }

    // Fallback: check if it's loaded but we can't read the source
    let store = state.workflow_store.read().await;
    if store.get(name).is_some() {
        json!({
            "name": name,
            "loaded": true,
            "note": "Workflow is loaded but YAML source file not found in ~/.nexus/workflows/"
        })
        .to_string()
    } else {
        format!("Workflow '{name}' not found")
    }
}

fn validate_workflow(args: &Value) -> String {
    let yaml = match args["yaml"].as_str() {
        Some(y) => y,
        None => return "Error: missing 'yaml' argument".to_string(),
    };

    match serde_yaml::from_str::<WorkflowDef>(yaml) {
        Ok(def) => {
            let step_count = def.do_.len();
            json!({
                "valid": true,
                "steps": step_count,
            })
            .to_string()
        }
        Err(e) => {
            json!({
                "valid": false,
                "error": e.to_string(),
            })
            .to_string()
        }
    }
}

async fn reload_workflows(state: &SharedState) -> String {
    let dir = match workflows_dir() {
        Some(d) => d,
        None => return "Error: cannot determine home directory".to_string(),
    };

    let mut store = state.workflow_store.write().await;

    // Full clear-and-reload — picks up new, modified, and deleted workflows
    store.clear();
    store.load_from_dir(&dir);

    let names: Vec<String> = store.names().iter().map(|s| s.to_string()).collect();

    json!({
        "reloaded": true,
        "directory": dir.display().to_string(),
        "total_workflows": names.len(),
        "workflows": names,
    })
    .to_string()
}

async fn delete_workflow(args: &Value, state: &SharedState) -> String {
    let name = match args["name"].as_str() {
        Some(n) => n,
        None => return "Error: missing 'name' argument".to_string(),
    };

    // Remove from the in-memory store
    let removed = {
        let mut store = state.workflow_store.write().await;
        store.remove(name)
    };

    if !removed {
        return format!("Workflow '{name}' not found");
    }

    // Try to delete the file from disk
    let mut file_deleted = false;
    if let Some(dir) = workflows_dir() {
        for ext in &["yaml", "yml"] {
            let path = dir.join(format!("{name}.{ext}"));
            if std::fs::remove_file(&path).is_ok() {
                file_deleted = true;
                break;
            }
        }
    }

    json!({
        "deleted": true,
        "name": name,
        "file_deleted": file_deleted,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// Task tools
// ---------------------------------------------------------------------------

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
