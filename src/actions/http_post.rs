use crate::config::RuleConfig;
use crate::routing::resolve_template;
use cloudevents::Event;
use tracing::{error, info};

use super::{Action, ActionError};

pub struct HttpPostAction {
    client: reqwest::Client,
}

impl HttpPostAction {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Action for HttpPostAction {
    fn action_type(&self) -> &str {
        "http_post"
    }

    fn execute(
        &self,
        rule: &RuleConfig,
        event: &Event,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), ActionError>> + Send + '_>>
    {
        let url = rule.url.clone();
        let rule_name = rule.name.clone();
        let body = rule
            .body_template
            .as_deref()
            .map(|t| resolve_template(t, event))
            .unwrap_or_default();
        Box::pin(async move {
            let url = url
                .as_deref()
                .ok_or_else(|| ActionError::Config(format!("url missing on rule '{rule_name}'")))?;
            call(url, &body, &self.client).await?;
            Ok(())
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HttpPostError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("endpoint returned {status}: {body}")]
    BadStatus { status: u16, body: String },
}

impl From<HttpPostError> for ActionError {
    fn from(e: HttpPostError) -> Self {
        ActionError::Execute(e.to_string())
    }
}

pub async fn call(
    url: &str,
    body: &str,
    client: &reqwest::Client,
) -> Result<(), HttpPostError> {
    info!(url, body_len = body.len(), "sending HTTP POST");

    let resp = client
        .post(url)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await?;

    let status = resp.status().as_u16();

    if !(200..300).contains(&status) {
        let resp_body = resp.text().await.unwrap_or_default();
        error!(status, body = %resp_body, url, "HTTP POST failed");
        return Err(HttpPostError::BadStatus {
            status,
            body: resp_body,
        });
    }

    info!(status, url, "HTTP POST succeeded");
    Ok(())
}
