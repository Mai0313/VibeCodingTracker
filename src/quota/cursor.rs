//! Cursor quota fetcher.
//!
//! Reads the WorkOS session JWT from the Cursor CLI's `auth.json`, synthesizes
//! the `WorkosCursorSessionToken` cookie the way the official client does, and
//! calls `GET /api/usage-summary`. Cursor needs no client impersonation.
//!
//! The access token is valid ~60 days and the official Cursor CLI / IDE keeps
//! `auth.json` fresh in the background, so refresh is **reactive**: the file is
//! re-read every tick and its token used while the JWT `exp` is still in the
//! future. We never write the file back. An expired token or a 401/403 surfaces
//! a `cursor login` hint.

use crate::models::{CursorQuotaSnapshot, CursorUsageSummary, QuotaSource, QuotaWindow};
use crate::quota::http::iso_to_unix_secs;
use crate::quota::provider::QuotaOutcome;
use crate::utils::get_cursor_auth_path;
use anyhow::{Context, Result};
use base64::Engine;
use reqwest::blocking::Client;

/// The Cursor usage-summary endpoint.
const CURSOR_USAGE_URL: &str = "https://cursor.com/api/usage-summary";
/// Login hint shown when the Cursor session is expired / rejected. The CLI that
/// manages `auth.json` is `cursor-agent` (`cursor` is the editor launcher).
pub const CURSOR_LOGIN_HINT: &str = "run: cursor-agent login";

/// A usable Cursor session: the synthesized cookie header + the JWT expiry.
struct CursorSession {
    cookie: String,
    exp: i64,
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

/// Builds the `WorkosCursorSessionToken` cookie + reads `exp` from `auth.json`.
///
/// The cookie value is `<userID>::<accessToken>` with `::` percent-encoded,
/// where `userID` is the JWT `sub` claim after the final `|`.
fn read_cursor_session(body: &str) -> Option<CursorSession> {
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

/// Maps a `/api/usage-summary` body into a [`CursorQuotaSnapshot`] (pure).
///
/// # Errors
///
/// Returns an error if the body is not valid JSON in the expected shape.
pub fn map_cursor_usage(body: &str, now: i64) -> Result<CursorQuotaSnapshot> {
    let resp: CursorUsageSummary =
        serde_json::from_str(body).context("Failed to parse Cursor usage summary")?;

    let reset = resp.billing_cycle_end.as_deref().and_then(iso_to_unix_secs);
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

    let on_demand_dollars = resp
        .individual_usage
        .as_ref()
        .and_then(|u| u.on_demand.as_ref())
        .filter(|d| d.enabled == Some(true))
        .and_then(|d| d.used)
        .map(|cents| cents / 100.0);

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
    /// 401/403 → the session is rejected; re-login.
    Unauthorized,
    /// Network / other non-success error → keep last-known-good.
    Transient,
}

/// Calls the Cursor usage API with the synthesized session cookie.
fn fetch_cursor_usage(client: &Client, cookie: &str, now: i64) -> FetchResult {
    let resp = match client
        .get(CURSOR_USAGE_URL)
        .header(reqwest::header::COOKIE, cookie)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
    {
        Ok(r) => r,
        Err(_) => return FetchResult::Transient,
    };
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return FetchResult::Unauthorized;
    }
    if !status.is_success() {
        return FetchResult::Transient;
    }
    match resp.text() {
        Ok(text) => match map_cursor_usage(&text, now) {
            Ok(snap) => FetchResult::Ok(snap),
            Err(_) => FetchResult::Transient,
        },
        Err(_) => FetchResult::Transient,
    }
}

/// Per-worker Cursor state. Refresh is reactive (the official CLI keeps
/// `auth.json` fresh), so nothing is cached between ticks.
#[derive(Default)]
pub struct CursorState;

impl CursorState {
    /// One worker tick: re-read the session, call the usage API.
    pub fn resolve(&mut self, client: &Client) -> QuotaOutcome<CursorQuotaSnapshot> {
        let now = chrono::Local::now().timestamp();
        let path = match get_cursor_auth_path() {
            Ok(p) => p,
            Err(_) => return QuotaOutcome::Transient,
        };
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(_) => return QuotaOutcome::Transient,
        };
        let session = match read_cursor_session(&body) {
            Some(s) => s,
            None => return QuotaOutcome::Transient,
        };
        // The token is expired and we cannot refresh it ourselves; nudge login.
        if session.exp > 0 && session.exp <= now {
            return QuotaOutcome::NeedsLogin;
        }
        match fetch_cursor_usage(client, &session.cookie, now) {
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
        // On-demand disabled → no dollar figure.
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
    fn missing_usage_is_tolerated() {
        let snap = map_cursor_usage(r#"{ "membershipType": "free" }"#, 1).unwrap();
        assert!(snap.total.is_none());
        assert!(snap.auto.is_none());
        assert!(snap.api.is_none());
        assert!(!snap.limit_reached);
    }
}
