use std::sync::Arc;

use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::Value;

use crate::server::SharedState;

use super::tools;

/// MCP server facade over the running nexus-server.
pub struct NexusMcpServer {
    shared: Arc<SharedState>,
}

impl NexusMcpServer {
    pub fn new(shared: Arc<SharedState>) -> Self {
        Self { shared }
    }
}

impl ServerHandler for NexusMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2025_03_26,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "nexus-server".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                title: None,
                description: None,
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Nexus webhook automation server — manage workflows, view stats, and control the running instance."
                    .to_string(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let defs = tools::definitions();
        let tools: Vec<Tool> = defs
            .into_iter()
            .filter_map(|v| {
                let name = v["name"].as_str()?.to_owned();
                let description = v["description"].as_str().map(|s| s.to_owned());
                let input_schema = v.get("inputSchema").cloned().unwrap_or(
                    serde_json::json!({"type": "object", "properties": {}}),
                );
                Some(Tool {
                    name: name.into(),
                    description: description.map(Into::into),
                    input_schema: serde_json::from_value(input_schema).ok()?,
                    annotations: None,
                    title: None,
                    output_schema: None,
                    execution: None,
                    icons: None,
                    meta: None,
                })
            })
            .collect();

        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args: Value = request
            .arguments
            .map(|m| serde_json::to_value(m).unwrap_or_default())
            .unwrap_or(Value::Object(Default::default()));

        let result = tools::execute(&request.name, &args, &self.shared).await;

        Ok(CallToolResult {
            content: vec![Content::text(result)],
            is_error: None,
            meta: None,
            structured_content: None,
        })
    }
}
