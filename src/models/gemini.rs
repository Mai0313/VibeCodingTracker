use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Top-level session metadata for a Gemini CLI chat log.
///
/// Two Gemini file formats are in the wild:
///
/// - **Legacy single-object JSON** (`chats/<session>.json`): one big
///   pretty-printed object with all messages inlined in the `messages` array.
/// - **Current JSONL event stream** (`chats/session-*.jsonl`): the first line
///   is a pure session-meta record (`{sessionId, projectHash, startTime,
///   lastUpdated, kind}`) with **no** `messages` field, followed by one
///   event per line.
///
/// We deserialise both into the same struct: every non-identifier field is
/// `#[serde(default)]` so a meta-only line still parses, and `messages`
/// stays empty for the JSONL case (the analyzer walks the rest of the
/// stream line-by-line instead of relying on this vec).
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
    /// Present on JSONL session-meta records (e.g. `"main"`); absent on
    /// legacy single-object exports.
    #[serde(default)]
    pub kind: Option<String>,
    /// Populated by the legacy single-object format; always empty for the
    /// JSONL event stream (events come on subsequent lines).
    #[serde(default)]
    pub messages: Vec<GeminiMessage>,
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
