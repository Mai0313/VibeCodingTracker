//! Codex `wham/usage` API client + token refresh.
//!
//! Reads the bearer token + account id from `~/.codex/auth.json`, calls the
//! ChatGPT backend usage endpoint, and maps the response into the normalized
//! [`CodexQuotaSnapshot`]. On a 401 the caller refreshes the token via
//! [`refresh_codex`] (which rotates and writes back the refresh token) and
//! retries. Tokens are never logged nor stored in a `Debug`-formatted struct;
//! only the HTTP status appears in errors.

use crate::models::{
    CodexAuthJson, CodexQuotaSnapshot, CodexRefreshResponse, QuotaSource, QuotaWindow,
    WhamUsageResponse, WhamWindow,
};
use crate::quota::refresh::{
    file_mtime, now_rfc3339_utc_nanos, send_refresh, update_json_file_in_place,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::OnceLock;

/// The ChatGPT backend usage endpoint.
const WHAM_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// The Codex OAuth token endpoint.
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// The Codex OAuth client id (public PKCE client).
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Fallback Codex CLI version for the User-Agent when `codex --version` cannot be
/// resolved. Bump occasionally.
const CODEX_FALLBACK_VERSION: &str = "0.142.5";
/// Codex CLI originator (headless invocation; the TUI reports `codex-tui`). Sent
/// only for client-identity parity; the ChatGPT usage endpoint does not need it.
const CODEX_ORIGINATOR: &str = "codex_cli_rs";

/// Builds the Codex CLI request User-Agent, e.g. `codex_cli_rs/0.142.5 (linux; x86_64)`.
///
/// The version is detected from the installed CLI (see
/// [`crate::quota::http::detect_cli_version`]); os/arch come from the build target.
fn codex_ua() -> &'static str {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        format!(
            "codex_cli_rs/{} ({}; {})",
            crate::quota::http::detect_cli_version(
                "codex",
                "codex_version.json",
                CODEX_FALLBACK_VERSION
            ),
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
    })
    .as_str()
}

/// Maps a wham/usage window into the normalized [`QuotaWindow`].
fn map_window(w: &WhamWindow, now: i64) -> QuotaWindow {
    let resets_at_unix = w
        .reset_at
        .or_else(|| w.reset_after_seconds.map(|s| now + s));
    QuotaWindow {
        used_percent: w.used_percent.unwrap_or(0.0),
        resets_at_unix,
    }
}

/// Maps a wham/usage response body into a [`CodexQuotaSnapshot`] (pure).
///
/// # Errors
///
/// Returns an error if the body is not valid JSON in the expected shape.
pub fn map_wham_response(body: &str, now: i64) -> Result<CodexQuotaSnapshot> {
    let resp: WhamUsageResponse =
        serde_json::from_str(body).context("Failed to parse wham/usage response")?;

    let (primary, secondary, rate_reached) = match &resp.rate_limit {
        Some(rl) => (
            rl.primary_window.as_ref().map(|w| map_window(w, now)),
            rl.secondary_window.as_ref().map(|w| map_window(w, now)),
            rl.limit_reached,
        ),
        None => (None, None, None),
    };

    let (credits_balance, has_credits, unlimited, overage, approx_messages) = match &resp.credits {
        Some(c) => (
            c.balance.clone(),
            c.has_credits,
            c.unlimited,
            c.overage_limit_reached,
            approx_pair(&c.approx_local_messages),
        ),
        None => (None, None, None, None, None),
    };

    let (spend_reached, spend_limit) = match &resp.spend_control {
        Some(s) => (s.reached, s.individual_limit),
        None => (None, None),
    };

    // A limit is "reached" if the rate window, the credit overage, or the spend
    // cap says so. Stay `None` only when no source reports at all.
    let reached = [rate_reached, overage, spend_reached];
    let limit_reached = reached
        .iter()
        .any(Option::is_some)
        .then(|| reached.contains(&Some(true)));

    Ok(CodexQuotaSnapshot {
        source: QuotaSource::Api,
        fetched_at: now,
        plan_type: resp.plan_type,
        primary,
        secondary,
        credits_balance,
        has_credits,
        unlimited,
        reset_credits_available: resp
            .rate_limit_reset_credits
            .and_then(|r| r.available_count),
        approx_messages,
        spend_limit,
        limit_reached,
        needs_login: false,
    })
}

/// Extracts an `[low, high]` approximate-messages pair, dropping the all-zero
/// case (no credits → nothing useful to show).
fn approx_pair(v: &Option<Vec<i64>>) -> Option<(i64, i64)> {
    let v = v.as_ref()?;
    let low = *v.first()?;
    let high = *v.get(1).unwrap_or(&low);
    (high > 0).then_some((low, high))
}

/// Parses `~/.codex/auth.json`, returning `(access_token, account_id)` (pure).
///
/// # Errors
///
/// Returns an error if the JSON is malformed or has no `tokens.access_token`.
pub fn parse_auth(body: &str) -> Result<(String, Option<String>)> {
    let auth: CodexAuthJson = serde_json::from_str(body).context("Failed to parse auth.json")?;
    let tokens = auth.tokens.context("auth.json has no tokens")?;
    let access_token = tokens
        .access_token
        .filter(|t| !t.is_empty())
        .context("auth.json has no tokens.access_token")?;
    Ok((access_token, tokens.account_id))
}

/// Outcome of a single wham/usage request.
pub enum WhamResult {
    /// Mapped snapshot.
    Ok(CodexQuotaSnapshot),
    /// 401 → refresh and retry.
    Unauthorized,
    /// Network / non-401 error → keep last-known-good.
    Transient,
}

/// Calls the wham endpoint with an explicit bearer token.
pub fn call_wham(
    client: &reqwest::blocking::Client,
    token: &str,
    account_id: Option<&str>,
    now: i64,
) -> WhamResult {
    let mut req = client
        .get(WHAM_URL)
        .header(reqwest::header::USER_AGENT, codex_ua())
        .header("originator", CODEX_ORIGINATOR)
        .bearer_auth(token);
    if let Some(id) = account_id {
        req = req.header("ChatGPT-Account-Id", id);
    }
    let resp = match req.send() {
        Ok(r) => r,
        Err(_) => return WhamResult::Transient,
    };
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return WhamResult::Unauthorized;
    }
    if !status.is_success() {
        return WhamResult::Transient;
    }
    match resp.text() {
        Ok(text) => match map_wham_response(&text, now) {
            Ok(snap) => WhamResult::Ok(snap),
            Err(_) => WhamResult::Transient,
        },
        Err(_) => WhamResult::Transient,
    }
}

/// Refreshes the Codex token and writes it back (rotation-safe). Returns the new
/// access token.
///
/// The refresh token rotates: the response's new refresh token must be persisted
/// or the next refresh reuses the old one and 401s. The write-back re-checks the
/// file mtime and aborts if a concurrent Codex CLI rotated it first.
///
/// # Errors
///
/// Returns an error if the auth file has no refresh token, the request fails, or
/// the status is non-success. The token never appears in an error.
pub fn refresh_codex(client: &reqwest::blocking::Client, auth_path: &Path) -> Result<String> {
    // Capture the mtime with the refresh token from the same read so the
    // write-back guards on the exact file version we send.
    let expected_mtime = file_mtime(auth_path);
    let body = std::fs::read_to_string(auth_path)
        .with_context(|| format!("Failed to read {}", auth_path.display()))?;
    let root: Value = serde_json::from_str(&body).context("Failed to parse auth.json")?;
    let refresh_token = root["tokens"]["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("auth.json has no tokens.refresh_token")?
        .to_string();

    let req_body = json!({
        "client_id": CODEX_CLIENT_ID,
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
    });
    let req = client
        .post(CODEX_TOKEN_URL)
        .header(reqwest::header::USER_AGENT, codex_ua())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&req_body);
    let (status, text) = send_refresh(req)?;
    if !status.is_success() {
        bail!("codex token refresh returned status {status}");
    }
    let parsed: CodexRefreshResponse =
        serde_json::from_str(&text).context("Failed to parse Codex refresh response")?;
    let access = parsed
        .access_token
        .clone()
        .filter(|s| !s.is_empty())
        .context("codex refresh response had no access_token")?;

    let wrote = update_json_file_in_place(auth_path, expected_mtime, |root| {
        let t = root
            .get_mut("tokens")
            .and_then(|v| v.as_object_mut())
            .context("auth.json missing tokens object")?;
        t.insert("access_token".into(), json!(access));
        if let Some(i) = &parsed.id_token
            && !i.is_empty()
        {
            t.insert("id_token".into(), json!(i));
        }
        if let Some(r) = &parsed.refresh_token
            && !r.is_empty()
        {
            t.insert("refresh_token".into(), json!(r));
        }
        root["last_refresh"] = json!(now_rfc3339_utc_nanos());
        Ok(())
    })?;
    // The refresh already rotated the refresh token server-side; if we could not
    // persist the new one, auth.json now holds a stale/invalid refresh token, so
    // treat it as a refresh failure rather than reporting success.
    if !wrote {
        bail!("codex token rotated but the new token could not be persisted");
    }
    Ok(access)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "plan_type": "plus",
      "rate_limit": {
        "allowed": true, "limit_reached": false,
        "primary_window":   { "used_percent": 27, "limit_window_seconds": 18000,  "reset_after_seconds": 16816, "reset_at": 1782770922 },
        "secondary_window": { "used_percent": 4,  "limit_window_seconds": 604800, "reset_after_seconds": 603616, "reset_at": 1783357722 }
      },
      "credits": { "has_credits": false, "unlimited": false, "balance": "0" },
      "rate_limit_reset_credits": { "available_count": 2 }
    }"#;

    #[test]
    fn maps_full_response() {
        let snap = map_wham_response(SAMPLE, 1_000_000).unwrap();
        assert_eq!(snap.source, QuotaSource::Api);
        assert_eq!(snap.plan_type.as_deref(), Some("plus"));
        assert_eq!(snap.primary.as_ref().unwrap().used_percent, 27.0);
        assert_eq!(
            snap.primary.as_ref().unwrap().resets_at_unix,
            Some(1782770922)
        );
        assert_eq!(snap.secondary.as_ref().unwrap().used_percent, 4.0);
        assert_eq!(snap.credits_balance.as_deref(), Some("0"));
        assert_eq!(snap.has_credits, Some(false));
        assert_eq!(snap.reset_credits_available, Some(2));
        assert_eq!(snap.limit_reached, Some(false));
        assert!(!snap.needs_login);
    }

    #[test]
    fn accepts_numeric_credit_balance() {
        let body = r#"{"plan_type":"plus","credits":{"has_credits":true,"balance":12}}"#;
        let snap = map_wham_response(body, 1_000).unwrap();
        assert_eq!(snap.credits_balance.as_deref(), Some("12"));
        assert_eq!(snap.has_credits, Some(true));
    }

    #[test]
    fn window_uses_relative_reset_when_no_absolute() {
        let body =
            r#"{"rate_limit":{"primary_window":{"used_percent":10,"reset_after_seconds":100}}}"#;
        let snap = map_wham_response(body, 1_000).unwrap();
        assert_eq!(snap.primary.unwrap().resets_at_unix, Some(1_100));
    }

    #[test]
    fn parse_auth_extracts_token_and_account() {
        let body = r#"{"tokens":{"access_token":"tok","account_id":"acct"}}"#;
        let (tok, acct) = parse_auth(body).unwrap();
        assert_eq!(tok, "tok");
        assert_eq!(acct.as_deref(), Some("acct"));
    }

    #[test]
    fn parse_auth_errors_without_token() {
        assert!(parse_auth(r#"{"tokens":{"account_id":"acct"}}"#).is_err());
        assert!(parse_auth(r#"{}"#).is_err());
    }

    #[test]
    fn maps_spend_control_and_approx_messages() {
        let body = r#"{
          "plan_type": "plus",
          "credits": { "balance": "12", "has_credits": true, "overage_limit_reached": false,
                       "approx_local_messages": [120, 150], "approx_cloud_messages": [0, 0] },
          "spend_control": { "reached": false, "individual_limit": 50.0 }
        }"#;
        let snap = map_wham_response(body, 1_000).unwrap();
        assert_eq!(snap.approx_messages, Some((120, 150)));
        assert_eq!(snap.spend_limit, Some(50.0));
        assert_eq!(snap.limit_reached, Some(false));
    }

    #[test]
    fn overage_or_spend_trips_limit_reached() {
        let body = r#"{
          "rate_limit": { "limit_reached": false, "primary_window": { "used_percent": 10 } },
          "credits": { "overage_limit_reached": true },
          "spend_control": { "reached": false }
        }"#;
        let snap = map_wham_response(body, 1).unwrap();
        assert_eq!(snap.limit_reached, Some(true));
    }

    #[test]
    fn zero_approx_messages_are_dropped() {
        let body = r#"{ "credits": { "approx_local_messages": [0, 0] } }"#;
        let snap = map_wham_response(body, 1).unwrap();
        assert!(snap.approx_messages.is_none());
    }

    #[test]
    fn limit_reached_is_none_when_unreported() {
        // No rate_limit / credits / spend_control at all → stays None.
        let snap = map_wham_response(r#"{"plan_type":"plus"}"#, 1).unwrap();
        assert_eq!(snap.limit_reached, None);
    }
}
