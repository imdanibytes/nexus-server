pub mod agent;
pub mod claude;
pub mod forward;
pub mod http_post;
pub mod workflow;

use std::collections::HashMap;
use std::sync::Arc;

use cloudevents::Event;

use crate::config::RuleConfig;
use crate::server::SharedState;

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("execution failed: {0}")]
    Execute(String),
    #[error("unknown action type '{0}'")]
    UnknownAction(String),
}

/// An action consumes a CloudEvent and performs a side effect.
pub trait Action: Send + Sync {
    /// The action type identifier (e.g., "claude", "http_post").
    fn action_type(&self) -> &str;

    /// Execute the action for the given rule and event.
    fn execute(
        &self,
        rule: &RuleConfig,
        event: &Event,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ActionError>> + Send + '_>>;
}

/// Registry of available actions, keyed by action type.
pub struct ActionRegistry {
    actions: HashMap<String, Arc<dyn Action>>,
}

impl ActionRegistry {
    pub fn new(shared: &Arc<SharedState>) -> Self {
        let mut actions: HashMap<String, Arc<dyn Action>> = HashMap::new();

        // Register built-in actions
        if let Some(ref claude_config) = shared.claude {
            actions.insert(
                "claude".into(),
                Arc::new(claude::ClaudeAction::new(
                    claude_config.clone(),
                    shared.http_client.clone(),
                )),
            );
            actions.insert(
                "agent".into(),
                Arc::new(agent::AgentAction::new(
                    claude_config.clone(),
                    shared.clone(),
                )),
            );
        }

        actions.insert(
            "http_post".into(),
            Arc::new(http_post::HttpPostAction::new(shared.http_client.clone())),
        );
        actions.insert(
            "forward".into(),
            Arc::new(forward::ForwardAction::new(shared.http_client.clone())),
        );
        actions.insert(
            "workflow".into(),
            Arc::new(workflow::WorkflowAction::new(shared.clone())),
        );

        Self { actions }
    }

    pub fn get(&self, action_type: &str) -> Option<Arc<dyn Action>> {
        self.actions.get(action_type).cloned()
    }
}

/// Dispatch an event to the action specified by the rule.
pub async fn dispatch(
    rule: &RuleConfig,
    event: &Event,
    registry: &ActionRegistry,
) -> Result<(), ActionError> {
    let action = registry
        .get(&rule.action)
        .ok_or_else(|| ActionError::UnknownAction(rule.action.clone()))?;

    action.execute(rule, event).await
}
