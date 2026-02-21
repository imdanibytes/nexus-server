pub mod tools;

use serde_json::{json, Value};
use tracing::{info, warn};

use crate::config::ClaudeConfig;

const MAX_TURNS: usize = 20;

pub struct AgentLoop {
    config: ClaudeConfig,
    github_token: String,
    client: reqwest::Client,
    tools: Vec<Value>,
}

impl AgentLoop {
    pub fn new(
        config: ClaudeConfig,
        github_token: String,
        client: reqwest::Client,
    ) -> Self {
        Self {
            config,
            github_token,
            tools: tools::github_tool_definitions(),
            client,
        }
    }

    /// Run the agent loop with the given prompt. Returns the final text output.
    pub async fn run(&self, prompt: &str) -> Result<String, AgentError> {
        let api_key = std::env::var(&self.config.api_key_env)
            .map_err(|_| AgentError::ApiKeyMissing(self.config.api_key_env.clone()))?;

        let mut messages: Vec<Value> = vec![json!({
            "role": "user",
            "content": prompt,
        })];

        let mut final_text = String::new();

        for turn in 0..MAX_TURNS {
            info!(turn, "agent turn");

            let body = json!({
                "model": self.config.model,
                "max_tokens": self.config.max_tokens,
                "tools": self.tools,
                "messages": messages,
            });

            let resp = self
                .client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(AgentError::Request)?;

            let status = resp.status().as_u16();
            let resp_text = resp.text().await.map_err(AgentError::Request)?;

            if status != 200 {
                return Err(AgentError::ApiError {
                    status,
                    body: resp_text,
                });
            }

            let parsed: Value = serde_json::from_str(&resp_text)
                .map_err(|e| AgentError::ParseError(e.to_string()))?;

            let stop_reason = parsed["stop_reason"].as_str().unwrap_or("unknown");
            let content = parsed["content"].as_array().cloned().unwrap_or_default();

            // Process content blocks
            let mut assistant_content: Vec<Value> = Vec::new();
            let mut tool_results: Vec<Value> = Vec::new();

            for block in &content {
                let block_type = block["type"].as_str().unwrap_or("");
                match block_type {
                    "text" => {
                        let text = block["text"].as_str().unwrap_or("");
                        final_text = text.to_string();
                        assistant_content.push(block.clone());
                        info!(len = text.len(), "agent produced text");
                    }
                    "tool_use" => {
                        let tool_id = block["id"].as_str().unwrap_or("");
                        let tool_name = block["name"].as_str().unwrap_or("");
                        let tool_input = &block["input"];

                        info!(tool = tool_name, id = tool_id, "agent calling tool");

                        let result = tools::execute(
                            tool_name,
                            tool_input,
                            &self.client,
                            &self.github_token,
                        )
                        .await;

                        assistant_content.push(block.clone());
                        tool_results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": tool_id,
                            "content": result,
                        }));
                    }
                    _ => {
                        warn!(block_type, "unknown content block type");
                    }
                }
            }

            // Append assistant message
            messages.push(json!({
                "role": "assistant",
                "content": assistant_content,
            }));

            // If there were tool calls, append results and continue
            if !tool_results.is_empty() {
                messages.push(json!({
                    "role": "user",
                    "content": tool_results,
                }));
            }

            if stop_reason == "end_turn" {
                info!(turns = turn + 1, "agent finished");
                return Ok(final_text);
            }

            if stop_reason != "tool_use" {
                warn!(stop_reason, "unexpected stop reason");
                return Ok(final_text);
            }
        }

        warn!("agent hit max turns limit");
        Ok(final_text)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("API key env var '{0}' not set")]
    ApiKeyMissing(String),
    #[error("API request failed: {0}")]
    Request(reqwest::Error),
    #[error("API returned {status}: {body}")]
    ApiError { status: u16, body: String },
    #[error("failed to parse response: {0}")]
    ParseError(String),
}
