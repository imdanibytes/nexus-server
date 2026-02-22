use std::collections::HashMap;
use std::sync::Arc;

use crate::config::{ClaudeConfig, RuleConfig};
use crate::routing::resolve_template;
use crate::sandbox::{self, Sandbox, SandboxConfig};
use crate::server::SharedState;
use cloudevents::Event;
use tracing::{error, info, warn};

use super::{Action, ActionError};

pub struct AgentAction {
    config: ClaudeConfig,
    shared: Arc<SharedState>,
}

impl AgentAction {
    pub fn new(config: ClaudeConfig, shared: Arc<SharedState>) -> Self {
        Self { config, shared }
    }
}

impl Action for AgentAction {
    fn action_type(&self) -> &str {
        "agent"
    }

    fn execute(
        &self,
        rule: &RuleConfig,
        event: &Event,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ActionError>> + Send + '_>>
    {
        let prompt_template = rule.prompt.clone();
        let rule_name = rule.name.clone();
        let sandbox_enabled = rule.sandbox;
        let event = event.clone();
        Box::pin(async move {
            let template = prompt_template.as_deref().ok_or_else(|| {
                ActionError::Config(format!("prompt missing on rule '{rule_name}'"))
            })?;
            let prompt = resolve_template(template, &event);

            let github_token = self
                .shared
                .github_token()
                .await
                .map_err(|e| ActionError::Config(format!("github token: {e}")))?;

            // Optionally create sandbox
            let sandbox_state = if sandbox_enabled {
                match setup_sandbox(&event, &github_token).await {
                    Ok(state) => {
                        info!(
                            rule = %rule_name,
                            container = %state.sandbox.container_name,
                            "sandbox attached to agent"
                        );
                        Some(state)
                    }
                    Err(e) => {
                        warn!(rule = %rule_name, error = %e, "failed to create sandbox, running without");
                        None
                    }
                }
            } else {
                None
            };

            let mut agent = crate::agent::AgentLoop::new(
                self.config.clone(),
                github_token,
                self.shared.http_client.clone(),
            );

            if let Some(ref state) = sandbox_state {
                agent = agent.with_sandbox(state.sandbox.clone());
            }

            info!(rule = %rule_name, prompt_len = prompt.len(), "starting agent loop");

            let result = agent.run(&prompt).await;

            // Cleanup sandbox regardless of agent outcome
            if let Some(state) = sandbox_state {
                if let Err(e) = state.sandbox.destroy().await {
                    error!(rule = %rule_name, error = %e, "failed to destroy sandbox");
                }
                if let Some(ref temp_dir) = state.temp_dir {
                    sandbox::cleanup_temp_dir(temp_dir);
                }
            }

            match result {
                Ok(output) => {
                    info!(rule = %rule_name, output_len = output.len(), "agent completed");
                    Ok(())
                }
                Err(e) => {
                    error!(rule = %rule_name, error = %e, "agent failed");
                    Err(ActionError::Execute(e.to_string()))
                }
            }
        })
    }
}

/// State tracking a sandbox + its temp dir for cleanup.
struct SandboxState {
    sandbox: Arc<Sandbox>,
    temp_dir: Option<String>,
}

/// Extract repo info from event data and create a sandbox.
async fn setup_sandbox(
    event: &Event,
    github_token: &str,
) -> Result<SandboxState, ActionError> {
    let event_data = match event.data() {
        Some(cloudevents::Data::Json(v)) => v.clone(),
        _ => return Err(ActionError::Config("event has no JSON data".into())),
    };

    let repo_url = event_data["repository"]["clone_url"]
        .as_str()
        .ok_or_else(|| {
            ActionError::Config("event data missing repository.clone_url".into())
        })?;

    // Try to get a meaningful branch/ref
    let git_ref = event_data["pull_request"]["head"]["ref"]
        .as_str()
        .or_else(|| event_data["ref"].as_str())
        .unwrap_or("main");

    let container_name = format!("nexus-sandbox-{}", uuid::Uuid::new_v4().simple());

    info!(repo = %repo_url, git_ref = %git_ref, container = %container_name, "creating sandbox from event");

    // Clone repo to host temp dir
    let temp_dir = sandbox::clone_repo(repo_url, git_ref, github_token)
        .await
        .map_err(|e| ActionError::Execute(format!("failed to clone repo: {e}")))?;

    // Build sandbox config from cloned repo (reads devcontainer.json if present)
    let config = SandboxConfig::from_repo(
        &container_name,
        std::path::Path::new(&temp_dir),
        HashMap::new(),
    );

    // Create the container
    let sandbox = Sandbox::create(&config)
        .await
        .map_err(|e| {
            // Clean up temp dir on failure
            sandbox::cleanup_temp_dir(&temp_dir);
            ActionError::Execute(format!("failed to create sandbox: {e}"))
        })?;

    Ok(SandboxState {
        sandbox: Arc::new(sandbox),
        temp_dir: Some(temp_dir),
    })
}
