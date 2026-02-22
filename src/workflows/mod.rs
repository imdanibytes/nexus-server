//! Serverless Workflow engine — CNCF spec-compatible with nexus extensions.
//!
//! Implements the CNCF Serverless Workflow DSL (v1.0) as the execution model.
//! Task types: `do`, `call`, `set`, `switch`, `try`/`catch`/`finally`.
//! Expression language: jq via jaq.
//! Extension: `finally` block on try/catch for guaranteed cleanup.

pub mod audit;
pub mod checkpoint;
pub mod context;
pub mod error;
pub mod executor;
pub mod expressions;
pub mod handle;
pub mod model;
pub mod task;
pub mod tasks;

use std::collections::HashMap;
use std::path::Path;

use cloudevents::event::AttributesReader;
use cloudevents::Event;
use tracing::{info, warn};

use error::WorkflowError;
use model::WorkflowDef;

/// Default directory for auto-discovered workflow definitions.
const DEFAULT_WORKFLOW_DIR: &str = ".nexus/workflows";

/// In-memory store of loaded workflow definitions.
pub struct WorkflowStore {
    workflows: HashMap<String, WorkflowDef>,
}

impl WorkflowStore {
    pub fn new() -> Self {
        Self {
            workflows: HashMap::new(),
        }
    }

    /// Load a workflow definition from a YAML file.
    pub fn load(&mut self, name: &str, path: &Path) -> Result<(), WorkflowError> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| WorkflowError::Task(format!("failed to read {}: {e}", path.display())))?;
        let def: WorkflowDef = serde_yaml::from_str(&content)
            .map_err(|e| WorkflowError::Task(format!("failed to parse {}: {e}", path.display())))?;
        self.workflows.insert(name.to_string(), def);
        Ok(())
    }

    /// Auto-discover workflow definitions from `~/.nexus/workflows/`.
    ///
    /// Scans for `.yaml` and `.yml` files, using the filename stem as the
    /// workflow name. Skips files that conflict with already-loaded workflows
    /// (config-defined workflows take precedence).
    pub fn load_from_default_dir(&mut self) {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let dir = home.join(DEFAULT_WORKFLOW_DIR);
        self.load_from_dir(&dir);
    }

    /// Auto-discover workflow definitions from a directory.
    pub fn load_from_dir(&mut self, dir: &Path) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return, // directory doesn't exist — not an error
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str());
            if !matches!(ext, Some("yaml" | "yml")) {
                continue;
            }

            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };

            if self.workflows.contains_key(name) {
                info!(name, "workflow already loaded from config, skipping auto-discovery");
                continue;
            }

            match self.load(name, &path) {
                Ok(()) => info!(name, path = %path.display(), "auto-discovered workflow"),
                Err(e) => warn!(name, path = %path.display(), error = %e, "failed to load workflow"),
            }
        }
    }

    /// Get a workflow definition by name.
    pub fn get(&self, name: &str) -> Option<&WorkflowDef> {
        self.workflows.get(name)
    }

    /// Remove a workflow definition by name. Returns true if it existed.
    pub fn remove(&mut self, name: &str) -> bool {
        self.workflows.remove(name).is_some()
    }

    /// List all loaded workflow names.
    pub fn names(&self) -> Vec<&str> {
        self.workflows.keys().map(|s| s.as_str()).collect()
    }

    /// Clear all loaded workflows.
    pub fn clear(&mut self) {
        self.workflows.clear();
    }

    /// Return all workflows whose `schedule.on.events` triggers match the given event.
    ///
    /// Type matching is prefix-based (same as old rules — `com.github.issues` matches
    /// `com.github.issues.opened`). Source matching is exact if specified.
    /// Workflows without a `schedule` block are never auto-triggered.
    pub fn match_triggers(&self, event: &Event) -> Vec<(String, WorkflowDef)> {
        let event_type = event.ty();
        let event_source = event.source();

        self.workflows
            .iter()
            .filter(|(_, def)| {
                let Some(schedule) = &def.schedule else { return false };
                let Some(triggers) = &schedule.on else { return false };

                triggers.events.iter().any(|filter| {
                    let type_matches = event_type.starts_with(&filter.with.type_);
                    let source_matches = filter
                        .with
                        .source
                        .as_ref()
                        .map_or(true, |s| event_source == s.as_str());
                    type_matches && source_matches
                })
            })
            .map(|(name, def)| (name.clone(), def.clone()))
            .collect()
    }

    /// Iterate workflows with their trigger event types (for MCP listing).
    pub fn list_with_triggers(&self) -> Vec<(&str, Vec<&str>)> {
        self.workflows
            .iter()
            .map(|(name, def)| {
                let triggers: Vec<&str> = def
                    .schedule
                    .as_ref()
                    .and_then(|s| s.on.as_ref())
                    .map(|t| t.events.iter().map(|e| e.with.type_.as_str()).collect())
                    .unwrap_or_default();
                (name.as_str(), triggers)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudevents::{EventBuilder, EventBuilderV10};
    use serde_json::json;

    fn make_event(type_: &str, source: &str) -> Event {
        EventBuilderV10::new()
            .id("test-1")
            .ty(type_)
            .source(source)
            .data("application/json", json!({}))
            .build()
            .unwrap()
    }

    fn store_with_triggered_workflow(name: &str, type_prefix: &str, source: Option<&str>) -> WorkflowStore {
        let source_yaml = if let Some(s) = source {
            format!(
                r#"
schedule:
  on:
    events:
      - with:
          type: {type_prefix}
          source: {s}
do:
  - step:
      set:
        x: 1
"#
            )
        } else {
            format!(
                r#"
schedule:
  on:
    events:
      - with:
          type: {type_prefix}
do:
  - step:
      set:
        x: 1
"#
            )
        };

        let def: WorkflowDef = serde_yaml::from_str(&source_yaml).unwrap();
        let mut store = WorkflowStore::new();
        store.workflows.insert(name.to_string(), def);
        store
    }

    #[test]
    fn match_triggers_prefix() {
        let store = store_with_triggered_workflow("issues", "com.github.issues", None);
        let event = make_event("com.github.issues.opened", "test");
        let matched = store.match_triggers(&event);
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].0, "issues");
    }

    #[test]
    fn match_triggers_no_match() {
        let store = store_with_triggered_workflow("issues", "com.github.issues", None);
        let event = make_event("com.gitlab.merge_request", "test");
        let matched = store.match_triggers(&event);
        assert!(matched.is_empty());
    }

    #[test]
    fn match_triggers_source_filter() {
        let store = store_with_triggered_workflow("issues", "com.github.issues", Some("my-org/my-repo"));
        // Matching source
        let event = make_event("com.github.issues.opened", "my-org/my-repo");
        assert_eq!(store.match_triggers(&event).len(), 1);
        // Non-matching source
        let event = make_event("com.github.issues.opened", "other-org/other-repo");
        assert!(store.match_triggers(&event).is_empty());
    }

    #[test]
    fn match_triggers_no_schedule() {
        let yaml = r#"
do:
  - step:
      set:
        x: 1
"#;
        let def: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let mut store = WorkflowStore::new();
        store.workflows.insert("no-trigger".to_string(), def);

        let event = make_event("com.anything", "test");
        assert!(store.match_triggers(&event).is_empty());
    }

    #[test]
    fn match_triggers_multiple() {
        let mut store = store_with_triggered_workflow("wf1", "com.github", None);
        let def2: WorkflowDef = serde_yaml::from_str(r#"
schedule:
  on:
    events:
      - with:
          type: com.github.issues
do:
  - step:
      set:
        x: 2
"#).unwrap();
        store.workflows.insert("wf2".to_string(), def2);

        let event = make_event("com.github.issues.opened", "test");
        let matched = store.match_triggers(&event);
        assert_eq!(matched.len(), 2);
    }
}
