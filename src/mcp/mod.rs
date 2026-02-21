mod server;
mod tools;

use std::sync::Arc;

use axum::Router;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, tower::StreamableHttpService,
    StreamableHttpServerConfig,
};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::server::SharedState;

/// Mount the MCP management endpoint onto the router.
pub fn mount(router: Router, shared: Arc<SharedState>) -> Router {
    let cancel = CancellationToken::new();
    let config = StreamableHttpServerConfig {
        stateful_mode: true,
        cancellation_token: cancel.clone(),
        ..Default::default()
    };

    let shared_for_factory = shared;
    let service = StreamableHttpService::new(
        move || Ok(server::NexusMcpServer::new(shared_for_factory.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    info!("mounting MCP management endpoint at /mcp");

    router.nest_service("/mcp", service)
}
