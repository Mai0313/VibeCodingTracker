//! Quota / rate-limit data models for the `usage` quota panels.
//!
//! Three input shapes feed one normalized output:
//! - Claude Code statusLine stdin ([`ClaudeStatuslineInput`]) → persisted
//!   [`ClaudeRateLimitsCache`].
//! - Codex `wham/usage` API response ([`WhamUsageResponse`]).
//! - Codex session-log fallback ([`CodexSessionRateLimits`]).
//!
//! All three normalize into [`QuotaWindow`] / [`CodexQuotaSnapshot`] so the TUI
//! gauges render every provider identically.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

// ---- Claude statusLine ingest ----

/// Claude Code statusLine stdin payload (only the parts we keep).
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeStatuslineInput {
    /// Rate-limit windows injected by Claude Code (absent on older versions).
    #[serde(default)]
    pub rate_limits: Option<ClaudeRateLimitsIn>,
    /// Model descriptor, used only by the printed default status line.
    #[serde(default)]
    pub model: Option<Value>,
}

/// The `rate_limits` object from a Claude statusLine payload.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeRateLimitsIn {
    /// 5-hour window.
    #[serde(default)]
    pub five_hour: Option<ClaudeWindowIn>,
    /// Weekly window.
    #[serde(default)]
    pub seven_day: Option<ClaudeWindowIn>,
}

/// One Claude rate-limit window.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeWindowIn {
    /// Percent of the window consumed (0..100; integer or float).
    #[serde(default)]
    pub used_percentage: f64,
    /// Absolute reset time, Unix seconds.
    #[serde(default)]
    pub resets_at: i64,
}

/// Persisted Claude rate-limit cache
/// (`~/.vibe_coding_tracker/claude_rate_limits.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClaudeRateLimitsCache {
    /// Unix seconds when ingest wrote this snapshot.
    pub fetched_at: i64,
    /// 5-hour window.
    pub five_hour: Option<QuotaWindow>,
    /// Weekly window.
    pub seven_day: Option<QuotaWindow>,
}

// ---- Codex wham/usage API ----

/// `https://chatgpt.com/backend-api/wham/usage` response (subset we read).
#[derive(Debug, Clone, Deserialize)]
pub struct WhamUsageResponse {
    /// Plan tier, e.g. "plus".
    #[serde(default)]
    pub plan_type: Option<String>,
    /// Rate-limit windows + status.
    #[serde(default)]
    pub rate_limit: Option<WhamRateLimit>,
    /// Credit balance info.
    #[serde(default)]
    pub credits: Option<WhamCredits>,
    /// Rate-limit reset credits.
    #[serde(default)]
    pub rate_limit_reset_credits: Option<WhamResetCredits>,
}

/// The `rate_limit` object of a wham/usage response.
#[derive(Debug, Clone, Deserialize)]
pub struct WhamRateLimit {
    /// Whether a limit has been reached.
    #[serde(default)]
    pub limit_reached: Option<bool>,
    /// 5-hour window.
    #[serde(default)]
    pub primary_window: Option<WhamWindow>,
    /// Weekly window.
    #[serde(default)]
    pub secondary_window: Option<WhamWindow>,
}

/// One wham/usage rate-limit window.
#[derive(Debug, Clone, Deserialize)]
pub struct WhamWindow {
    /// Percent of the window consumed (0..100).
    #[serde(default)]
    pub used_percent: Option<f64>,
    /// Window length in seconds (18000 = 5h, 604800 = 7d).
    #[serde(default)]
    pub limit_window_seconds: Option<i64>,
    /// Seconds until reset (relative).
    #[serde(default)]
    pub reset_after_seconds: Option<i64>,
    /// Absolute reset time, Unix seconds.
    #[serde(default)]
    pub reset_at: Option<i64>,
}

/// The `credits` object of a wham/usage response.
#[derive(Debug, Clone, Deserialize)]
pub struct WhamCredits {
    /// Whether the account has purchasable credits enabled.
    #[serde(default)]
    pub has_credits: Option<bool>,
    /// Whether usage is unlimited.
    #[serde(default)]
    pub unlimited: Option<bool>,
    /// Whether the overage limit has been reached.
    #[serde(default)]
    pub overage_limit_reached: Option<bool>,
    /// Credit balance, kept as a string to match the API's `"0"`.
    #[serde(default)]
    pub balance: Option<String>,
}

/// The `rate_limit_reset_credits` object of a wham/usage response.
#[derive(Debug, Clone, Deserialize)]
pub struct WhamResetCredits {
    /// Number of rate-limit reset credits available.
    #[serde(default)]
    pub available_count: Option<i64>,
}

// ---- ~/.codex/auth.json ----

/// `~/.codex/auth.json` (token fields only; deserialize-only, never logged).
#[derive(Debug, Clone, Deserialize)]
pub struct CodexAuthJson {
    /// OAuth token bundle.
    #[serde(default)]
    pub tokens: Option<CodexAuthTokens>,
}

/// The `tokens` object of `~/.codex/auth.json`.
///
/// `Debug` is implemented by hand to redact both fields: the access token is a
/// bearer credential and the account id is an identifier, so neither should
/// reach a log or assertion message. The wham client relies on this guarantee.
#[derive(Clone, Deserialize)]
pub struct CodexAuthTokens {
    /// Bearer access token for the ChatGPT backend.
    #[serde(default)]
    pub access_token: Option<String>,
    /// Account id sent as the `ChatGPT-Account-Id` header.
    #[serde(default)]
    pub account_id: Option<String>,
}

impl fmt::Debug for CodexAuthTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Show presence without leaking the values.
        let redact = |v: &Option<String>| v.as_ref().map(|_| "<redacted>");
        f.debug_struct("CodexAuthTokens")
            .field("access_token", &redact(&self.access_token))
            .field("account_id", &redact(&self.account_id))
            .finish()
    }
}

// ---- Codex session-log fallback ----

/// The `rate_limits` object embedded in Codex `token_count` events.
#[derive(Debug, Clone, Deserialize)]
pub struct CodexSessionRateLimits {
    /// Plan tier (e.g. "plus"), alongside the windows.
    #[serde(default)]
    pub plan_type: Option<String>,
    /// 5-hour window.
    #[serde(default)]
    pub primary: Option<CodexSessionWindow>,
    /// Weekly window.
    #[serde(default)]
    pub secondary: Option<CodexSessionWindow>,
}

/// One Codex session rate-limit window.
#[derive(Debug, Clone, Deserialize)]
pub struct CodexSessionWindow {
    /// Percent of the window consumed (0..100).
    #[serde(default)]
    pub used_percent: Option<f64>,
    /// Window length in minutes (300 = 5h, 10080 = 7d).
    #[serde(default)]
    pub window_minutes: Option<i64>,
    /// Absolute reset time, Unix seconds.
    #[serde(default)]
    pub resets_at: Option<i64>,
}

// ---- Normalized output (render target + on-disk cache) ----

/// Which source produced a Codex quota snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaSource {
    /// No data available.
    #[default]
    None,
    /// Live `wham/usage` API.
    Api,
    /// Newest Codex session-log `rate_limits`.
    SessionFallback,
}

/// One normalized rate-limit window, shared by Claude and Codex rendering.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuotaWindow {
    /// Percent of the window consumed (0..100).
    pub used_percent: f64,
    /// Absolute reset time in Unix seconds, when known.
    pub resets_at_unix: Option<i64>,
}

/// Normalized Codex quota snapshot, shared via `Arc<Mutex>` and persisted to
/// `~/.vibe_coding_tracker/codex_usage.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodexQuotaSnapshot {
    /// Which source produced this snapshot.
    pub source: QuotaSource,
    /// Unix seconds when this snapshot was produced.
    pub fetched_at: i64,
    /// Plan tier, e.g. "plus".
    pub plan_type: Option<String>,
    /// 5-hour window.
    pub primary: Option<QuotaWindow>,
    /// Weekly window.
    pub secondary: Option<QuotaWindow>,
    /// Credit balance (string, matching the API's `"0"`).
    pub credits_balance: Option<String>,
    /// Whether the account has purchasable credits enabled.
    pub has_credits: Option<bool>,
    /// Whether usage is unlimited.
    pub unlimited: Option<bool>,
    /// Number of rate-limit reset credits available.
    pub reset_credits_available: Option<i64>,
    /// Whether a rate limit has been reached.
    pub limit_reached: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_tokens_debug_redacts_secrets() {
        let tokens = CodexAuthTokens {
            access_token: Some("sk-super-secret-value".into()),
            account_id: Some("acct-1234567890".into()),
        };
        let direct = format!("{tokens:?}");
        assert!(!direct.contains("sk-super-secret-value"));
        assert!(!direct.contains("acct-1234567890"));
        assert!(direct.contains("<redacted>"));

        // The wrapper's derived Debug must inherit the redaction.
        let wrapped = format!(
            "{:?}",
            CodexAuthJson {
                tokens: Some(tokens)
            }
        );
        assert!(!wrapped.contains("sk-super-secret-value"));
        assert!(!wrapped.contains("acct-1234567890"));
    }
}
