pub mod claude;
pub mod http_post;

use crate::cloud_event::CloudEvent;
use crate::config::{ClaudeConfig, RuleConfig};
use crate::routing::resolve_template;

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("claude config missing — add [claude] section to config")]
    ClaudeConfigMissing,
    #[error("prompt missing on claude rule '{0}'")]
    PromptMissing(String),
    #[error("url missing on http_post rule '{0}'")]
    UrlMissing(String),
    #[error("unknown action type '{0}'")]
    UnknownAction(String),
    #[error("{0}")]
    Claude(#[from] claude::ClaudeError),
    #[error("{0}")]
    HttpPost(#[from] http_post::HttpPostError),
}

pub async fn dispatch(
    rule: &RuleConfig,
    event: &CloudEvent,
    claude_config: Option<&ClaudeConfig>,
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
        other => Err(ActionError::UnknownAction(other.to_string())),
    }
}
