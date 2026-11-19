//! Claude Code quota fetcher.
//!
//! Reads the OAuth access token from `~/.claude/.credentials.json` and queries `GET /api/oauth/usage`. When the token is near expiry or rejected with 401, refreshes it against the Claude OAuth token endpoint and persists the rotated token in place without clobbering sibling fields. Refreshed access tokens are cached in memory per credential file mtime; a refresh failure arms a cooldown and surfaces [`CLAUDE_LOGIN_HINT`].

use crate::models::{
    ClaudeCredentials, ClaudeOauth, ClaudeQuotaSnapshot, ClaudeRefreshResponse,
    ClaudeUsageResponse, ClaudeUsageWindow, QuotaSource, QuotaWindow,
};
use crate::quota::http::iso_to_unix_secs;
use crate::quota::provider::QuotaOutcome;
use crate::quota::refresh::{
    EXPIRY_SKEW_SECS, RefreshCooldown, file_mtime, is_expiring, send_refresh,
    update_json_file_in_place,
};
use crate::utils::get_claude_credentials_path;
use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::Client;
use serde_json::json;
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

/// The Claude OAuth usage endpoint.
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// OAuth beta header that unlocks the richer usage response (`limits` / `spend`).
const CLAUDE_OAUTH_BETA: &str = "oauth-2025-04-20";
/// Fallback Claude Code version for the `User-Agent` header when `claude --version` is unavailable.
const CLAUDE_FALLBACK_VERSION: &str = "2.1.201";
/// `x-app` value the Claude Code CLI sends.
const CLAUDE_APP: &str = "cli";
/// Anthropic API version pinned by the Claude Code client.
const CLAUDE_ANTHROPIC_VERSION: &str = "2023-06-01";
/// The Claude Code OAuth client id (public PKCE client).
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Current token endpoint host.
const CLAUDE_TOKEN_URL_PRIMARY: &str = "https://platform.claude.com/v1/oauth/token";
/// Legacy token endpoint host (fallback on 404/405 from the primary).
const CLAUDE_TOKEN_URL_LEGACY: &str = "https://console.anthropic.com/v1/oauth/token";
/// Login hint surfaced when stored Claude credentials are missing or cannot be refreshed.
pub const CLAUDE_LOGIN_HINT: &str = "run: claude auth login";

/// Returns Claude Code's `User-Agent` header value, using [`crate::quota::http::detect_cli_version`].
fn claude_ua() -> &'static str {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        format!(
            "claude-cli/{} (external, cli)",
            crate::quota::http::detect_cli_version("claude", "claude", CLAUDE_FALLBACK_VERSION,)
        )
    })
    .as_str()
}

/// Returns the token refresh endpoints in preference order (primary, then legacy).
fn claude_token_urls_default() -> Vec<String> {
    vec![
        CLAUDE_TOKEN_URL_PRIMARY.to_string(),
        CLAUDE_TOKEN_URL_LEGACY.to_string(),
    ]
}

/// Reads the `claudeAiOauth` block from the credentials file.
fn read_claude_oauth(path: &Path) -> Option<ClaudeOauth> {
    parse_claude_oauth(&std::fs::read_to_string(path).ok()?)
}

/// Parses the `claudeAiOauth` block from a `.credentials.json` body.
fn parse_claude_oauth(body: &str) -> Option<ClaudeOauth> {
    let creds: ClaudeCredentials = serde_json::from_str(body).ok()?;
    creds.claude_ai_oauth
}

/// Fetches the raw `/api/oauth/usage` response as `(status_code, body)` using the stored access token verbatim.
///
/// Does not refresh tokens or modify credentials on disk; non-2xx responses are returned as `Ok((status, body))`.
///
/// # Errors
///
/// Returns an error if the credentials file is missing, has no access token, or the HTTP request fails.
pub fn fetch_claude_raw(client: &Client) -> Result<(u16, String)> {
    fetch_claude_raw_from(client, CLAUDE_USAGE_URL, &get_claude_credentials_path()?)
}

/// Injectable core of [`fetch_claude_raw`] reading the access token from `creds_path` and querying `usage_url`.
pub(crate) fn fetch_claude_raw_from(
    client: &Client,
    usage_url: &str,
    creds_path: &Path,
) -> Result<(u16, String)> {
    let path = creds_path;
    let token = read_claude_oauth(path)
        .and_then(|o| o.access_token)
        .filter(|t| !t.is_empty())
        .with_context(|| {
            format!(
                "no Claude access token in {} ({CLAUDE_LOGIN_HINT})",
                path.display()
            )
        })?;
    let resp = client
        .get(usage_url)
        .header(reqwest::header::USER_AGENT, claude_ua())
        .header("x-app", CLAUDE_APP)
        .header("anthropic-version", CLAUDE_ANTHROPIC_VERSION)
        .header("anthropic-beta", CLAUDE_OAUTH_BETA)
        .header("anthropic-dangerous-direct-browser-access", "true")
        .bearer_auth(&token)
        .send()
        .context("Failed to send Claude usage request")?;
    let status = resp.status().as_u16();
    let body = resp
        .text()
        .context("Failed to read Claude usage response body")?;
    Ok((status, body))
}

/// Returns the prettified plan tier from `path`, preferring `rateLimitTier` over `subscriptionType`.
fn read_claude_plan(path: &Path) -> Option<String> {
    let oauth = read_claude_oauth(path)?;
    let raw = oauth.rate_limit_tier.or(oauth.subscription_type)?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| prettify_tier(trimmed))
}

/// Cleans a raw tier string for display, e.g. `default_claude_max_20x` -> `max 20x`.
fn prettify_tier(raw: &str) -> String {
    raw.trim_start_matches("default_")
        .trim_start_matches("claude_")
        .replace('_', " ")
}

/// Maps a `/api/oauth/usage` body into a [`ClaudeQuotaSnapshot`] (pure).
///
/// # Errors
///
/// Returns an error if the body is not valid JSON in the expected shape.
pub fn map_claude_usage(body: &str, now: i64) -> Result<ClaudeQuotaSnapshot> {
    let resp: ClaudeUsageResponse =
        serde_json::from_str(body).context("Failed to parse Claude usage response")?;
    let win = |w: &ClaudeUsageWindow| QuotaWindow {
        used_percent: w.utilization,
        resets_at_unix: w.resets_at.as_deref().and_then(iso_to_unix_secs),
    };

    // Per-model weekly caps are optional and volatile upstream; prefer an active entry, then highest percent, and omit the row if unlabeled.
    let scoped = resp
        .limits
        .iter()
        .filter(|l| l.kind.as_deref() == Some("weekly_scoped"))
        .max_by(|a, b| {
            a.is_active.cmp(&b.is_active).then(
                a.percent
                    .partial_cmp(&b.percent)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
    // Carry the full display name; the panel renderer decides truncation.
    let scoped_label = scoped
        .and_then(|l| l.scope.as_ref())
        .and_then(|s| s.model.as_ref())
        .and_then(|m| m.display_name.as_deref())
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    let scoped_weekly = scoped
        .filter(|_| scoped_label.is_some())
        .map(|l| QuotaWindow {
            used_percent: l.percent,
            resets_at_unix: l.resets_at.as_deref().and_then(iso_to_unix_secs),
        });

    let (balance, spend_used) = match &resp.spend {
        Some(sp) => (
            sp.balance.as_ref().map(|m| m.as_display()),
            sp.used.as_ref().map(|m| m.as_display()),
        ),
        None => (None, None),
    };

    // A cap is hit when any window is at/over 100% or a limit's severity says so.
    let severity_reached =
        |s: &Option<String>| matches!(s.as_deref(), Some("reached" | "exceeded" | "blocked"));
    let limit_reached = [resp.five_hour.as_ref(), resp.seven_day.as_ref()]
        .into_iter()
        .flatten()
        .any(|w| w.utilization >= 100.0)
        || resp
            .limits
            .iter()
            .any(|l| l.percent >= 100.0 || severity_reached(&l.severity));

    Ok(ClaudeQuotaSnapshot {
        source: QuotaSource::Api,
        fetched_at: now,
        // Set by the worker from the credentials file, not the usage body.
        plan_type: None,
        five_hour: resp.five_hour.as_ref().map(win),
        seven_day: resp.seven_day.as_ref().map(win),
        scoped_weekly,
        scoped_label,
        balance,
        spend_used,
        limit_reached,
        needs_login: false,
    })
}

/// Outcome of a single usage request.
enum FetchResult {
    Ok(ClaudeQuotaSnapshot),
    /// 401 → the token is rejected; refresh and retry.
    Unauthorized,
    /// Network / non-401 error → keep last-known-good.
    Transient,
}

/// Calls the usage API once and classifies the response for the worker.
fn fetch_claude_usage(client: &Client, token: &str, now: i64, usage_url: &str) -> FetchResult {
    let resp = match client
        .get(usage_url)
        .header(reqwest::header::USER_AGENT, claude_ua())
        .header("x-app", CLAUDE_APP)
        .header("anthropic-version", CLAUDE_ANTHROPIC_VERSION)
        .header("anthropic-beta", CLAUDE_OAUTH_BETA)
        .header("anthropic-dangerous-direct-browser-access", "true")
        .bearer_auth(token)
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            log::warn!("claude quota fetch: request failed: {e}");
            return FetchResult::Transient;
        }
    };
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return FetchResult::Unauthorized;
    }
    if !status.is_success() {
        // 403 and friends are not treated as "needs login".
        log::warn!("claude quota fetch: HTTP {status}");
        return FetchResult::Transient;
    }
    match resp.text() {
        Ok(text) => match map_claude_usage(&text, now) {
            Ok(snap) => FetchResult::Ok(snap),
            Err(e) => {
                log::warn!("claude quota fetch: failed to parse usage response: {e}");
                FetchResult::Transient
            }
        },
        Err(e) => {
            log::warn!("claude quota fetch: failed to read response body: {e}");
            FetchResult::Transient
        }
    }
}

/// Refreshes the Claude OAuth token, persists it to `path` if `expected_mtime` still matches, and returns `(access_token, expires_ms)`.
///
/// Falls back to subsequent `token_urls` only on transport error or HTTP 404/405, not on 400/401.
fn refresh_claude(
    client: &Client,
    path: &Path,
    refresh_token: &str,
    scopes: &[String],
    expected_mtime: Option<SystemTime>,
    token_urls: &[String],
) -> Result<(String, i64)> {
    let mut body = json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLAUDE_CLIENT_ID,
    });
    // Re-send only previously granted scopes; omitting `scope` preserves the original grant instead of narrowing it.
    if !scopes.is_empty() {
        body["scope"] = json!(scopes.join(" "));
    }

    let mut parsed: Option<ClaudeRefreshResponse> = None;
    let mut last_err: Option<anyhow::Error> = None;
    for (i, url) in token_urls.iter().enumerate() {
        let has_next = i + 1 < token_urls.len();
        let req = client
            .post(url)
            .header(reqwest::header::USER_AGENT, claude_ua())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body);
        match send_refresh(req) {
            Ok((status, text)) if status.is_success() => {
                parsed = Some(
                    serde_json::from_str(&text)
                        .context("Failed to parse Claude refresh response")?,
                );
                break;
            }
            Ok((status, _)) => {
                let moved = status == reqwest::StatusCode::NOT_FOUND
                    || status == reqwest::StatusCode::METHOD_NOT_ALLOWED;
                if moved && has_next {
                    last_err = Some(anyhow!("claude token refresh returned status {status}"));
                    continue;
                }
                bail!("claude token refresh returned status {status}");
            }
            Err(e) => {
                if has_next {
                    last_err = Some(e);
                    continue;
                }
                return Err(e);
            }
        }
    }
    let resp =
        parsed.ok_or_else(|| last_err.unwrap_or_else(|| anyhow!("claude token refresh failed")))?;
    let access = resp
        .access_token
        .clone()
        .filter(|s| !s.is_empty())
        .context("claude refresh response had no access_token")?;
    let expires_in = resp.expires_in.unwrap_or(28_800);
    let expires_ms = chrono::Local::now().timestamp_millis() + expires_in * 1000;

    // Write the rotated token back, preserving designOauth and any unknown keys.
    let new_refresh = resp.refresh_token.clone();
    let new_scope = resp.scope.clone();
    let wrote = update_json_file_in_place(path, expected_mtime, |root| {
        let o = root
            .get_mut("claudeAiOauth")
            .and_then(|v| v.as_object_mut())
            .context("credentials missing claudeAiOauth object")?;
        o.insert("accessToken".into(), json!(access));
        if let Some(r) = &new_refresh
            && !r.is_empty()
        {
            o.insert("refreshToken".into(), json!(r));
        }
        o.insert("expiresAt".into(), json!(expires_ms));
        if let Some(sc) = &new_scope {
            let list: Vec<&str> = sc.split_whitespace().collect();
            if !list.is_empty() {
                o.insert("scopes".into(), json!(list));
            }
        }
        Ok(())
    })?;
    // Server-side rotation already invalidated the old refresh token, so failing to persist leaves the file broken; fail rather than caching an ephemeral access token.
    if !wrote {
        bail!("claude token rotated but the new token could not be persisted");
    }
    Ok((access, expires_ms))
}

/// The result of obtaining a usable access token.
enum EnsureToken {
    Token(String),
    NeedsLogin,
    Transient,
}

/// Per-worker Claude state: an in-memory access token + refresh backoff.
#[derive(Default)]
pub struct ClaudeState {
    /// Cached `(access_token, expires_at_ms, credential_mtime)`; invalidated whenever the credentials file mtime changes.
    token: Option<(String, i64, Option<SystemTime>)>,
    cooldown: RefreshCooldown,
}

impl ClaudeState {
    /// One worker tick: ensure a token, fetch usage, refresh reactively on 401.
    pub fn resolve(&mut self, client: &Client) -> QuotaOutcome<ClaudeQuotaSnapshot> {
        let now_secs = chrono::Local::now().timestamp();
        let now_ms = chrono::Local::now().timestamp_millis();
        let path = match get_claude_credentials_path() {
            Ok(p) => p,
            Err(_) => return QuotaOutcome::Transient,
        };

        let token = match self.ensure_token(client, &path, now_secs, now_ms) {
            EnsureToken::Token(t) => t,
            EnsureToken::NeedsLogin => return QuotaOutcome::NeedsLogin,
            EnsureToken::Transient => return QuotaOutcome::Transient,
        };

        match fetch_claude_usage(client, &token, now_secs, CLAUDE_USAGE_URL) {
            FetchResult::Ok(mut snap) => {
                self.cooldown.clear();
                snap.plan_type = read_claude_plan(&path);
                QuotaOutcome::Data(snap)
            }
            FetchResult::Unauthorized => {
                let mtime = file_mtime(&path);
                if self.cooldown.active(now_secs, mtime) {
                    self.token = None;
                    return QuotaOutcome::NeedsLogin;
                }
                match self.force_refresh(client, &path) {
                    Some(t) => match fetch_claude_usage(client, &t, now_secs, CLAUDE_USAGE_URL) {
                        FetchResult::Ok(mut snap) => {
                            self.cooldown.clear();
                            snap.plan_type = read_claude_plan(&path);
                            QuotaOutcome::Data(snap)
                        }
                        // Keep the freshly refreshed token and last-known-good snapshot on a transient retry error.
                        FetchResult::Transient => QuotaOutcome::Transient,
                        // Re-read mtime because the successful refresh above just rewrote the file.
                        FetchResult::Unauthorized => {
                            self.cooldown.arm(now_secs, file_mtime(&path));
                            self.token = None;
                            QuotaOutcome::NeedsLogin
                        }
                    },
                    None => {
                        self.cooldown.arm(now_secs, file_mtime(&path));
                        self.token = None;
                        QuotaOutcome::NeedsLogin
                    }
                }
            }
            FetchResult::Transient => QuotaOutcome::Transient,
        }
    }

    /// Returns a usable access token, refreshing proactively near expiry.
    fn ensure_token(
        &mut self,
        client: &Client,
        path: &Path,
        now_secs: i64,
        now_ms: i64,
    ) -> EnsureToken {
        if let Some((tok, exp_ms, cred_mtime)) = &self.token
            && exp_ms - now_ms > EXPIRY_SKEW_SECS * 1000
            && *cred_mtime == file_mtime(path)
        {
            return EnsureToken::Token(tok.clone());
        }

        // Treat an absent or unreadable file as transient (e.g. an atomic rewrite in flight), but unparseable JSON as an auth failure.
        let body = match std::fs::read_to_string(path) {
            Ok(b) => b,
            Err(_) => return EnsureToken::Transient,
        };
        let oauth = match parse_claude_oauth(&body) {
            Some(o) => o,
            None => return EnsureToken::NeedsLogin,
        };
        let access = oauth.access_token.clone().filter(|s| !s.is_empty());
        let expires_secs = oauth.expires_at.map(|ms| ms / 1000);
        let need_refresh =
            access.is_none() || is_expiring(expires_secs, now_secs, EXPIRY_SKEW_SECS);

        if need_refresh {
            let mtime = file_mtime(path);
            if self.cooldown.active(now_secs, mtime) {
                return EnsureToken::NeedsLogin;
            }
            match self.force_refresh(client, path) {
                Some(t) => EnsureToken::Token(t),
                None => {
                    // Re-read: a partial write could have changed the mtime.
                    self.cooldown.arm(now_secs, file_mtime(path));
                    EnsureToken::NeedsLogin
                }
            }
        } else {
            let tok = access.expect("access is some when need_refresh is false");
            self.token = Some((
                tok.clone(),
                oauth.expires_at.unwrap_or(now_ms),
                file_mtime(path),
            ));
            EnsureToken::Token(tok)
        }
    }

    /// Reads the refresh token from `path` with its mtime guard and refreshes the access token once.
    fn force_refresh(&mut self, client: &Client, path: &Path) -> Option<String> {
        let expected_mtime = file_mtime(path);
        let oauth = read_claude_oauth(path)?;
        let refresh_token = oauth.refresh_token.filter(|s| !s.is_empty())?;
        match refresh_claude(
            client,
            path,
            &refresh_token,
            &oauth.scopes,
            expected_mtime,
            &claude_token_urls_default(),
        ) {
            Ok((access, expires_ms)) => {
                self.cooldown.clear();
                self.token = Some((access.clone(), expires_ms, file_mtime(path)));
                Some(access)
            }
            Err(e) => {
                log::warn!("claude token refresh failed: {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "five_hour": { "utilization": 5.0, "resets_at": "2026-07-03T17:09:59.651608+00:00" },
      "seven_day": { "utilization": 34.0, "resets_at": "2026-07-09T10:59:59.651631+00:00" },
      "limits": []
    }"#;

    #[test]
    fn maps_claude_usage() {
        let snap = map_claude_usage(SAMPLE, 1_000_000).unwrap();
        assert_eq!(snap.source, QuotaSource::Api);
        assert_eq!(snap.five_hour.as_ref().unwrap().used_percent, 5.0);
        assert!(snap.five_hour.as_ref().unwrap().resets_at_unix.unwrap() > 0);
        assert_eq!(snap.seven_day.as_ref().unwrap().used_percent, 34.0);
        assert!(!snap.needs_login);
    }

    #[test]
    fn maps_missing_windows() {
        let snap = map_claude_usage("{}", 5).unwrap();
        assert!(snap.five_hour.is_none());
        assert!(snap.seven_day.is_none());
        assert!(snap.scoped_weekly.is_none());
        assert!(snap.balance.is_none());
        assert!(!snap.limit_reached);
    }

    // The richer body returned once the `anthropic-beta` header is sent.
    const FULL: &str = r#"{
      "five_hour": { "utilization": 15.0, "resets_at": "2026-07-03T22:09:59.594819+00:00" },
      "seven_day": { "utilization": 40.0, "resets_at": "2026-07-09T10:59:59.594840+00:00" },
      "limits": [
        { "kind": "session", "group": "session", "percent": 15, "severity": "normal", "is_active": false },
        { "kind": "weekly_all", "group": "weekly", "percent": 40, "severity": "normal", "is_active": false },
        { "kind": "weekly_scoped", "group": "weekly", "percent": 60, "severity": "normal",
          "resets_at": "2026-07-09T10:59:59.595109+00:00",
          "scope": { "model": { "id": null, "display_name": "Fable" } }, "is_active": true }
      ],
      "spend": {
        "used": { "amount_minor": 0, "currency": "USD", "exponent": 2 },
        "limit": null, "enabled": false, "balance": null
      }
    }"#;

    #[test]
    fn maps_scoped_weekly_and_spend() {
        let snap = map_claude_usage(FULL, 1_000_000).unwrap();
        // 5h / 7d still come from the top-level windows.
        assert_eq!(snap.five_hour.as_ref().unwrap().used_percent, 15.0);
        assert_eq!(snap.seven_day.as_ref().unwrap().used_percent, 40.0);
        // The per-model weekly cap comes from the active weekly_scoped limit.
        let scoped = snap.scoped_weekly.as_ref().unwrap();
        assert_eq!(scoped.used_percent, 60.0);
        assert!(scoped.resets_at_unix.unwrap() > 0);
        assert_eq!(snap.scoped_label.as_deref(), Some("Fable"));
        // Spend disabled: no balance, but the used amount is formatted.
        assert!(snap.balance.is_none());
        assert_eq!(snap.spend_used.as_deref(), Some("$0.00"));
        assert!(!snap.limit_reached);
    }

    #[test]
    fn flags_limit_and_balance() {
        let body = r#"{
          "five_hour": { "utilization": 100.0 },
          "limits": [ { "kind": "weekly_scoped", "percent": 30, "severity": "normal",
                        "scope": { "model": { "display_name": "Opus" } }, "is_active": true } ],
          "spend": { "balance": { "amount_minor": 500, "currency": "USD", "exponent": 2 }, "enabled": true }
        }"#;
        let snap = map_claude_usage(body, 1).unwrap();
        assert!(snap.limit_reached, "5h at 100% trips the LIMIT flag");
        assert_eq!(snap.balance.as_deref(), Some("$5.00"));
        assert_eq!(snap.scoped_label.as_deref(), Some("Opus"));
    }

    #[test]
    fn scoped_label_is_carried_whole() {
        // Anthropic sends a full display name; shortening is left to the panel renderer.
        let body = r#"{
          "limits": [ { "kind": "weekly_scoped", "percent": 30, "is_active": true,
                        "scope": { "model": { "display_name": "Claude Opus 4.5" } } } ]
        }"#;
        let snap = map_claude_usage(body, 1).unwrap();
        assert_eq!(snap.scoped_label.as_deref(), Some("Claude Opus 4.5"));
    }

    #[test]
    fn scoped_hidden_without_model_label() {
        // A `weekly_scoped` entry without a model name is silently omitted.
        let body = r#"{
          "limits": [ { "kind": "weekly_scoped", "percent": 55, "is_active": true } ]
        }"#;
        let snap = map_claude_usage(body, 1).unwrap();
        assert!(snap.scoped_weekly.is_none());
        assert!(snap.scoped_label.is_none());
    }

    #[test]
    fn no_scoped_limit_is_silent() {
        // Without any `weekly_scoped` limit, 5h and 7d still map without error.
        let body = r#"{
          "five_hour": { "utilization": 10.0 },
          "seven_day": { "utilization": 20.0 },
          "limits": [ { "kind": "session", "percent": 10 }, { "kind": "weekly_all", "percent": 20 } ]
        }"#;
        let snap = map_claude_usage(body, 1).unwrap();
        assert!(snap.scoped_weekly.is_none());
        assert!(snap.scoped_label.is_none());
        assert!(snap.five_hour.is_some());
        assert!(snap.seven_day.is_some());
    }

    #[test]
    fn scoped_prefers_active_over_higher_percent() {
        let body = r#"{
          "limits": [
            { "kind": "weekly_scoped", "percent": 90, "scope": { "model": { "display_name": "Haiku" } }, "is_active": false },
            { "kind": "weekly_scoped", "percent": 20, "scope": { "model": { "display_name": "Opus" } }, "is_active": true }
          ]
        }"#;
        let snap = map_claude_usage(body, 1).unwrap();
        assert_eq!(snap.scoped_label.as_deref(), Some("Opus"));
        assert_eq!(snap.scoped_weekly.unwrap().used_percent, 20.0);
    }

    #[test]
    fn prettifies_tier() {
        assert_eq!(prettify_tier("default_claude_max_20x"), "max 20x");
        assert_eq!(prettify_tier("default_claude_pro"), "pro");
        assert_eq!(prettify_tier("max"), "max");
    }

    #[test]
    fn reads_plan_prefers_rate_limit_tier() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".credentials.json");
        std::fs::write(
            &p,
            r#"{"claudeAiOauth":{"accessToken":"x","rateLimitTier":"default_claude_max_20x","subscriptionType":"max"}}"#,
        )
        .unwrap();
        assert_eq!(read_claude_plan(&p).as_deref(), Some("max 20x"));
        // Falls back to subscriptionType when rateLimitTier is absent.
        std::fs::write(
            &p,
            r#"{"claudeAiOauth":{"accessToken":"x","subscriptionType":"pro"}}"#,
        )
        .unwrap();
        assert_eq!(read_claude_plan(&p).as_deref(), Some("pro"));
        // Neither present -> None (Plan line omitted), no error.
        std::fs::write(&p, r#"{"claudeAiOauth":{"accessToken":"x"}}"#).unwrap();
        assert_eq!(read_claude_plan(&p), None);
    }

    #[test]
    fn malformed_credentials_need_login_not_transient() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        let client = build_client().unwrap();
        let mut state = ClaudeState::default();
        assert!(matches!(
            state.ensure_token(&client, &path, 1_000_000, 1_000_000_000),
            EnsureToken::NeedsLogin
        ));
    }

    #[test]
    fn unreadable_credentials_stay_transient() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");
        let client = build_client().unwrap();
        let mut state = ClaudeState::default();
        assert!(matches!(
            state.ensure_token(&client, &path, 1_000_000, 1_000_000_000),
            EnsureToken::Transient
        ));
    }

    #[test]
    fn malformed_scoped_never_breaks_the_body() {
        // Null `percent` drops the scoped entry while null 5h `utilization` maps to 0.
        let body = r#"{
          "five_hour": { "utilization": null, "resets_at": "2026-07-03T22:00:00+00:00" },
          "seven_day": { "utilization": 40.0 },
          "limits": [
            { "kind": "session", "percent": 15 },
            { "kind": "weekly_scoped", "percent": null, "scope": { "model": { "display_name": "Fable" } } }
          ]
        }"#;
        let snap = map_claude_usage(body, 1).unwrap();
        assert!(snap.five_hour.is_some());
        assert_eq!(snap.five_hour.as_ref().unwrap().used_percent, 0.0);
        assert_eq!(snap.seven_day.as_ref().unwrap().used_percent, 40.0);
        assert!(
            snap.scoped_weekly.is_none(),
            "null-percent scoped entry is dropped, not fatal"
        );
    }

    #[test]
    fn limits_non_array_is_tolerated() {
        let snap =
            map_claude_usage(r#"{"five_hour":{"utilization":5.0},"limits":null}"#, 1).unwrap();
        assert!(snap.five_hour.is_some());
        assert!(snap.scoped_weekly.is_none());
    }

    // ---- HTTP-layer tests against a local mock server (no real API) ----

    use crate::quota::http::build_client;
    use httpmock::prelude::*;

    #[test]
    fn fetch_claude_usage_maps_200_body() {
        let server = MockServer::start();
        let endpoint = server.mock(|when, then| {
            when.method(GET).path("/usage");
            then.status(200).body(SAMPLE);
        });
        let client = build_client().unwrap();

        let result = fetch_claude_usage(&client, "tok", 1_000_000, &server.url("/usage"));
        endpoint.assert();
        match result {
            FetchResult::Ok(snap) => {
                assert_eq!(snap.five_hour.as_ref().unwrap().used_percent, 5.0);
                assert_eq!(snap.seven_day.as_ref().unwrap().used_percent, 34.0);
            }
            _ => panic!("expected FetchResult::Ok"),
        }
    }

    #[test]
    fn fetch_claude_usage_401_is_unauthorized() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/usage");
            then.status(401);
        });
        let client = build_client().unwrap();
        let result = fetch_claude_usage(&client, "tok", 0, &server.url("/usage"));
        assert!(matches!(result, FetchResult::Unauthorized));
    }

    #[test]
    fn refresh_claude_falls_back_primary_to_legacy_and_writes_back() {
        let server = MockServer::start();
        // Primary host answers 404 (moved) → the fetcher must try the legacy host.
        let primary = server.mock(|when, then| {
            when.method(POST).path("/primary");
            then.status(404);
        });
        let legacy = server.mock(|when, then| {
            when.method(POST).path("/legacy");
            then.status(200).json_body(serde_json::json!({
                "access_token": "fresh-access",
                "refresh_token": "fresh-refresh",
                "expires_in": 3600
            }));
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(
            &path,
            r#"{"claudeAiOauth":{"accessToken":"old","refreshToken":"old-refresh","scopes":[]}}"#,
        )
        .unwrap();

        let client = build_client().unwrap();
        let urls = vec![server.url("/primary"), server.url("/legacy")];
        let (access, expires_ms) =
            refresh_claude(&client, &path, "old-refresh", &[], file_mtime(&path), &urls)
                .expect("refresh should fall back to the legacy host");

        primary.assert();
        legacy.assert();
        assert_eq!(access, "fresh-access");
        assert!(expires_ms > 0);

        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["claudeAiOauth"]["accessToken"], "fresh-access");
        assert_eq!(written["claudeAiOauth"]["refreshToken"], "fresh-refresh");
    }

    #[test]
    fn fetch_claude_raw_from_reads_creds_and_returns_status_body() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/usage");
            then.status(200).body(SAMPLE);
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        std::fs::write(&path, r#"{"claudeAiOauth":{"accessToken":"tok"}}"#).unwrap();

        let client = build_client().unwrap();
        let (status, body) =
            fetch_claude_raw_from(&client, &server.url("/usage"), &path).expect("raw fetch");
        assert_eq!(status, 200);
        assert!(body.contains("five_hour"));
    }
}
