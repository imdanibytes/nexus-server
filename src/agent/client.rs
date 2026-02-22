//! Chat client abstraction for the coding agent.
//!
//! The `ChatClient` trait decouples the agent loop from the LLM provider.
//! Ship with `AnthropicClient` for Claude. Implement the trait for any
//! other provider (OpenAI, local models, mock for tests).

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

/// A content block in a chat response.
#[derive(Debug, Clone)]
pub enum ContentBlock {
    /// Text output from the model.
    Text(String),
    /// Tool use request from the model.
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

/// Parsed response from a chat completion API.
#[derive(Debug, Clone)]
pub struct MessageResponse {
    /// Why the model stopped: "end_turn", "tool_use", etc.
    pub stop_reason: String,
    /// Content blocks (text and/or tool calls).
    pub content: Vec<ContentBlock>,
}

/// Errors from chat client operations.
#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("API request failed: {0}")]
    Request(String),
    #[error("API returned {status}: {body}")]
    ApiError { status: u16, body: String },
    #[error("failed to parse response: {0}")]
    Parse(String),
}

/// Trait abstracting LLM chat completion APIs.
///
/// Implementors handle authentication, HTTP transport, and response parsing.
/// The agent loop calls `send_message` each turn and processes the typed response.
pub trait ChatClient: Send + Sync {
    fn send_message(
        &self,
        model: &str,
        max_tokens: u32,
        tools: &[Value],
        messages: &[Value],
    ) -> Pin<Box<dyn Future<Output = Result<MessageResponse, ChatError>> + Send + '_>>;
}

/// Claude API client via Anthropic's messages endpoint.
pub struct AnthropicClient {
    client: reqwest::Client,
    api_key: String,
}

impl AnthropicClient {
    pub fn new(client: reqwest::Client, api_key: String) -> Self {
        Self { client, api_key }
    }
}

impl ChatClient for AnthropicClient {
    fn send_message(
        &self,
        model: &str,
        max_tokens: u32,
        tools: &[Value],
        messages: &[Value],
    ) -> Pin<Box<dyn Future<Output = Result<MessageResponse, ChatError>> + Send + '_>> {
        let model = model.to_string();
        let tools = tools.to_vec();
        let messages = messages.to_vec();

        Box::pin(async move {
            let body = serde_json::json!({
                "model": model,
                "max_tokens": max_tokens,
                "tools": tools,
                "messages": messages,
            });

            let resp = self
                .client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| ChatError::Request(e.to_string()))?;

            let status = resp.status().as_u16();
            let resp_text = resp
                .text()
                .await
                .map_err(|e| ChatError::Request(e.to_string()))?;

            if status != 200 {
                return Err(ChatError::ApiError {
                    status,
                    body: resp_text,
                });
            }

            let parsed: Value =
                serde_json::from_str(&resp_text).map_err(|e| ChatError::Parse(e.to_string()))?;

            let stop_reason = parsed["stop_reason"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();

            let raw_content = parsed["content"].as_array().cloned().unwrap_or_default();

            let content = raw_content
                .iter()
                .filter_map(|block| {
                    let block_type = block["type"].as_str()?;
                    match block_type {
                        "text" => {
                            let text = block["text"].as_str().unwrap_or("").to_string();
                            Some(ContentBlock::Text(text))
                        }
                        "tool_use" => {
                            let id = block["id"].as_str().unwrap_or("").to_string();
                            let name = block["name"].as_str().unwrap_or("").to_string();
                            let input = block["input"].clone();
                            Some(ContentBlock::ToolUse { id, name, input })
                        }
                        _ => None,
                    }
                })
                .collect();

            Ok(MessageResponse {
                stop_reason,
                content,
            })
        })
    }
}
