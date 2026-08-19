//! Quota / rate-limit data models for the `usage` quota panels.
//!
//! Each provider has its own raw wire shape — and, where a token is involved,
//! its own credential file — all normalizing into one shared output
//! ([`QuotaWindow`] / per-provider `*QuotaSnapshot`) so the TUI gauges render
//! every provider identically. The section banners below group each provider's
//! types with the endpoint they come from.
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
    /// Parsed leniently so one malformed / volatile entry never fails the body.
    #[serde(default, deserialize_with = "de_lenient_limits")]
    pub limits: Vec<ClaudeLimit>,
    /// Pay-as-you-go spend / credit balance.
    #[serde(default)]
    pub spend: Option<ClaudeSpend>,
}

/// One Claude usage window.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeUsageWindow {
    /// Percent of the window consumed (0..100). Null/wrong-type reads as 0.
    #[serde(default, deserialize_with = "de_f64_or_zero")]
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
    #[serde(default, deserialize_with = "de_bool_or_false")]
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
    /// Rate-limit tier (e.g. "default_claude_max_20x"), preferred for the Plan
    /// line because it distinguishes 5x / 20x where `subscription_type` does not.
    #[serde(rename = "rateLimitTier", default)]
    pub rate_limit_tier: Option<String>,
    /// Subscription tier (e.g. "max" / "pro"); Plan-line fallback.
    #[serde(rename = "subscriptionType", default)]
    pub subscription_type: Option<String>,
}

impl fmt::Debug for ClaudeOauth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeOauth")
            .field("access_token", &redact(&self.access_token))
            .field("refresh_token", &redact(&self.refresh_token))
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .field("rate_limit_tier", &self.rate_limit_tier)
            .field("subscription_type", &self.subscription_type)
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
    /// Plan tier from the credentials file (e.g. "max 20x"), shown as Plan.
    #[serde(default)]
    pub plan_type: Option<String>,
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
    /// First rate-limit window reported by the API.
    #[serde(default)]
    pub primary_window: Option<WhamWindow>,
    /// Second rate-limit window reported by the API.
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

/// Deserializes a JSON number into `f64`, mapping null / wrong-type to 0.0.
///
/// The usage windows are volatile (a scoped tier can appear or vanish); a stray
/// `null` percent must not fail the whole response, only read as 0.
fn de_f64_or_zero<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<Value>::deserialize(deserializer)? {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    })
}

/// Deserializes a JSON bool, mapping null / wrong-type to false.
fn de_bool_or_false<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(matches!(
        Option::<Value>::deserialize(deserializer)?,
        Some(Value::Bool(true))
    ))
}

/// Deserializes the `limits` array leniently: an element that fails to parse (a
/// volatile / malformed limit entry) is skipped rather than failing the whole
/// response, and a non-array value yields an empty list. This keeps a broken
/// per-model scoped entry from taking down the 5h / 7d / balance rows.
fn de_lenient_limits<'de, D>(deserializer: D) -> Result<Vec<ClaudeLimit>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Value::deserialize(deserializer)? {
        Value::Array(arr) => arr
            .into_iter()
            .filter_map(|e| serde_json::from_value(e).ok())
            .collect(),
        _ => Vec::new(),
    })
}

/// The reset-credit summary embedded in a wham/usage response.
#[derive(Debug, Clone, Deserialize)]
pub struct WhamResetCredits {
    /// Number of rate-limit reset credits available.
    #[serde(default)]
    pub available_count: Option<i64>,
}

/// The response from the reset-credit details endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct WhamResetCreditsDetails {
    /// Per-credit details. The backend may cap this list.
    #[serde(default)]
    pub credits: Vec<WhamResetCreditDetails>,
    /// Authoritative number of available reset credits, when reported. Absent
    /// means "unknown", not zero: the caller keeps the count wham/usage gave it.
    #[serde(default)]
    pub available_count: Option<i64>,
}

/// One earned rate-limit reset credit.
#[derive(Debug, Clone, Deserialize)]
pub struct WhamResetCreditDetails {
    /// Stable backend identifier, only ever quoted back in an error message.
    #[serde(default)]
    pub id: String,
    /// Lifecycle state, e.g. `available` / `redeeming` / `redeemed`. An entry
    /// that omits it is not readable as available, so it is skipped.
    #[serde(default)]
    pub status: String,
    /// RFC3339 expiry time, or `None` when the credit does not expire.
    #[serde(default)]
    pub expires_at: Option<String>,
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
    /// First rate-limit window reported by the session log.
    #[serde(default)]
    pub primary: Option<CodexSessionWindow>,
    /// Second rate-limit window reported by the session log.
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
    /// The provider's live quota API.
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
/// `~/.vct/codex_usage.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodexQuotaSnapshot {
    /// Which source produced this snapshot.
    pub source: QuotaSource,
    /// Unix seconds when this snapshot was produced.
    pub fetched_at: i64,
    /// Plan tier, e.g. "plus".
    pub plan_type: Option<String>,
    /// Normalized 5-hour window, regardless of its source field.
    pub primary: Option<QuotaWindow>,
    /// Normalized weekly window, regardless of its source field.
    pub secondary: Option<QuotaWindow>,
    /// Credit balance (string, matching the API's `"0"`).
    pub credits_balance: Option<String>,
    /// Whether the account has purchasable credits enabled.
    pub has_credits: Option<bool>,
    /// Whether usage is unlimited.
    pub unlimited: Option<bool>,
    /// Number of rate-limit reset credits available.
    pub reset_credits_available: Option<i64>,
    /// Expiry times for fetched `available` reset-credit details. The outer
    /// `None` means details were unavailable; an inner `None` never expires.
    /// The backend may cap this list, so its length is not the total count.
    #[serde(default)]
    pub reset_credit_expirations: Option<Vec<Option<i64>>>,
    /// Approximate `[low, high]` local (CLI) messages the remaining credits
    /// still buy; the cloud-task pair is not carried.
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

// ---- GitHub Copilot usage API (GET /copilot_internal/user) ----

/// `https://api.github.com/copilot_internal/user` response (subset we read).
///
/// Field names match the API's snake_case shape directly.
#[derive(Debug, Clone, Deserialize)]
pub struct CopilotUserResponse {
    /// Plan tier, e.g. "individual" / "business".
    #[serde(default)]
    pub copilot_plan: Option<String>,
    /// Quota reset instant (ISO-8601), preferred over the date-only field.
    #[serde(default)]
    pub quota_reset_date_utc: Option<String>,
    /// Quota reset date (`YYYY-MM-DD`), fallback when the UTC instant is absent.
    #[serde(default)]
    pub quota_reset_date: Option<String>,
    /// Per-quota snapshots (premium interactions / chat / completions).
    #[serde(default)]
    pub quota_snapshots: Option<CopilotQuotaSnapshots>,
}

/// The `quota_snapshots` object of a Copilot user response.
#[derive(Debug, Clone, Deserialize)]
pub struct CopilotQuotaSnapshots {
    /// Premium (model) request quota — the headline gauge.
    #[serde(default)]
    pub premium_interactions: Option<CopilotQuotaEntry>,
}

/// One Copilot quota snapshot entry.
#[derive(Debug, Clone, Deserialize)]
pub struct CopilotQuotaEntry {
    /// Percent of the quota still available (0..100).
    #[serde(default)]
    pub percent_remaining: Option<f64>,
    /// Absolute remaining request count.
    #[serde(default)]
    pub remaining: Option<f64>,
    /// Total request entitlement for the period.
    #[serde(default)]
    pub entitlement: Option<f64>,
    /// Whether this quota is unlimited.
    #[serde(default)]
    pub unlimited: Option<bool>,
}

// ---- Cursor usage API (GET /api/usage-summary) ----

/// `https://cursor.com/api/usage-summary` response (subset we read).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorUsageSummary {
    /// Plan tier, e.g. "free" / "pro" / "enterprise".
    #[serde(default)]
    pub membership_type: Option<String>,
    /// Whether usage is unlimited.
    #[serde(default)]
    pub is_unlimited: Option<bool>,
    /// Billing cycle end (ISO-8601), used as the reset time for every gauge.
    #[serde(default)]
    pub billing_cycle_end: Option<String>,
    /// Per-user usage breakdown.
    #[serde(default)]
    pub individual_usage: Option<CursorIndividualUsage>,
    /// Team / enterprise usage breakdown (on-demand may live here instead).
    #[serde(default)]
    pub team_usage: Option<CursorTeamUsage>,
}

/// The `teamUsage` object of a Cursor usage summary.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorTeamUsage {
    /// Shared team on-demand (overage) spend.
    #[serde(default)]
    pub on_demand: Option<CursorOnDemand>,
}

/// The `individualUsage` object of a Cursor usage summary.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorIndividualUsage {
    /// Included-plan usage percentages.
    #[serde(default)]
    pub plan: Option<CursorPlanUsage>,
    /// On-demand (overage) spend.
    #[serde(default)]
    pub on_demand: Option<CursorOnDemand>,
}

/// The `individualUsage.plan` object (percentages are already in percent units).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorPlanUsage {
    /// Auto / Composer usage percent.
    #[serde(default)]
    pub auto_percent_used: Option<f64>,
    /// Named-model (API) usage percent.
    #[serde(default)]
    pub api_percent_used: Option<f64>,
    /// Headline total usage percent.
    #[serde(default)]
    pub total_percent_used: Option<f64>,
}

/// The `individualUsage.onDemand` object.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorOnDemand {
    /// Whether on-demand spend is enabled.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Amount spent this period, in cents.
    #[serde(default)]
    pub used: Option<f64>,
}

// ---- Grok billing API (GET /v1/billing?format=credits) ----

/// `https://cli-chat-proxy.grok.com/v1/billing?format=credits` response.
///
/// The credits config arrives wrapped in a `config` object. The CLI fills
/// `subscription_tier` / `on_demand_enabled` into the same object from
/// `/v1/settings`, so a bare fetch returns only `config`.
#[derive(Debug, Clone, Deserialize)]
pub struct GrokBillingResponse {
    /// The credit configuration, absent on an error body.
    #[serde(default)]
    pub config: Option<GrokCreditsConfig>,
}

/// The `config` object of a Grok credits response (subset we read).
///
/// This is a proto3 `GetGrokCreditsConfig` rendered as JSON, so every
/// zero-valued scalar may be omitted — including the headline
/// `creditUsagePercent`. Every field is therefore optional and an absent one
/// reads as zero, never as an error. The wire response is a superset of the
/// fields below (a live account also returns `topUpMethod`), so unknown keys are
/// ignored rather than rejected.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokCreditsConfig {
    /// Included-allowance usage (0..100) — what the CLI status bar shows.
    #[serde(default, deserialize_with = "de_f64_or_zero")]
    pub credit_usage_percent: f64,
    /// The current billing period (weekly or monthly) with its RFC3339 bounds.
    #[serde(default)]
    pub current_period: Option<GrokUsagePeriod>,
    /// Pay-as-you-go cap for this period.
    #[serde(default)]
    pub on_demand_cap: Option<GrokMoney>,
    /// Pay-as-you-go spend this period.
    #[serde(default)]
    pub on_demand_used: Option<GrokMoney>,
    /// Remaining purchased ("bought") credit balance.
    #[serde(default)]
    pub prepaid_balance: Option<GrokMoney>,
    /// Deprecated legacy period end, still emitted by older servers; the
    /// fallback reset time when `current_period` is absent.
    #[serde(default)]
    pub billing_period_end: Option<String>,
}

/// The `currentPeriod` object of a Grok credits config.
#[derive(Debug, Clone, Deserialize)]
pub struct GrokUsagePeriod {
    /// Period kind, e.g. `USAGE_PERIOD_TYPE_WEEKLY` / `..._MONTHLY`.
    #[serde(default, rename = "type")]
    pub period_type: Option<String>,
    /// RFC3339 period end — when the included allowance resets.
    #[serde(default)]
    pub end: Option<String>,
}

/// A Grok money amount in **cents**, e.g. `{"val": 1840}` for $18.40.
///
/// A zero amount can arrive as `{}` (proto3 omits zero scalars) or as an
/// explicit `{"val": 0}`; both read as zero.
#[derive(Debug, Clone, Deserialize)]
pub struct GrokMoney {
    /// Amount in cents.
    #[serde(default, deserialize_with = "de_f64_or_zero")]
    pub val: f64,
}

impl GrokMoney {
    /// The amount in whole currency units (dollars).
    pub fn as_dollars(&self) -> f64 {
        self.val / 100.0
    }
}

/// `https://cli-chat-proxy.grok.com/v1/settings` response (the plan label only).
///
/// The real body carries well over a hundred feature flags; only the display
/// tier is read, so every other key is ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct GrokSettingsResponse {
    /// Human-readable plan tier, e.g. "Free" / "SuperGrok".
    #[serde(default)]
    pub subscription_tier_display: Option<String>,
}

// ---- ~/.grok/auth.json ----

/// One entry of `~/.grok/auth.json`, keyed in the file by login scope
/// (`"<issuer>::<client_id>"`, `"xai::api_key"`, or the legacy sign-in URL).
///
/// The access token is the **`key`** field, not a field named `access_token`.
/// `Debug` is written by hand so no bearer, refresh token, or account
/// identifier can reach a log or assertion message.
#[derive(Clone, Default, Deserialize)]
pub struct GrokAuthEntry {
    /// Bearer access token (a JWT).
    #[serde(default)]
    pub key: Option<String>,
    /// Refresh token (rotates on refresh; must be persisted).
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Access-token expiry as an RFC3339 timestamp.
    #[serde(default)]
    pub expires_at: Option<String>,
    /// OIDC issuer this login came from; the base for token-endpoint discovery.
    #[serde(default)]
    pub oidc_issuer: Option<String>,
    /// OAuth client id, sent as a form field on refresh.
    #[serde(default)]
    pub oidc_client_id: Option<String>,
    /// Account id, sent as the `x-userid` header.
    #[serde(default)]
    pub user_id: Option<String>,
    /// Principal kind for a team login, re-sent on refresh.
    #[serde(default)]
    pub principal_type: Option<String>,
    /// Principal id for a team login, re-sent on refresh.
    #[serde(default)]
    pub principal_id: Option<String>,
}

impl fmt::Debug for GrokAuthEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrokAuthEntry")
            .field("key", &redact(&self.key))
            .field("refresh_token", &redact(&self.refresh_token))
            .field("expires_at", &self.expires_at)
            .field("oidc_issuer", &self.oidc_issuer)
            .field("oidc_client_id", &self.oidc_client_id)
            .field("user_id", &redact(&self.user_id))
            .field("principal_type", &self.principal_type)
            .field("principal_id", &redact(&self.principal_id))
            .finish()
    }
}

/// An xAI OIDC token-endpoint refresh response.
#[derive(Clone, Deserialize)]
pub struct GrokRefreshResponse {
    /// New bearer access token (written back into the entry's `key`).
    #[serde(default)]
    pub access_token: Option<String>,
    /// New refresh token (rotates).
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Lifetime of the new access token, in seconds.
    #[serde(default)]
    pub expires_in: Option<i64>,
}

impl fmt::Debug for GrokRefreshResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrokRefreshResponse")
            .field("access_token", &redact(&self.access_token))
            .field("refresh_token", &redact(&self.refresh_token))
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

// ---- Normalized Copilot / Cursor / Grok snapshots (worker output + cache) ----

/// Normalized Copilot quota snapshot, shared via `Arc<Mutex>` and persisted to
/// `~/.vct/copilot_usage.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CopilotQuotaSnapshot {
    /// Which source produced this snapshot.
    #[serde(default)]
    pub source: QuotaSource,
    /// Unix seconds when this snapshot was produced.
    pub fetched_at: i64,
    /// Plan tier (e.g. "individual"), shown as Plan.
    #[serde(default)]
    pub plan_type: Option<String>,
    /// Premium-interactions window (the headline gauge).
    #[serde(default)]
    pub premium: Option<QuotaWindow>,
    /// Remaining premium requests.
    #[serde(default)]
    pub premium_remaining: Option<i64>,
    /// Total premium request entitlement.
    #[serde(default)]
    pub premium_entitlement: Option<i64>,
    /// Whether premium interactions are unlimited.
    #[serde(default)]
    pub premium_unlimited: bool,
    /// Whether the premium quota has been exhausted (drives the `LIMIT` flag).
    #[serde(default)]
    pub limit_reached: bool,
    /// Credentials present but the token is unusable (401/403); the panel shows
    /// a `copilot login` hint.
    #[serde(default)]
    pub needs_login: bool,
}

/// Normalized Cursor quota snapshot, shared via `Arc<Mutex>` and persisted to
/// `~/.vct/cursor_usage.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CursorQuotaSnapshot {
    /// Which source produced this snapshot.
    #[serde(default)]
    pub source: QuotaSource,
    /// Unix seconds when this snapshot was produced.
    pub fetched_at: i64,
    /// Plan tier (e.g. "free" / "pro"), shown as Plan.
    #[serde(default)]
    pub plan_type: Option<String>,
    /// Headline total-usage window.
    #[serde(default)]
    pub total: Option<QuotaWindow>,
    /// Auto / Composer usage window.
    #[serde(default)]
    pub auto: Option<QuotaWindow>,
    /// Named-model (API) usage window.
    #[serde(default)]
    pub api: Option<QuotaWindow>,
    /// On-demand spend this period, in USD, when enabled.
    #[serde(default)]
    pub on_demand_dollars: Option<f64>,
    /// Whether the plan usage has hit 100% (drives the `LIMIT` flag).
    #[serde(default)]
    pub limit_reached: bool,
    /// Credentials present but the token is unusable (expired / 401); the panel
    /// shows a `cursor-agent login` hint.
    #[serde(default)]
    pub needs_login: bool,
}

/// Normalized Grok quota snapshot, shared via `Arc<Mutex>` and persisted to
/// `~/.vct/grok_usage.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GrokQuotaSnapshot {
    /// Which source produced this snapshot.
    #[serde(default)]
    pub source: QuotaSource,
    /// Unix seconds when this snapshot was produced.
    pub fetched_at: i64,
    /// Plan tier (e.g. "Free"), from `/v1/settings`, shown as Plan.
    #[serde(default)]
    pub plan_type: Option<String>,
    /// Included-allowance usage for the current period.
    #[serde(default)]
    pub included: Option<QuotaWindow>,
    /// Short label for the included window's period, e.g. "week" / "month".
    #[serde(default)]
    pub period_label: Option<String>,
    /// Pay-as-you-go spend this period, in USD.
    #[serde(default)]
    pub on_demand_dollars: Option<f64>,
    /// Pay-as-you-go cap this period, in USD, when one is set.
    #[serde(default)]
    pub on_demand_cap_dollars: Option<f64>,
    /// Remaining prepaid credit balance, in USD.
    #[serde(default)]
    pub prepaid_balance_dollars: Option<f64>,
    /// Whether the included allowance is exhausted (drives the `LIMIT` flag).
    #[serde(default)]
    pub limit_reached: bool,
    /// Credentials present but the token is unusable (expired / refresh failed /
    /// 401); the panel shows a `grok login` hint.
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
            rate_limit_tier: Some("default_claude_max_20x".into()),
            subscription_type: Some("max".into()),
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
