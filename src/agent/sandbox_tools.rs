//! Sandbox-scoped tools for the coding agent.
//!
//! These tools let the agent read, write, and execute commands
//! inside a Docker sandbox container.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::sandbox::Sandbox;

/// Tool definitions for sandbox operations.
pub fn sandbox_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "read_file",
            "description": "Read the contents of a file in the workspace. Returns the file content as text. Use relative paths from the workspace root.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path (relative to workspace root, or absolute)"
                    }
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "write_file",
            "description": "Create or overwrite a file in the workspace. Creates parent directories as needed.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path (relative to workspace root, or absolute)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Full content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }
        }),
        json!({
            "name": "edit_file",
            "description": "Edit a file by replacing a specific text string with new text. The old_text must appear exactly once in the file.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path (relative to workspace root, or absolute)"
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Exact text to find in the file (must appear exactly once)"
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Text to replace old_text with"
                    }
                },
                "required": ["path", "old_text", "new_text"]
            }
        }),
        json!({
            "name": "exec_command",
            "description": "Execute a shell command in the workspace. Returns stdout, stderr, and exit code. Use for running tests, builds, git operations, etc.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute (run via bash -c)"
                    },
                    "workdir": {
                        "type": "string",
                        "description": "Working directory (relative to workspace root, or absolute). Defaults to workspace root."
                    }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "list_directory",
            "description": "List files and directories at the given path. Returns a detailed listing with permissions, sizes, and names.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path (relative to workspace root, or absolute). Defaults to workspace root."
                    }
                },
                "required": []
            }
        }),
    ]
}

/// Dispatch a sandbox tool call by name, returning Result.
pub(crate) async fn dispatch(
    name: &str,
    input: &Value,
    sandbox: &Arc<Sandbox>,
) -> Result<String, String> {
    match name {
        "read_file" => read_file(input, sandbox).await,
        "write_file" => write_file(input, sandbox).await,
        "edit_file" => edit_file(input, sandbox).await,
        "exec_command" => exec_command(input, sandbox).await,
        "list_directory" => list_directory(input, sandbox).await,
        _ => Err(format!("unknown sandbox tool: {name}")),
    }
}


async fn read_file(input: &Value, sandbox: &Sandbox) -> Result<String, String> {
    let path = input["path"].as_str().ok_or("missing path")?;
    sandbox.read_file(path).await.map_err(|e| e.to_string())
}

async fn write_file(input: &Value, sandbox: &Sandbox) -> Result<String, String> {
    let path = input["path"].as_str().ok_or("missing path")?;
    let content = input["content"].as_str().ok_or("missing content")?;
    sandbox
        .write_file(path, content)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("File written: {path}"))
}

async fn edit_file(input: &Value, sandbox: &Sandbox) -> Result<String, String> {
    let path = input["path"].as_str().ok_or("missing path")?;
    let old_text = input["old_text"].as_str().ok_or("missing old_text")?;
    let new_text = input["new_text"].as_str().ok_or("missing new_text")?;

    let content = sandbox.read_file(path).await.map_err(|e| e.to_string())?;

    let count = content.matches(old_text).count();
    if count == 0 {
        return Err(format!("old_text not found in {path}"));
    }
    if count > 1 {
        return Err(format!(
            "old_text appears {count} times in {path} — must be unique"
        ));
    }

    let new_content = content.replacen(old_text, new_text, 1);
    sandbox
        .write_file(path, &new_content)
        .await
        .map_err(|e| e.to_string())?;
    Ok(format!("File edited: {path}"))
}

async fn exec_command(input: &Value, sandbox: &Sandbox) -> Result<String, String> {
    let command = input["command"].as_str().ok_or("missing command")?;
    let workdir = input["workdir"].as_str();

    let result = if let Some(dir) = workdir {
        sandbox.exec_in(command, dir).await
    } else {
        sandbox.exec_shell(command).await
    }
    .map_err(|e| e.to_string())?;

    // Format output for the agent
    let mut output = String::new();
    if !result.stdout.is_empty() {
        output.push_str(&result.stdout);
    }
    if !result.stderr.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("stderr: ");
        output.push_str(&result.stderr);
    }
    if result.exit_code != 0 {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!("exit code: {}", result.exit_code));
    }
    if output.is_empty() {
        output = "(no output)".to_string();
    }

    // Truncate very long output
    if output.len() > 50_000 {
        output.truncate(50_000);
        output.push_str("\n\n[output truncated]");
    }

    Ok(output)
}

async fn list_directory(input: &Value, sandbox: &Sandbox) -> Result<String, String> {
    let path = input["path"].as_str().unwrap_or(".");
    sandbox
        .list_directory(path)
        .await
        .map_err(|e| e.to_string())
}
