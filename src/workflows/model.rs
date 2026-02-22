//! Workflow definition models — spec-compatible with CNCF Serverless Workflow DSL.
//!
//! We define our own types rather than depending on the alpha SDK.
//! The YAML structure matches the spec so existing workflow definitions parse correctly.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// Top-level workflow definition.
#[derive(Debug, Deserialize, Clone)]
pub struct WorkflowDef {
    /// Metadata about the workflow.
    #[serde(default)]
    pub document: Option<DocumentDef>,
    /// Event triggers — CNCF `schedule.on` block.
    /// Workflows with triggers are auto-dispatched when matching events arrive.
    /// Workflows without triggers are only callable from other workflows.
    #[serde(default)]
    pub schedule: Option<ScheduleDef>,
    /// Top-level task list — sequential by default.
    #[serde(rename = "do")]
    pub do_: Vec<BTreeMap<String, TaskDef>>,
}

/// Schedule block — declares when this workflow should be triggered.
#[derive(Debug, Deserialize, Clone)]
pub struct ScheduleDef {
    pub on: Option<EventTriggers>,
}

/// Container for event-based triggers.
#[derive(Debug, Deserialize, Clone)]
pub struct EventTriggers {
    pub events: Vec<EventFilter>,
}

/// A single event filter within a trigger declaration.
#[derive(Debug, Deserialize, Clone)]
pub struct EventFilter {
    /// Match criteria for the CloudEvent.
    #[serde(rename = "with")]
    pub with: EventMatch,
}

/// Match criteria for a CloudEvent.
#[derive(Debug, Deserialize, Clone)]
pub struct EventMatch {
    /// CloudEvent type — matched as a prefix (e.g. `com.github.issues` matches `com.github.issues.opened`).
    #[serde(rename = "type")]
    pub type_: String,
    /// CloudEvent source — exact match if specified.
    #[serde(default)]
    pub source: Option<String>,
}

/// Workflow document metadata.
#[derive(Debug, Deserialize, Clone)]
pub struct DocumentDef {
    #[serde(default)]
    pub dsl: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// A single task in the workflow.
///
/// Discriminated by which field is present (same as the spec).
/// Only one of `call`, `set`, `switch`, `do_`, `try_` should be set.
#[derive(Debug, Deserialize, Clone)]
pub struct TaskDef {
    // -- Task type discriminators --
    /// `call: http` or `call: custom:<action>`
    #[serde(default)]
    pub call: Option<String>,
    /// Arguments for `call` tasks.
    #[serde(default, rename = "with")]
    pub with: Option<Value>,
    /// `set` task: key-value pairs to merge into context.
    #[serde(default)]
    pub set: Option<BTreeMap<String, Value>>,
    /// `switch` task: list of cases with conditions.
    #[serde(default)]
    pub switch: Option<Vec<BTreeMap<String, CaseDef>>>,
    /// Nested `do` task: sequential subtask list.
    #[serde(default, rename = "do")]
    pub do_: Option<Vec<BTreeMap<String, TaskDef>>>,
    /// `try` task: tasks to attempt.
    #[serde(default, rename = "try")]
    pub try_: Option<TryBlock>,
    /// `catch` block for `try` tasks.
    #[serde(default)]
    pub catch: Option<CatchDef>,
    /// `finally` block — nexus extension. Always runs after try/catch.
    #[serde(default)]
    pub finally: Option<FinallyDef>,

    // -- Common task fields --
    /// Conditional execution: only run if expression is truthy.
    #[serde(default, rename = "if")]
    pub if_: Option<String>,
    /// Input transformation.
    #[serde(default)]
    pub input: Option<InputDef>,
    /// Output transformation.
    #[serde(default)]
    pub output: Option<OutputDef>,
    /// Export to context.
    #[serde(default)]
    pub export: Option<ExportDef>,
    /// Flow directive: "continue", "exit", or a task name to jump to.
    #[serde(default)]
    pub then: Option<String>,
    /// Per-step timeout in milliseconds. Overrides the handle's default.
    #[serde(default)]
    pub timeout: Option<u64>,
}

/// Try block — contains tasks to attempt.
#[derive(Debug, Deserialize, Clone)]
pub struct TryBlock {
    #[serde(rename = "do")]
    pub do_: Vec<BTreeMap<String, TaskDef>>,
}

/// Catch block — runs on error.
#[derive(Debug, Deserialize, Clone)]
pub struct CatchDef {
    /// Variable name for the error (default: "error").
    #[serde(default, rename = "as")]
    pub as_: Option<String>,
    /// Tasks to run on error.
    #[serde(rename = "do")]
    pub do_: Vec<BTreeMap<String, TaskDef>>,
}

/// Finally block (nexus extension) — always runs.
#[derive(Debug, Deserialize, Clone)]
pub struct FinallyDef {
    #[serde(rename = "do")]
    pub do_: Vec<BTreeMap<String, TaskDef>>,
}

/// Switch case definition.
#[derive(Debug, Deserialize, Clone)]
pub struct CaseDef {
    /// Condition expression (jq). If absent, this is the default case.
    #[serde(default)]
    pub when: Option<String>,
    /// Flow directive on match.
    #[serde(default)]
    pub then: Option<String>,
}

/// Input transformation spec.
#[derive(Debug, Deserialize, Clone)]
pub struct InputDef {
    /// jq expression to transform input.
    pub from: Option<String>,
}

/// Output transformation spec.
#[derive(Debug, Deserialize, Clone)]
pub struct OutputDef {
    /// jq expression to transform output.
    #[serde(rename = "as")]
    pub as_: Option<String>,
}

/// Export to context spec.
#[derive(Debug, Deserialize, Clone)]
pub struct ExportDef {
    /// jq expression to update context.
    #[serde(rename = "as")]
    pub as_: Option<String>,
}

/// What kind of task this is — derived from which fields are set.
pub enum TaskKind<'a> {
    Call { target: &'a str, with: Option<&'a Value> },
    Set(&'a BTreeMap<String, Value>),
    Switch(&'a Vec<BTreeMap<String, CaseDef>>),
    Do(&'a Vec<BTreeMap<String, TaskDef>>),
    Try,
    Unknown,
}

impl TaskDef {
    /// Determine the task kind from which fields are populated.
    pub fn kind(&self) -> TaskKind<'_> {
        if let Some(ref call) = self.call {
            TaskKind::Call { target: call, with: self.with.as_ref() }
        } else if let Some(ref set) = self.set {
            TaskKind::Set(set)
        } else if let Some(ref switch) = self.switch {
            TaskKind::Switch(switch)
        } else if let Some(ref do_) = self.do_ {
            TaskKind::Do(do_)
        } else if self.try_.is_some() {
            TaskKind::Try
        } else {
            TaskKind::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_schedule_triggers() {
        let yaml = r#"
schedule:
  on:
    events:
      - with:
          type: com.github.issues.opened
      - with:
          type: com.github.pull_request
          source: my-org/my-repo
do:
  - step:
      set:
        x: 1
"#;
        let wf: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let schedule = wf.schedule.as_ref().unwrap();
        let triggers = schedule.on.as_ref().unwrap();
        assert_eq!(triggers.events.len(), 2);
        assert_eq!(triggers.events[0].with.type_, "com.github.issues.opened");
        assert!(triggers.events[0].with.source.is_none());
        assert_eq!(triggers.events[1].with.type_, "com.github.pull_request");
        assert_eq!(triggers.events[1].with.source.as_deref(), Some("my-org/my-repo"));
    }

    #[test]
    fn parse_no_schedule() {
        let yaml = r#"
do:
  - step:
      set:
        x: 1
"#;
        let wf: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        assert!(wf.schedule.is_none());
    }

    #[test]
    fn parse_sequential_workflow() {
        let yaml = r#"
document:
  dsl: "1.0.0"
  namespace: test
  name: example
  version: "0.1.0"
do:
  - step1:
      call: http
      with:
        method: get
        endpoint: https://api.example.com/data
  - step2:
      set:
        result: "${ .body }"
  - step3:
      call: "custom:claude"
      with:
        prompt: "Analyze: ${ .result }"
"#;
        let wf: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(wf.do_.len(), 3);

        let step1_map = &wf.do_[0];
        let step1 = &step1_map["step1"];
        assert_eq!(step1.call.as_deref(), Some("http"));

        let step2 = &wf.do_[1]["step2"];
        assert!(step2.set.is_some());

        let step3 = &wf.do_[2]["step3"];
        assert_eq!(step3.call.as_deref(), Some("custom:claude"));
    }

    #[test]
    fn parse_switch() {
        let yaml = r#"
do:
  - route:
      switch:
        - large:
            when: ".size > 1000"
            then: handleLarge
        - default:
            then: handleNormal
"#;
        let wf: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let task = &wf.do_[0]["route"];
        let cases = task.switch.as_ref().unwrap();
        assert_eq!(cases.len(), 2);
        assert!(cases[0]["large"].when.is_some());
        assert!(cases[1]["default"].when.is_none());
    }

    #[test]
    fn parse_try_catch_finally() {
        let yaml = r#"
do:
  - protected:
      try:
        do:
          - riskyStep:
              call: http
              with:
                method: post
                endpoint: https://api.example.com/risky
      catch:
        as: error
        do:
          - logError:
              call: http
              with:
                method: post
                endpoint: https://api.example.com/log
      finally:
        do:
          - cleanup:
              call: http
              with:
                method: delete
                endpoint: https://api.example.com/resource
"#;
        let wf: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let task = &wf.do_[0]["protected"];
        assert!(task.try_.is_some());
        assert!(task.catch.is_some());
        assert!(task.finally.is_some());

        let catch = task.catch.as_ref().unwrap();
        assert_eq!(catch.as_.as_deref(), Some("error"));
        assert_eq!(catch.do_.len(), 1);

        let finally = task.finally.as_ref().unwrap();
        assert_eq!(finally.do_.len(), 1);
    }

    #[test]
    fn parse_nested_do() {
        let yaml = r#"
do:
  - outer:
      do:
        - inner1:
            set:
              x: 1
        - inner2:
            set:
              y: 2
"#;
        let wf: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let task = &wf.do_[0]["outer"];
        let inner = task.do_.as_ref().unwrap();
        assert_eq!(inner.len(), 2);
    }

    #[test]
    fn parse_with_transforms() {
        let yaml = r#"
do:
  - step:
      call: http
      with:
        method: get
        endpoint: https://api.example.com
      input:
        from: ".query"
      output:
        as: ".body"
      export:
        as: "$context + {result: .}"
      then: exit
"#;
        let wf: WorkflowDef = serde_yaml::from_str(yaml).unwrap();
        let task = &wf.do_[0]["step"];
        assert_eq!(task.input.as_ref().unwrap().from.as_deref(), Some(".query"));
        assert_eq!(task.output.as_ref().unwrap().as_.as_deref(), Some(".body"));
        assert_eq!(task.then.as_deref(), Some("exit"));
    }
}
