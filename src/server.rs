use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
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

use crate::actions::{self, ActionRegistry};
use crate::config::RuleConfig;
use crate::github_auth::GitHubAppAuth;
use crate::routing::match_rules;
use crate::sources::{self, Source};

/// Max webhook request body size (256 KB).
const MAX_BODY_SIZE: usize = 256 * 1024;

/// Max number of delivery IDs to track for replay protection.
const MAX_SEEN_DELIVERIES: usize = 10_000;

/// Max number of recent events to keep.
const MAX_RECENT_EVENTS: usize = 100;

/// Shared state across all routes.
pub struct SharedState {
    pub rules: RwLock<Vec<RuleConfig>>,
    pub claude: Option<crate::config::ClaudeConfig>,
    pub github_token_env: String,
    pub github_app: Option<GitHubAppAuth>,
    pub http_client: reqwest::Client,
    pub source_count: usize,
    pub config_path: PathBuf,
    pub stats: ServerStats,
    pub recent_events: Mutex<VecDeque<RecentEvent>>,
    pub seen_deliveries: Mutex<DeliveryTracker>,
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
    pub matched_rules: Vec<String>,
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
    registry: Arc<ActionRegistry>,
}

pub fn build_router(config: crate::config::Config, config_path: PathBuf) -> Router {
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

    let shared = Arc::new(SharedState {
        source_count: config.sources.len(),
        rules: RwLock::new(config.rules),
        claude: config.claude,
        github_token_env,
        github_app,
        http_client,
        config_path,
        stats: ServerStats::new(),
        recent_events: Mutex::new(VecDeque::new()),
        seen_deliveries: Mutex::new(DeliveryTracker::new()),
    });

    // Build action registry after shared state — AgentAction needs Arc<SharedState>
    let registry = Arc::new(ActionRegistry::new(&shared));

    let mut router = Router::new()
        .route("/health", get(health))
        .route(
            "/status",
            get({
                let shared = shared.clone();
                move || async move {
                    let rules_count = shared.rules.read().await.len();
                    axum::Json(json!({
                        "sources": shared.source_count,
                        "rules": rules_count,
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
                    registry: registry.clone(),
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

    dispatch_event(&event, &state.shared, &state.registry).await
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

/// Shared dispatch pipeline — matches rules and executes actions.
async fn dispatch_event(
    event: &Event,
    shared: &Arc<SharedState>,
    registry: &ActionRegistry,
) -> Result<axum::Json<serde_json::Value>, StatusCode> {
    shared.stats.events_received.fetch_add(1, Ordering::Relaxed);

    let rules = shared.rules.read().await;
    let matched = match_rules(event, &rules);
    if matched.is_empty() {
        info!(event_type = %event.ty(), "no rules matched");
        return Ok(axum::Json(json!({"matched_rules": 0})));
    }

    shared.stats.events_matched.fetch_add(1, Ordering::Relaxed);
    let matched_names: Vec<String> = matched.iter().map(|r| r.name.clone()).collect();

    info!(
        event_type = %event.ty(),
        matched = matched.len(),
        "dispatching matched rules"
    );

    for rule in &matched {
        info!(rule = %rule.name, action = %rule.action, "executing rule");
        if let Err(e) = actions::dispatch(rule, event, registry).await
        {
            shared.stats.actions_failed.fetch_add(1, Ordering::Relaxed);
            error!(rule = %rule.name, error = %e, "action failed");
        } else {
            shared
                .stats
                .actions_succeeded
                .fetch_add(1, Ordering::Relaxed);
        }
    }
    drop(rules);

    // Record recent event
    let mut recent = shared.recent_events.lock().await;
    if recent.len() >= MAX_RECENT_EVENTS {
        recent.pop_front();
    }
    recent.push_back(RecentEvent {
        event_type: event.ty().to_string(),
        source: event.source().to_string(),
        matched_rules: matched_names.clone(),
        timestamp: event.time().copied().unwrap_or_else(Utc::now),
    });

    Ok(axum::Json(json!({"matched_rules": matched_names.len()})))
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
