//! Serde models for the Google Gemini CLI chat-log JSONL format.
//!
//! The first line of each chat file is a [`GeminiSession`] meta record; the
//! remaining lines are individual events the parser handles as plain `Value`s,
//! materializing only assistant turns into [`GeminiMessage`]. The `content`
//! field is polymorphic and handled by the `deserialize_content` helper.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Top-level session metadata line of a Gemini CLI chat log.
///
/// Current Gemini CLI writes chats as JSONL event streams under
/// `~/.gemini/tmp/<project>/chats/session-*.jsonl`. The very first line is a
/// pure session-meta record:
///
/// ```json
/// {"sessionId":"...","projectHash":"...","startTime":"...","lastUpdated":"...","kind":"main"}
/// ```
///
/// Subsequent lines are individual assistant / user / info events that the
/// parser handles one-by-one as plain `Value`s (see
/// `parse_gemini_events`), so this struct only needs to capture the
/// identifiers found on that opening meta line. Legacy single-object
/// exports (`chats/<session>.json` with an inline `messages` array) are no
/// longer supported — the filesystem filter ignores `.json` entirely.
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
    /// Present on JSONL session-meta records (e.g. `"main"`), but not
    /// required — older CLI builds occasionally omit it.
    #[serde(default)]
    pub kind: Option<String>,
}

/// Single message within a Gemini session
///
/// Used both for the legacy `messages[]` entries and for JSONL events whose
/// `type == "gemini"`. Non-assistant events (`"user"`, `"info"`, `$set`
/// meta-updates, …) are filtered out by the analyzer before reaching this
/// type, so fields such as `content` and `model` can stay absent without
/// breaking deserialisation.
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

/// Flattens a polymorphic message `content` field into a single `String`.
///
/// Content may be a string, an array of `{text: "..."}` objects, null, or
/// missing entirely. Arrays are joined with newlines, `null` becomes an empty
/// string, and any other JSON value is stringified.
///
/// # Errors
///
/// Returns a deserialization error if the underlying tokens are not valid JSON
/// for [`Value`].
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
    /// Input (prompt) tokens.
    #[serde(default)]
    pub input: i64,
    /// Output (response) tokens.
    #[serde(default)]
    pub output: i64,
    /// Tokens served from cache.
    #[serde(default)]
    pub cached: i64,
    /// Tokens spent on reasoning / thoughts.
    #[serde(default)]
    pub thoughts: i64,
    /// Tokens attributed to tool use.
    #[serde(default)]
    pub tool: i64,
    /// Total token count for the message.
    #[serde(default)]
    pub total: i64,
}
