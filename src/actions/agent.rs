use std::sync::Arc;

use crate::config::{ClaudeConfig, RuleConfig};
use crate::routing::resolve_template;
use crate::server::SharedState;
use cloudevents::Event;
use tracing::{error, info};

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

            let agent = crate::agent::AgentLoop::new(
                self.config.clone(),
                github_token,
                self.shared.http_client.clone(),
            );

            info!(rule = %rule_name, prompt_len = prompt.len(), "starting agent loop");

            match agent.run(&prompt).await {
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
