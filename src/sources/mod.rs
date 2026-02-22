pub mod cloudevents_source;
pub mod github;

use axum::body::Bytes;
use axum::http::HeaderMap;
use cloudevents::Event;

use crate::config::SourceConfig;

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("verification failed: {0}")]
    Verification(String),
    #[error("invalid body: {0}")]
    InvalidBody(String),
    #[error("missing required header: {0}")]
    MissingHeader(String),
    #[error("configuration error: {0}")]
    Config(String),
}

/// A source produces CloudEvents from incoming HTTP requests.
pub trait Source: Send + Sync {
    /// Source identifier (matches config id).
    fn id(&self) -> &str;

    /// Verify request authenticity (HMAC, Bearer, etc.).
    fn verify(&self, headers: &HeaderMap, body: &[u8]) -> Result<(), SourceError>;

    /// Extract a replay-protection key, if this source supports it.
    fn delivery_id(&self, headers: &HeaderMap) -> Option<String>;

    /// Parse the HTTP request into a CloudEvent.
    fn build_event(&self, headers: &HeaderMap, body: Bytes) -> Result<Event, SourceError>;
}

/// Create a source from config.
pub fn create(config: &SourceConfig) -> Result<Box<dyn Source>, SourceError> {
    match config.type_.as_str() {
        "github" => Ok(Box::new(github::GitHubSource::new(config)?)),
        "cloudevents" => Ok(Box::new(
            cloudevents_source::CloudEventsSource::new(config)?,
        )),
        other => Err(SourceError::Config(format!(
            "unknown source type '{other}'"
        ))),
    }
}
