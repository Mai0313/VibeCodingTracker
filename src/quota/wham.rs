//! Codex `wham/usage` API client.
//!
//! Reads the bearer token + account id from `~/.codex/auth.json`, calls the
//! ChatGPT backend usage endpoint, and maps the response into the normalized
//! [`CodexQuotaSnapshot`]. The token is never logged nor stored in a struct
//! that gets `Debug`-formatted; only the HTTP status appears in errors.

use crate::models::{
    CodexAuthJson, CodexQuotaSnapshot, QuotaSource, QuotaWindow, WhamUsageResponse, WhamWindow,
};
use anyhow::{Context, Result, bail};
use std::path::Path;

/// The ChatGPT backend usage endpoint.
const WHAM_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// User-Agent the Codex CLI sends; mirrored here.
const CODEX_UA: &str = "codex-cli";

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

    let (primary, secondary, limit_reached) = match &resp.rate_limit {
        Some(rl) => (
            rl.primary_window.as_ref().map(|w| map_window(w, now)),
            rl.secondary_window.as_ref().map(|w| map_window(w, now)),
            rl.limit_reached,
        ),
        None => (None, None, None),
    };

    let (credits_balance, has_credits, unlimited) = match &resp.credits {
        Some(c) => (c.balance.clone(), c.has_credits, c.unlimited),
        None => (None, None, None),
    };

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
        limit_reached,
    })
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

/// Builds the shared blocking HTTP client (UA + 8s timeout).
///
/// # Errors
///
/// Returns an error if the client cannot be constructed.
pub fn build_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(CODEX_UA)
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .context("Failed to build HTTP client")
}

/// Fetches Codex usage from the wham endpoint using credentials in `auth_path`.
///
/// # Errors
///
/// Returns an error if the auth file cannot be read/parsed, the request fails,
/// the status is non-success, or the body cannot be mapped. The token is never
/// included in any error or log; only the HTTP status is reported.
pub fn fetch_codex_usage(
    auth_path: &Path,
    client: &reqwest::blocking::Client,
) -> Result<CodexQuotaSnapshot> {
    let body = std::fs::read_to_string(auth_path)
        .with_context(|| format!("Failed to read {}", auth_path.display()))?;
    let (token, account_id) = parse_auth(&body)?;

    let mut req = client.get(WHAM_URL).bearer_auth(&token);
    if let Some(id) = account_id {
        req = req.header("ChatGPT-Account-Id", id);
    }
    let resp = req.send().context("wham/usage request failed")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("wham/usage returned status {status}");
    }
    let text = resp.text().context("Failed to read wham/usage body")?;
    map_wham_response(&text, chrono::Local::now().timestamp())
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
}
