//! Docker container lifecycle management for coding sandboxes.
//!
//! All operations use `tokio::process::Command` calling the Docker CLI.

use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use super::{SandboxConfig, SandboxError};

/// A running sandbox container.
pub struct Sandbox {
    pub(crate) container_name: String,
    pub(crate) workspace_folder: String,
    pub(crate) remote_user: String,
}

/// Result of executing a command inside the sandbox.
#[derive(Debug)]
pub struct ExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Sandbox {
    /// Create and start a sandbox container.
    ///
    /// 1. Pull/build the image
    /// 2. Create the container
    /// 3. Copy repo into container
    /// 4. Start the container
    /// 5. Run postCreateCommand
    pub async fn create(config: &SandboxConfig) -> Result<Self, SandboxError> {
        let name = &config.container_name;
        let image = &config.image;
        let workspace = &config.workspace_folder;
        let user = &config.remote_user;

        info!(
            container = %name,
            image = %image,
            workspace = %workspace,
            "sandbox: creating container"
        );

        // Build env args
        let mut create_args = vec![
            "create".to_string(),
            "--name".to_string(),
            name.clone(),
            "-w".to_string(),
            workspace.clone(),
        ];

        for (key, val) in &config.container_env {
            create_args.push("-e".to_string());
            create_args.push(format!("{key}={val}"));
        }

        // Keep the container alive
        create_args.push(image.clone());
        create_args.push("sleep".to_string());
        create_args.push("infinity".to_string());

        // Create container
        docker_run(&create_args).await.map_err(|e| {
            SandboxError::Docker(format!("failed to create container '{name}': {e}"))
        })?;

        // Copy repo into container
        if let Some(ref repo_path) = config.repo_host_path {
            info!(container = %name, "sandbox: copying repo into container");
            let src = format!("{}/.", repo_path);
            let dst = format!("{name}:{workspace}");
            docker_run(&["cp", &src, &dst]).await.map_err(|e| {
                SandboxError::Docker(format!("failed to copy repo into container: {e}"))
            })?;
        }

        // Start container
        docker_run(&["start", name]).await.map_err(|e| {
            SandboxError::Docker(format!("failed to start container '{name}': {e}"))
        })?;

        info!(container = %name, "sandbox: container started");

        let sandbox = Self {
            container_name: name.clone(),
            workspace_folder: workspace.clone(),
            remote_user: user.clone(),
        };

        // Run postCreateCommand
        if let Some(ref commands) = config.post_create_commands {
            for cmd in commands {
                info!(container = %name, cmd = %cmd, "sandbox: running postCreateCommand");
                let result = sandbox.exec_shell(cmd).await?;
                if result.exit_code != 0 {
                    warn!(
                        container = %name,
                        exit_code = result.exit_code,
                        stderr = %result.stderr,
                        "sandbox: postCreateCommand failed"
                    );
                }
            }
        }

        Ok(sandbox)
    }

    /// Execute a shell command inside the container.
    pub async fn exec_shell(&self, command: &str) -> Result<ExecResult, SandboxError> {
        let output = tokio::process::Command::new("docker")
            .args([
                "exec",
                "-u",
                &self.remote_user,
                "-w",
                &self.workspace_folder,
                &self.container_name,
                "bash",
                "-c",
                command,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| SandboxError::Docker(format!("docker exec failed: {e}")))?;

        Ok(ExecResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Execute a command with a specific working directory.
    pub async fn exec_in(
        &self,
        command: &str,
        workdir: &str,
    ) -> Result<ExecResult, SandboxError> {
        let output = tokio::process::Command::new("docker")
            .args([
                "exec",
                "-u",
                &self.remote_user,
                "-w",
                workdir,
                &self.container_name,
                "bash",
                "-c",
                command,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| SandboxError::Docker(format!("docker exec failed: {e}")))?;

        Ok(ExecResult {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Read a file from the container.
    pub async fn read_file(&self, path: &str) -> Result<String, SandboxError> {
        let resolved = self.resolve_path(path);
        let result = self.exec_shell(&format!("cat '{}'", shell_escape(&resolved))).await?;
        if result.exit_code != 0 {
            return Err(SandboxError::Docker(format!(
                "failed to read '{}': {}",
                resolved,
                result.stderr.trim()
            )));
        }
        Ok(result.stdout)
    }

    /// Write content to a file in the container.
    pub async fn write_file(&self, path: &str, content: &str) -> Result<(), SandboxError> {
        let resolved = self.resolve_path(path);

        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(&resolved).parent() {
            self.exec_shell(&format!("mkdir -p '{}'", shell_escape(&parent.to_string_lossy())))
                .await?;
        }

        // Pipe content via stdin to tee
        let mut child = tokio::process::Command::new("docker")
            .args([
                "exec",
                "-i",
                "-u",
                &self.remote_user,
                &self.container_name,
                "tee",
                &resolved,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| SandboxError::Docker(format!("docker exec tee failed: {e}")))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(content.as_bytes())
                .await
                .map_err(|e| SandboxError::Docker(format!("write to stdin failed: {e}")))?;
            // Close stdin so tee finishes
            drop(stdin);
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| SandboxError::Docker(format!("waiting for tee failed: {e}")))?;

        if !output.status.success() {
            return Err(SandboxError::Docker(format!(
                "failed to write '{}': {}",
                resolved,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        Ok(())
    }

    /// List a directory in the container.
    pub async fn list_directory(&self, path: &str) -> Result<String, SandboxError> {
        let resolved = self.resolve_path(path);
        let result = self
            .exec_shell(&format!("ls -la '{}'", shell_escape(&resolved)))
            .await?;
        if result.exit_code != 0 {
            return Err(SandboxError::Docker(format!(
                "failed to list '{}': {}",
                resolved,
                result.stderr.trim()
            )));
        }
        Ok(result.stdout)
    }

    /// Destroy the container (force remove).
    pub async fn destroy(&self) -> Result<(), SandboxError> {
        info!(container = %self.container_name, "sandbox: destroying container");
        docker_run(&["rm", "-f", &self.container_name])
            .await
            .map_err(|e| {
                SandboxError::Docker(format!(
                    "failed to remove container '{}': {e}",
                    self.container_name
                ))
            })?;
        Ok(())
    }

    /// Resolve a path — make relative paths relative to workspace folder.
    fn resolve_path(&self, path: &str) -> String {
        if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{}", self.workspace_folder, path)
        }
    }
}

/// Run a docker command and return stdout on success.
async fn docker_run(args: &[impl AsRef<str>]) -> Result<String, String> {
    let str_args: Vec<&str> = args.iter().map(|a| a.as_ref()).collect();

    let output = tokio::process::Command::new("docker")
        .args(&str_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("failed to run docker: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(stderr)
    }
}

/// Escape single quotes for shell.
fn shell_escape(s: &str) -> String {
    s.replace('\'', "'\\''")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_absolute_path() {
        let sandbox = Sandbox {
            container_name: "test".into(),
            workspace_folder: "/workspace".into(),
            remote_user: "root".into(),
        };
        assert_eq!(sandbox.resolve_path("/etc/hosts"), "/etc/hosts");
    }

    #[test]
    fn resolve_relative_path() {
        let sandbox = Sandbox {
            container_name: "test".into(),
            workspace_folder: "/workspace".into(),
            remote_user: "root".into(),
        };
        assert_eq!(sandbox.resolve_path("src/main.rs"), "/workspace/src/main.rs");
    }

    #[test]
    fn shell_escape_quotes() {
        assert_eq!(shell_escape("it's"), "it'\\''s");
    }
}
