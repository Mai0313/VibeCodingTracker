//! Claude Code quota fetcher.
//!
//! Reads the OAuth access token from `~/.claude/.credentials.json`, calls the
//! official usage API (`GET /api/oauth/usage`), and — when the token is
//! near-expiry or rejected — refreshes it against the Claude token endpoint and
//! writes the rotated token back, preserving every other field. A refreshed
//! access token is cached in memory so the 10s worker reuses it rather than
//! refreshing each tick. A refresh failure arms a cooldown (so a revoked token
//! cannot spin the endpoint) and surfaces a `claude auth login` hint.

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
use std::time::SystemTime;

/// The Claude OAuth usage endpoint.
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// The Claude Code OAuth client id (public PKCE client).
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Current token endpoint host.
const CLAUDE_TOKEN_URL_PRIMARY: &str = "https://platform.claude.com/v1/oauth/token";
/// Legacy token endpoint host (fallback on 404/405 from the primary).
const CLAUDE_TOKEN_URL_LEGACY: &str = "https://console.anthropic.com/v1/oauth/token";
/// Login hint shown when refresh fails.
pub const CLAUDE_LOGIN_HINT: &str = "run: claude auth login";

/// The token endpoints to try, in order (overridable for testing / drift).
fn claude_token_urls() -> Vec<String> {
    if let Ok(u) = std::env::var("VCT_CLAUDE_TOKEN_URL")
        && !u.is_empty()
    {
        return vec![u];
    }
    vec![
        CLAUDE_TOKEN_URL_PRIMARY.to_string(),
        CLAUDE_TOKEN_URL_LEGACY.to_string(),
    ]
}

/// Reads the `claudeAiOauth` block from the credentials file.
fn read_claude_oauth(path: &Path) -> Option<ClaudeOauth> {
    let body = std::fs::read_to_string(path).ok()?;
    let creds: ClaudeCredentials = serde_json::from_str(&body).ok()?;
    creds.claude_ai_oauth
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
    Ok(ClaudeQuotaSnapshot {
        source: QuotaSource::Api,
        fetched_at: now,
        five_hour: resp.five_hour.as_ref().map(win),
        seven_day: resp.seven_day.as_ref().map(win),
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

/// Calls the usage API with `token`.
fn fetch_claude_usage(client: &Client, token: &str, now: i64) -> FetchResult {
    let resp = match client.get(CLAUDE_USAGE_URL).bearer_auth(token).send() {
        Ok(r) => r,
        Err(_) => return FetchResult::Transient,
    };
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return FetchResult::Unauthorized;
    }
    if !status.is_success() {
        // 403 and friends are not treated as "needs login" (S9).
        return FetchResult::Transient;
    }
    match resp.text() {
        Ok(text) => match map_claude_usage(&text, now) {
            Ok(snap) => FetchResult::Ok(snap),
            Err(_) => FetchResult::Transient,
        },
        Err(_) => FetchResult::Transient,
    }
}

/// Refreshes the Claude token and writes it back. Returns `(access, expires_ms)`.
///
/// Tries the primary host first, falling back to the legacy host only on
/// 404/405 (a moved endpoint), never on 400/401 (`invalid_grant`, meaning the
/// refresh token is stale — likely rotated by a running Claude Code).
fn refresh_claude(
    client: &Client,
    path: &Path,
    refresh_token: &str,
    scopes: &[String],
    expected_mtime: Option<SystemTime>,
) -> Result<(String, i64)> {
    let scope = if scopes.is_empty() {
        "user:inference".to_string()
    } else {
        scopes.join(" ")
    };
    let body = json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": CLAUDE_CLIENT_ID,
        "scope": scope,
    });

    let urls = claude_token_urls();
    let mut parsed: Option<ClaudeRefreshResponse> = None;
    let mut last_err: Option<anyhow::Error> = None;
    for (i, url) in urls.iter().enumerate() {
        let has_next = i + 1 < urls.len();
        let req = client
            .post(url)
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
    // The refresh already rotated the refresh token server-side; if we could not
    // persist the new one, the file now holds a stale/invalid refresh token, so
    // treat it as a refresh failure (surface needs_login) rather than caching an
    // ephemeral access token over a broken login.
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
    /// Cached fresh token: `(access_token, expires_at_ms, credential-file mtime)`.
    /// The mtime pins the cache to the file it came from, so a re-login / account
    /// switch invalidates it even while the old token is unexpired.
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

        match fetch_claude_usage(client, &token, now_secs) {
            FetchResult::Ok(snap) => {
                self.cooldown.clear();
                QuotaOutcome::Data(snap)
            }
            FetchResult::Unauthorized => {
                let mtime = file_mtime(&path);
                if self.cooldown.active(now_secs, mtime) {
                    self.token = None;
                    return QuotaOutcome::NeedsLogin;
                }
                match self.force_refresh(client, &path, mtime) {
                    Some(t) => match fetch_claude_usage(client, &t, now_secs) {
                        FetchResult::Ok(snap) => {
                            self.cooldown.clear();
                            QuotaOutcome::Data(snap)
                        }
                        // Arm with the CURRENT mtime (re-read): a successful
                        // refresh just rewrote the file, so `mtime` is stale and
                        // arming with it would never suppress the next tick.
                        _ => {
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
        // In-memory token still valid AND the credential file unchanged since we
        // cached it? A re-login / account switch rewrites `.credentials.json`, so
        // the cached token must be dropped even while it is unexpired.
        if let Some((tok, exp_ms, cred_mtime)) = &self.token
            && exp_ms - now_ms > EXPIRY_SKEW_SECS * 1000
            && *cred_mtime == file_mtime(path)
        {
            return EnsureToken::Token(tok.clone());
        }

        let oauth = match read_claude_oauth(path) {
            Some(o) => o,
            None => return EnsureToken::Transient,
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
            match self.force_refresh(client, path, mtime) {
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

    /// Re-reads the file for the freshest refresh token and refreshes once.
    fn force_refresh(
        &mut self,
        client: &Client,
        path: &Path,
        mtime: Option<SystemTime>,
    ) -> Option<String> {
        let oauth = read_claude_oauth(path)?;
        let refresh_token = oauth.refresh_token.filter(|s| !s.is_empty())?;
        match refresh_claude(client, path, &refresh_token, &oauth.scopes, mtime) {
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
    }
}
