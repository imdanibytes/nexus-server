use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub tunnel: Option<TunnelConfig>,
    #[serde(default)]
    pub claude: Option<ClaudeConfig>,
    #[serde(default)]
    pub github: Option<GithubConfig>,
    #[serde(default)]
    pub webhooks: Vec<WebhookConfig>,
    #[serde(default)]
    pub rules: Vec<RuleConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
}

fn default_bind() -> String {
    "0.0.0.0:8090".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct TunnelConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional fixed ngrok domain (e.g. "my-app.ngrok-free.app")
    #[serde(default)]
    pub domain: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
pub struct ClaudeConfig {
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_api_key_env() -> String {
    "ANTHROPIC_API_KEY".to_string()
}

fn default_model() -> String {
    "claude-sonnet-4-20250514".to_string()
}

fn default_max_tokens() -> u32 {
    4096
}

#[derive(Debug, Deserialize)]
pub struct WebhookConfig {
    pub id: String,
    pub path: String,
    pub event_type_prefix: String,
    #[serde(default)]
    pub verification: Option<VerificationConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct VerificationConfig {
    pub method: String,
    #[serde(default)]
    pub secret_env: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RuleConfig {
    pub name: String,
    pub filter: FilterConfig,
    pub action: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub body_template: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GithubConfig {
    /// Static token (PAT) — used if no app config is present
    #[serde(default = "default_github_token_env")]
    pub token_env: String,
    /// GitHub App auth — takes precedence over token_env
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub private_key_path: Option<String>,
}

fn default_github_token_env() -> String {
    "GITHUB_TOKEN".to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct FilterConfig {
    pub type_prefix: String,
}

impl Config {
    pub fn load(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let toml = r#"
[server]
bind = "127.0.0.1:9000"

[claude]
api_key_env = "MY_KEY"
model = "claude-sonnet-4-20250514"
max_tokens = 2048

[[webhooks]]
id = "gh"
path = "/hook/github"
event_type_prefix = "com.github"
verification = { method = "github-hmac", secret_env = "GH_SECRET" }

[[rules]]
name = "Test rule"
filter = { type_prefix = "com.github.issues" }
action = "claude"
prompt = "Hello {{event.data.issue.title}}"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.server.bind, "127.0.0.1:9000");
        assert_eq!(config.webhooks.len(), 1);
        assert_eq!(config.webhooks[0].id, "gh");
        assert_eq!(config.rules.len(), 1);
        assert_eq!(config.rules[0].action, "claude");
        let claude = config.claude.unwrap();
        assert_eq!(claude.max_tokens, 2048);
    }

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
[server]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.server.bind, "0.0.0.0:8090");
        assert!(config.claude.is_none());
        assert!(config.webhooks.is_empty());
        assert!(config.rules.is_empty());
    }
}
