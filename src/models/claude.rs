use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Single log entry from a Claude Code session file.
///
/// Only fields the analyzer actually reads are materialised. Large unrelated
/// payloads — assistant text content, `tool_result` bodies, `parentUuid`,
/// version metadata — are dropped by serde during parse so they never retain
/// memory, which is what keeps long sessions from ballooning the working set.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeLog {
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default, rename = "type")]
    pub log_type: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub message: Option<ClaudeMessage>,
    #[serde(default, deserialize_with = "deserialize_tool_use_result")]
    pub tool_use_result: Option<ClaudeToolUseResult>,
}

/// Assistant/user message with only the fields `session::claude::parse_claude_logs` inspects.
///
/// `content` may appear in the source as either an array of typed blocks
/// (assistant messages) or a plain string (user messages like `"Caveat: ..."`).
/// Only the array form carries analyzer-relevant data, so the string form is
/// swallowed without allocating.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClaudeMessage {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub usage: Option<Value>,
    #[serde(default, deserialize_with = "deserialize_content_items")]
    pub content: Vec<ClaudeContentItem>,
}

/// One element of a message's `content` array. Non-`tool_use` items collapse
/// to `Other` so their payload (text blocks, tool_result bodies, thinking
/// traces) is discarded at parse time.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClaudeContentItem {
    ToolUse {
        #[serde(default)]
        name: String,
        #[serde(default)]
        input: Option<ClaudeBashInput>,
    },
    #[serde(other)]
    Other,
}

/// Bash tool input. Other tools share the same `input` slot but with
/// different shapes — serde silently ignores unknown fields, so non-Bash
/// inputs deserialize into an all-`None` value that the analyzer treats as
/// "no command to record".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClaudeBashInput {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Object form of `toolUseResult`. String-shaped values (user-rejection error
/// messages, etc.) are swallowed by `deserialize_tool_use_result` without
/// allocating their body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeToolUseResult {
    #[serde(default, rename = "type")]
    pub result_type: Option<String>,
    #[serde(default)]
    pub file: Option<ClaudeToolUseFile>,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub new_string: Option<String>,
    #[serde(default)]
    pub old_string: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeToolUseFile {
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
}

/// `toolUseResult` can legally be either an object or a scalar string. The
/// analyzer only cares about the object form, so scalar values are consumed
/// via `IgnoredAny` — serde walks the JSON tokens but allocates nothing.
fn deserialize_tool_use_result<'de, D>(
    deserializer: D,
) -> Result<Option<ClaudeToolUseResult>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::IgnoredAny;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Object(Box<ClaudeToolUseResult>),
        #[allow(dead_code)]
        Ignored(IgnoredAny),
    }

    match Option::<Repr>::deserialize(deserializer)? {
        Some(Repr::Object(obj)) => Ok(Some(*obj)),
        _ => Ok(None),
    }
}

/// `message.content` is an array for assistant turns but a plain string for
/// some user turns (e.g. `"Caveat: ..."`). Non-array shapes carry nothing the
/// analyzer needs, so we consume them via `IgnoredAny` and return an empty
/// `Vec` rather than failing the whole record.
fn deserialize_content_items<'de, D>(deserializer: D) -> Result<Vec<ClaudeContentItem>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::IgnoredAny;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Array(Vec<ClaudeContentItem>),
        #[allow(dead_code)]
        Ignored(IgnoredAny),
    }

    match Option::<Repr>::deserialize(deserializer)? {
        Some(Repr::Array(arr)) => Ok(arr),
        _ => Ok(Vec::new()),
    }
}
