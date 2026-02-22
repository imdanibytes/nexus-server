//! Parse `.devcontainer/devcontainer.json` — subset of the Dev Container spec.
//!
//! We only parse the fields relevant to creating a coding sandbox.
//! Everything IDE-specific (features, forwardPorts, extensions) is ignored.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

/// Parsed devcontainer.json — subset of the full spec.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DevContainerConfig {
    /// Base Docker image (mutually exclusive with `build`).
    #[serde(default)]
    pub image: Option<String>,
    /// Build from Dockerfile (mutually exclusive with `image`).
    #[serde(default)]
    pub build: Option<BuildConfig>,
    /// Environment variables set in the container.
    #[serde(default)]
    pub container_env: Option<HashMap<String, String>>,
    /// Commands to run after container creation.
    #[serde(default)]
    pub post_create_command: Option<LifecycleCommand>,
    /// Working directory in the container.
    #[serde(default)]
    pub workspace_folder: Option<String>,
    /// User to run commands as inside the container.
    #[serde(default)]
    pub remote_user: Option<String>,
}

/// Dockerfile build configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct BuildConfig {
    /// Path to Dockerfile relative to devcontainer.json.
    pub dockerfile: String,
    /// Build context directory (defaults to devcontainer.json's parent).
    #[serde(default)]
    pub context: Option<String>,
    /// Docker build arguments.
    #[serde(default)]
    pub args: Option<HashMap<String, String>>,
}

/// Lifecycle command — the spec allows three forms.
#[derive(Debug, Clone)]
pub enum LifecycleCommand {
    /// Single string: `"npm install"`
    String(String),
    /// Array of strings: `["npm", "install"]`
    Array(Vec<String>),
    /// Named commands: `{"install": "npm install", "build": "npm run build"}`
    Object(HashMap<String, String>),
}

impl<'de> Deserialize<'de> for LifecycleCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => Ok(LifecycleCommand::String(s)),
            serde_json::Value::Array(arr) => {
                let strings: Vec<String> = arr
                    .into_iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                Ok(LifecycleCommand::Array(strings))
            }
            serde_json::Value::Object(map) => {
                let commands: HashMap<String, String> = map
                    .into_iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                    .collect();
                Ok(LifecycleCommand::Object(commands))
            }
            _ => Err(serde::de::Error::custom(
                "postCreateCommand must be a string, array, or object",
            )),
        }
    }
}

impl LifecycleCommand {
    /// Flatten into shell commands to execute sequentially.
    pub fn to_shell_commands(&self) -> Vec<String> {
        match self {
            LifecycleCommand::String(s) => vec![s.clone()],
            LifecycleCommand::Array(arr) => {
                // Array form is a single command with args — join with spaces
                vec![arr.join(" ")]
            }
            LifecycleCommand::Object(map) => {
                // Named commands — execute in sorted order for determinism
                let mut commands: Vec<_> = map.iter().collect();
                commands.sort_by_key(|(k, _)| (*k).clone());
                commands.into_iter().map(|(_, v)| v.clone()).collect()
            }
        }
    }
}

/// Default base image when no devcontainer.json is found.
pub const DEFAULT_IMAGE: &str = "mcr.microsoft.com/devcontainers/base:debian";

/// Default workspace folder inside the container.
pub const DEFAULT_WORKSPACE_FOLDER: &str = "/workspace";

/// Default remote user.
pub const DEFAULT_REMOTE_USER: &str = "root";

impl DevContainerConfig {
    /// Try to load from a repo directory. Checks standard locations:
    /// 1. `.devcontainer/devcontainer.json`
    /// 2. `.devcontainer.json`
    pub fn from_repo(repo_path: &Path) -> Option<Self> {
        let candidates = [
            repo_path.join(".devcontainer/devcontainer.json"),
            repo_path.join(".devcontainer.json"),
        ];

        for path in &candidates {
            if path.exists() {
                if let Ok(content) = std::fs::read_to_string(path) {
                    // devcontainer.json allows JSON with comments (JSONC).
                    // Strip single-line comments before parsing.
                    let stripped = strip_jsonc_comments(&content);
                    match serde_json::from_str::<DevContainerConfig>(&stripped) {
                        Ok(config) => return Some(config),
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "failed to parse devcontainer.json, using defaults"
                            );
                            return None;
                        }
                    }
                }
            }
        }

        None
    }

    /// Resolve the Docker image to use.
    pub fn image_or_default(&self) -> &str {
        self.image.as_deref().unwrap_or(DEFAULT_IMAGE)
    }

    /// Resolve the workspace folder.
    pub fn workspace_folder_or_default(&self) -> &str {
        self.workspace_folder
            .as_deref()
            .unwrap_or(DEFAULT_WORKSPACE_FOLDER)
    }

    /// Resolve the remote user.
    pub fn remote_user_or_default(&self) -> &str {
        self.remote_user.as_deref().unwrap_or(DEFAULT_REMOTE_USER)
    }
}

/// Strip single-line comments (`//`) from JSONC content.
/// Does not handle block comments — good enough for devcontainer.json.
fn strip_jsonc_comments(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escape = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if escape {
            result.push(c);
            escape = false;
            continue;
        }

        if c == '\\' && in_string {
            result.push(c);
            escape = true;
            continue;
        }

        if c == '"' {
            in_string = !in_string;
            result.push(c);
            continue;
        }

        if !in_string && c == '/' {
            if chars.peek() == Some(&'/') {
                // Skip to end of line
                for remaining in chars.by_ref() {
                    if remaining == '\n' {
                        result.push('\n');
                        break;
                    }
                }
                continue;
            }
        }

        result.push(c);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_image_based() {
        let json = r#"{
            "image": "node:20",
            "containerEnv": { "NODE_ENV": "development" },
            "postCreateCommand": "npm install",
            "workspaceFolder": "/app",
            "remoteUser": "node"
        }"#;

        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.image.as_deref(), Some("node:20"));
        assert!(config.build.is_none());
        assert_eq!(
            config.container_env.as_ref().unwrap()["NODE_ENV"],
            "development"
        );
        assert_eq!(config.workspace_folder.as_deref(), Some("/app"));
        assert_eq!(config.remote_user.as_deref(), Some("node"));

        match config.post_create_command.unwrap() {
            LifecycleCommand::String(s) => assert_eq!(s, "npm install"),
            _ => panic!("expected string command"),
        }
    }

    #[test]
    fn parse_dockerfile_based() {
        let json = r#"{
            "build": {
                "dockerfile": "Dockerfile",
                "context": "..",
                "args": { "VARIANT": "bookworm" }
            }
        }"#;

        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert!(config.image.is_none());
        let build = config.build.unwrap();
        assert_eq!(build.dockerfile, "Dockerfile");
        assert_eq!(build.context.as_deref(), Some(".."));
        assert_eq!(build.args.as_ref().unwrap()["VARIANT"], "bookworm");
    }

    #[test]
    fn parse_array_command() {
        let json = r#"{
            "image": "python:3.12",
            "postCreateCommand": ["pip", "install", "-r", "requirements.txt"]
        }"#;

        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let cmds = config.post_create_command.unwrap().to_shell_commands();
        assert_eq!(cmds, vec!["pip install -r requirements.txt"]);
    }

    #[test]
    fn parse_object_command() {
        let json = r#"{
            "image": "node:20",
            "postCreateCommand": {
                "install": "npm install",
                "build": "npm run build"
            }
        }"#;

        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let cmds = config.post_create_command.unwrap().to_shell_commands();
        // Object commands sorted by key for determinism
        assert_eq!(cmds, vec!["npm run build", "npm install"]);
    }

    #[test]
    fn defaults() {
        let json = r#"{ "image": "ubuntu:22.04" }"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.workspace_folder_or_default(), "/workspace");
        assert_eq!(config.remote_user_or_default(), "root");
    }

    #[test]
    fn minimal_config() {
        let json = r#"{}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.image_or_default(), DEFAULT_IMAGE);
    }

    #[test]
    fn strip_comments() {
        let jsonc = r#"{
            // This is a comment
            "image": "node:20", // inline comment
            "workspaceFolder": "/app"
        }"#;

        let stripped = strip_jsonc_comments(jsonc);
        let config: DevContainerConfig = serde_json::from_str(&stripped).unwrap();
        assert_eq!(config.image.as_deref(), Some("node:20"));
        assert_eq!(config.workspace_folder.as_deref(), Some("/app"));
    }

    #[test]
    fn comment_inside_string_preserved() {
        let jsonc = r#"{
            "image": "node:20",
            "postCreateCommand": "echo // this is not a comment"
        }"#;

        let stripped = strip_jsonc_comments(jsonc);
        let config: DevContainerConfig = serde_json::from_str(&stripped).unwrap();
        match config.post_create_command.unwrap() {
            LifecycleCommand::String(s) => {
                assert_eq!(s, "echo // this is not a comment")
            }
            _ => panic!("expected string"),
        }
    }
}
