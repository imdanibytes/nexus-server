use serde_json::{json, Value};
use tracing::{info, warn};

/// Tool definitions sent to the Claude API.
pub fn github_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "create_comment",
            "description": "Post a comment on a GitHub issue or pull request.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo": { "type": "string", "description": "Repository name" },
                    "issue_number": { "type": "integer", "description": "Issue or PR number" },
                    "body": { "type": "string", "description": "Comment body (markdown)" }
                },
                "required": ["owner", "repo", "issue_number", "body"]
            }
        }),
        json!({
            "name": "add_labels",
            "description": "Add labels to a GitHub issue or pull request. Creates labels if they don't exist.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo": { "type": "string", "description": "Repository name" },
                    "issue_number": { "type": "integer", "description": "Issue or PR number" },
                    "labels": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Label names to add"
                    }
                },
                "required": ["owner", "repo", "issue_number", "labels"]
            }
        }),
        json!({
            "name": "close_issue",
            "description": "Close a GitHub issue with an optional reason.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo": { "type": "string", "description": "Repository name" },
                    "issue_number": { "type": "integer", "description": "Issue number" },
                    "reason": {
                        "type": "string",
                        "enum": ["completed", "not_planned"],
                        "description": "Close reason"
                    }
                },
                "required": ["owner", "repo", "issue_number"]
            }
        }),
        json!({
            "name": "create_review",
            "description": "Submit a review on a GitHub pull request.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo": { "type": "string", "description": "Repository name" },
                    "pull_number": { "type": "integer", "description": "PR number" },
                    "body": { "type": "string", "description": "Review body (markdown)" },
                    "event": {
                        "type": "string",
                        "enum": ["APPROVE", "REQUEST_CHANGES", "COMMENT"],
                        "description": "Review action"
                    }
                },
                "required": ["owner", "repo", "pull_number", "body", "event"]
            }
        }),
        json!({
            "name": "get_pull_request_diff",
            "description": "Get the diff of a pull request to review the changes.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo": { "type": "string", "description": "Repository name" },
                    "pull_number": { "type": "integer", "description": "PR number" }
                },
                "required": ["owner", "repo", "pull_number"]
            }
        }),
    ]
}

/// Execute a tool call and return the result as a string.
pub async fn execute(
    name: &str,
    input: &Value,
    client: &reqwest::Client,
    github_token: &str,
) -> String {
    let result = match name {
        "create_comment" => create_comment(input, client, github_token).await,
        "add_labels" => add_labels(input, client, github_token).await,
        "close_issue" => close_issue(input, client, github_token).await,
        "create_review" => create_review(input, client, github_token).await,
        "get_pull_request_diff" => get_pr_diff(input, client, github_token).await,
        _ => Err(format!("unknown tool: {name}")),
    };

    match result {
        Ok(msg) => {
            info!(tool = name, "tool call succeeded");
            msg
        }
        Err(e) => {
            warn!(tool = name, error = %e, "tool call failed");
            format!("Error: {e}")
        }
    }
}

fn github_api(path: &str) -> String {
    format!("https://api.github.com{path}")
}

async fn create_comment(
    input: &Value,
    client: &reqwest::Client,
    token: &str,
) -> Result<String, String> {
    let owner = input["owner"].as_str().ok_or("missing owner")?;
    let repo = input["repo"].as_str().ok_or("missing repo")?;
    let number = input["issue_number"].as_i64().ok_or("missing issue_number")?;
    let body = input["body"].as_str().ok_or("missing body")?;

    let resp = client
        .post(github_api(&format!("/repos/{owner}/{repo}/issues/{number}/comments")))
        .header("authorization", format!("Bearer {token}"))
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github+json")
        .json(&json!({ "body": body }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status().as_u16();
    if status == 201 {
        Ok(format!("Comment posted on {owner}/{repo}#{number}"))
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(format!("GitHub API {status}: {body}"))
    }
}

async fn add_labels(
    input: &Value,
    client: &reqwest::Client,
    token: &str,
) -> Result<String, String> {
    let owner = input["owner"].as_str().ok_or("missing owner")?;
    let repo = input["repo"].as_str().ok_or("missing repo")?;
    let number = input["issue_number"].as_i64().ok_or("missing issue_number")?;
    let labels = input["labels"]
        .as_array()
        .ok_or("missing labels")?
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>();

    let resp = client
        .post(github_api(&format!("/repos/{owner}/{repo}/issues/{number}/labels")))
        .header("authorization", format!("Bearer {token}"))
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github+json")
        .json(&json!({ "labels": labels }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status().as_u16();
    if status == 200 {
        Ok(format!("Labels {:?} added to {owner}/{repo}#{number}", labels))
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(format!("GitHub API {status}: {body}"))
    }
}

async fn close_issue(
    input: &Value,
    client: &reqwest::Client,
    token: &str,
) -> Result<String, String> {
    let owner = input["owner"].as_str().ok_or("missing owner")?;
    let repo = input["repo"].as_str().ok_or("missing repo")?;
    let number = input["issue_number"].as_i64().ok_or("missing issue_number")?;
    let reason = input["reason"].as_str().unwrap_or("completed");

    let resp = client
        .patch(github_api(&format!("/repos/{owner}/{repo}/issues/{number}")))
        .header("authorization", format!("Bearer {token}"))
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github+json")
        .json(&json!({ "state": "closed", "state_reason": reason }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status().as_u16();
    if status == 200 {
        Ok(format!("Issue {owner}/{repo}#{number} closed as {reason}"))
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(format!("GitHub API {status}: {body}"))
    }
}

async fn create_review(
    input: &Value,
    client: &reqwest::Client,
    token: &str,
) -> Result<String, String> {
    let owner = input["owner"].as_str().ok_or("missing owner")?;
    let repo = input["repo"].as_str().ok_or("missing repo")?;
    let number = input["pull_number"].as_i64().ok_or("missing pull_number")?;
    let body = input["body"].as_str().ok_or("missing body")?;
    let event = input["event"].as_str().ok_or("missing event")?;

    let resp = client
        .post(github_api(&format!("/repos/{owner}/{repo}/pulls/{number}/reviews")))
        .header("authorization", format!("Bearer {token}"))
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github+json")
        .json(&json!({ "body": body, "event": event }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status().as_u16();
    if status == 200 {
        Ok(format!("Review ({event}) submitted on {owner}/{repo}#{number}"))
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(format!("GitHub API {status}: {body}"))
    }
}

async fn get_pr_diff(
    input: &Value,
    client: &reqwest::Client,
    token: &str,
) -> Result<String, String> {
    let owner = input["owner"].as_str().ok_or("missing owner")?;
    let repo = input["repo"].as_str().ok_or("missing repo")?;
    let number = input["pull_number"].as_i64().ok_or("missing pull_number")?;

    let resp = client
        .get(github_api(&format!("/repos/{owner}/{repo}/pulls/{number}")))
        .header("authorization", format!("Bearer {token}"))
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github.v3.diff")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status().as_u16();
    if status == 200 {
        let diff = resp.text().await.unwrap_or_default();
        // Truncate very large diffs
        if diff.len() > 50_000 {
            Ok(format!("{}...\n\n[diff truncated, {} bytes total]", &diff[..50_000], diff.len()))
        } else {
            Ok(diff)
        }
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(format!("GitHub API {status}: {body}"))
    }
}
