//! Sandbox — temporary Docker containers for coding agents.
//!
//! Uses the Dev Container spec (devcontainer.json) to configure the
//! container image and setup commands. Repos without a devcontainer.json
//! get a sensible default.

pub mod devcontainer;
pub mod docker;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::info;

use devcontainer::DevContainerConfig;
pub use docker::Sandbox;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("docker error: {0}")]
    Docker(String),
    #[error("git error: {0}")]
    Git(String),
    #[error("config error: {0}")]
    Config(String),
}

/// Configuration for creating a sandbox.
pub struct SandboxConfig {
    /// Container name (e.g., `nexus-sandbox-{task_id}`).
    pub container_name: String,
    /// Docker image to use.
    pub image: String,
    /// Environment variables for the container.
    pub container_env: HashMap<String, String>,
    /// Commands to run after container creation.
    pub post_create_commands: Option<Vec<String>>,
    /// Working directory inside the container.
    pub workspace_folder: String,
    /// User to run as inside the container.
    pub remote_user: String,
    /// Path to the cloned repo on the host (for docker cp).
    pub repo_host_path: Option<String>,
}

impl SandboxConfig {
    /// Build a SandboxConfig from a cloned repo directory.
    ///
    /// Reads `.devcontainer/devcontainer.json` if present, otherwise uses defaults.
    pub fn from_repo(
        container_name: &str,
        repo_path: &Path,
        extra_env: HashMap<String, String>,
    ) -> Self {
        let devcontainer = DevContainerConfig::from_repo(repo_path);

        if let Some(ref dc) = devcontainer {
            info!(
                image = dc.image_or_default(),
                workspace = dc.workspace_folder_or_default(),
                "sandbox: using devcontainer.json"
            );
        } else {
            info!("sandbox: no devcontainer.json found, using defaults");
        }

        let dc = devcontainer.unwrap_or_else(|| serde_json::from_str("{}").unwrap());

        let image = dc.image_or_default().to_string();
        let workspace_folder = dc.workspace_folder_or_default().to_string();
        let remote_user = dc.remote_user_or_default().to_string();

        let mut env = dc.container_env.unwrap_or_default();
        env.extend(extra_env);

        let post_create = dc
            .post_create_command
            .map(|cmd| cmd.to_shell_commands());

        SandboxConfig {
            container_name: container_name.to_string(),
            image,
            container_env: env,
            post_create_commands: post_create,
            workspace_folder,
            remote_user,
            repo_host_path: Some(repo_path.to_string_lossy().into_owned()),
        }
    }
}

/// Clone a git repo to a temp directory on the host.
///
/// Returns the path to the temp directory. Caller is responsible for cleanup.
pub async fn clone_repo(
    repo_url: &str,
    git_ref: &str,
    github_token: &str,
) -> Result<String, SandboxError> {
    let temp_dir = std::env::temp_dir().join(format!("nexus-clone-{}", uuid::Uuid::new_v4()));
    let temp_path = temp_dir.to_string_lossy().into_owned();

    // Build authenticated URL for private repos
    let clone_url = if !github_token.is_empty() && repo_url.starts_with("https://") {
        repo_url.replacen(
            "https://",
            &format!("https://x-access-token:{github_token}@"),
            1,
        )
    } else {
        repo_url.to_string()
    };

    info!(repo = %repo_url, git_ref = %git_ref, "sandbox: cloning repo");

    let output = tokio::process::Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--branch",
            git_ref,
            &clone_url,
            &temp_path,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| SandboxError::Git(format!("failed to run git clone: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Sanitize: never log the token
        let sanitized = stderr.replace(github_token, "***");
        return Err(SandboxError::Git(format!("git clone failed: {sanitized}")));
    }

    info!(path = %temp_path, "sandbox: repo cloned");
    Ok(temp_path)
}

/// Shared registry of active sandboxes — used by workflow custom actions.
///
/// Sandboxes are keyed by container name. The registry holds `Arc<Sandbox>`
/// so multiple workflow steps can reference the same sandbox.
#[derive(Clone)]
pub struct SandboxRegistry {
    sandboxes: Arc<Mutex<HashMap<String, Arc<Sandbox>>>>,
}

impl SandboxRegistry {
    pub fn new() -> Self {
        Self {
            sandboxes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a sandbox. Returns the container name as the key.
    pub async fn insert(&self, sandbox: Arc<Sandbox>) -> String {
        let name = sandbox.container_name.clone();
        self.sandboxes.lock().await.insert(name.clone(), sandbox);
        name
    }

    /// Look up a sandbox by container name.
    pub async fn get(&self, name: &str) -> Option<Arc<Sandbox>> {
        self.sandboxes.lock().await.get(name).cloned()
    }

    /// Remove a sandbox from the registry (does NOT destroy the container).
    pub async fn remove(&self, name: &str) -> Option<Arc<Sandbox>> {
        self.sandboxes.lock().await.remove(name)
    }
}

/// Clean up a host temp directory.
pub fn cleanup_temp_dir(path: &str) {
    if let Err(e) = std::fs::remove_dir_all(path) {
        tracing::warn!(path = %path, error = %e, "sandbox: failed to clean temp dir");
    }
}
