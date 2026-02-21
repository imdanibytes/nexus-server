use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::actions;
use crate::cloud_event::CloudEvent;
use crate::config::{ClaudeConfig, RuleConfig, VerificationConfig, WebhookConfig};
use crate::github_auth::GitHubAppAuth;
use crate::routing::match_rules;
use crate::verification;

/// Max webhook request body size (256 KB).
const MAX_BODY_SIZE: usize = 256 * 1024;

/// Max number of delivery IDs to track for replay protection.
const MAX_SEEN_DELIVERIES: usize = 10_000;

/// Shared state across all routes.
pub struct SharedState {
    pub rules: Vec<RuleConfig>,
    pub claude: Option<ClaudeConfig>,
    pub github_token_env: String,
    pub github_app: Option<GitHubAppAuth>,
    pub http_client: reqwest::Client,
    pub webhook_count: usize,
    /// Recently seen delivery IDs for replay protection.
    pub seen_deliveries: Mutex<DeliveryTracker>,
}

/// Bounded set of recently seen delivery IDs.
pub struct DeliveryTracker {
    ids: HashSet<String>,
    order: Vec<String>,
}

impl DeliveryTracker {
    pub fn new() -> Self {
        Self {
            ids: HashSet::new(),
            order: Vec::new(),
        }
    }

    /// Returns `true` if this delivery ID was already seen.
    pub fn check_and_insert(&mut self, id: String) -> bool {
        if self.ids.contains(&id) {
            return true;
        }
        // Evict oldest if at capacity
        if self.order.len() >= MAX_SEEN_DELIVERIES {
            if let Some(old) = self.order.first().cloned() {
                self.ids.remove(&old);
                self.order.remove(0);
            }
        }
        self.ids.insert(id.clone());
        self.order.push(id);
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

/// Per-webhook route state — each webhook endpoint gets its own copy.
#[derive(Clone)]
struct WebhookState {
    shared: Arc<SharedState>,
    webhook: Arc<WebhookConfig>,
}

pub fn build_router(config: crate::config::Config) -> Router {
    let http_client = reqwest::Client::new();

    let github_token_env = config
        .github
        .as_ref()
        .map(|g| g.token_env.clone())
        .unwrap_or_else(|| "GITHUB_TOKEN".to_string());

    let github_app = config
        .github
        .as_ref()
        .and_then(|g| {
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
        webhook_count: config.webhooks.len(),
        rules: config.rules,
        claude: config.claude,
        github_token_env,
        github_app,
        http_client,
        seen_deliveries: Mutex::new(DeliveryTracker::new()),
    });

    let mut router = Router::new()
        .route("/health", get(health))
        .route(
            "/status",
            get({
                let shared = shared.clone();
                move || async move {
                    axum::Json(json!({
                        "webhooks": shared.webhook_count,
                        "rules": shared.rules.len(),
                    }))
                }
            }),
        );

    for wh in config.webhooks {
        let path = wh.path.clone();

        if wh.verification.is_none() {
            warn!(
                id = %wh.id,
                path = %path,
                "webhook has NO signature verification — any source can trigger it"
            );
        }

        info!(id = %wh.id, path = %path, "mounting webhook endpoint");

        let wh_state = WebhookState {
            shared: shared.clone(),
            webhook: Arc::new(wh),
        };

        router = router.route(
            &path,
            post(handle_webhook)
                .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
                .with_state(wh_state),
        );
    }

    router
}

async fn health() -> impl IntoResponse {
    axum::Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn handle_webhook(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, StatusCode> {
    let webhook = &state.webhook;

    // Verify signature if configured
    if let Some(ref verification) = webhook.verification {
        verify_request(verification, &headers, &body)?;
    }

    // Replay protection via X-GitHub-Delivery header
    if let Some(delivery_id) = headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
    {
        let is_replay = state
            .shared
            .seen_deliveries
            .lock()
            .await
            .check_and_insert(delivery_id.to_string());
        if is_replay {
            warn!(delivery_id, "duplicate delivery rejected");
            return Err(StatusCode::OK);
        }
    }

    // Parse body
    let data: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| {
            warn!(error = %e, "invalid JSON body");
            StatusCode::BAD_REQUEST
        })?;

    // Build CloudEvent
    let event = build_cloud_event(webhook, &headers, data);
    info!(
        event_type = %event.type_,
        event_id = %event.id,
        webhook = %webhook.id,
        "received webhook event"
    );

    // Match rules and dispatch
    let matched = match_rules(&event, &state.shared.rules);
    if matched.is_empty() {
        info!(event_type = %event.type_, "no rules matched");
        return Ok(axum::Json(json!({"matched_rules": 0})));
    }

    info!(
        event_type = %event.type_,
        matched = matched.len(),
        "dispatching matched rules"
    );

    let claude_config = state.shared.claude.as_ref();
    for rule in &matched {
        info!(rule = %rule.name, action = %rule.action, "executing rule");
        if let Err(e) = actions::dispatch(
            rule,
            &event,
            claude_config,
            &state.shared,
            &state.shared.http_client,
        )
        .await
        {
            error!(rule = %rule.name, error = %e, "action failed");
        }
    }

    Ok(axum::Json(json!({"matched_rules": matched.len()})))
}

fn verify_request(
    verification: &VerificationConfig,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), StatusCode> {
    match verification.method.as_str() {
        "github-hmac" => {
            let secret_env = verification
                .secret_env
                .as_deref()
                .unwrap_or("GITHUB_WEBHOOK_SECRET");
            let secret = std::env::var(secret_env).map_err(|_| {
                error!(env = secret_env, "webhook secret env var not set");
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

            let sig_header = headers
                .get("x-hub-signature-256")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    warn!("missing X-Hub-Signature-256 header");
                    StatusCode::UNAUTHORIZED
                })?;

            verification::verify_github_hmac(
                secret.as_bytes(),
                sig_header,
                body,
            )
            .map_err(|e| {
                warn!(error = %e, "signature verification failed");
                StatusCode::UNAUTHORIZED
            })
        }
        "none" => Ok(()),
        other => {
            warn!(method = other, "unknown verification method");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn build_cloud_event(
    webhook: &WebhookConfig,
    headers: &HeaderMap,
    data: serde_json::Value,
) -> CloudEvent {
    // For GitHub, the event type comes from X-GitHub-Event header
    let github_event = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    // GitHub sends action in the payload (opened, closed, etc.)
    let action = data
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let type_ = if action.is_empty() {
        format!("{}.{}", webhook.event_type_prefix, github_event)
    } else {
        format!(
            "{}.{}.{}",
            webhook.event_type_prefix, github_event, action
        )
    };

    let source = data
        .get("repository")
        .and_then(|r| r.get("full_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    CloudEvent::new(type_, source, data)
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
        // Fill to capacity
        for i in 0..MAX_SEEN_DELIVERIES {
            assert!(!tracker.check_and_insert(format!("id-{i}")));
        }
        // Oldest should still be tracked
        assert!(tracker.check_and_insert("id-0".to_string()));

        // Insert one more to trigger eviction of id-0
        assert!(!tracker.check_and_insert("new-id".to_string()));

        // id-0 was evicted, no longer detected as replay
        assert!(!tracker.check_and_insert("id-0".to_string()));

        // Recent IDs should still be tracked (id-2 hasn't been evicted)
        assert!(tracker.check_and_insert("id-2".to_string()));
    }
}
