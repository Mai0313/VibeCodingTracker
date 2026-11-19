//! Codex `wham/usage` client, reset-credit details client, and OAuth token refresh.
//!
//! Reads credentials from `~/.codex/auth.json` and maps backend responses into [`CodexQuotaSnapshot`]. Tokens are never logged or included in error messages; only HTTP status codes are reported.

use crate::models::{
    CodexAuthJson, CodexQuotaSnapshot, CodexRefreshResponse, QuotaSource, QuotaWindow,
    WhamResetCreditsDetails, WhamUsageResponse, WhamWindow,
};
use crate::quota::refresh::{file_mtime, send_refresh, update_json_file_in_place};
use crate::quota::{CodexWindowSlot, assign_codex_window};
use crate::utils::now_rfc3339_utc_nanos;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::OnceLock;

/// ChatGPT backend quota usage endpoint.
pub(crate) const WHAM_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// ChatGPT backend earned reset-credit details endpoint.
pub(crate) const WHAM_RESET_CREDITS_URL: &str =
    "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
/// Codex OAuth token refresh endpoint.
pub(crate) const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// The Codex OAuth client id (public PKCE client).
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Fallback Codex CLI version for the `User-Agent` header when `codex --version` is unavailable.
const CODEX_FALLBACK_VERSION: &str = "0.142.5";
/// Headless Codex CLI `originator` header value sent for client-identity parity.
const CODEX_ORIGINATOR: &str = "codex_cli_rs";

/// Returns the Codex CLI `User-Agent` header value, using [`crate::quota::http::detect_cli_version`] and target OS/arch.
fn codex_ua() -> &'static str {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        format!(
            "codex_cli_rs/{} ({}; {})",
            crate::quota::http::detect_cli_version("codex", "codex", CODEX_FALLBACK_VERSION),
            std::env::consts::OS,
            std::env::consts::ARCH,
        )
    })
    .as_str()
}

/// Builds an authenticated Codex backend GET request with the CLI identity.
fn codex_get(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &str,
    account_id: Option<&str>,
) -> reqwest::blocking::RequestBuilder {
    let req = client
        .get(url)
        .header(reqwest::header::USER_AGENT, codex_ua())
        .header("originator", CODEX_ORIGINATOR)
        .bearer_auth(token);
    match account_id {
        Some(id) => req.header("ChatGPT-Account-Id", id),
        None => req,
    }
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
        Some(rl) => {
            let mut primary = None;
            let mut secondary = None;
            if let Some(window) = &rl.primary_window {
                assign_codex_window(
                    &mut primary,
                    &mut secondary,
                    map_window(window, now),
                    window.limit_window_seconds,
                    CodexWindowSlot::FiveHour,
                );
            }
            if let Some(window) = &rl.secondary_window {
                assign_codex_window(
                    &mut primary,
                    &mut secondary,
                    map_window(window, now),
                    window.limit_window_seconds,
                    CodexWindowSlot::SevenDay,
                );
            }
            (primary, secondary, rl.limit_reached)
        }
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

    // Report `Some(true)` if any limit source is reached, `Some(false)` if any reported false, and `None` only when none reported.
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
        reset_credit_expirations: None,
        approx_messages,
        spend_limit,
        limit_reached,
        needs_login: false,
    })
}

/// Maps a `wham/rate-limit-reset-credits` response into `(available_count, expirations)` for `available` credits.
///
/// `expirations` is sorted ascending with non-expiring (`None`) credits last, and may contain fewer entries than `available_count` when the backend truncates detail rows. A `None` count indicates the response omitted `available_count`.
///
/// # Errors
///
/// Returns an error if the JSON shape or any available credit's RFC3339 `expires_at` timestamp is malformed.
pub fn map_reset_credits_response(body: &str) -> Result<(Option<i64>, Vec<Option<i64>>)> {
    let resp: WhamResetCreditsDetails = serde_json::from_str(body)
        .context("Failed to parse wham/rate-limit-reset-credits response")?;
    let available_count = resp.available_count.map(|count| count.max(0));

    let mut expirations = Vec::new();
    for credit in resp.credits.into_iter().filter(|c| c.status == "available") {
        let expires_at = credit
            .expires_at
            .as_deref()
            .map(|timestamp| {
                chrono::DateTime::parse_from_rfc3339(timestamp)
                    .with_context(|| format!("invalid expires_at for reset credit `{}`", credit.id))
                    .map(|parsed| parsed.timestamp())
            })
            .transpose()?;
        expirations.push(expires_at);
    }
    expirations.sort_by_key(|expires_at| expires_at.unwrap_or(i64::MAX));

    Ok((available_count, expirations))
}

/// Extracts a `(low, high)` approximate-messages pair, returning `None` when `high <= 0`.
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

/// Outcome of the optional reset-credit details request.
pub enum ResetCreditsResult {
    /// Authoritative count (`None` if omitted by the response) plus sorted available-credit expirations.
    Ok {
        available_count: Option<i64>,
        expirations: Vec<Option<i64>>,
    },
    /// 401: the caller may refresh the token and retry once.
    Unauthorized,
    /// Network, parse, or other HTTP error: keep the usage summary.
    Transient,
}

/// Calls the wham endpoint at `wham_url` with an explicit bearer token.
pub fn call_wham(
    client: &reqwest::blocking::Client,
    token: &str,
    account_id: Option<&str>,
    now: i64,
    wham_url: &str,
) -> WhamResult {
    let resp = match codex_get(client, wham_url, token, account_id).send() {
        Ok(r) => r,
        Err(e) => {
            log::warn!("codex quota fetch: request failed: {e}");
            return WhamResult::Transient;
        }
    };
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return WhamResult::Unauthorized;
    }
    if !status.is_success() {
        log::warn!("codex quota fetch: HTTP {status}");
        return WhamResult::Transient;
    }
    match resp.text() {
        Ok(text) => match map_wham_response(&text, now) {
            Ok(snap) => WhamResult::Ok(snap),
            Err(e) => {
                log::warn!("codex quota fetch: failed to parse usage response: {e}");
                WhamResult::Transient
            }
        },
        Err(e) => {
            log::warn!("codex quota fetch: failed to read response body: {e}");
            WhamResult::Transient
        }
    }
}

/// Calls and maps the earned reset-credit details endpoint.
pub fn call_reset_credit_details(
    client: &reqwest::blocking::Client,
    token: &str,
    account_id: Option<&str>,
    reset_credits_url: &str,
) -> ResetCreditsResult {
    let resp = match codex_get(client, reset_credits_url, token, account_id).send() {
        Ok(resp) => resp,
        Err(e) => {
            log::warn!("codex reset-credit details fetch: request failed: {e}");
            return ResetCreditsResult::Transient;
        }
    };
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return ResetCreditsResult::Unauthorized;
    }
    if !status.is_success() {
        log::warn!("codex reset-credit details fetch: HTTP {status}");
        return ResetCreditsResult::Transient;
    }
    let text = match resp.text() {
        Ok(text) => text,
        Err(e) => {
            log::warn!("codex reset-credit details fetch: failed to read response body: {e}");
            return ResetCreditsResult::Transient;
        }
    };
    match map_reset_credits_response(&text) {
        Ok((available_count, expirations)) => ResetCreditsResult::Ok {
            available_count,
            expirations,
        },
        Err(e) => {
            log::warn!("codex reset-credit details fetch: failed to parse response: {e}");
            ResetCreditsResult::Transient
        }
    }
}

/// Fetches the raw `wham/usage` response as `(status_code, body)` using the stored access token verbatim.
///
/// Does not refresh tokens or modify `auth.json`; non-2xx responses are returned as `Ok((status, body))`.
///
/// # Errors
///
/// Returns an error if `auth.json` is missing, has no access token, or the HTTP request fails.
pub fn fetch_codex_raw(client: &reqwest::blocking::Client) -> Result<(u16, String)> {
    let auth_path = crate::utils::resolve_paths()?.codex_dir.join("auth.json");
    fetch_codex_raw_from(client, WHAM_URL, &auth_path)
}

/// Injectable core of [`fetch_codex_raw`] reading credentials from `auth_path` and querying `wham_url`.
pub(crate) fn fetch_codex_raw_from(
    client: &reqwest::blocking::Client,
    wham_url: &str,
    auth_path: &Path,
) -> Result<(u16, String)> {
    let body = std::fs::read_to_string(auth_path).with_context(|| {
        format!(
            "no Codex credentials at {} ({})",
            auth_path.display(),
            crate::quota::CODEX_LOGIN_HINT
        )
    })?;
    let (access_token, account_id) = parse_auth(&body)?;
    let resp = codex_get(client, wham_url, &access_token, account_id.as_deref())
        .send()
        .context("Failed to send Codex usage request")?;
    let status = resp.status().as_u16();
    let text = resp
        .text()
        .context("Failed to read Codex usage response body")?;
    Ok((status, text))
}

/// Refreshes the Codex OAuth token at `auth_path`, writes the rotated credentials back, and returns the new access token.
///
/// Guards the write-back on `auth_path`'s pre-read mtime so a concurrent rotation by the Codex CLI is not overwritten. Tokens never appear in returned errors.
///
/// # Errors
///
/// Returns an error if `auth_path` cannot be read or has no refresh token, if the token endpoint fails or returns a non-2xx status, or if the rotated token cannot be persisted.
pub fn refresh_codex(
    client: &reqwest::blocking::Client,
    auth_path: &Path,
    token_url: &str,
) -> Result<String> {
    // Capture mtime before reading so a concurrent CLI rotation aborts write-back instead of being clobbered.
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
        .post(token_url)
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
    // Server-side rotation already invalidated the old refresh token; fail rather than reporting success with a stale `auth.json`.
    if !wrote {
        bail!("codex token rotated but the new token could not be persisted");
    }
    Ok(access)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESET_CREDITS_SAMPLE: &str = include_str!(
        "../../../../tests/fixtures/quota/wham_rate_limit_reset_credits_response.json"
    );
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
        assert!(snap.reset_credit_expirations.is_none());
        assert_eq!(snap.limit_reached, Some(false));
        assert!(!snap.needs_login);
    }

    #[test]
    fn maps_weekly_only_primary_window_to_weekly_slot() {
        let body = r#"{
          "plan_type": "plus",
          "rate_limit": {
            "primary_window": {
              "used_percent": 42,
              "limit_window_seconds": 604800,
              "reset_at": 1785946962
            },
            "secondary_window": null
          }
        }"#;

        let snap = map_wham_response(body, 1_000).unwrap();
        assert!(snap.primary.is_none());
        let weekly = snap.secondary.expect("weekly window");
        assert_eq!(weekly.used_percent, 42.0);
        assert_eq!(weekly.resets_at_unix, Some(1785946962));
    }

    #[test]
    fn maps_available_reset_credit_expirations_in_order() {
        let (count, expirations) = map_reset_credits_response(RESET_CREDITS_SAMPLE).unwrap();

        assert_eq!(count, Some(5), "top-level count stays authoritative");
        assert_eq!(expirations.len(), 3, "redeemed credit is excluded");
        assert!(expirations[0].unwrap() < expirations[1].unwrap());
        assert_eq!(expirations[2], None, "non-expiring credit sorts last");
    }

    #[test]
    fn rejects_malformed_available_credit_expiry() {
        let body = r#"{
          "credits": [{
            "id": "credit-1", "reset_type": "codex_rate_limits",
            "status": "available", "granted_at": "2026-07-01T00:00:00Z",
            "expires_at": "not-a-timestamp"
          }],
          "available_count": 1
        }"#;

        assert!(map_reset_credits_response(body).is_err());
    }

    #[test]
    fn maps_reset_credits_response_with_dropped_fields() {
        let body = r#"{
          "credits": [
            { "status": "available", "expires_at": "2026-08-01T00:00:00Z" },
            { "status": "available" },
            { "expires_at": "2026-07-01T00:00:00Z" }
          ],
          "available_count": 3
        }"#;

        let (count, expirations) = map_reset_credits_response(body).unwrap();

        assert_eq!(count, Some(3));
        assert_eq!(
            expirations,
            vec![Some(1785542400), None],
            "an entry with no status is not readable as available"
        );
    }

    #[test]
    fn missing_reset_credit_count_leaves_the_usage_count_standing() {
        let (count, expirations) =
            map_reset_credits_response(r#"{"credits":[{"status":"redeemed"}]}"#).unwrap();

        assert_eq!(count, None, "absent count means unknown, not zero");
        assert!(expirations.is_empty());

        let (count, expirations) = map_reset_credits_response("{}").unwrap();
        assert_eq!(count, None);
        assert!(expirations.is_empty());
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
        assert!(snap.secondary.is_none());
        assert_eq!(snap.primary.unwrap().resets_at_unix, Some(1_100));
    }

    #[test]
    fn durationless_windows_keep_positional_mapping() {
        let body = r#"{
          "rate_limit": {
            "primary_window": {"used_percent": 10, "reset_at": 1100},
            "secondary_window": {"used_percent": 20, "reset_at": 1200}
          }
        }"#;

        let snap = map_wham_response(body, 1_000).unwrap();
        assert_eq!(snap.primary.unwrap().used_percent, 10.0);
        assert_eq!(snap.secondary.unwrap().used_percent, 20.0);
    }

    #[test]
    fn unknown_window_period_is_not_mislabeled() {
        let body = r#"{
          "rate_limit": {
            "primary_window": {
              "used_percent": 42,
              "limit_window_seconds": 86400,
              "reset_at": 87400
            }
          }
        }"#;

        let snap = map_wham_response(body, 1_000).unwrap();
        assert!(snap.primary.is_none());
        assert!(snap.secondary.is_none());
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
        let snap = map_wham_response(r#"{"plan_type":"plus"}"#, 1).unwrap();
        assert_eq!(snap.limit_reached, None);
    }

    /// Uses the fallback version under test while CLI version probing is disabled.
    #[test]
    fn codex_ua_uses_the_fallback_version_under_test() {
        assert_eq!(
            codex_ua(),
            format!(
                "codex_cli_rs/{CODEX_FALLBACK_VERSION} ({}; {})",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        );
    }

    // ---- HTTP-layer test against a local mock server (no real API) ----

    #[test]
    fn fetch_codex_raw_from_reads_auth_and_returns_status_body() {
        use crate::quota::http::build_client;
        use httpmock::prelude::*;

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/wham");
            then.status(200).body(SAMPLE);
        });
        let dir = tempfile::tempdir().unwrap();
        let auth = dir.path().join("auth.json");
        std::fs::write(
            &auth,
            r#"{"tokens":{"access_token":"tok","account_id":"acct"}}"#,
        )
        .unwrap();

        let client = build_client().unwrap();
        let (status, body) =
            fetch_codex_raw_from(&client, &server.url("/wham"), &auth).expect("raw fetch");
        assert_eq!(status, 200);
        assert!(body.contains("plan_type"));
    }
}
