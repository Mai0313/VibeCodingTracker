//! Serde models for the OpenAI Codex CLI `*.jsonl` session format.
//!
//! Every line is a [`CodexLog`] wrapping a `type` discriminator and a
//! [`CodexPayload`]. The payload is a wide union of optional fields because the
//! same struct deserializes session-meta, turn-context, event-message, and
//! response-item records; only the fields relevant to each record type are
//! populated.

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::Value;

/// Normalizes JSON-encoded or structured tool arguments into the legacy string field.
fn deserialize_tool_arguments<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(value @ (Value::Object(_) | Value::Array(_))) => serde_json::to_string(&value)
            .map(Some)
            .map_err(|_| D::Error::custom("failed to normalize tool arguments")),
        Some(_) => Err(D::Error::custom(
            "tool arguments must be a string, object, or array",
        )),
    }
}

/// Normalizes string, object, and content-block output shapes into plain text.
fn deserialize_tool_output<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(value @ Value::Object(_)) => serde_json::to_string(&value)
            .map(Some)
            .map_err(|_| D::Error::custom("failed to normalize tool output")),
        Some(Value::Array(blocks)) => normalize_output_blocks(blocks).map(Some),
        Some(_) => Err(D::Error::custom(
            "tool output must be a string, object, or content-block array",
        )),
    }
}

fn normalize_output_blocks<E>(blocks: Vec<Value>) -> Result<String, E>
where
    E: serde::de::Error,
{
    let mut combined = String::new();
    for block in blocks {
        let text = match block {
            Value::String(text) => Some(text),
            Value::Object(object) => match object.get("text") {
                Some(Value::String(text)) => Some(text.clone()),
                Some(_) => {
                    return Err(E::custom("tool output block text must be a string"));
                }
                None if object.get("type").and_then(Value::as_str) == Some("input_image") => None,
                None => {
                    return Err(E::custom(
                        "tool output array contains an unsupported content block",
                    ));
                }
            },
            _ => {
                return Err(E::custom(
                    "tool output array contains an unsupported content block",
                ));
            }
        };

        if let Some(text) = text {
            if !combined.is_empty() && !combined.ends_with('\n') && !text.starts_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&text);
        }
    }
    Ok(combined)
}

/// A single line of a Codex/OpenAI session log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexLog {
    /// ISO-8601 timestamp string for the record.
    pub timestamp: String,
    /// Top-level record discriminator (`session_meta`, `event_msg`, …).
    #[serde(rename = "type")]
    pub log_type: String,
    /// Event-specific payload.
    pub payload: CodexPayload,
}

/// Event-specific payload of a [`CodexLog`].
///
/// A single flat union covering every Codex record type; each field is `Option`
/// and only the subset meaningful to the record's `payload_type` is present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexPayload {
    /// Inner payload discriminator (e.g. `message`, `function_call`).
    #[serde(rename = "type")]
    pub payload_type: Option<String>,
    /// Message role (`user`, `assistant`, …) for message payloads.
    pub role: Option<String>,
    /// Message content blocks for message payloads.
    pub content: Option<Vec<CodexContent>>,
    /// Function name for function-call payloads (e.g. `shell`, `exec_command`).
    pub name: Option<String>,
    /// Raw JSON-encoded arguments or custom-tool input.
    #[serde(
        default,
        alias = "input",
        deserialize_with = "deserialize_tool_arguments"
    )]
    pub arguments: Option<String>,
    /// Correlation id linking a function call to its output.
    pub call_id: Option<String>,
    /// Normalized function/custom-tool output body; text blocks are flattened.
    #[serde(default, deserialize_with = "deserialize_tool_output")]
    pub output: Option<String>,
    /// Free-form message text for event-message payloads.
    pub message: Option<String>,
    /// Token-usage / event info blob, shape depends on the event.
    pub info: Option<Value>,
    /// Working directory recorded in session-meta / turn-context payloads.
    pub cwd: Option<String>,
    /// Approval policy in effect for the turn.
    pub approval_policy: Option<String>,
    /// Sandbox policy blob in effect for the turn.
    pub sandbox_policy: Option<Value>,
    /// Model name driving the turn.
    pub model: Option<String>,
    /// Reasoning-effort setting for the turn.
    pub effort: Option<String>,
    /// Reasoning summary text, when present.
    pub summary: Option<String>,
    /// Record identifier.
    pub id: Option<String>,
    /// Originator label (which client produced the session).
    pub originator: Option<String>,
    /// Git repository metadata captured at session start.
    pub git: Option<CodexGitInfo>,
    /// Whether a `patch_apply_end` event reported a successful apply.
    #[serde(default)]
    pub success: Option<bool>,
    /// Per-file changes of a `patch_apply_end` event, keyed by absolute path.
    #[serde(default)]
    pub changes: Option<Value>,
}

/// One content block of a Codex message payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexContent {
    /// Block type (e.g. `text`, `input_text`, `output_text`).
    #[serde(rename = "type")]
    pub content_type: String,
    /// Block text, when the block carries text.
    pub text: Option<String>,
}

/// Git repository metadata captured at the start of a Codex session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexGitInfo {
    /// Current commit hash.
    pub commit_hash: Option<String>,
    /// Current branch name.
    pub branch: Option<String>,
    /// Remote repository URL.
    pub repository_url: Option<String>,
}

/// Arguments of the legacy `name == "shell"` function call.
///
/// The command is an argv array, typically `["bash", "-lc", "<script>"]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexShellArguments {
    /// Command argv vector.
    pub command: Vec<String>,
}

/// Arguments for the current `name == "exec_command"` function call.
///
/// Codex CLI replaced the legacy `shell` function (whose arguments were a
/// `["bash", "-lc", "<script>"]` array) with a flat `{cmd, workdir, ...}`
/// object. The analyzer normalises both into the same `CodexShellCall`
/// downstream so the patch / sed / cat detection can stay shared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexExecCommandArguments {
    /// Full command string to execute.
    pub cmd: String,
    /// Working directory for the command (empty when unset).
    #[serde(default)]
    pub workdir: String,
}

/// Result of a Codex shell command: captured output plus optional metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexShellOutput {
    /// Combined stdout/stderr captured from the command.
    pub output: String,
    /// Exit-code and timing metadata, when the CLI recorded it.
    pub metadata: Option<CodexShellMetadata>,
}

/// Exit-code and timing metadata for a Codex shell command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexShellMetadata {
    /// Process exit code.
    pub exit_code: i32,
    /// Wall-clock duration of the command in seconds.
    pub duration_seconds: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(value: Value) -> CodexPayload {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn original_public_field_list_remains_constructible() {
        let payload = CodexPayload {
            payload_type: None,
            role: None,
            content: None,
            name: None,
            arguments: Some("{}".to_string()),
            call_id: None,
            output: Some("text".to_string()),
            message: None,
            info: None,
            cwd: None,
            approval_policy: None,
            sandbox_policy: None,
            model: None,
            effort: None,
            summary: None,
            id: None,
            originator: None,
            git: None,
            success: None,
            changes: None,
        };

        assert_eq!(payload.arguments.as_deref(), Some("{}"));
        assert_eq!(payload.output.as_deref(), Some("text"));
    }

    #[test]
    fn arguments_accept_legacy_strings_and_structured_json() {
        let string = payload(json!({ "arguments": "{\"cmd\":\"pwd\"}" }));
        assert_eq!(string.arguments.as_deref(), Some("{\"cmd\":\"pwd\"}"));

        let object = payload(json!({ "arguments": { "cmd": "pwd" } }));
        assert_eq!(
            serde_json::from_str::<Value>(object.arguments.as_deref().unwrap()).unwrap(),
            json!({ "cmd": "pwd" })
        );

        let array = payload(json!({ "arguments": ["bash", "-lc", "pwd"] }));
        assert_eq!(
            serde_json::from_str::<Value>(array.arguments.as_deref().unwrap()).unwrap(),
            json!(["bash", "-lc", "pwd"])
        );
    }

    #[test]
    fn custom_input_alias_populates_the_original_arguments_field() {
        let parsed = payload(json!({ "input": "await tools.exec_command({});" }));
        assert_eq!(
            parsed.arguments.as_deref(),
            Some("await tools.exec_command({});")
        );

        let serialized = serde_json::to_value(parsed).unwrap();
        assert_eq!(
            serialized["arguments"],
            json!("await tools.exec_command({});")
        );
        assert!(serialized.get("input").is_none());
    }

    #[test]
    fn output_accepts_missing_null_string_and_object_shapes() {
        assert_eq!(payload(json!({})).output, None);
        assert_eq!(payload(json!({ "output": null })).output, None);
        assert_eq!(
            payload(json!({ "output": "plain text" })).output.as_deref(),
            Some("plain text")
        );

        let object = payload(json!({
            "output": {
                "output": "command text",
                "metadata": { "exit_code": 0, "duration_seconds": 0.1 }
            }
        }));
        assert_eq!(
            serde_json::from_str::<Value>(object.output.as_deref().unwrap()).unwrap(),
            json!({
                "output": "command text",
                "metadata": { "exit_code": 0, "duration_seconds": 0.1 }
            })
        );
    }

    #[test]
    fn output_flattens_text_blocks_and_ignores_known_image_blocks() {
        let parsed = payload(json!({
            "output": [
                { "type": "input_text", "text": "first" },
                { "type": "input_text", "text": "second\n" },
                "third",
                { "type": "input_image", "image_url": "data:image/png;base64,redacted" }
            ]
        }));
        assert_eq!(parsed.output.as_deref(), Some("first\nsecond\nthird"));
    }

    #[test]
    fn unsupported_argument_and_output_shapes_are_rejected() {
        assert!(serde_json::from_value::<CodexPayload>(json!({ "arguments": 7 })).is_err());
        assert!(serde_json::from_value::<CodexPayload>(json!({ "output": true })).is_err());
        assert!(
            serde_json::from_value::<CodexPayload>(json!({
                "output": [{ "type": "future_block" }]
            }))
            .is_err()
        );
    }
}
