use serde::{Deserialize, Serialize};

/// Complete Gemini coding session with metadata and message history
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiSession {
    pub session_id: String,
    pub project_hash: String,
    pub start_time: String,
    pub last_updated: String,
    pub messages: Vec<GeminiMessage>,
}

/// Single message within a Gemini session
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiMessage {
    pub id: String,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub message_type: String,
    pub content: String,
    #[serde(default)]
    pub thoughts: Vec<GeminiThought>,
    pub tokens: Option<GeminiTokens>,
    pub model: Option<String>,
}

/// AI reasoning step captured during Gemini's thought process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiThought {
    pub subject: String,
    pub description: String,
    pub timestamp: String,
}

/// Token usage breakdown for a single Gemini message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiTokens {
    pub input: i64,
    pub output: i64,
    pub cached: i64,
    pub thoughts: i64,
    pub tool: i64,
    pub total: i64,
}
