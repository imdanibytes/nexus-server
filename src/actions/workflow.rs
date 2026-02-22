//! `workflow` action — executes a Serverless Workflow definition as a tracked task.

use std::pin::Pin;
use std::sync::Arc;

use cloudevents::Event;
use tracing::info;

use crate::actions::ActionError;
use crate::config::RuleConfig;
use crate::server::SharedState;

/// Action that looks up a workflow by name and runs it via the TaskStore.
pub struct WorkflowAction {
    shared: Arc<SharedState>,
}

impl WorkflowAction {
    pub fn new(shared: Arc<SharedState>) -> Self {
        Self { shared }
    }
}

impl super::Action for WorkflowAction {
    fn action_type(&self) -> &str {
        "workflow"
    }

    fn execute(
        &self,
        rule: &RuleConfig,
        event: &Event,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), ActionError>> + Send + '_>> {
        let shared = self.shared.clone();
        let workflow_name = rule.workflow.clone();
        let event = event.clone();

        Box::pin(async move {
            let workflow_name = workflow_name
                .as_deref()
                .ok_or_else(|| ActionError::Config("workflow action requires 'workflow' field on rule".into()))?;

            let def = shared
                .workflow_store
                .get(workflow_name)
                .ok_or_else(|| ActionError::Config(format!("workflow '{workflow_name}' not found")))?;

            let (task_id, _output) = shared
                .task_store
                .spawn(workflow_name, def, &event, &shared)
                .await
                .map_err(|e| ActionError::Execute(e.to_string()))?;

            info!(task_id = %task_id, workflow = %workflow_name, "workflow action completed");
            Ok(())
        })
    }
}
