use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use tracing::{error, info, warn};

use crate::actions;
use crate::cloud_event::CloudEvent;
use crate::config::{ClaudeConfig, RuleConfig, VerificationConfig, WebhookConfig};
use crate::routing::match_rules;
use crate::verification;

/// Shared state across all routes.
struct SharedState {
    rules: Vec<RuleConfig>,
    claude: Option<ClaudeConfig>,
    github_token_env: String,
    http_client: reqwest::Client,
    webhook_count: usize,
}

/// Per-webhook route state — each webhook endpoint gets its own copy.
#[derive(Clone)]
struct WebhookState {
    shared: Arc<SharedState>,
    webhook: Arc<WebhookConfig>,
}

pub fn build_router(config: crate::config::Config) -> Router {
    let github_token_env = config
        .github
        .as_ref()
        .map(|g| g.token_env.clone())
        .unwrap_or_else(|| "GITHUB_TOKEN".to_string());

    let shared = Arc::new(SharedState {
        webhook_count: config.webhooks.len(),
        rules: config.rules,
        claude: config.claude,
        github_token_env,
        http_client: reqwest::Client::new(),
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
        info!(id = %wh.id, path = %path, "mounting webhook endpoint");

        let wh_state = WebhookState {
            shared: shared.clone(),
            webhook: Arc::new(wh),
        };

        router = router.route(
            &path,
            post(handle_webhook).with_state(wh_state),
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
            &state.shared.github_token_env,
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
