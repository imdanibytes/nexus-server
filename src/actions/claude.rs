use crate::config::{ClaudeConfig, RuleConfig};
use crate::routing::resolve_template;
use cloudevents::Event;
use serde_json::json;
use tracing::{error, info};

use super::{Action, ActionError};

pub struct ClaudeAction {
    config: ClaudeConfig,
    client: reqwest::Client,
}

impl ClaudeAction {
    pub fn new(config: ClaudeConfig, client: reqwest::Client) -> Self {
        Self { config, client }
    }
}

impl Action for ClaudeAction {
    fn action_type(&self) -> &str {
        "claude"
    }

    fn execute(
        &self,
        rule: &RuleConfig,
        event: &Event,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ActionError>> + Send + '_>>
    {
        let prompt_template = rule.prompt.clone();
        let rule_name = rule.name.clone();
        let event = event.clone();
        Box::pin(async move {
            let template = prompt_template
                .as_deref()
                .ok_or_else(|| ActionError::Config(format!("prompt missing on rule '{rule_name}'")))?;
            let prompt = resolve_template(template, &event);
            call(&self.config, &prompt, &self.client).await?;
            Ok(())
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ClaudeError {
    #[error("ANTHROPIC_API_KEY env var '{0}' not set")]
    ApiKeyMissing(String),
    #[error("API request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("API returned {status}: {body}")]
    ApiError { status: u16, body: String },
}

impl From<ClaudeError> for ActionError {
    fn from(e: ClaudeError) -> Self {
        ActionError::Execute(e.to_string())
    }
}

pub async fn call(
    config: &ClaudeConfig,
    prompt: &str,
    client: &reqwest::Client,
) -> Result<String, ClaudeError> {
    let api_key = std::env::var(&config.api_key_env)
        .map_err(|_| ClaudeError::ApiKeyMissing(config.api_key_env.clone()))?;

    info!(model = %config.model, prompt_len = prompt.len(), "calling Claude API");

    let body = json!({
        "model": config.model,
        "max_tokens": config.max_tokens,
        "messages": [{"role": "user", "content": prompt}]
    });

    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    let status = resp.status().as_u16();
    let resp_body = resp.text().await?;

    if status != 200 {
        error!(status, body = %resp_body, "Claude API error");
        return Err(ClaudeError::ApiError {
            status,
            body: resp_body,
        });
    }

    let parsed: serde_json::Value =
        serde_json::from_str(&resp_body).unwrap_or(json!({"raw": resp_body}));

    let text = parsed["content"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|block| block["text"].as_str())
        .unwrap_or("")
        .to_string();

    info!(response_len = text.len(), "Claude response received");
    tracing::debug!(response = %text, "Claude response body");

    Ok(text)
}
