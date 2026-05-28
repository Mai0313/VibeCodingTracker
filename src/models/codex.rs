//! Serde models for the OpenAI Codex CLI `*.jsonl` session format.
//!
//! Every line is a [`CodexLog`] wrapping a `type` discriminator and a
//! [`CodexPayload`]. The payload is a wide union of optional fields because the
//! same struct deserializes session-meta, turn-context, event-message, and
//! response-item records; only the fields relevant to each record type are
//! populated.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// Raw JSON-encoded arguments string for function-call payloads.
    pub arguments: Option<String>,
    /// Correlation id linking a function call to its output.
    pub call_id: Option<String>,
    /// Function-call output body.
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
