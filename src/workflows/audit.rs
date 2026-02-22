//! Workflow audit events — structured execution telemetry.
//!
//! Events are emitted at workflow and step boundaries via a
//! `tokio::sync::broadcast` channel. Consumers subscribe to get
//! real-time events (SSE endpoint, JSONL file sink, etc.).

use serde::Serialize;
use tokio::sync::broadcast;
use tracing::warn;

/// Structured audit event emitted during workflow execution.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AuditEvent {
    WorkflowStarted {
        task_id: String,
        workflow_name: String,
    },
    StepStarted {
        task_id: String,
        step_name: String,
        step_index: usize,
    },
    StepCompleted {
        task_id: String,
        step_name: String,
        step_index: usize,
        duration_ms: u64,
    },
    StepFailed {
        task_id: String,
        step_name: String,
        step_index: usize,
        error: String,
    },
    WorkflowCompleted {
        task_id: String,
        duration_ms: u64,
        steps_executed: usize,
    },
    WorkflowFailed {
        task_id: String,
        error: String,
        steps_executed: usize,
    },
    WorkflowCancelled {
        task_id: String,
        steps_executed: usize,
    },
    CheckpointSaved {
        task_id: String,
        step_index: usize,
    },
}

/// Convenience type for the audit broadcast sender.
pub type AuditSender = broadcast::Sender<AuditEvent>;

/// Create a new audit broadcast channel.
///
/// `capacity` controls how many events can buffer before slow receivers
/// start losing events (lagged). 256 is generous for most workflows.
pub fn channel(capacity: usize) -> (AuditSender, broadcast::Receiver<AuditEvent>) {
    broadcast::channel(capacity)
}

/// Best-effort emit — logs a warning if all receivers have dropped.
pub fn emit(sender: &AuditSender, event: AuditEvent) {
    if sender.send(event).is_err() {
        // All receivers dropped — not an error, just means nobody is listening
    }
}

/// JSONL file sink — subscribes to the broadcast and appends events as
/// newline-delimited JSON to a file. Runs until the sender is dropped.
pub fn spawn_jsonl_sink(
    mut rx: broadcast::Receiver<AuditEvent>,
    path: std::path::PathBuf,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use std::io::Write;
        let file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "failed to open audit log");
                return;
            }
        };
        let mut writer = std::io::BufWriter::new(file);

        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Ok(line) = serde_json::to_string(&event) {
                        let _ = writeln!(writer, "{line}");
                        let _ = writer.flush();
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "audit JSONL sink lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broadcast_delivers_events() {
        let (tx, mut rx) = channel(16);

        emit(&tx, AuditEvent::WorkflowStarted {
            task_id: "t1".into(),
            workflow_name: "wf".into(),
        });
        emit(&tx, AuditEvent::StepStarted {
            task_id: "t1".into(),
            step_name: "step1".into(),
            step_index: 0,
        });
        emit(&tx, AuditEvent::StepCompleted {
            task_id: "t1".into(),
            step_name: "step1".into(),
            step_index: 0,
            duration_ms: 42,
        });
        emit(&tx, AuditEvent::WorkflowCompleted {
            task_id: "t1".into(),
            duration_ms: 100,
            steps_executed: 1,
        });

        // Drop sender so receiver will drain and close
        drop(tx);

        let mut events = Vec::new();
        while let Ok(ev) = rx.recv().await {
            events.push(ev);
        }

        assert_eq!(events.len(), 4);
        assert!(matches!(&events[0], AuditEvent::WorkflowStarted { .. }));
        assert!(matches!(&events[1], AuditEvent::StepStarted { .. }));
        assert!(matches!(&events[2], AuditEvent::StepCompleted { .. }));
        assert!(matches!(&events[3], AuditEvent::WorkflowCompleted { .. }));
    }

    #[tokio::test]
    async fn emit_with_no_receivers_does_not_panic() {
        let (tx, _) = channel(16);
        // All receivers dropped — emit should not panic
        emit(&tx, AuditEvent::WorkflowStarted {
            task_id: "t1".into(),
            workflow_name: "wf".into(),
        });
    }

    #[tokio::test]
    async fn jsonl_sink_writes_to_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("audit.jsonl");

        let (tx, rx) = channel(16);
        let handle = spawn_jsonl_sink(rx, path.clone());

        emit(&tx, AuditEvent::WorkflowStarted {
            task_id: "t1".into(),
            workflow_name: "test-wf".into(),
        });
        emit(&tx, AuditEvent::WorkflowCompleted {
            task_id: "t1".into(),
            duration_ms: 50,
            steps_executed: 2,
        });

        // Drop sender to close the sink
        drop(tx);
        handle.await.unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("workflow_started"));
        assert!(lines[1].contains("workflow_completed"));
    }

    #[test]
    fn audit_event_serializes_with_tag() {
        let event = AuditEvent::StepFailed {
            task_id: "t1".into(),
            step_name: "boom".into(),
            step_index: 3,
            error: "something broke".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""event":"step_failed""#));
        assert!(json.contains(r#""step_name":"boom""#));
    }
}
