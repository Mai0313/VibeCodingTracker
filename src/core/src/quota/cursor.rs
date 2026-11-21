//! Cursor quota fetcher.
//!
//! Reads the WorkOS session JWT from the Cursor CLI's `auth.json`, synthesizes the `WorkosCursorSessionToken` cookie, and calls `GET /api/usage-summary` with a `cursor-agent/<version>` `User-Agent`.
//!
//! `auth.json` is cached in memory per mtime and never modified; token refresh is left to the Cursor CLI/IDE. An expired JWT `exp` or HTTP 401/403 surfaces [`CURSOR_LOGIN_HINT`].

use crate::models::{CursorQuotaSnapshot, CursorUsageSummary, QuotaSource, QuotaWindow};
use crate::quota::http::{detect_cli_version, iso_to_unix_secs};
use crate::quota::provider::QuotaOutcome;
use crate::quota::refresh::file_mtime;
use crate::utils::get_cursor_auth_path;
use anyhow::{Context, Result};
use base64::Engine;
use reqwest::blocking::Client;
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

/// The Cursor usage-summary endpoint.
const CURSOR_USAGE_URL: &str = "https://cursor.com/api/usage-summary";
/// Fallback Cursor CLI version for `User-Agent` when `cursor-agent --version` cannot be resolved.
const CURSOR_FALLBACK_VERSION: &str = "2026.07.07";
/// Login hint shown when the Cursor session is expired or rejected.
///
/// `cursor-agent` manages `auth.json`; `cursor` is the editor launcher.
pub const CURSOR_LOGIN_HINT: &str = "run: cursor-agent login";

/// Returns the cached `cursor-agent/<version>` `User-Agent` string, resolved once per process.
pub(crate) fn cursor_ua() -> &'static str {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        format!(
            "cursor-agent/{}",
            detect_cli_version("cursor-agent", "cursor", CURSOR_FALLBACK_VERSION)
        )
    })
    .as_str()
}

/// Synthesized `WorkosCursorSessionToken` cookie header and JWT `exp` timestamp (Unix seconds).
#[derive(Clone)]
pub(crate) struct CursorSession {
    pub(crate) cookie: String,
    pub(crate) exp: i64,
}

/// Decodes a JWT payload segment (base64url, no padding) into JSON.
fn decode_jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    // JWT segments are base64url without padding; tolerate any stray padding.
    let trimmed = payload.trim_end_matches('=');
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(trimmed)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Parses `auth.json` into a [`CursorSession`] with cookie `WorkosCursorSessionToken=<uid>%3A%3A<accessToken>` (`uid` from the JWT `sub` claim after the last `|`).
pub(crate) fn read_cursor_session(body: &str) -> Option<CursorSession> {
    let root: serde_json::Value = serde_json::from_str(body).ok()?;
    let access = root.get("accessToken")?.as_str()?;
    if access.is_empty() {
        return None;
    }
    let claims = decode_jwt_payload(access)?;
    let sub = claims.get("sub")?.as_str()?;
    let uid = sub.rsplit('|').next().unwrap_or(sub);
    if uid.is_empty() {
        return None;
    }
    let exp = claims.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
    Some(CursorSession {
        cookie: format!("WorkosCursorSessionToken={uid}%3A%3A{access}"),
        exp,
    })
}

/// Maps a `/api/usage-summary` JSON response into a [`CursorQuotaSnapshot`] stamped at `now` (Unix seconds).
///
/// # Errors
///
/// Returns an error if `body` is not valid JSON matching [`CursorUsageSummary`].
pub fn map_cursor_usage(body: &str, now: i64) -> Result<CursorQuotaSnapshot> {
    let resp: CursorUsageSummary =
        serde_json::from_str(body).context("Failed to parse Cursor usage summary")?;

    let reset = resp.billing_cycle_end.as_deref().and_then(iso_to_unix_secs);
    // `*PercentUsed` is already percent consumed (not percent remaining); clamp verbatim without inverting.
    let win = |pct: Option<f64>| {
        pct.map(|p| QuotaWindow {
            used_percent: p.clamp(0.0, 100.0),
            resets_at_unix: reset,
        })
    };

    let plan = resp.individual_usage.as_ref().and_then(|u| u.plan.as_ref());
    let total = win(plan.and_then(|p| p.total_percent_used));
    let auto = win(plan.and_then(|p| p.auto_percent_used));
    let api = win(plan.and_then(|p| p.api_percent_used));

    // Prefer individual on-demand spend; team/enterprise accounts bill under `teamUsage.onDemand` while individual on-demand is disabled.
    let individual_od = resp
        .individual_usage
        .as_ref()
        .and_then(|u| u.on_demand.as_ref())
        .filter(|d| d.enabled == Some(true))
        .and_then(|d| d.used);
    let team_od = resp
        .team_usage
        .as_ref()
        .and_then(|t| t.on_demand.as_ref())
        .and_then(|d| d.used);
    let on_demand_dollars = individual_od.or(team_od).map(|cents| cents / 100.0);

    let is_unlimited = resp.is_unlimited.unwrap_or(false);
    let limit_reached = !is_unlimited
        && total
            .as_ref()
            .map(|w| w.used_percent >= 100.0)
            .unwrap_or(false);

    Ok(CursorQuotaSnapshot {
        source: QuotaSource::Api,
        fetched_at: now,
        plan_type: resp.membership_type.filter(|s| !s.is_empty()),
        total,
        auto,
        api,
        on_demand_dollars,
        limit_reached,
        needs_login: false,
    })
}

/// Outcome of a single usage request.
enum FetchResult {
    Ok(CursorQuotaSnapshot),
    /// HTTP 401 or 403; the session was rejected and requires re-login.
    Unauthorized,
    /// Network, parse, or other non-2xx failure; retain the last snapshot.
    Transient,
}

/// Calls the Cursor usage API at `usage_url` with the synthesized session cookie.
fn fetch_cursor_usage(client: &Client, cookie: &str, now: i64, usage_url: &str) -> FetchResult {
    let resp = match client
        .get(usage_url)
        .header(reqwest::header::COOKIE, cookie)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, cursor_ua())
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            log::warn!("cursor quota fetch: request failed: {e}");
            return FetchResult::Transient;
        }
    };
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return FetchResult::Unauthorized;
    }
    if !status.is_success() {
        log::warn!("cursor quota fetch: HTTP {status}");
        return FetchResult::Transient;
    }
    match resp.text() {
        Ok(text) => match map_cursor_usage(&text, now) {
            Ok(snap) => FetchResult::Ok(snap),
            Err(e) => {
                log::warn!("cursor quota fetch: failed to parse usage response: {e}");
                FetchResult::Transient
            }
        },
        Err(e) => {
            log::warn!("cursor quota fetch: failed to read response body: {e}");
            FetchResult::Transient
        }
    }
}

/// Returns `(status_code, body)` from `/api/usage-summary` using the stored session JWT.
///
/// Does not pre-check JWT `exp` or modify `auth.json`. Non-2xx responses are returned as `Ok((status_code, body))`.
///
/// # Errors
///
/// Returns an error if the auth path cannot be resolved, `auth.json` is missing or has no usable session, or the HTTP request or body read fails.
pub fn fetch_cursor_raw(client: &Client) -> Result<(u16, String)> {
    fetch_cursor_raw_from(client, CURSOR_USAGE_URL, &get_cursor_auth_path()?)
}

/// Returns `(status_code, body)` from `usage_url` using the session read from `auth_path`.
pub(crate) fn fetch_cursor_raw_from(
    client: &Client,
    usage_url: &str,
    auth_path: &Path,
) -> Result<(u16, String)> {
    let path = auth_path;
    let body = std::fs::read_to_string(path).with_context(|| {
        format!(
            "no Cursor credentials at {} ({CURSOR_LOGIN_HINT})",
            path.display()
        )
    })?;
    let session = read_cursor_session(&body).with_context(|| {
        format!(
            "no usable Cursor session in {} ({CURSOR_LOGIN_HINT})",
            path.display()
        )
    })?;
    let resp = client
        .get(usage_url)
        .header(reqwest::header::COOKIE, session.cookie.as_str())
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, cursor_ua())
        .send()
        .context("Failed to send Cursor usage request")?;
    let status = resp.status().as_u16();
    let text = resp
        .text()
        .context("Failed to read Cursor usage response body")?;
    Ok((status, text))
}

/// Per-worker Cursor quota state caching the parsed session per `auth.json` mtime.
#[derive(Default)]
pub struct CursorState {
    session: Option<(CursorSession, Option<SystemTime>)>,
}

impl CursorState {
    /// Reads `auth.json` (or reuses the mtime-cached session), validates the JWT expiry, and fetches a fresh quota snapshot.
    pub fn resolve(&mut self, client: &Client) -> QuotaOutcome<CursorQuotaSnapshot> {
        let now = chrono::Local::now().timestamp();
        let path = match get_cursor_auth_path() {
            Ok(p) => p,
            Err(_) => return QuotaOutcome::Transient,
        };
        let cur_mtime = file_mtime(&path);
        let session = match &self.session {
            Some((cached, m)) if cur_mtime.is_some() && *m == cur_mtime => cached.clone(),
            _ => {
                let body = match std::fs::read_to_string(&path) {
                    Ok(b) => b,
                    Err(_) => return QuotaOutcome::Transient,
                };
                // A readable file with no usable session (e.g. cleared on logout) is an auth failure, not a transient read error.
                match read_cursor_session(&body) {
                    Some(s) => {
                        self.session = Some((s.clone(), cur_mtime));
                        s
                    }
                    None => {
                        self.session = None;
                        return QuotaOutcome::NeedsLogin;
                    }
                }
            }
        };
        // The token is expired and we cannot refresh it ourselves; nudge login.
        if session.exp > 0 && session.exp <= now {
            return QuotaOutcome::NeedsLogin;
        }
        match fetch_cursor_usage(client, &session.cookie, now, CURSOR_USAGE_URL) {
            FetchResult::Ok(snap) => QuotaOutcome::Data(snap),
            FetchResult::Unauthorized => QuotaOutcome::NeedsLogin,
            FetchResult::Transient => QuotaOutcome::Transient,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    /// Builds a fake JWT (`header.payload.sig`) from a payload JSON object.
    fn fake_jwt(payload: &serde_json::Value) -> String {
        let enc = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
        format!(
            "{}.{}.{}",
            enc(b"{\"alg\":\"none\"}"),
            enc(payload.to_string().as_bytes()),
            "sig"
        )
    }

    #[test]
    fn builds_cookie_from_jwt_sub() {
        let jwt = fake_jwt(&serde_json::json!({
            "sub": "github|user_01ABC",
            "exp": 9_999_999_999i64,
        }));
        let body = format!(r#"{{ "accessToken": "{jwt}", "refreshToken": "rt" }}"#);
        let session = read_cursor_session(&body).unwrap();
        assert_eq!(
            session.cookie,
            format!("WorkosCursorSessionToken=user_01ABC%3A%3A{jwt}")
        );
        assert_eq!(session.exp, 9_999_999_999);
    }

    #[test]
    fn sub_without_pipe_uses_whole_value() {
        let jwt = fake_jwt(&serde_json::json!({ "sub": "user_only", "exp": 1 }));
        let body = format!(r#"{{ "accessToken": "{jwt}" }}"#);
        let session = read_cursor_session(&body).unwrap();
        assert!(session.cookie.contains("=user_only%3A%3A"));
    }

    #[test]
    fn missing_access_token_is_none() {
        assert!(read_cursor_session(r#"{ "refreshToken": "rt" }"#).is_none());
    }

    const SUMMARY: &str = r#"{
      "billingCycleStart": "2026-06-23T17:36:23.480Z",
      "billingCycleEnd": "2026-07-23T17:36:23.480Z",
      "membershipType": "free",
      "isUnlimited": false,
      "individualUsage": {
        "plan": { "autoPercentUsed": 100, "apiPercentUsed": 44, "totalPercentUsed": 94 },
        "onDemand": { "enabled": false, "used": 0, "limit": null }
      }
    }"#;

    #[test]
    fn maps_cursor_usage() {
        let snap = map_cursor_usage(SUMMARY, 1_000_000).unwrap();
        assert_eq!(snap.source, QuotaSource::Api);
        assert_eq!(snap.plan_type.as_deref(), Some("free"));
        assert_eq!(snap.total.as_ref().unwrap().used_percent, 94.0);
        assert_eq!(snap.auto.as_ref().unwrap().used_percent, 100.0);
        assert_eq!(snap.api.as_ref().unwrap().used_percent, 44.0);
        assert!(snap.total.as_ref().unwrap().resets_at_unix.unwrap() > 0);
        assert!(snap.on_demand_dollars.is_none());
        assert!(!snap.limit_reached);
    }

    #[test]
    fn on_demand_dollars_from_cents_when_enabled() {
        let body = r#"{ "membershipType": "pro", "individualUsage": { "onDemand": { "enabled": true, "used": 1840 } } }"#;
        let snap = map_cursor_usage(body, 1).unwrap();
        assert_eq!(snap.on_demand_dollars, Some(18.40));
    }

    #[test]
    fn on_demand_falls_back_to_team_usage() {
        let body = r#"{
          "membershipType": "enterprise",
          "individualUsage": { "onDemand": { "enabled": false } },
          "teamUsage": { "onDemand": { "used": 5000 } }
        }"#;
        let snap = map_cursor_usage(body, 1).unwrap();
        assert_eq!(snap.on_demand_dollars, Some(50.0));
    }

    #[test]
    fn flags_limit_when_total_maxed() {
        let body = r#"{ "isUnlimited": false, "individualUsage": { "plan": { "totalPercentUsed": 100 } } }"#;
        let snap = map_cursor_usage(body, 1).unwrap();
        assert!(snap.limit_reached);
    }

    #[test]
    fn unlimited_never_flags_limit() {
        let body = r#"{ "isUnlimited": true, "individualUsage": { "plan": { "totalPercentUsed": 100 } } }"#;
        let snap = map_cursor_usage(body, 1).unwrap();
        assert!(!snap.limit_reached);
    }

    #[test]
    fn idle_account_reads_as_unused() {
        // Real never-used plan shape; inverting `*PercentUsed` would render full gauges and a spurious LIMIT flag.
        let body = r#"{
          "membershipType": "free",
          "isUnlimited": false,
          "individualUsage": {
            "plan": {
              "used": 0, "limit": 0, "remaining": 0,
              "autoPercentUsed": 0, "apiPercentUsed": 0, "totalPercentUsed": 0
            }
          }
        }"#;
        let snap = map_cursor_usage(body, 1).unwrap();
        assert_eq!(snap.total.as_ref().unwrap().used_percent, 0.0);
        assert_eq!(snap.auto.as_ref().unwrap().used_percent, 0.0);
        assert_eq!(snap.api.as_ref().unwrap().used_percent, 0.0);
        assert!(!snap.limit_reached);
    }

    #[test]
    fn out_of_range_percentages_are_clamped() {
        // Values outside 0..=100 would corrupt bar widths and the limit comparison.
        let body = r#"{ "individualUsage": { "plan": { "totalPercentUsed": 150, "autoPercentUsed": -5 } } }"#;
        let snap = map_cursor_usage(body, 1).unwrap();
        assert_eq!(snap.total.as_ref().unwrap().used_percent, 100.0);
        assert_eq!(snap.auto.as_ref().unwrap().used_percent, 0.0);
        assert!(snap.limit_reached);
    }

    #[test]
    fn missing_usage_is_tolerated() {
        let snap = map_cursor_usage(r#"{ "membershipType": "free" }"#, 1).unwrap();
        assert!(snap.total.is_none());
        assert!(snap.auto.is_none());
        assert!(snap.api.is_none());
        assert!(!snap.limit_reached);
    }

    // ---- HTTP-layer tests against a local mock server (no real API) ----

    #[test]
    fn fetch_cursor_usage_maps_200_and_401() {
        use crate::quota::http::build_client;
        use httpmock::prelude::*;

        let server = MockServer::start();
        let ok = server.mock(|when, then| {
            when.method(GET).path("/ok");
            then.status(200).body(SUMMARY);
        });
        server.mock(|when, then| {
            when.method(GET).path("/forbidden");
            then.status(403);
        });
        let client = build_client().unwrap();

        match fetch_cursor_usage(&client, "cookie", 1_000_000, &server.url("/ok")) {
            FetchResult::Ok(snap) => {
                assert_eq!(snap.plan_type.as_deref(), Some("free"));
                assert_eq!(snap.total.as_ref().unwrap().used_percent, 94.0);
            }
            _ => panic!("expected Ok"),
        }
        ok.assert();
        assert!(matches!(
            fetch_cursor_usage(&client, "cookie", 0, &server.url("/forbidden")),
            FetchResult::Unauthorized
        ));
    }

    #[test]
    fn fetch_cursor_raw_from_reads_session_and_returns_body() {
        use crate::quota::http::build_client;
        use httpmock::prelude::*;

        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/usage");
            then.status(200).body(SUMMARY);
        });
        let jwt = fake_jwt(&serde_json::json!({ "sub": "github|u1", "exp": 9_999_999_999i64 }));
        let dir = tempfile::tempdir().unwrap();
        let auth = dir.path().join("auth.json");
        std::fs::write(&auth, format!(r#"{{ "accessToken": "{jwt}" }}"#)).unwrap();

        let client = build_client().unwrap();
        let (status, body) =
            fetch_cursor_raw_from(&client, &server.url("/usage"), &auth).expect("raw fetch");
        assert_eq!(status, 200);
        assert!(body.contains("membershipType"));
    }
}
