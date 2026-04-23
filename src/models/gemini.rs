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
/// analyzer parses one-by-one as plain `Value`s (see
/// `analyze_gemini_events`), so this struct only needs to capture the
/// identifiers found on that opening meta line. Legacy single-object
/// exports (`chats/<session>.json` with an inline `messages` array) are no
/// longer supported — the filesystem filter ignores `.json` entirely.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiSession {
    pub session_id: String,
    #[serde(default)]
    pub project_hash: String,
    #[serde(default)]
    pub start_time: String,
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
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(rename = "type", default)]
    pub message_type: String,
    #[serde(default, deserialize_with = "deserialize_content")]
    pub content: String,
    #[serde(default)]
    pub thoughts: Vec<GeminiThought>,
    pub tokens: Option<GeminiTokens>,
    pub model: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<Value>,
}

/// Deserialize content that can be either a string, an array of
/// `{text: "..."}` objects, null, or missing entirely.
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

/// AI reasoning step captured during Gemini's thought process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiThought {
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub timestamp: String,
}

/// Token usage breakdown for a single Gemini message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiTokens {
    #[serde(default)]
    pub input: i64,
    #[serde(default)]
    pub output: i64,
    #[serde(default)]
    pub cached: i64,
    #[serde(default)]
    pub thoughts: i64,
    #[serde(default)]
    pub tool: i64,
    #[serde(default)]
    pub total: i64,
}
