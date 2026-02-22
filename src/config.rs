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
    pub sources: Vec<SourceConfig>,
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

#[derive(Debug, Deserialize, Clone)]
pub struct SourceConfig {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub path: String,
    #[serde(default)]
    pub event_type_prefix: Option<String>,
    #[serde(default)]
    pub verification: Option<VerificationConfig>,
    #[serde(default)]
    pub secret_env: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
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
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub body_template: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub target_secret_env: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GithubConfig {
    #[serde(default = "default_github_token_env")]
    pub token_env: String,
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

[[sources]]
id = "gh"
type = "github"
path = "/hook/github"
event_type_prefix = "com.github"
verification = { method = "github-hmac", secret_env = "GH_SECRET" }

[[sources]]
id = "events"
type = "cloudevents"
path = "/events"
secret_env = "EVENTS_TOKEN"

[[rules]]
name = "Test rule"
filter = { type_prefix = "com.github.issues" }
action = "claude"
prompt = "Hello {{event.data.issue.title}}"

[[rules]]
name = "Forward to dev"
filter = { type_prefix = "com.github" }
action = "forward"
target = "http://localhost:8091/events"
target_secret_env = "DEV_EVENTS_TOKEN"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.server.bind, "127.0.0.1:9000");
        assert_eq!(config.sources.len(), 2);
        assert_eq!(config.sources[0].id, "gh");
        assert_eq!(config.sources[0].type_, "github");
        assert_eq!(config.sources[1].id, "events");
        assert_eq!(config.sources[1].type_, "cloudevents");
        assert_eq!(config.rules.len(), 2);
        assert_eq!(config.rules[0].action, "claude");
        assert_eq!(config.rules[1].action, "forward");
        assert_eq!(
            config.rules[1].target.as_deref(),
            Some("http://localhost:8091/events")
        );
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
        assert!(config.sources.is_empty());
        assert!(config.rules.is_empty());
    }
}
