use serde_json::{json, Value};

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
            "description": "Submit a review on a GitHub pull request, optionally with inline comments on specific lines.",
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
                    },
                    "comments": {
                        "type": "array",
                        "description": "Inline comments on specific lines of the diff",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "File path relative to repo root" },
                                "line": { "type": "integer", "description": "Line number in the diff to comment on" },
                                "side": {
                                    "type": "string",
                                    "enum": ["LEFT", "RIGHT"],
                                    "description": "Which side of the diff (LEFT=original, RIGHT=modified). Default: RIGHT"
                                },
                                "body": { "type": "string", "description": "Comment body (markdown)" }
                            },
                            "required": ["path", "line", "body"]
                        }
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
        json!({
            "name": "get_review_comments",
            "description": "Get all review comments on a pull request. Returns comment id, path, line, body, user, and in_reply_to_id for threading.",
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
        json!({
            "name": "reply_to_review_comment",
            "description": "Reply to a review comment thread on a pull request.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo": { "type": "string", "description": "Repository name" },
                    "pull_number": { "type": "integer", "description": "PR number" },
                    "comment_id": { "type": "integer", "description": "ID of the comment to reply to" },
                    "body": { "type": "string", "description": "Reply body (markdown)" }
                },
                "required": ["owner", "repo", "pull_number", "comment_id", "body"]
            }
        }),
        json!({
            "name": "merge_pull_request",
            "description": "Merge a pull request.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo": { "type": "string", "description": "Repository name" },
                    "pull_number": { "type": "integer", "description": "PR number" },
                    "merge_method": {
                        "type": "string",
                        "enum": ["merge", "squash", "rebase"],
                        "description": "Merge method (default: squash)"
                    }
                },
                "required": ["owner", "repo", "pull_number"]
            }
        }),
        json!({
            "name": "get_issue",
            "description": "Get details of a GitHub issue including title, body, labels, and state.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo": { "type": "string", "description": "Repository name" },
                    "issue_number": { "type": "integer", "description": "Issue number" }
                },
                "required": ["owner", "repo", "issue_number"]
            }
        }),
        json!({
            "name": "create_pull_request",
            "description": "Create a pull request on a GitHub repository.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "owner": { "type": "string", "description": "Repository owner" },
                    "repo": { "type": "string", "description": "Repository name" },
                    "title": { "type": "string", "description": "PR title" },
                    "body": { "type": "string", "description": "PR body (markdown)" },
                    "head": { "type": "string", "description": "Branch name containing the changes" },
                    "base": { "type": "string", "description": "Branch to merge into (default: main)" }
                },
                "required": ["owner", "repo", "title", "body", "head"]
            }
        }),
    ]
}

/// Dispatch a tool call by name, returning Result.
pub(crate) async fn dispatch(
    name: &str,
    input: &Value,
    client: &reqwest::Client,
    token: &str,
) -> Result<String, String> {
    match name {
        "create_comment" => create_comment(input, client, token).await,
        "add_labels" => add_labels(input, client, token).await,
        "close_issue" => close_issue(input, client, token).await,
        "create_review" => create_review(input, client, token).await,
        "get_pull_request_diff" => get_pr_diff(input, client, token).await,
        "get_review_comments" => get_review_comments(input, client, token).await,
        "reply_to_review_comment" => reply_to_review_comment(input, client, token).await,
        "merge_pull_request" => merge_pull_request(input, client, token).await,
        "get_issue" => get_issue(input, client, token).await,
        "create_pull_request" => create_pull_request(input, client, token).await,
        _ => Err(format!("unknown tool: {name}")),
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

    let mut payload = json!({ "body": body, "event": event });

    if let Some(comments) = input["comments"].as_array() {
        let inline: Vec<Value> = comments
            .iter()
            .filter_map(|c| {
                let path = c["path"].as_str()?;
                let line = c["line"].as_i64()?;
                let body = c["body"].as_str()?;
                let side = c["side"].as_str().unwrap_or("RIGHT");
                Some(json!({ "path": path, "line": line, "side": side, "body": body }))
            })
            .collect();
        if !inline.is_empty() {
            payload["comments"] = json!(inline);
        }
    }

    let resp = client
        .post(github_api(&format!("/repos/{owner}/{repo}/pulls/{number}/reviews")))
        .header("authorization", format!("Bearer {token}"))
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github+json")
        .json(&payload)
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

async fn merge_pull_request(
    input: &Value,
    client: &reqwest::Client,
    token: &str,
) -> Result<String, String> {
    let owner = input["owner"].as_str().ok_or("missing owner")?;
    let repo = input["repo"].as_str().ok_or("missing repo")?;
    let number = input["pull_number"].as_i64().ok_or("missing pull_number")?;
    let method = input["merge_method"].as_str().unwrap_or("squash");

    let resp = client
        .put(github_api(&format!("/repos/{owner}/{repo}/pulls/{number}/merge")))
        .header("authorization", format!("Bearer {token}"))
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github+json")
        .json(&json!({ "merge_method": method }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status().as_u16();
    if status == 200 {
        Ok(format!("PR {owner}/{repo}#{number} merged via {method}"))
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(format!("GitHub API {status}: {body}"))
    }
}

async fn get_review_comments(
    input: &Value,
    client: &reqwest::Client,
    token: &str,
) -> Result<String, String> {
    let owner = input["owner"].as_str().ok_or("missing owner")?;
    let repo = input["repo"].as_str().ok_or("missing repo")?;
    let number = input["pull_number"].as_i64().ok_or("missing pull_number")?;

    let per_page = 100;
    let mut page = 1u32;
    let mut all_comments: Vec<Value> = Vec::new();

    loop {
        let resp = client
            .get(github_api(&format!(
                "/repos/{owner}/{repo}/pulls/{number}/comments?per_page={per_page}&page={page}"
            )))
            .header("authorization", format!("Bearer {token}"))
            .header("user-agent", "nexus-server")
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| e.to_string())?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("GitHub API {status}: {body}"));
        }

        let batch: Vec<Value> = resp.json().await.map_err(|e| e.to_string())?;
        let count = batch.len();
        all_comments.extend(batch);

        if count < per_page {
            break;
        }
        page += 1;
    }

    let summary: Vec<Value> = all_comments
        .iter()
        .map(|c| {
            json!({
                "id": c["id"],
                "user": c["user"]["login"],
                "path": c["path"],
                "line": c["line"],
                "body": c["body"],
                "in_reply_to_id": c["in_reply_to_id"],
                "created_at": c["created_at"],
            })
        })
        .collect();

    Ok(serde_json::to_string_pretty(&summary).unwrap_or_default())
}

async fn reply_to_review_comment(
    input: &Value,
    client: &reqwest::Client,
    token: &str,
) -> Result<String, String> {
    let owner = input["owner"].as_str().ok_or("missing owner")?;
    let repo = input["repo"].as_str().ok_or("missing repo")?;
    let number = input["pull_number"].as_i64().ok_or("missing pull_number")?;
    let comment_id = input["comment_id"].as_i64().ok_or("missing comment_id")?;
    let body = input["body"].as_str().ok_or("missing body")?;

    let resp = client
        .post(github_api(&format!(
            "/repos/{owner}/{repo}/pulls/{number}/comments"
        )))
        .header("authorization", format!("Bearer {token}"))
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github+json")
        .json(&json!({ "body": body, "in_reply_to": comment_id }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status().as_u16();
    if status == 201 {
        Ok(format!(
            "Reply posted to comment {comment_id} on {owner}/{repo}#{number}"
        ))
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

async fn get_issue(
    input: &Value,
    client: &reqwest::Client,
    token: &str,
) -> Result<String, String> {
    let owner = input["owner"].as_str().ok_or("missing owner")?;
    let repo = input["repo"].as_str().ok_or("missing repo")?;
    let number = input["issue_number"].as_i64().ok_or("missing issue_number")?;

    let resp = client
        .get(github_api(&format!("/repos/{owner}/{repo}/issues/{number}")))
        .header("authorization", format!("Bearer {token}"))
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status().as_u16();
    if status != 200 {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("GitHub API {status}: {body}"));
    }

    let issue: Value = resp.json().await.map_err(|e| e.to_string())?;

    let labels: Vec<&str> = issue["labels"]
        .as_array()
        .map(|arr| arr.iter().filter_map(|l| l["name"].as_str()).collect())
        .unwrap_or_default();

    let compact = json!({
        "number": issue["number"],
        "title": issue["title"],
        "body": issue["body"],
        "state": issue["state"],
        "labels": labels,
        "user": issue["user"]["login"],
        "html_url": issue["html_url"],
        "comments": issue["comments"],
    });

    Ok(serde_json::to_string_pretty(&compact).unwrap_or_default())
}

async fn create_pull_request(
    input: &Value,
    client: &reqwest::Client,
    token: &str,
) -> Result<String, String> {
    let owner = input["owner"].as_str().ok_or("missing owner")?;
    let repo = input["repo"].as_str().ok_or("missing repo")?;
    let title = input["title"].as_str().ok_or("missing title")?;
    let body = input["body"].as_str().ok_or("missing body")?;
    let head = input["head"].as_str().ok_or("missing head")?;
    let base = input["base"].as_str().unwrap_or("main");

    let resp = client
        .post(github_api(&format!("/repos/{owner}/{repo}/pulls")))
        .header("authorization", format!("Bearer {token}"))
        .header("user-agent", "nexus-server")
        .header("accept", "application/vnd.github+json")
        .json(&json!({
            "title": title,
            "body": body,
            "head": head,
            "base": base,
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let status = resp.status().as_u16();
    if status == 201 {
        let pr: Value = resp.json().await.map_err(|e| e.to_string())?;
        let number = pr["number"].as_i64().unwrap_or(0);
        let html_url = pr["html_url"].as_str().unwrap_or("");
        Ok(format!("PR #{number} created: {html_url}"))
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(format!("GitHub API {status}: {body}"))
    }
}
