//! Workflow execution context — tracks data flow between steps.

use cloudevents::event::AttributesReader;
use cloudevents::Event;
use serde_json::Value;

use super::expressions;

/// Mutable state carried through workflow execution.
pub struct WorkflowContext {
    /// The triggering CloudEvent's data (immutable).
    pub event: Value,
    /// Mutable workflow-level state, modified by `set` and `export.as`.
    pub context: Value,
    /// Current task input (set before each task).
    pub input: Value,
    /// Last task output (set after each task).
    pub output: Value,
}

impl WorkflowContext {
    /// Create a new context from the triggering CloudEvent.
    /// The event's data becomes the initial input and context.
    pub fn from_event(event: &Event) -> Self {
        let event_data = match event.data() {
            Some(cloudevents::Data::Json(v)) => v.clone(),
            _ => Value::Null,
        };

        // Build a richer event value including attributes
        let event_value = serde_json::json!({
            "type": event.ty(),
            "source": event.source(),
            "id": event.id(),
            "data": &event_data,
        });

        Self {
            context: event_data.clone(),
            input: event_data,
            output: Value::Null,
            event: event_value,
        }
    }

    /// Build named variable bindings for jq expression evaluation.
    pub fn vars(&self) -> Vec<(&str, Value)> {
        vec![
            ("context", self.context.clone()),
            ("event", self.event.clone()),
            ("input", self.input.clone()),
            ("output", self.output.clone()),
        ]
    }

    /// Apply `input.from` transformation if present.
    pub fn apply_input_transform(&mut self, from_expr: &str) -> Result<(), expressions::ExprError> {
        self.input = expressions::evaluate(from_expr, &self.context, &self.vars())?;
        Ok(())
    }

    /// Apply `output.as` transformation if present.
    pub fn apply_output_transform(
        &mut self,
        as_expr: &str,
    ) -> Result<(), expressions::ExprError> {
        self.output = expressions::evaluate(as_expr, &self.output, &self.vars())?;
        Ok(())
    }

    /// Apply `export.as` transformation — merges into context.
    pub fn apply_export(&mut self, as_expr: &str) -> Result<(), expressions::ExprError> {
        self.context = expressions::evaluate(as_expr, &self.output, &self.vars())?;
        Ok(())
    }

    /// Advance: set next task's input to current output.
    pub fn advance(&mut self) {
        self.input = self.output.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudevents::{EventBuilder, EventBuilderV10};
    use serde_json::json;

    fn test_event() -> Event {
        EventBuilderV10::new()
            .id("test-1")
            .ty("com.test.event")
            .source("test")
            .data("application/json", json!({"repo": "nexus", "action": "opened"}))
            .build()
            .unwrap()
    }

    #[test]
    fn from_event_sets_initial_state() {
        let event = test_event();
        let ctx = WorkflowContext::from_event(&event);

        assert_eq!(ctx.input, json!({"repo": "nexus", "action": "opened"}));
        assert_eq!(ctx.context, json!({"repo": "nexus", "action": "opened"}));
        assert_eq!(ctx.output, Value::Null);
        assert_eq!(ctx.event["type"], json!("com.test.event"));
        assert_eq!(ctx.event["data"]["repo"], json!("nexus"));
    }

    #[test]
    fn vars_includes_all_bindings() {
        let event = test_event();
        let ctx = WorkflowContext::from_event(&event);
        let vars = ctx.vars();
        let names: Vec<&str> = vars.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, vec!["context", "event", "input", "output"]);
    }

    #[test]
    fn advance_moves_output_to_input() {
        let event = test_event();
        let mut ctx = WorkflowContext::from_event(&event);
        ctx.output = json!({"result": "done"});
        ctx.advance();
        assert_eq!(ctx.input, json!({"result": "done"}));
    }

    #[test]
    fn input_transform() {
        let event = test_event();
        let mut ctx = WorkflowContext::from_event(&event);
        ctx.apply_input_transform(".repo").unwrap();
        assert_eq!(ctx.input, json!("nexus"));
    }

    #[test]
    fn output_transform() {
        let event = test_event();
        let mut ctx = WorkflowContext::from_event(&event);
        ctx.output = json!({"status": 200, "body": "ok"});
        ctx.apply_output_transform(".body").unwrap();
        assert_eq!(ctx.output, json!("ok"));
    }

    #[test]
    fn export_modifies_context() {
        let event = test_event();
        let mut ctx = WorkflowContext::from_event(&event);
        ctx.output = json!({"new_state": true});
        ctx.apply_export(".").unwrap();
        assert_eq!(ctx.context, json!({"new_state": true}));
    }
}
