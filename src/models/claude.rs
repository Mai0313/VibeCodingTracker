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
    /// `true` for records inside a subagent JSONL
    /// (`<session>/subagents/agent-*.jsonl`). Subagent records do not carry
    /// the top-level `toolUseResult` field, so the analyzer falls back to
    /// scanning `message.content[].tool_result` for them. Main-session
    /// records (`isSidechain == false` or missing) skip the fallback to
    /// avoid double-counting tool results that already arrived via
    /// `toolUseResult`.
    #[serde(default)]
    pub is_sidechain: bool,
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

/// One element of a message's `content` array.
///
/// `ToolUse` carries the assistant-side invocation. `ToolResult` carries the
/// matching result block from the *user*-role record — used as a fallback
/// when the legacy top-level `toolUseResult` field is absent (Claude Code
/// subagent JSONL files under `<session>/subagents/agent-*.jsonl` only
/// embed results inside `message.content[].tool_result` blocks). Anything
/// else (text, thinking traces, images, …) collapses to `Other` and is
/// discarded at parse time.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClaudeContentItem {
    ToolUse {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        input: Option<ClaudeToolInput>,
    },
    ToolResult {
        #[serde(default)]
        tool_use_id: String,
        #[serde(default, deserialize_with = "deserialize_tool_result_content")]
        content: String,
    },
    #[serde(other)]
    Other,
}

/// Tool input across all tools we care about. Each tool only populates a
/// subset of fields; serde silently ignores unknown fields and unset fields
/// stay `None`. Unrelated tools (Glob, Grep, WebSearch, …) deserialize into
/// an all-`None` value that the analyzer treats as a no-op.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClaudeToolInput {
    // Bash
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    // Read / Write / Edit share `file_path`
    #[serde(default)]
    pub file_path: Option<String>,
    // Write
    #[serde(default)]
    pub content: Option<String>,
    // Edit
    #[serde(default)]
    pub old_string: Option<String>,
    #[serde(default)]
    pub new_string: Option<String>,
}

/// Tool-result `content` can be either a plain string or an array of typed
/// blocks (e.g. `[{"type":"text","text":"..."}]`). Both shapes flatten to a
/// single `String` for the analyzer's line-counting helpers.
fn deserialize_tool_result_content<'de, D>(deserializer: D) -> Result<String, D::Error>
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
