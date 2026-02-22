//! Server-specific agent tooling.
//!
//! The generic agent loop lives in the `nexus-agent` crate.
//! This module provides ToolHandler implementations that wire
//! the crate's tool system to the server's GitHub and sandbox tools.

pub mod sandbox_tools;
pub mod tools;

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::sandbox::Sandbox;

/// GitHub API tool handler. Each instance handles one specific tool.
pub struct GitHubToolHandler {
    name: String,
    client: reqwest::Client,
    token: String,
}

#[async_trait]
impl nexus_agent::ToolHandler for GitHubToolHandler {
    async fn call(&self, input: &Value) -> Result<String, String> {
        tools::dispatch(&self.name, input, &self.client, &self.token).await
    }
}

/// Sandbox tool handler. Each instance handles one specific tool.
pub struct SandboxToolHandler {
    name: String,
    sandbox: Arc<Sandbox>,
}

#[async_trait]
impl nexus_agent::ToolHandler for SandboxToolHandler {
    async fn call(&self, input: &Value) -> Result<String, String> {
        sandbox_tools::dispatch(&self.name, input, &self.sandbox).await
    }
}

/// Build a ToolRegistry with all GitHub tools.
pub fn github_toolset(client: reqwest::Client, token: String) -> nexus_agent::ToolRegistry {
    let defs = tools::github_tool_definitions();
    let mut ts = nexus_agent::ToolRegistry::new();
    for def in defs {
        let name = def["name"].as_str().unwrap_or("unknown").to_string();
        ts = ts.add(
            &name,
            def,
            GitHubToolHandler {
                name: name.clone(),
                client: client.clone(),
                token: token.clone(),
            },
        );
    }
    ts
}

/// Extend a ToolRegistry with sandbox tools.
pub fn add_sandbox_tools(
    mut ts: nexus_agent::ToolRegistry,
    sandbox: Arc<Sandbox>,
) -> nexus_agent::ToolRegistry {
    let defs = sandbox_tools::sandbox_tool_definitions();
    for def in defs {
        let name = def["name"].as_str().unwrap_or("unknown").to_string();
        ts = ts.add(
            &name,
            def,
            SandboxToolHandler {
                name: name.clone(),
                sandbox: sandbox.clone(),
            },
        );
    }
    ts
}
