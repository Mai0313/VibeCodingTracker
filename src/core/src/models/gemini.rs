//! Serde models for the Google Gemini CLI chat-log JSONL format (`~/.gemini/tmp/<project>/chats/session-*.jsonl`).
//!
//! Each file opens with a [`GeminiSession`] metadata line followed by per-event JSON records.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Opening session-metadata line of a Gemini CLI chat log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiSession {
    /// Session identifier.
    pub session_id: String,
    /// Hash identifying the project the session belongs to.
    #[serde(default)]
    pub project_hash: String,
    /// ISO-8601 session start time.
    #[serde(default)]
    pub start_time: String,
    /// ISO-8601 timestamp of the last update to the session.
    #[serde(default)]
    pub last_updated: String,
    /// Session kind (for example `"main"`), when present.
    #[serde(default)]
    pub kind: Option<String>,
}

/// Wire shape of a `type == "gemini"` chat event.
///
/// All fields default when absent so partially populated events still deserialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiMessage {
    /// Message identifier.
    #[serde(default)]
    pub id: String,
    /// ISO-8601 timestamp string for the message.
    #[serde(default)]
    pub timestamp: String,
    /// Message type discriminator (e.g. `gemini`, `user`, `info`).
    #[serde(rename = "type", default)]
    pub message_type: String,
    /// Flattened message text (string or joined `{text}` array blocks).
    #[serde(default, deserialize_with = "deserialize_content")]
    pub content: String,
    /// Reasoning steps recorded for the turn.
    #[serde(default)]
    pub thoughts: Vec<GeminiThought>,
    /// Token-usage breakdown for the turn, when reported.
    pub tokens: Option<GeminiTokens>,
    /// Model that produced the turn, when reported.
    pub model: Option<String>,
    /// Raw tool-call entries for the turn.
    #[serde(default)]
    pub tool_calls: Vec<Value>,
}

/// Flattens a message `content` field (string, `{text}` array, `null`, or other JSON value) into one `String`.
fn deserialize_content<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::String(s) => Ok(s),
        Value::Array(arr) => {
            let texts: Vec<&str> = arr
                .iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect();
            Ok(texts.join("\n"))
        }
        Value::Null => Ok(String::new()),
        _ => Ok(value.to_string()),
    }
}

/// A single reasoning step captured during Gemini's thought process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiThought {
    /// Short subject line for the reasoning step.
    #[serde(default)]
    pub subject: String,
    /// Detailed description of the reasoning step.
    #[serde(default)]
    pub description: String,
    /// ISO-8601 timestamp string for the reasoning step.
    #[serde(default)]
    pub timestamp: String,
}

/// Token-usage breakdown for a single Gemini message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiTokens {
    /// Total prompt tokens, including the `cached` subset.
    #[serde(default)]
    pub input: i64,
    /// Output (response) tokens.
    #[serde(default)]
    pub output: i64,
    /// Tokens served from cache.
    #[serde(default)]
    pub cached: i64,
    /// Reasoning tokens, disjoint from `output` (`total == input + output + thoughts`).
    #[serde(default)]
    pub thoughts: i64,
    /// Tokens attributed to tool use.
    #[serde(default)]
    pub tool: i64,
    /// Total token count for the message.
    #[serde(default)]
    pub total: i64,
}
