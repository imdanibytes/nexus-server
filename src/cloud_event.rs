use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudEvent {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub source: String,
    #[serde(default)]
    pub subject: Option<String>,
    pub time: DateTime<Utc>,
    pub data: serde_json::Value,
}

impl CloudEvent {
    pub fn new(type_: String, source: String, data: serde_json::Value) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            type_,
            source,
            subject: None,
            time: Utc::now(),
            data,
        }
    }
}
