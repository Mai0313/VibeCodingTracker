use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

// =============================================================================
// Copilot CLI `events.jsonl` format
// =============================================================================
//
// Current Copilot CLI writes `~/.copilot/session-state/<sessionId>/events.jsonl`
// where every line is a single event carrying its own `type` discriminator.
// The analyzer walks these events in order and pulls session metadata from
// `session.start`, model switches from `session.model_change`, tool calls
// from the `tool.execution_start` / `tool.execution_complete` pair, and
// authoritative per-model token usage from `session.shutdown.modelMetrics`.
//
// Earlier Copilot CLI releases wrote a single pretty-printed JSON object
// under `~/.copilot/history-session-state/<sessionId>.json` with no token
// accounting at all; that layout is no longer supported.

/// Single line of the Copilot `events.jsonl` stream.
///
/// `data` intentionally stays as raw [`Value`] — every event type has a
/// different payload, so the analyzer branches on `event_type` and then
/// deserialises `data` into the concrete shape on demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotEvent {
    #[serde(rename = "type", default)]
    pub event_type: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// `session.start` payload — session-scoped identifiers and workspace context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotSessionStartData {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub copilot_version: String,
    #[serde(default)]
    pub start_time: String,
    #[serde(default)]
    pub context: Option<CopilotSessionContext>,
}

/// Workspace context recorded at the start of a Copilot CLI session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotSessionContext {
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub git_root: String,
    #[serde(default)]
    pub branch: String,
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
    #[serde(default)]
    pub model_metrics: BTreeMap<String, CopilotModelMetric>,
    #[serde(default)]
    pub current_model: String,
}

/// Per-model block inside `CopilotShutdownData::model_metrics`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotModelMetric {
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
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: i64,
    #[serde(default)]
    pub cache_write_tokens: i64,
    #[serde(default)]
    pub reasoning_tokens: i64,
}

/// `tool.execution_start` payload — paired with `tool.execution_complete`
/// via `tool_call_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotToolStartData {
    #[serde(default)]
    pub tool_call_id: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// `tool.execution_complete` payload — carries the tool's actual output
/// (file contents on `view`, creation confirmation on `create`, …) under
/// `result`, plus the model that invoked the tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotToolCompleteData {
    #[serde(default)]
    pub tool_call_id: String,
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub result: Value,
    #[serde(default)]
    pub model: String,
}
