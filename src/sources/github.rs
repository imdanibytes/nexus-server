use axum::body::Bytes;
use axum::http::HeaderMap;
use chrono::Utc;
use cloudevents::{Event, EventBuilder, EventBuilderV10};
use tracing::{error, warn};

use crate::config::SourceConfig;
use crate::verification;

use super::{Source, SourceError};

pub struct GitHubSource {
    id: String,
    event_type_prefix: String,
    secret_env: Option<String>,
}

impl GitHubSource {
    pub fn new(config: &SourceConfig) -> Result<Self, SourceError> {
        let prefix = config.event_type_prefix.as_deref().ok_or_else(|| {
            SourceError::Config(format!(
                "source '{}': github type requires event_type_prefix",
                config.id
            ))
        })?;

        let secret_env = config
            .verification
            .as_ref()
            .and_then(|v| v.secret_env.clone());

        Ok(Self {
            id: config.id.clone(),
            event_type_prefix: prefix.to_string(),
            secret_env,
        })
    }
}

impl Source for GitHubSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), SourceError> {
        let Some(ref secret_env) = self.secret_env else {
            return Ok(());
        };

        let secret = std::env::var(secret_env).map_err(|_| {
            error!(env = %secret_env, "webhook secret env var not set");
            SourceError::Config(format!("env var '{secret_env}' not set"))
        })?;

        let sig_header = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                warn!("missing X-Hub-Signature-256 header");
                SourceError::Verification("missing X-Hub-Signature-256 header".into())
            })?;

        verification::verify_github_hmac(secret.as_bytes(), sig_header, body).map_err(|e| {
            warn!(error = %e, "signature verification failed");
            SourceError::Verification(e.to_string())
        })
    }

    fn delivery_id(&self, headers: &HeaderMap) -> Option<String> {
        headers
            .get("x-github-delivery")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    }

    fn build_event(&self, headers: &HeaderMap, body: Bytes) -> Result<Event, SourceError> {
        let data: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|e| SourceError::InvalidBody(e.to_string()))?;

        let github_event = headers
            .get("x-github-event")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown");

        let action = data
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let type_ = if action.is_empty() {
            format!("{}.{}", self.event_type_prefix, github_event)
        } else {
            format!("{}.{}.{}", self.event_type_prefix, github_event, action)
        };

        let source = data
            .get("repository")
            .and_then(|r| r.get("full_name"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let event = EventBuilderV10::new()
            .id(uuid::Uuid::new_v4().to_string())
            .ty(type_)
            .source(source)
            .time(Utc::now())
            .data("application/json", data)
            .build()
            .expect("required CloudEvent fields are always set");

        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudevents::event::AttributesReader;
    use serde_json::json;

    fn make_source() -> GitHubSource {
        GitHubSource {
            id: "test-gh".to_string(),
            event_type_prefix: "com.github".to_string(),
            secret_env: None,
        }
    }

    fn github_headers(event: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-github-event", event.parse().unwrap());
        headers.insert(
            "x-github-delivery",
            "abc-123".parse().unwrap(),
        );
        headers
    }

    #[test]
    fn builds_event_with_action() {
        let source = make_source();
        let headers = github_headers("issues");
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "action": "opened",
                "repository": { "full_name": "org/repo" },
                "issue": { "title": "Bug" }
            }))
            .unwrap(),
        );

        let event = source.build_event(&headers, body).unwrap();
        assert_eq!(event.ty(), "com.github.issues.opened");
        assert_eq!(event.source(), "org/repo");
    }

    #[test]
    fn builds_event_without_action() {
        let source = make_source();
        let headers = github_headers("push");
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "repository": { "full_name": "org/repo" },
                "ref": "refs/heads/main"
            }))
            .unwrap(),
        );

        let event = source.build_event(&headers, body).unwrap();
        assert_eq!(event.ty(), "com.github.push");
    }

    #[test]
    fn delivery_id_from_header() {
        let source = make_source();
        let headers = github_headers("push");
        assert_eq!(
            source.delivery_id(&headers),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn delivery_id_missing() {
        let source = make_source();
        let headers = HeaderMap::new();
        assert_eq!(source.delivery_id(&headers), None);
    }

    #[test]
    fn no_verification_when_unconfigured() {
        let source = make_source();
        let headers = HeaderMap::new();
        assert!(source.verify(&headers, b"anything").is_ok());
    }
}
