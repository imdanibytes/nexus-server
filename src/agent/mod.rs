pub mod client;
pub mod sandbox_tools;
pub mod tools;

use std::sync::Arc;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::sandbox::Sandbox;

use client::{ChatClient, ContentBlock};

const MAX_TURNS: usize = 20;

pub struct AgentLoop {
    client: Box<dyn ChatClient>,
    model: String,
    max_tokens: u32,
    tools: Vec<Value>,
    sandbox: Option<Arc<Sandbox>>,
    cancel: Option<CancellationToken>,
    github_token: String,
    http_client: reqwest::Client,
}

impl AgentLoop {
    pub fn new(
        config: crate::config::ClaudeConfig,
        github_token: String,
        http_client: reqwest::Client,
    ) -> Self {
        let api_key = std::env::var(&config.api_key_env).unwrap_or_default();
        let chat_client = client::AnthropicClient::new(http_client.clone(), api_key);
        Self {
            client: Box::new(chat_client),
            model: config.model,
            max_tokens: config.max_tokens,
            tools: tools::github_tool_definitions(),
            sandbox: None,
            cancel: None,
            github_token,
            http_client,
        }
    }

    /// Create an agent loop with a custom chat client (for testing or alternative providers).
    pub fn with_client(
        client: Box<dyn ChatClient>,
        model: String,
        max_tokens: u32,
        github_token: String,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            client,
            model,
            max_tokens,
            tools: tools::github_tool_definitions(),
            sandbox: None,
            cancel: None,
            github_token,
            http_client,
        }
    }

    pub fn with_cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn with_sandbox(mut self, sandbox: Arc<Sandbox>) -> Self {
        let mut all_tools = self.tools;
        all_tools.extend(sandbox_tools::sandbox_tool_definitions());
        self.tools = all_tools;
        self.sandbox = Some(sandbox);
        self
    }

    /// Run the agent loop with the given prompt. Returns the final text output.
    pub async fn run(&self, prompt: &str) -> Result<String, AgentError> {
        let mut messages: Vec<Value> = vec![json!({
            "role": "user",
            "content": prompt,
        })];

        let mut final_text = String::new();

        for turn in 0..MAX_TURNS {
            // Check cancellation between turns
            if let Some(ref cancel) = self.cancel {
                if cancel.is_cancelled() {
                    info!(turn, "agent cancelled");
                    return Err(AgentError::Cancelled);
                }
            }

            info!(turn, "agent turn");

            // Call the chat client
            let api_future = self.client.send_message(
                &self.model,
                self.max_tokens,
                &self.tools,
                &messages,
            );

            // Race the API call against cancellation
            let response = if let Some(ref cancel) = self.cancel {
                tokio::select! {
                    result = api_future => result.map_err(AgentError::Chat)?,
                    _ = cancel.cancelled() => {
                        info!(turn, "agent cancelled during API call");
                        return Err(AgentError::Cancelled);
                    }
                }
            } else {
                api_future.await.map_err(AgentError::Chat)?
            };

            // Process content blocks
            let mut assistant_content: Vec<Value> = Vec::new();
            let mut tool_results: Vec<Value> = Vec::new();

            for block in &response.content {
                match block {
                    ContentBlock::Text(text) => {
                        final_text = text.clone();
                        assistant_content.push(json!({
                            "type": "text",
                            "text": text,
                        }));
                        info!(len = text.len(), "agent produced text");
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        info!(tool = %name, id = %id, "agent calling tool");

                        let result = if sandbox_tools::is_sandbox_tool(name) {
                            if let Some(ref sandbox) = self.sandbox {
                                sandbox_tools::execute(name, input, sandbox).await
                            } else {
                                "Error: no sandbox available — this tool requires a sandbox environment".to_string()
                            }
                        } else {
                            tools::execute(
                                name,
                                input,
                                &self.http_client,
                                &self.github_token,
                            )
                            .await
                        };

                        assistant_content.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input,
                        }));
                        tool_results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": id,
                            "content": result,
                        }));
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

            if response.stop_reason == "end_turn" {
                info!(turns = turn + 1, "agent finished");
                return Ok(final_text);
            }

            if response.stop_reason != "tool_use" {
                warn!(stop_reason = %response.stop_reason, "unexpected stop reason");
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
    #[error("chat client error: {0}")]
    Chat(#[from] client::ChatError),
    #[error("agent cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use client::{ChatError, ContentBlock, MessageResponse};
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use tokio::sync::Mutex;

    /// Mock chat client that returns pre-configured responses.
    struct MockChatClient {
        responses: Mutex<VecDeque<Result<MessageResponse, ChatError>>>,
    }

    impl MockChatClient {
        fn new(responses: Vec<MessageResponse>) -> Self {
            Self {
                responses: Mutex::new(
                    responses.into_iter().map(Ok).collect(),
                ),
            }
        }

        fn with_error(responses: Vec<MessageResponse>, error: ChatError) -> Self {
            let mut queue: VecDeque<Result<MessageResponse, ChatError>> =
                responses.into_iter().map(Ok).collect();
            queue.push_back(Err(error));
            Self {
                responses: Mutex::new(queue),
            }
        }
    }

    impl ChatClient for MockChatClient {
        fn send_message(
            &self,
            _model: &str,
            _max_tokens: u32,
            _tools: &[Value],
            _messages: &[Value],
        ) -> Pin<Box<dyn Future<Output = Result<MessageResponse, ChatError>> + Send + '_>>
        {
            Box::pin(async {
                self.responses
                    .lock()
                    .await
                    .pop_front()
                    .unwrap_or(Err(ChatError::Request("no more mock responses".into())))
            })
        }
    }

    fn make_agent(client: MockChatClient) -> AgentLoop {
        AgentLoop::with_client(
            Box::new(client),
            "test-model".into(),
            1024,
            "fake-token".into(),
            reqwest::Client::new(),
        )
    }

    #[tokio::test]
    async fn single_turn_text_response() {
        let mock = MockChatClient::new(vec![MessageResponse {
            stop_reason: "end_turn".into(),
            content: vec![ContentBlock::Text("Hello, world!".into())],
        }]);

        let agent = make_agent(mock);
        let result = agent.run("Say hello").await.unwrap();
        assert_eq!(result, "Hello, world!");
    }

    #[tokio::test]
    async fn multi_turn_with_tool_use() {
        let mock = MockChatClient::new(vec![
            // Turn 1: model calls a tool
            MessageResponse {
                stop_reason: "tool_use".into(),
                content: vec![
                    ContentBlock::Text("Let me check that.".into()),
                    ContentBlock::ToolUse {
                        id: "tool_1".into(),
                        name: "get_pull_request_diff".into(),
                        input: json!({"owner": "test", "repo": "repo", "pull_number": 1}),
                    },
                ],
            },
            // Turn 2: model produces final text
            MessageResponse {
                stop_reason: "end_turn".into(),
                content: vec![ContentBlock::Text("The diff looks good.".into())],
            },
        ]);

        let agent = make_agent(mock);
        let result = agent.run("Review PR #1").await.unwrap();
        assert_eq!(result, "The diff looks good.");
    }

    #[tokio::test]
    async fn unexpected_stop_reason_returns_text() {
        let mock = MockChatClient::new(vec![MessageResponse {
            stop_reason: "max_tokens".into(),
            content: vec![ContentBlock::Text("partial output".into())],
        }]);

        let agent = make_agent(mock);
        let result = agent.run("Write a lot").await.unwrap();
        assert_eq!(result, "partial output");
    }

    #[tokio::test]
    async fn empty_response_returns_empty_string() {
        let mock = MockChatClient::new(vec![MessageResponse {
            stop_reason: "end_turn".into(),
            content: vec![],
        }]);

        let agent = make_agent(mock);
        let result = agent.run("Do nothing").await.unwrap();
        assert_eq!(result, "");
    }

    #[tokio::test]
    async fn api_error_propagates() {
        let mock = MockChatClient::with_error(
            vec![],
            ChatError::ApiError {
                status: 429,
                body: "rate limited".into(),
            },
        );

        let agent = make_agent(mock);
        let err = agent.run("anything").await.unwrap_err();
        assert!(err.to_string().contains("429"));
    }

    #[tokio::test]
    async fn cancellation_before_first_turn() {
        let mock = MockChatClient::new(vec![MessageResponse {
            stop_reason: "end_turn".into(),
            content: vec![ContentBlock::Text("should not reach".into())],
        }]);

        let cancel = CancellationToken::new();
        cancel.cancel();

        let agent = make_agent(mock).with_cancel(cancel);
        let err = agent.run("anything").await.unwrap_err();
        assert!(matches!(err, AgentError::Cancelled));
    }

    #[tokio::test]
    async fn max_turns_returns_last_text() {
        // Return tool_use every turn — agent will hit max turns
        let responses: Vec<MessageResponse> = (0..MAX_TURNS)
            .map(|i| MessageResponse {
                stop_reason: "tool_use".into(),
                content: vec![
                    ContentBlock::Text(format!("turn {i}")),
                    ContentBlock::ToolUse {
                        id: format!("tool_{i}"),
                        name: "create_comment".into(),
                        input: json!({"owner": "o", "repo": "r", "issue_number": 1, "body": "hi"}),
                    },
                ],
            })
            .collect();

        let mock = MockChatClient::new(responses);
        let agent = make_agent(mock);
        let result = agent.run("Keep going").await.unwrap();
        assert_eq!(result, format!("turn {}", MAX_TURNS - 1));
    }
}
