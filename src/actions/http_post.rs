use tracing::{error, info};

#[derive(Debug, thiserror::Error)]
pub enum HttpPostError {
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("endpoint returned {status}: {body}")]
    BadStatus { status: u16, body: String },
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
