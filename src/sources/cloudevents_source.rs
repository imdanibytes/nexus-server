use axum::body::Bytes;
use axum::http::HeaderMap;
use chrono::Utc;
use cloudevents::{Event, EventBuilder, EventBuilderV10};
use tracing::warn;

use crate::config::SourceConfig;

use super::{Source, SourceError};

pub struct CloudEventsSource {
    id: String,
    secret: Option<String>,
}

impl CloudEventsSource {
    pub fn new(config: &SourceConfig) -> Result<Self, SourceError> {
        let secret = config
            .secret_env
            .as_deref()
            .and_then(|env_name| {
                std::env::var(env_name)
                    .map_err(|_| {
                        warn!(
                            env = env_name,
                            "cloudevents source secret env var not set — endpoint will reject all requests"
                        );
                    })
                    .ok()
            });

        Ok(Self {
            id: config.id.clone(),
            secret,
        })
    }
}

impl Source for CloudEventsSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn verify(&self, headers: &HeaderMap, _body: &[u8]) -> Result<(), SourceError> {
        let Some(ref expected) = self.secret else {
            return Err(SourceError::Config(
                "no secret configured for cloudevents source".into(),
            ));
        };

        let provided = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| {
                SourceError::Verification("missing or malformed Authorization header".into())
            })?;

        if provided != expected {
            return Err(SourceError::Verification("invalid bearer token".into()));
        }

        Ok(())
    }

    fn delivery_id(&self, _headers: &HeaderMap) -> Option<String> {
        None
    }

    fn build_event(&self, headers: &HeaderMap, body: Bytes) -> Result<Event, SourceError> {
        let content_type = headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if content_type.starts_with("application/cloudevents+json") {
            // Structured mode: body is the full CloudEvent JSON
            serde_json::from_slice::<Event>(&body)
                .map_err(|e| SourceError::InvalidBody(format!("invalid CloudEvent JSON: {e}")))
        } else {
            // Binary mode: ce-* headers + body as data
            build_from_binary(headers, body)
        }
    }
}

/// Parse a CloudEvent from binary content mode (ce-* headers + body).
fn build_from_binary(headers: &HeaderMap, body: Bytes) -> Result<Event, SourceError> {
    let get_ce = |name: &str| -> Result<String, SourceError> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| SourceError::MissingHeader(name.to_string()))
    };

    let id = get_ce("ce-id")?;
    let type_ = get_ce("ce-type")?;
    let source = get_ce("ce-source")?;

    let time = headers
        .get("ce-time")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<chrono::DateTime<Utc>>().ok())
        .unwrap_or_else(Utc::now);

    let data: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| SourceError::InvalidBody(e.to_string()))?;

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json");

    EventBuilderV10::new()
        .id(id)
        .ty(type_)
        .source(source)
        .time(time)
        .data(content_type, data)
        .build()
        .map_err(|e| SourceError::InvalidBody(format!("failed to build CloudEvent: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudevents::event::AttributesReader;
    use serde_json::json;

    fn make_source() -> CloudEventsSource {
        CloudEventsSource {
            id: "test-ce".to_string(),
            secret: Some("test-secret".to_string()),
        }
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    #[test]
    fn verify_valid_bearer() {
        let source = make_source();
        let headers = bearer_headers("test-secret");
        assert!(source.verify(&headers, b"").is_ok());
    }

    #[test]
    fn verify_invalid_bearer() {
        let source = make_source();
        let headers = bearer_headers("wrong-secret");
        assert!(source.verify(&headers, b"").is_err());
    }

    #[test]
    fn verify_missing_header() {
        let source = make_source();
        assert!(source.verify(&HeaderMap::new(), b"").is_err());
    }

    #[test]
    fn parse_structured_mode() {
        let source = make_source();
        let event_json = json!({
            "specversion": "1.0",
            "id": "test-id",
            "type": "com.example.test",
            "source": "test-source",
            "data": {"key": "value"}
        });

        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            "application/cloudevents+json".parse().unwrap(),
        );

        let body = Bytes::from(serde_json::to_vec(&event_json).unwrap());
        let event = source.build_event(&headers, body).unwrap();

        assert_eq!(event.ty(), "com.example.test");
        assert_eq!(event.source(), "test-source");
        assert_eq!(event.id(), "test-id");
    }

    #[test]
    fn parse_binary_mode() {
        let source = make_source();
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("ce-id", "bin-id".parse().unwrap());
        headers.insert("ce-type", "com.example.binary".parse().unwrap());
        headers.insert("ce-source", "binary-source".parse().unwrap());
        headers.insert("ce-specversion", "1.0".parse().unwrap());

        let body = Bytes::from(serde_json::to_vec(&json!({"data": 42})).unwrap());
        let event = source.build_event(&headers, body).unwrap();

        assert_eq!(event.ty(), "com.example.binary");
        assert_eq!(event.source(), "binary-source");
        assert_eq!(event.id(), "bin-id");
    }

    #[test]
    fn binary_mode_missing_required_header() {
        let source = make_source();
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("ce-id", "bin-id".parse().unwrap());
        // Missing ce-type and ce-source

        let body = Bytes::from(b"{}".to_vec());
        assert!(source.build_event(&headers, body).is_err());
    }

    #[test]
    fn no_delivery_id() {
        let source = make_source();
        assert_eq!(source.delivery_id(&HeaderMap::new()), None);
    }
}
