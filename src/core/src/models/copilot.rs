//! Serde models for the GitHub Copilot CLI `events.jsonl` session format (`~/.copilot/session-state/<sessionId>/events.jsonl`).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Single line of a Copilot `events.jsonl` stream.
///
/// [`Self::data`] stays as raw [`Value`] and is deserialized per [`Self::event_type`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotEvent {
    /// Event discriminator (e.g. `session.start`, `tool.execution_complete`).
    #[serde(rename = "type", default)]
    pub event_type: String,
    /// Raw event payload, deserialized on demand based on `event_type`.
    #[serde(default)]
    pub data: Value,
    /// Event identifier.
    #[serde(default)]
    pub id: String,
    /// ISO-8601 timestamp string for the event.
    #[serde(default)]
    pub timestamp: String,
    /// Identifier of the parent event, when the event is nested.
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// `session.start` payload — session-scoped identifiers and workspace context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotSessionStartData {
    /// Session identifier.
    #[serde(default)]
    pub session_id: String,
    /// Workspace context, when the session ran inside a project.
    #[serde(default)]
    pub context: Option<CopilotSessionContext>,
}

/// Workspace context recorded at the start of a Copilot CLI session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotSessionContext {
    /// Working directory the session ran in.
    #[serde(default)]
    pub cwd: String,
    /// Absolute path of the git repository root, when inside a repo.
    #[serde(default)]
    pub git_root: String,
    /// Current git branch name.
    #[serde(default)]
    pub branch: String,
    /// Commit hash checked out at session start.
    #[serde(default)]
    pub head_commit: String,
    /// Repository slug (for example `"Mai0313/VibeCodingTracker"`), empty outside a git repo.
    #[serde(default)]
    pub repository: String,
    /// Git host (for example `"github.com"`), paired with `repository` to form the remote URL.
    #[serde(default)]
    pub repository_host: String,
}

/// `session.shutdown` payload carrying authoritative per-model token usage keyed by model name.
///
/// Only this event records input tokens; individual `assistant.message` events carry only `outputTokens`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotShutdownData {
    /// Per-model token metrics, keyed by model name.
    #[serde(default)]
    pub model_metrics: BTreeMap<String, CopilotModelMetric>,
}

/// Per-model block inside [`CopilotShutdownData::model_metrics`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotModelMetric {
    /// Token counts for the model, when present.
    #[serde(default)]
    pub usage: Option<CopilotModelUsage>,
}

/// Token counts reported by Copilot CLI at session shutdown, matching its camelCase wire keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotModelUsage {
    /// Input (prompt) tokens.
    #[serde(default)]
    pub input_tokens: i64,
    /// Output (completion) tokens, already **including** `reasoning_tokens`.
    #[serde(default)]
    pub output_tokens: i64,
    /// Tokens served from the prompt cache.
    #[serde(default)]
    pub cache_read_tokens: i64,
    /// Tokens written into the prompt cache.
    #[serde(default)]
    pub cache_write_tokens: i64,
    /// Reasoning tokens, included as a subset of `output_tokens` rather than billed separately.
    #[serde(default)]
    pub reasoning_tokens: i64,
}

/// `tool.execution_start` payload, paired with [`CopilotToolCompleteData`] via `tool_call_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotToolStartData {
    /// Correlation id paired with the matching `tool.execution_complete`.
    #[serde(default)]
    pub tool_call_id: String,
    /// Name of the invoked tool (e.g. `view`, `create`).
    #[serde(default)]
    pub tool_name: String,
    /// Raw tool arguments.
    #[serde(default)]
    pub arguments: Value,
}

/// `tool.execution_complete` payload carrying the tool result and invoking model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotToolCompleteData {
    /// Correlation id matching the originating `tool.execution_start`.
    #[serde(default)]
    pub tool_call_id: String,
    /// Whether the tool call succeeded.
    #[serde(default)]
    pub success: bool,
    /// Tool output (file contents, confirmation, …).
    #[serde(default)]
    pub result: Value,
    /// Model that invoked the tool.
    #[serde(default)]
    pub model: String,
}
