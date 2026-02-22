use crate::config::RuleConfig;
use cloudevents::binding::reqwest::RequestBuilderExt;
use cloudevents::Event;
use tracing::info;

use super::{Action, ActionError};

pub struct ForwardAction {
    client: reqwest::Client,
}

impl ForwardAction {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Action for ForwardAction {
    fn action_type(&self) -> &str {
        "forward"
    }

    fn execute(
        &self,
        rule: &RuleConfig,
        event: &Event,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ActionError>> + Send + '_>>
    {
        let target = rule.target.clone();
        let rule_name = rule.name.clone();
        let target_secret_env = rule.target_secret_env.clone();
        let event = event.clone();
        Box::pin(async move {
            let target = target
                .as_deref()
                .ok_or_else(|| ActionError::Config(format!("target missing on rule '{rule_name}'")))?;

            info!(target, "forwarding CloudEvent");

            let mut req = self
                .client
                .post(target)
                .event(event)
                .map_err(|e| ActionError::Execute(format!("failed to build forward request: {e}")))?;

            if let Some(env_name) = &target_secret_env {
                let secret = std::env::var(env_name).map_err(|_| {
                    ActionError::Config(format!("env var '{env_name}' not set"))
                })?;
                req = req.bearer_auth(&secret);
            }

            let resp = req
                .send()
                .await
                .map_err(|e| ActionError::Execute(format!("forward request failed: {e}")))?;

            if !resp.status().is_success() {
                return Err(ActionError::Execute(format!(
                    "{} returned {}",
                    target,
                    resp.status()
                )));
            }

            info!(target, "forward succeeded");
            Ok(())
        })
    }
}
