//! Serde models for the GitHub Copilot CLI `events.jsonl` session format.
//!
//! Current Copilot CLI writes `~/.copilot/session-state/<sessionId>/events.jsonl`
//! where every line is a single event carrying its own `type` discriminator.
//! The analyzer walks these events in order and pulls session metadata from
//! `session.start`, model switches from `session.model_change`, tool calls
//! from the `tool.execution_start` / `tool.execution_complete` pair, and
//! authoritative per-model token usage from `session.shutdown.modelMetrics`.
//!
//! Earlier Copilot CLI releases wrote a single pretty-printed JSON object
//! under `~/.copilot/history-session-state/<sessionId>.json` with no token
//! accounting at all; that layout is no longer supported.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Single line of the Copilot `events.jsonl` stream.
///
/// `data` intentionally stays as raw [`Value`] — every event type has a
/// different payload, so the analyzer branches on `event_type` and then
/// deserialises `data` into the concrete shape on demand.
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
    /// Copilot CLI version that produced the session.
    #[serde(default)]
    pub copilot_version: String,
    /// ISO-8601 session start time.
    #[serde(default)]
    pub start_time: String,
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
    /// E.g. `"Mai0313/VibeCodingTracker"` — empty when the session did not
    /// run inside a git repo.
    #[serde(default)]
    pub repository: String,
    /// E.g. `"github"`; used together with `repository_host` to reconstruct
    /// the remote URL when the CLI omits the full URL.
    #[serde(default)]
    pub host_type: String,
    /// E.g. `"github.com"`.
    #[serde(default)]
    pub repository_host: String,
}

/// `session.model_change` payload — each session may switch between models
/// at any point, so the analyzer uses the most recent one when attributing
/// streaming `assistant.message` tokens that arrive before `session.shutdown`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotModelChangeData {
    /// Model the session switched to.
    #[serde(default)]
    pub new_model: String,
}

/// `session.shutdown` payload — authoritative per-model token usage.
///
/// The map key is the model name (e.g. `"claude-sonnet-4.6"`). Copilot CLI
/// writes this event on graceful shutdown after totalling up every API
/// interaction in the session, which is the only place that carries *input*
/// tokens (individual `assistant.message` events only carry `outputTokens`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotShutdownData {
    /// Per-model token metrics, keyed by model name.
    #[serde(default)]
    pub model_metrics: BTreeMap<String, CopilotModelMetric>,
    /// Model that was active when the session shut down.
    #[serde(default)]
    pub current_model: String,
}

/// Per-model block inside [`CopilotShutdownData::model_metrics`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotModelMetric {
    /// Token counts for the model, when present.
    #[serde(default)]
    pub usage: Option<CopilotModelUsage>,
}

/// Token counts captured by Copilot CLI at session shutdown.
///
/// Field names mirror the camelCase keys the CLI writes to disk — the
/// usage_processor normalises these into the Claude-style field names
/// (`input_tokens`, `cache_read_input_tokens`, …) before storing them in
/// `conversation_usage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotModelUsage {
    /// Input (prompt) tokens, including cache reads and writes.
    #[serde(default)]
    pub input_tokens: i64,
    /// Output (completion) tokens, including reasoning tokens.
    #[serde(default)]
    pub output_tokens: i64,
    /// Tokens served from the prompt cache.
    #[serde(default)]
    pub cache_read_tokens: i64,
    /// Tokens written into the prompt cache.
    #[serde(default)]
    pub cache_write_tokens: i64,
    /// Reasoning tokens included in `output_tokens`.
    #[serde(default)]
    pub reasoning_tokens: i64,
}

/// `tool.execution_start` payload — paired with `tool.execution_complete`
/// via `tool_call_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotToolStartData {
    /// Correlation id paired with the matching `tool.execution_complete`.
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub tool_call_id: String,
    /// Name of the invoked tool (e.g. `view`, `create`).
    #[serde(default)]
    pub tool_name: String,
    /// Raw tool arguments.
    #[serde(default)]
    pub arguments: Value,
}

fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// `tool.execution_complete` payload — carries the tool's actual output
/// (file contents on `view`, creation confirmation on `create`, …) under
/// `result`, plus the model that invoked the tool.
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
