use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use chrono::{DateTime, Utc};
use cloudevents::event::AttributesReader;
use cloudevents::Event;
use serde::Serialize;
use serde_json::json;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info, warn};

use crate::github_auth::GitHubAppAuth;
use crate::sandbox::SandboxRegistry;
use crate::sources::{self, Source};
use crate::workflows::task::TaskStore;
use crate::workflows::WorkflowStore;

/// Max webhook request body size (256 KB).
const MAX_BODY_SIZE: usize = 256 * 1024;

/// Max number of delivery IDs to track for replay protection.
const MAX_SEEN_DELIVERIES: usize = 10_000;

/// Max number of recent events to keep.
const MAX_RECENT_EVENTS: usize = 100;

/// Shared state across all routes.
pub struct SharedState {
    pub claude: Option<crate::config::ClaudeConfig>,
    pub github_token_env: String,
    pub github_app: Option<GitHubAppAuth>,
    pub http_client: reqwest::Client,
    pub source_count: usize,
    pub stats: ServerStats,
    pub recent_events: Mutex<VecDeque<RecentEvent>>,
    pub seen_deliveries: Mutex<DeliveryTracker>,
    pub workflow_store: RwLock<WorkflowStore>,
    pub task_store: TaskStore,
    pub sandbox_registry: SandboxRegistry,
}

/// Server-wide counters. Atomics — no locks needed for reads.
pub struct ServerStats {
    pub started_at: DateTime<Utc>,
    pub events_received: AtomicU64,
    pub events_matched: AtomicU64,
    pub actions_succeeded: AtomicU64,
    pub actions_failed: AtomicU64,
}

impl ServerStats {
    pub fn new() -> Self {
        Self {
            started_at: Utc::now(),
            events_received: AtomicU64::new(0),
            events_matched: AtomicU64::new(0),
            actions_succeeded: AtomicU64::new(0),
            actions_failed: AtomicU64::new(0),
        }
    }
}

/// A recently processed event for the MCP facade.
#[derive(Clone, Serialize)]
pub struct RecentEvent {
    pub event_type: String,
    pub source: String,
    pub matched_workflows: Vec<String>,
    pub timestamp: DateTime<Utc>,
}

/// Bounded set of recently seen delivery IDs.
pub(crate) struct DeliveryTracker {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl DeliveryTracker {
    pub(crate) fn new() -> Self {
        Self {
            ids: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// Returns `true` if this delivery ID was already seen.
    pub(crate) fn check_and_insert(&mut self, id: String) -> bool {
        if self.ids.contains(&id) {
            return true;
        }
        if self.order.len() >= MAX_SEEN_DELIVERIES {
            if let Some(old) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
        self.ids.insert(id.clone());
        self.order.push_back(id);
        false
    }
}

impl SharedState {
    /// Resolve the GitHub token — app auth takes precedence over static PAT.
    pub async fn github_token(&self) -> Result<String, String> {
        if let Some(ref app) = self.github_app {
            app.get_token()
                .await
                .map_err(|e| format!("GitHub App auth failed: {e}"))
        } else {
            std::env::var(&self.github_token_env)
                .map_err(|_| format!("env var '{}' not set", self.github_token_env))
        }
    }
}

/// Per-source route state.
#[derive(Clone)]
struct SourceState {
    shared: Arc<SharedState>,
    source: Arc<dyn Source>,
}

pub fn build_router(config: crate::config::Config) -> Router {
    let http_client = reqwest::Client::new();

    let github_token_env = config
        .github
        .as_ref()
        .map(|g| g.token_env.clone())
        .unwrap_or_else(|| "GITHUB_TOKEN".to_string());

    let github_app = config.github.as_ref().and_then(|g| {
        match (g.app_id.as_deref(), g.private_key_path.as_deref()) {
            (Some(app_id), Some(key_path)) => {
                match GitHubAppAuth::new(
                    app_id.to_string(),
                    key_path,
                    http_client.clone(),
                ) {
                    Ok(auth) => {
                        info!("GitHub App auth configured (app_id={app_id})");
                        Some(auth)
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to init GitHub App auth, falling back to PAT");
                        None
                    }
                }
            }
            _ => None,
        }
    });

    // Auto-discover workflow definitions from ~/.nexus/workflows/
    let mut workflow_store = WorkflowStore::new();
    workflow_store.load_from_default_dir();

    let workflow_count = workflow_store.names().len();

    let shared = Arc::new(SharedState {
        source_count: config.sources.len(),
        claude: config.claude,
        github_token_env,
        github_app,
        http_client,
        stats: ServerStats::new(),
        recent_events: Mutex::new(VecDeque::new()),
        seen_deliveries: Mutex::new(DeliveryTracker::new()),
        workflow_store: RwLock::new(workflow_store),
        task_store: TaskStore::new(),
        sandbox_registry: SandboxRegistry::new(),
    });

    let mut router = Router::new()
        .route("/health", get(health))
        .route(
            "/status",
            get({
                let shared = shared.clone();
                move || async move {
                    let wf_count = shared.workflow_store.read().await.names().len();
                    axum::Json(json!({
                        "sources": shared.source_count,
                        "workflows": wf_count,
                    }))
                }
            }),
        );

    // Mount each configured source
    for source_config in &config.sources {
        match sources::create(source_config) {
            Ok(source) => {
                let source: Arc<dyn Source> = Arc::from(source);
                let path = source_config.path.clone();

                let unsecured = source_config.verification.is_none()
                    && source_config.secret_env.is_none();
                if unsecured && source_config.type_ != "cloudevents" {
                    warn!(
                        id = %source_config.id,
                        path = %path,
                        "source has NO verification — any sender can trigger it"
                    );
                }

                info!(
                    id = %source_config.id,
                    type_ = %source_config.type_,
                    path = %path,
                    "mounting source"
                );

                let state = SourceState {
                    shared: shared.clone(),
                    source,
                };

                router = router.route(
                    &path,
                    post(handle_source)
                        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
                        .with_state(state),
                );

                // CloudEvents sources get the OPTIONS handshake
                if source_config.type_ == "cloudevents" {
                    router = router.route(
                        &path,
                        axum::routing::options(handle_events_options),
                    );
                }
            }
            Err(e) => {
                error!(
                    id = %source_config.id,
                    error = %e,
                    "failed to create source, skipping"
                );
            }
        }
    }

    info!(
        sources = config.sources.len(),
        workflows = workflow_count,
        "router built"
    );

    // Mount MCP management endpoint
    router = crate::mcp::mount(router, shared);

    router
}

async fn health() -> impl IntoResponse {
    axum::Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Generic handler for all source types.
async fn handle_source(
    State(state): State<SourceState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, StatusCode> {
    let source = &state.source;

    // Verify authenticity
    source.verify(&headers, &body).map_err(|e| {
        warn!(source = source.id(), error = %e, "verification failed");
        StatusCode::UNAUTHORIZED
    })?;

    // Replay protection
    if let Some(did) = source.delivery_id(&headers) {
        let key = format!("{}:{}", source.id(), did);
        let is_replay = state
            .shared
            .seen_deliveries
            .lock()
            .await
            .check_and_insert(key);
        if is_replay {
            warn!(source = source.id(), "duplicate delivery rejected");
            return Ok(axum::Json(json!({"duplicate": true})));
        }
    }

    // Build CloudEvent
    let event = source.build_event(&headers, body).map_err(|e| {
        warn!(source = source.id(), error = %e, "failed to build event");
        StatusCode::BAD_REQUEST
    })?;

    info!(
        source = source.id(),
        event_type = %event.ty(),
        event_id = %event.id(),
        "received event"
    );

    dispatch_event(&event, &state.shared).await
}

/// CloudEvents HTTP Webhook spec: OPTIONS validation handshake.
async fn handle_events_options(headers: HeaderMap) -> impl IntoResponse {
    let origin = headers
        .get("webhook-request-origin")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("*");

    (
        StatusCode::OK,
        [
            ("webhook-allowed-origin", origin.to_string()),
            ("webhook-allowed-rate", "*".to_string()),
            ("allow", "POST".to_string()),
        ],
    )
}

/// Dispatch pipeline — matches workflow triggers and spawns executions.
async fn dispatch_event(
    event: &Event,
    shared: &Arc<SharedState>,
) -> Result<axum::Json<serde_json::Value>, StatusCode> {
    shared.stats.events_received.fetch_add(1, Ordering::Relaxed);

    let matched = {
        let store = shared.workflow_store.read().await;
        store.match_triggers(event)
    };

    if matched.is_empty() {
        info!(event_type = %event.ty(), "no workflows matched");
        return Ok(axum::Json(json!({"matched_workflows": 0})));
    }

    shared.stats.events_matched.fetch_add(1, Ordering::Relaxed);
    let matched_names: Vec<String> = matched.iter().map(|(name, _)| name.clone()).collect();

    info!(
        event_type = %event.ty(),
        matched = matched.len(),
        "dispatching to matched workflows"
    );

    for (name, def) in &matched {
        info!(workflow = %name, "spawning workflow");
        match shared.task_store.spawn(name, def, event, shared).await {
            Ok((task_id, _output)) => {
                shared.stats.actions_succeeded.fetch_add(1, Ordering::Relaxed);
                info!(workflow = %name, task_id = %task_id, "workflow completed");
            }
            Err(e) => {
                shared.stats.actions_failed.fetch_add(1, Ordering::Relaxed);
                error!(workflow = %name, error = %e, "workflow failed");
            }
        }
    }

    // Record recent event
    let mut recent = shared.recent_events.lock().await;
    if recent.len() >= MAX_RECENT_EVENTS {
        recent.pop_front();
    }
    recent.push_back(RecentEvent {
        event_type: event.ty().to_string(),
        source: event.source().to_string(),
        matched_workflows: matched_names.clone(),
        timestamp: event.time().copied().unwrap_or_else(Utc::now),
    });

    Ok(axum::Json(json!({"matched_workflows": matched_names.len()})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_tracker_detects_replay() {
        let mut tracker = DeliveryTracker::new();
        assert!(!tracker.check_and_insert("aaa".to_string()));
        assert!(tracker.check_and_insert("aaa".to_string())); // replay
        assert!(!tracker.check_and_insert("bbb".to_string()));
    }

    #[test]
    fn delivery_tracker_evicts_oldest() {
        let mut tracker = DeliveryTracker::new();
        for i in 0..MAX_SEEN_DELIVERIES {
            assert!(!tracker.check_and_insert(format!("id-{i}")));
        }
        assert!(tracker.check_and_insert("id-0".to_string()));

        assert!(!tracker.check_and_insert("new-id".to_string()));
        assert!(!tracker.check_and_insert("id-0".to_string()));
        assert!(tracker.check_and_insert("id-2".to_string()));
    }
}
