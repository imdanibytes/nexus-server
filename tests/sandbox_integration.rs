//! Integration test for the sandbox — requires Docker.
//!
//! Run with: cargo test --test sandbox_integration

use std::collections::HashMap;

// Import the sandbox module from the crate
use nexus_server::sandbox::{Sandbox, SandboxConfig};

#[tokio::test]
async fn sandbox_lifecycle() {
    // Skip if Docker isn't available
    let docker_check = tokio::process::Command::new("docker")
        .args(["info"])
        .output()
        .await;
    if docker_check.is_err() || !docker_check.unwrap().status.success() {
        eprintln!("skipping: Docker not available");
        return;
    }

    let config = SandboxConfig {
        container_name: "nexus-sandbox-test-integration".to_string(),
        image: "mcr.microsoft.com/devcontainers/base:debian".to_string(),
        container_env: HashMap::from([("TEST_VAR".to_string(), "hello".to_string())]),
        post_create_commands: None,
        workspace_folder: "/workspace".to_string(),
        remote_user: "root".to_string(),
        repo_host_path: None,
    };

    // Create
    let sandbox = Sandbox::create(&config).await.expect("create failed");

    // Exec — check env var
    let result = sandbox.exec_shell("echo $TEST_VAR").await.expect("exec failed");
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout.trim(), "hello");

    // Write file
    sandbox
        .write_file("test.txt", "file content here")
        .await
        .expect("write failed");

    // Read file
    let content = sandbox.read_file("test.txt").await.expect("read failed");
    assert_eq!(content, "file content here");

    // Write to nested path
    sandbox
        .write_file("src/main.rs", "fn main() {}")
        .await
        .expect("nested write failed");

    let nested = sandbox.read_file("src/main.rs").await.expect("nested read failed");
    assert_eq!(nested, "fn main() {}");

    // List directory
    let listing = sandbox.list_directory(".").await.expect("list failed");
    assert!(listing.contains("test.txt"));
    assert!(listing.contains("src"));

    // Exec with working directory
    let result = sandbox.exec_in("pwd", "/workspace/src").await.expect("exec_in failed");
    assert_eq!(result.stdout.trim(), "/workspace/src");

    // Destroy
    sandbox.destroy().await.expect("destroy failed");

    // Verify container is gone
    let check = tokio::process::Command::new("docker")
        .args(["inspect", "nexus-sandbox-test-integration"])
        .output()
        .await
        .unwrap();
    assert!(!check.status.success(), "container should be removed");
}
