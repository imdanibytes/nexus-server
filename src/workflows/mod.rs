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

use error::WorkflowError;
use model::WorkflowDef;

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

    /// Get a workflow definition by name.
    pub fn get(&self, name: &str) -> Option<&WorkflowDef> {
        self.workflows.get(name)
    }

    /// List all loaded workflow names.
    pub fn names(&self) -> Vec<&str> {
        self.workflows.keys().map(|s| s.as_str()).collect()
    }
}
