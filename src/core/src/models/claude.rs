//! Serde models for the Claude Code `*.jsonl` session format.
//!
//! Only fields read by the analyzer are materialized; unrelated payloads are discarded during deserialization, and polymorphic fields (string-or-array, object-or-scalar) are normalized to a single shape.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Single log entry from a Claude Code session file.
///
/// Unrelated fields (assistant text, `parentUuid`, version metadata) are ignored during deserialization.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeLog {
    /// Working directory the session ran in.
    #[serde(default)]
    pub cwd: String,
    /// Session identifier.
    #[serde(default)]
    pub session_id: String,
    /// Record type discriminator (e.g. `user`, `assistant`).
    #[serde(default, rename = "type")]
    pub log_type: String,
    /// ISO-8601 timestamp string for the record.
    #[serde(default)]
    pub timestamp: String,
    /// The message body, when the record carries one.
    #[serde(default)]
    pub message: Option<ClaudeMessage>,
    /// API request identifier, paired with `message.id` to deduplicate repeated usage records per billed request.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Legacy top-level `toolUseResult`; absent on subagent records.
    #[serde(default, deserialize_with = "deserialize_tool_use_result")]
    pub tool_use_result: Option<ClaudeToolUseResult>,
    /// Whether this record belongs to a subagent log (`<session>/subagents/agent-*.jsonl`), where tool results appear only under `message.content[].tool_result` instead of top-level `toolUseResult`.
    #[serde(default)]
    pub is_sidechain: bool,
}

/// Message body within a [`ClaudeCodeLog`].
///
/// Non-array `content` values (such as plain user-message strings) deserialize as an empty vector without allocating.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClaudeMessage {
    /// API response identifier (`msg_...`), paired with [`ClaudeCodeLog::request_id`] to identify one billed request.
    #[serde(default)]
    pub id: Option<String>,
    /// Model name that produced an assistant message.
    #[serde(default)]
    pub model: Option<String>,
    /// Raw token-usage object as written by Claude Code.
    #[serde(default)]
    pub usage: Option<Value>,
    /// Typed content blocks; empty when `content` was a plain string.
    #[serde(default, deserialize_with = "deserialize_content_items")]
    pub content: Vec<ClaudeContentItem>,
}

/// Element of a Claude message `content` array.
///
/// Only `tool_use` and `tool_result` blocks are retained; all other block types deserialize as [`Self::Other`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClaudeContentItem {
    /// An assistant-side tool invocation.
    ToolUse {
        /// Tool-call identifier, matched against a later `ToolResult`.
        #[serde(default)]
        id: String,
        /// Tool name (e.g. `Read`, `Write`, `Bash`).
        #[serde(default)]
        name: String,
        /// Tool input parameters.
        #[serde(default)]
        input: Option<ClaudeToolInput>,
    },
    /// A tool result block from a user-role record (subagent fallback path).
    ToolResult {
        /// Identifier of the `ToolUse` this result answers.
        #[serde(default)]
        tool_use_id: String,
        /// Flattened result text (string or joined array blocks).
        #[serde(default, deserialize_with = "deserialize_tool_result_content")]
        content: String,
        /// Whether the call came back as an error rather than a result.
        #[serde(default)]
        is_error: bool,
    },
    /// Any other content block (text, thinking, image, …); discarded.
    #[serde(other)]
    Other,
}

/// Union of tool input fields across tracked Claude tools; untracked tools deserialize as all-`None`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClaudeToolInput {
    /// Shell command line (Bash tool).
    #[serde(default)]
    pub command: Option<String>,
    /// Human-readable command description (Bash tool).
    #[serde(default)]
    pub description: Option<String>,
    /// Target file path (Read / Write / Edit tools).
    #[serde(default)]
    pub file_path: Option<String>,
    /// File content to write (Write tool).
    #[serde(default)]
    pub content: Option<String>,
    /// Text to replace (Edit tool).
    #[serde(default)]
    pub old_string: Option<String>,
    /// Replacement text (Edit tool).
    #[serde(default)]
    pub new_string: Option<String>,
}

/// Flattens a tool-result `content` field (string, `{text}` block array, `null`, or other JSON value) into one `String`.
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

/// Object form of top-level `toolUseResult`; scalar values deserialize as `None`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeToolUseResult {
    /// Result kind discriminator, when the source provides one.
    #[serde(default, rename = "type")]
    pub result_type: Option<String>,
    /// File payload for read/write results.
    #[serde(default)]
    pub file: Option<ClaudeToolUseFile>,
    /// Target file path, when reported at the top level.
    #[serde(default)]
    pub file_path: Option<String>,
    /// Result content body (e.g. file contents read).
    #[serde(default)]
    pub content: Option<String>,
    /// Replacement text for edit results.
    #[serde(default)]
    pub new_string: Option<String>,
    /// Replaced text for edit results.
    #[serde(default)]
    pub old_string: Option<String>,
}

/// File payload nested inside a [`ClaudeToolUseResult`] for read/write tools.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeToolUseFile {
    /// Path of the file the tool acted on.
    #[serde(default)]
    pub file_path: Option<String>,
    /// File content captured by the tool.
    #[serde(default)]
    pub content: Option<String>,
}

/// Deserializes `toolUseResult` when it is an object, discarding scalar values as `None`.
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

/// Deserializes `message.content` when it is an array, discarding non-array values as an empty `Vec`.
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
