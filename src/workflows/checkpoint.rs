//! Checkpoint persistence — save/restore workflow state for crash recovery.
//!
//! After each top-level step, the executor writes a checkpoint to disk.
//! On server restart, incomplete checkpoints are scanned and workflows
//! resume from their last saved position.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

/// Snapshot of a workflow execution at a top-level step boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub task_id: String,
    pub workflow_name: String,
    /// Number of top-level steps completed (resume starts at this index).
    pub step_index: usize,
    /// Total step counter across all nested task lists.
    pub global_steps: usize,
    /// Serialized WorkflowContext fields.
    pub context: CheckpointContext,
    /// When execution started (epoch ms).
    pub started_at_epoch_ms: u64,
    /// Name of the last completed step (for debugging).
    pub last_step_name: Option<String>,
}

/// Serializable snapshot of WorkflowContext state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointContext {
    pub event: Value,
    pub context: Value,
    pub input: Value,
    pub output: Value,
}

/// File-based checkpoint writer. One JSON file per task under a directory.
#[derive(Debug, Clone)]
pub struct FileCheckpointWriter {
    dir: PathBuf,
}

impl FileCheckpointWriter {
    /// Create a new writer, creating the directory if needed.
    pub fn new(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn checkpoint_path(&self, task_id: &str) -> PathBuf {
        self.dir.join(format!("{task_id}.json"))
    }

    /// Save a checkpoint to disk (atomic write via temp + rename).
    pub async fn save(&self, checkpoint: &Checkpoint) -> Result<(), CheckpointError> {
        let path = self.checkpoint_path(&checkpoint.task_id);
        let tmp_path = path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(checkpoint)
            .map_err(|e| CheckpointError::Serialize(e.to_string()))?;

        let tmp = tmp_path.clone();
        let final_path = path.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::write(&tmp, &data)?;
            std::fs::rename(&tmp, &final_path)?;
            Ok::<(), std::io::Error>(())
        })
        .await
        .map_err(|e| CheckpointError::Io(e.to_string()))?
        .map_err(|e| CheckpointError::Io(e.to_string()))?;

        Ok(())
    }

    /// Load a checkpoint by task ID.
    pub async fn load(&self, task_id: &str) -> Result<Option<Checkpoint>, CheckpointError> {
        let path = self.checkpoint_path(task_id);
        if !path.exists() {
            return Ok(None);
        }

        let path_clone = path.clone();
        let data = tokio::task::spawn_blocking(move || std::fs::read(path_clone))
            .await
            .map_err(|e| CheckpointError::Io(e.to_string()))?
            .map_err(|e| CheckpointError::Io(e.to_string()))?;

        let checkpoint: Checkpoint = serde_json::from_slice(&data)
            .map_err(|e| CheckpointError::Serialize(e.to_string()))?;

        Ok(Some(checkpoint))
    }

    /// Remove a checkpoint (called on workflow completion).
    pub async fn remove(&self, task_id: &str) -> Result<(), CheckpointError> {
        let path = self.checkpoint_path(task_id);
        if path.exists() {
            let path_clone = path.clone();
            tokio::task::spawn_blocking(move || std::fs::remove_file(path_clone))
                .await
                .map_err(|e| CheckpointError::Io(e.to_string()))?
                .map_err(|e| CheckpointError::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// Scan directory for incomplete checkpoints (crash recovery).
    pub async fn list_incomplete(&self) -> Result<Vec<Checkpoint>, CheckpointError> {
        let dir = self.dir.clone();
        let entries = tokio::task::spawn_blocking(move || -> Result<Vec<Checkpoint>, CheckpointError> {
            let mut results = Vec::new();
            let read_dir = std::fs::read_dir(&dir)
                .map_err(|e| CheckpointError::Io(e.to_string()))?;

            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    match std::fs::read(&path) {
                        Ok(data) => match serde_json::from_slice::<Checkpoint>(&data) {
                            Ok(cp) => results.push(cp),
                            Err(e) => {
                                warn!(path = %path.display(), error = %e, "skipping corrupt checkpoint");
                            }
                        },
                        Err(e) => {
                            warn!(path = %path.display(), error = %e, "failed to read checkpoint");
                        }
                    }
                }
            }
            Ok(results)
        })
        .await
        .map_err(|e| CheckpointError::Io(e.to_string()))??;

        Ok(entries)
    }

    /// Get the checkpoint directory path.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("checkpoint I/O error: {0}")]
    Io(String),
    #[error("checkpoint serialization error: {0}")]
    Serialize(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_checkpoint(task_id: &str) -> Checkpoint {
        Checkpoint {
            task_id: task_id.to_string(),
            workflow_name: "test-wf".to_string(),
            step_index: 2,
            global_steps: 5,
            context: CheckpointContext {
                event: serde_json::json!({"type": "test"}),
                context: serde_json::json!({"x": 1, "y": 2}),
                input: serde_json::json!({"y": 2}),
                output: serde_json::json!({"y": 2}),
            },
            started_at_epoch_ms: 1700000000000,
            last_step_name: Some("step2".to_string()),
        }
    }

    #[tokio::test]
    async fn save_load_round_trip() {
        let tmp = TempDir::new().unwrap();
        let writer = FileCheckpointWriter::new(tmp.path()).unwrap();

        let cp = sample_checkpoint("task-1");
        writer.save(&cp).await.unwrap();

        let loaded = writer.load("task-1").await.unwrap().unwrap();
        assert_eq!(loaded.task_id, "task-1");
        assert_eq!(loaded.step_index, 2);
        assert_eq!(loaded.global_steps, 5);
        assert_eq!(loaded.context.context, serde_json::json!({"x": 1, "y": 2}));
        assert_eq!(loaded.last_step_name.as_deref(), Some("step2"));
    }

    #[tokio::test]
    async fn load_missing_returns_none() {
        let tmp = TempDir::new().unwrap();
        let writer = FileCheckpointWriter::new(tmp.path()).unwrap();
        assert!(writer.load("nonexistent").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn remove_deletes_file() {
        let tmp = TempDir::new().unwrap();
        let writer = FileCheckpointWriter::new(tmp.path()).unwrap();

        let cp = sample_checkpoint("task-rm");
        writer.save(&cp).await.unwrap();
        assert!(writer.load("task-rm").await.unwrap().is_some());

        writer.remove("task-rm").await.unwrap();
        assert!(writer.load("task-rm").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn remove_nonexistent_is_ok() {
        let tmp = TempDir::new().unwrap();
        let writer = FileCheckpointWriter::new(tmp.path()).unwrap();
        writer.remove("ghost").await.unwrap(); // no error
    }

    #[tokio::test]
    async fn list_incomplete_finds_all() {
        let tmp = TempDir::new().unwrap();
        let writer = FileCheckpointWriter::new(tmp.path()).unwrap();

        writer.save(&sample_checkpoint("t1")).await.unwrap();
        writer.save(&sample_checkpoint("t2")).await.unwrap();
        writer.save(&sample_checkpoint("t3")).await.unwrap();

        let incomplete = writer.list_incomplete().await.unwrap();
        assert_eq!(incomplete.len(), 3);

        let mut ids: Vec<_> = incomplete.iter().map(|c| c.task_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["t1", "t2", "t3"]);
    }
}
