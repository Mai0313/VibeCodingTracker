//! Quota / rate-limit data models for the `usage` quota panels.
//!
//! Each provider has a raw wire shape that normalizes into one shared output
//! ([`QuotaWindow`] / per-provider `*QuotaSnapshot`) so the TUI gauges render
//! every provider identically:
//!
//! - **Claude** — `GET /api/oauth/usage` ([`ClaudeUsageResponse`]) plus the
//!   OAuth credentials in `~/.claude/.credentials.json` ([`ClaudeCredentials`]).
//! - **Codex** — `wham/usage` API ([`WhamUsageResponse`]) with a session-log
//!   fallback ([`CodexSessionRateLimits`]) and `~/.codex/auth.json`.
//!
//! Structs holding bearer tokens use a hand-written [`fmt::Debug`] that redacts
//! the secret so a token can never reach a log or assertion message.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// Renders an optional secret as `Some("<redacted>")` / `None` for `Debug`.
fn redact(v: &Option<String>) -> Option<&'static str> {
    v.as_ref().map(|_| "<redacted>")
}

// ---- Claude usage API (GET /api/oauth/usage) ----

/// `https://api.anthropic.com/api/oauth/usage` response (subset we read).
///
/// The richer `limits` / `spend` fields only appear when the request carries the
/// `anthropic-beta: oauth-2025-04-20` header; without it they stay empty and the
/// panel falls back to just the two top-level windows.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeUsageResponse {
    /// 5-hour window.
    #[serde(default)]
    pub five_hour: Option<ClaudeUsageWindow>,
    /// Weekly window.
    #[serde(default)]
    pub seven_day: Option<ClaudeUsageWindow>,
    /// Per-scope limit entries (session / weekly_all / weekly_scoped, ...).
    #[serde(default)]
    pub limits: Vec<ClaudeLimit>,
    /// Pay-as-you-go spend / credit balance.
    #[serde(default)]
    pub spend: Option<ClaudeSpend>,
}

/// One Claude usage window.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeUsageWindow {
    /// Percent of the window consumed (0..100).
    #[serde(default)]
    pub utilization: f64,
    /// Absolute reset time as an ISO-8601 string.
    #[serde(default)]
    pub resets_at: Option<String>,
}

/// One entry of the `limits` array; carries the per-model weekly scope.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeLimit {
    /// Limit kind, e.g. `session` / `weekly_all` / `weekly_scoped`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Percent of the window consumed (0..100).
    #[serde(default)]
    pub percent: f64,
    /// Severity, e.g. `normal` / `warning` / `reached`.
    #[serde(default)]
    pub severity: Option<String>,
    /// Absolute reset time as an ISO-8601 string.
    #[serde(default)]
    pub resets_at: Option<String>,
    /// Scope (present for `weekly_scoped`: the model this cap applies to).
    #[serde(default)]
    pub scope: Option<ClaudeScope>,
    /// Whether this limit is the currently active/binding one.
    #[serde(default)]
    pub is_active: bool,
}

/// The `scope` object of a `weekly_scoped` limit.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeScope {
    /// The model this scoped limit applies to.
    #[serde(default)]
    pub model: Option<ClaudeScopeModel>,
}

/// The `scope.model` object of a `weekly_scoped` limit.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeScopeModel {
    /// Human-readable model name, e.g. "Opus".
    #[serde(default)]
    pub display_name: Option<String>,
}

/// The `spend` object of a usage response (pay-as-you-go credit / spend).
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeSpend {
    /// Amount spent this period.
    #[serde(default)]
    pub used: Option<ClaudeMoney>,
    /// Remaining prepaid credit balance, when enabled.
    #[serde(default)]
    pub balance: Option<ClaudeMoney>,
    /// Whether pay-as-you-go spend is enabled for this account.
    #[serde(default)]
    pub enabled: bool,
}

/// A money amount in minor units (e.g. cents) with an explicit exponent.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeMoney {
    /// Amount in minor units (e.g. cents when `exponent == 2`).
    #[serde(default)]
    pub amount_minor: i64,
    /// ISO currency code, e.g. "USD".
    #[serde(default)]
    pub currency: Option<String>,
    /// Power of ten separating minor units from major (2 = cents).
    #[serde(default)]
    pub exponent: i32,
}

impl ClaudeMoney {
    /// Formats the amount as a currency string, e.g. `$0.00`.
    pub fn as_display(&self) -> String {
        let value = self.amount_minor as f64 / 10f64.powi(self.exponent.max(0));
        match self.currency.as_deref() {
            Some("USD") | None => format!("${value:.2}"),
            Some(cur) => format!("{value:.2} {cur}"),
        }
    }
}

// ---- ~/.claude/.credentials.json ----

/// `~/.claude/.credentials.json` (only the `claudeAiOauth` block; the sibling
/// `designOauth` and any unknown keys are preserved on write-back).
#[derive(Clone, Deserialize)]
pub struct ClaudeCredentials {
    /// The Claude subscription OAuth token bundle.
    #[serde(rename = "claudeAiOauth", default)]
    pub claude_ai_oauth: Option<ClaudeOauth>,
}

impl fmt::Debug for ClaudeCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeCredentials")
            .field("claude_ai_oauth", &self.claude_ai_oauth)
            .finish()
    }
}

/// The `claudeAiOauth` object of `~/.claude/.credentials.json`.
#[derive(Clone, Deserialize)]
pub struct ClaudeOauth {
    /// Bearer access token for the OAuth usage API.
    #[serde(rename = "accessToken", default)]
    pub access_token: Option<String>,
    /// Refresh token (rotates on refresh; must be persisted).
    #[serde(rename = "refreshToken", default)]
    pub refresh_token: Option<String>,
    /// Access-token expiry, Unix **milliseconds**.
    #[serde(rename = "expiresAt", default)]
    pub expires_at: Option<i64>,
    /// OAuth scopes, carried back into the refresh request.
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl fmt::Debug for ClaudeOauth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeOauth")
            .field("access_token", &redact(&self.access_token))
            .field("refresh_token", &redact(&self.refresh_token))
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// `platform.claude.com/v1/oauth/token` refresh response.
#[derive(Clone, Deserialize)]
pub struct ClaudeRefreshResponse {
    /// New bearer access token.
    #[serde(default)]
    pub access_token: Option<String>,
    /// New refresh token (rotates).
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Lifetime of the new access token, in seconds.
    #[serde(default)]
    pub expires_in: Option<i64>,
    /// Space-separated granted scopes.
    #[serde(default)]
    pub scope: Option<String>,
}

impl fmt::Debug for ClaudeRefreshResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeRefreshResponse")
            .field("access_token", &redact(&self.access_token))
            .field("refresh_token", &redact(&self.refresh_token))
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .finish()
    }
}

/// Normalized Claude quota snapshot (worker output + on-disk cache).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClaudeQuotaSnapshot {
    /// Which source produced this snapshot.
    #[serde(default)]
    pub source: QuotaSource,
    /// Unix seconds when this snapshot was produced.
    pub fetched_at: i64,
    /// 5-hour window.
    pub five_hour: Option<QuotaWindow>,
    /// Weekly window (all models).
    pub seven_day: Option<QuotaWindow>,
    /// Per-model weekly window (`weekly_scoped`), when present.
    #[serde(default)]
    pub scoped_weekly: Option<QuotaWindow>,
    /// Model label for [`Self::scoped_weekly`], e.g. "Opus".
    #[serde(default)]
    pub scoped_label: Option<String>,
    /// Prepaid credit balance, pre-formatted (e.g. `$5.00`), when enabled.
    #[serde(default)]
    pub balance: Option<String>,
    /// Amount spent this period, pre-formatted (e.g. `$0.00`).
    #[serde(default)]
    pub spend_used: Option<String>,
    /// Whether any window has hit its cap (drives the `LIMIT` flag).
    #[serde(default)]
    pub limit_reached: bool,
    /// Credentials present but the token is unusable (expired / refresh
    /// failed / 401); the panel shows a `claude auth login` hint.
    #[serde(default)]
    pub needs_login: bool,
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
    /// Per-account spend cap.
    #[serde(default)]
    pub spend_control: Option<WhamSpendControl>,
}

/// The `spend_control` object of a wham/usage response.
#[derive(Debug, Clone, Deserialize)]
pub struct WhamSpendControl {
    /// Whether the spend cap has been reached.
    #[serde(default)]
    pub reached: Option<bool>,
    /// The configured spend cap, when set.
    #[serde(default)]
    pub individual_limit: Option<f64>,
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
    #[serde(default, deserialize_with = "de_string_or_number")]
    pub balance: Option<String>,
    /// Approximate `[low, high]` local (CLI) messages the credits still buy.
    #[serde(default)]
    pub approx_local_messages: Option<Vec<i64>>,
    /// Approximate `[low, high]` cloud-task messages the credits still buy.
    #[serde(default)]
    pub approx_cloud_messages: Option<Vec<i64>>,
}

/// Deserializes a JSON string or number into `Option<String>`.
///
/// The wham/usage `balance` is usually the string `"0"`, but some accounts
/// return it as a number; accepting both keeps a numeric balance from failing
/// the entire response. Any other type (or null) becomes `None`.
fn de_string_or_number<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<Value>::deserialize(deserializer)? {
        Some(Value::String(s)) => Some(s),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
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
/// `Debug` is implemented by hand to redact the secrets: the tokens are bearer
/// credentials and the account id is an identifier, so none should reach a log
/// or assertion message. The wham client relies on this guarantee.
#[derive(Clone, Deserialize)]
pub struct CodexAuthTokens {
    /// OIDC id token (JWT); refreshed alongside the access token.
    #[serde(default)]
    pub id_token: Option<String>,
    /// Bearer access token for the ChatGPT backend.
    #[serde(default)]
    pub access_token: Option<String>,
    /// Refresh token (rotates on refresh; must be persisted).
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Account id sent as the `ChatGPT-Account-Id` header.
    #[serde(default)]
    pub account_id: Option<String>,
}

impl fmt::Debug for CodexAuthTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexAuthTokens")
            .field("id_token", &redact(&self.id_token))
            .field("access_token", &redact(&self.access_token))
            .field("refresh_token", &redact(&self.refresh_token))
            .field("account_id", &redact(&self.account_id))
            .finish()
    }
}

/// `https://auth.openai.com/oauth/token` refresh response.
#[derive(Clone, Deserialize)]
pub struct CodexRefreshResponse {
    /// New OIDC id token.
    #[serde(default)]
    pub id_token: Option<String>,
    /// New bearer access token.
    #[serde(default)]
    pub access_token: Option<String>,
    /// New refresh token (rotates).
    #[serde(default)]
    pub refresh_token: Option<String>,
}

impl fmt::Debug for CodexRefreshResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexRefreshResponse")
            .field("id_token", &redact(&self.id_token))
            .field("access_token", &redact(&self.access_token))
            .field("refresh_token", &redact(&self.refresh_token))
            .finish()
    }
}

// ---- Codex session-log fallback ----

/// The `rate_limits` object embedded in Codex `token_count` events.
#[derive(Debug, Clone, Deserialize)]
pub struct CodexSessionRateLimits {
    /// Limit family this snapshot describes; only the main `codex` account
    /// quota maps to the 5h/7d panel, so other families are skipped.
    #[serde(default)]
    pub limit_id: Option<String>,
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

/// Which source produced a quota snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaSource {
    /// No data available.
    #[default]
    None,
    /// Live API (`wham/usage` or Claude usage).
    Api,
    /// Newest Codex session-log `rate_limits`.
    SessionFallback,
}

/// One normalized rate-limit window, shared by every provider's rendering.
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
    /// Approximate `[low, high]` messages the remaining credits still buy.
    #[serde(default)]
    pub approx_messages: Option<(i64, i64)>,
    /// Configured spend cap, when set.
    #[serde(default)]
    pub spend_limit: Option<f64>,
    /// Whether a rate limit (or credit / spend cap) has been reached.
    pub limit_reached: Option<bool>,
    /// Token present but unusable (refresh failed / 401); the panel shows a
    /// `codex auth login` hint alongside any session-fallback data.
    #[serde(default)]
    pub needs_login: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_tokens_debug_redacts_secrets() {
        let tokens = CodexAuthTokens {
            id_token: Some("jwt-header.payload.sig".into()),
            access_token: Some("sk-super-secret-value".into()),
            refresh_token: Some("rt-super-secret".into()),
            account_id: Some("acct-1234567890".into()),
        };
        let direct = format!("{tokens:?}");
        assert!(!direct.contains("sk-super-secret-value"));
        assert!(!direct.contains("rt-super-secret"));
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

    #[test]
    fn claude_oauth_debug_redacts_secrets() {
        let oauth = ClaudeOauth {
            access_token: Some("claude-access-secret".into()),
            refresh_token: Some("claude-refresh-secret".into()),
            expires_at: Some(1783108188604),
            scopes: vec!["user:inference".into()],
        };
        let s = format!("{oauth:?}");
        assert!(!s.contains("claude-access-secret"));
        assert!(!s.contains("claude-refresh-secret"));
        assert!(s.contains("<redacted>"));
        // Non-secret fields are still visible.
        assert!(s.contains("1783108188604"));
        assert!(s.contains("user:inference"));
    }

    #[test]
    fn refresh_responses_debug_redact_secrets() {
        let c = ClaudeRefreshResponse {
            access_token: Some("new-access".into()),
            refresh_token: Some("new-refresh".into()),
            expires_in: Some(28800),
            scope: Some("user:inference".into()),
        };
        let cs = format!("{c:?}");
        assert!(!cs.contains("new-access"));
        assert!(!cs.contains("new-refresh"));
        assert!(cs.contains("28800"));

        let x = CodexRefreshResponse {
            id_token: Some("id-secret".into()),
            access_token: Some("acc-secret".into()),
            refresh_token: Some("ref-secret".into()),
        };
        let xs = format!("{x:?}");
        assert!(!xs.contains("id-secret"));
        assert!(!xs.contains("acc-secret"));
        assert!(!xs.contains("ref-secret"));
    }
}
