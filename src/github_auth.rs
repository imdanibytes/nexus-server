use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("failed to read private key: {0}")]
    KeyRead(std::io::Error),
    #[error("invalid private key: {0}")]
    KeyParse(jsonwebtoken::errors::Error),
    #[error("JWT encoding failed: {0}")]
    JwtEncode(jsonwebtoken::errors::Error),
    #[error("failed to get installation token: {0}")]
    Request(reqwest::Error),
    #[error("GitHub API error {status}: {body}")]
    ApiError { status: u16, body: String },
    #[error("no installation found for app")]
    NoInstallation,
}

#[derive(Serialize)]
struct JwtClaims {
    iat: i64,
    exp: i64,
    iss: String,
}

#[derive(Deserialize)]
struct InstallationToken {
    token: String,
}

/// Manages GitHub App authentication: JWT → installation token.
pub struct GitHubAppAuth {
    app_id: String,
    encoding_key: EncodingKey,
    client: reqwest::Client,
    /// Cached installation token + expiry
    cached_token: Arc<RwLock<Option<CachedToken>>>,
}

struct CachedToken {
    token: String,
    expires_at: i64,
}

impl GitHubAppAuth {
    pub fn new(
        app_id: String,
        private_key_path: &str,
        client: reqwest::Client,
    ) -> Result<Self, AuthError> {
        let key_pem =
            std::fs::read(private_key_path).map_err(AuthError::KeyRead)?;
        let encoding_key = EncodingKey::from_rsa_pem(&key_pem)
            .map_err(AuthError::KeyParse)?;

        Ok(Self {
            app_id,
            encoding_key,
            client,
            cached_token: Arc::new(RwLock::new(None)),
        })
    }

    /// Get a valid installation token, refreshing if needed.
    pub async fn get_token(&self) -> Result<String, AuthError> {
        // Check cache
        {
            let cache = self.cached_token.read().await;
            if let Some(ref cached) = *cache {
                let now = chrono::Utc::now().timestamp();
                // Refresh 60s before expiry
                if cached.expires_at - 60 > now {
                    return Ok(cached.token.clone());
                }
            }
        }

        // Refresh token
        let token = self.refresh_token().await?;
        Ok(token)
    }

    fn generate_jwt(&self) -> Result<String, AuthError> {
        let now = chrono::Utc::now().timestamp();
        let claims = JwtClaims {
            iat: now - 60, // clock skew allowance
            exp: now + (10 * 60), // 10 minutes max
            iss: self.app_id.clone(),
        };

        encode(
            &Header::new(Algorithm::RS256),
            &claims,
            &self.encoding_key,
        )
        .map_err(AuthError::JwtEncode)
    }

    async fn find_installation_id(&self, jwt: &str) -> Result<u64, AuthError> {
        let resp = self
            .client
            .get("https://api.github.com/app/installations")
            .header("authorization", format!("Bearer {jwt}"))
            .header("user-agent", "nexus-server")
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(AuthError::Request)?;

        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(AuthError::Request)?;

        if status != 200 {
            return Err(AuthError::ApiError { status, body });
        }

        let installations: Vec<Value> =
            serde_json::from_str(&body).unwrap_or_default();

        installations
            .first()
            .and_then(|i| i["id"].as_u64())
            .ok_or(AuthError::NoInstallation)
    }

    async fn refresh_token(&self) -> Result<String, AuthError> {
        let jwt = self.generate_jwt()?;

        let installation_id = self.find_installation_id(&jwt).await?;
        info!(installation_id, "found GitHub App installation");

        let resp = self
            .client
            .post(format!(
                "https://api.github.com/app/installations/{installation_id}/access_tokens"
            ))
            .header("authorization", format!("Bearer {jwt}"))
            .header("user-agent", "nexus-server")
            .header("accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(AuthError::Request)?;

        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(AuthError::Request)?;

        if status != 201 {
            return Err(AuthError::ApiError { status, body });
        }

        let token_resp: InstallationToken =
            serde_json::from_str(&body).unwrap();

        info!("GitHub App installation token acquired");

        // Cache for 55 minutes (tokens last 60)
        let expires_at = chrono::Utc::now().timestamp() + (55 * 60);
        let mut cache = self.cached_token.write().await;
        *cache = Some(CachedToken {
            token: token_resp.token.clone(),
            expires_at,
        });

        Ok(token_resp.token)
    }
}
