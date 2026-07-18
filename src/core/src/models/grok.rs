//! Minimal Grok CLI session telemetry shapes used by the local parser.

use serde::Deserialize;

/// Aggregate telemetry stored in each Grok CLI session's `signals.json`.
///
/// Grok records a current context-window gauge rather than billed token
/// buckets. Unknown fields are intentionally ignored so new telemetry does
/// not break older VCT releases.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GrokSignals {
    /// Resolved model used for session attribution.
    pub primary_model_id: String,
    /// Models observed in the session, used only when the primary model is absent.
    pub models_used: Vec<String>,
    /// Current number of tokens occupying the context window.
    pub context_tokens_used: u64,
}

/// Session metadata stored beside `signals.json` in `summary.json`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GrokSummary {
    /// Stable session identity and working directory.
    pub info: GrokSessionInfo,
    /// Last session update time in RFC 3339 format.
    pub updated_at: String,
    /// Fallback last-activity time in RFC 3339 format.
    pub last_active_at: String,
    /// Alias or model id used only when signals omit model attribution.
    pub current_model_id: String,
    /// Git remotes recorded for the working tree.
    pub git_remotes: Vec<String>,
}

/// Identity fields nested under `summary.json`'s `info` object.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GrokSessionInfo {
    /// Grok session id.
    pub id: String,
    /// Session working directory.
    pub cwd: String,
}
