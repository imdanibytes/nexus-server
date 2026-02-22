pub mod claude;
pub mod http_post;

use crate::agent::AgentLoop;
use crate::config::{ClaudeConfig, RuleConfig};
use crate::routing::resolve_template;
use crate::server::SharedState;
use cloudevents::binding::reqwest::RequestBuilderExt;
use cloudevents::Event;

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("claude config missing — add [claude] section to config")]
    ClaudeConfigMissing,
    #[error("github token unavailable: {0}")]
    GithubTokenMissing(String),
    #[error("prompt missing on rule '{0}'")]
    PromptMissing(String),
    #[error("url missing on http_post rule '{0}'")]
    UrlMissing(String),
    #[error("target missing on forward rule '{0}'")]
    TargetMissing(String),
    #[error("forward failed: {0}")]
    Forward(String),
    #[error("unknown action type '{0}'")]
    UnknownAction(String),
    #[error("{0}")]
    Claude(#[from] claude::ClaudeError),
    #[error("{0}")]
    HttpPost(#[from] http_post::HttpPostError),
    #[error("agent error: {0}")]
    Agent(#[from] crate::agent::AgentError),
}

pub async fn dispatch(
    rule: &RuleConfig,
    event: &Event,
    claude_config: Option<&ClaudeConfig>,
    shared: &SharedState,
    http_client: &reqwest::Client,
) -> Result<(), ActionError> {
    match rule.action.as_str() {
        "claude" => {
            let config =
                claude_config.ok_or(ActionError::ClaudeConfigMissing)?;
            let prompt_template = rule
                .prompt
                .as_deref()
                .ok_or_else(|| ActionError::PromptMissing(rule.name.clone()))?;
            let prompt = resolve_template(prompt_template, event);
            claude::call(config, &prompt, http_client).await?;
            Ok(())
        }
        "agent" => {
            let config =
                claude_config.ok_or(ActionError::ClaudeConfigMissing)?;
            let github_token = shared
                .github_token()
                .await
                .map_err(ActionError::GithubTokenMissing)?;

            let prompt_template = rule
                .prompt
                .as_deref()
                .ok_or_else(|| ActionError::PromptMissing(rule.name.clone()))?;

            let mut prompt = String::new();
            if let Some(system) = &rule.system_prompt {
                prompt.push_str(system);
                prompt.push_str("\n\n---\n\n");
            }
            prompt.push_str(&resolve_template(prompt_template, event));

            let agent = AgentLoop::new(
                config.clone(),
                github_token,
                http_client.clone(),
            );
            agent.run(&prompt).await?;
            Ok(())
        }
        "http_post" => {
            let url = rule
                .url
                .as_deref()
                .ok_or_else(|| ActionError::UrlMissing(rule.name.clone()))?;
            let body = rule
                .body_template
                .as_deref()
                .map(|t| resolve_template(t, event))
                .unwrap_or_default();
            http_post::call(url, &body, http_client).await?;
            Ok(())
        }
        "forward" => {
            let target = rule
                .target
                .as_deref()
                .ok_or_else(|| ActionError::TargetMissing(rule.name.clone()))?;
            let mut req = http_client
                .post(target)
                .event(event.clone())
                .map_err(|e| ActionError::Forward(e.to_string()))?;
            if let Some(env_name) = &rule.target_secret_env {
                let secret = std::env::var(env_name).map_err(|_| {
                    ActionError::Forward(format!("env var '{}' not set", env_name))
                })?;
                req = req.bearer_auth(&secret);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| ActionError::Forward(e.to_string()))?;
            if !resp.status().is_success() {
                return Err(ActionError::Forward(format!(
                    "{} returned {}",
                    target,
                    resp.status()
                )));
            }
            Ok(())
        }
        other => Err(ActionError::UnknownAction(other.to_string())),
    }
}
