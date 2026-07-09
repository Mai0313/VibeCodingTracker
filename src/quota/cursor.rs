//! Cursor quota fetcher.
//!
//! Reads the WorkOS session JWT from the Cursor CLI's `auth.json`, synthesizes
//! the `WorkosCursorSessionToken` cookie the way the official client does, and
//! calls `GET /api/usage-summary`, impersonating the Cursor CLI via a
//! `cursor-agent/<version>` User-Agent (version detected from
//! `cursor-agent --version`, cached for the day).
//!
//! The access token is valid ~60 days and the official Cursor CLI / IDE keeps
//! `auth.json` fresh in the background, so refresh is **reactive**: the file is
//! re-read every tick and its token used while the JWT `exp` is still in the
//! future. We never write the file back. An expired token or a 401/403 surfaces
//! a `cursor-agent login` hint.

use crate::models::{CursorQuotaSnapshot, CursorUsageSummary, QuotaSource, QuotaWindow};
use crate::quota::http::{detect_cli_version, iso_to_unix_secs};
use crate::quota::provider::QuotaOutcome;
use crate::utils::get_cursor_auth_path;
use anyhow::{Context, Result};
use base64::Engine;
use reqwest::blocking::Client;
use std::path::Path;
use std::sync::OnceLock;

/// The Cursor usage-summary endpoint.
const CURSOR_USAGE_URL: &str = "https://cursor.com/api/usage-summary";
/// Fallback Cursor CLI version for the User-Agent when `cursor-agent --version`
/// cannot be resolved (CLI absent or unreadable). Bump occasionally.
const CURSOR_FALLBACK_VERSION: &str = "2026.07.07";
/// Login hint shown when the Cursor session is expired / rejected. The CLI that
/// manages `auth.json` is `cursor-agent` (`cursor` is the editor launcher).
pub const CURSOR_LOGIN_HINT: &str = "run: cursor-agent login";

/// Builds the Cursor CLI's request User-Agent, e.g. `cursor-agent/2026.07.07`.
///
/// The version is detected from the installed CLI (see
/// [`crate::quota::http::detect_cli_version`]) so the UA tracks the real client
/// rather than drifting from a hardcoded constant.
pub(crate) fn cursor_ua() -> &'static str {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        format!(
            "cursor-agent/{}",
            detect_cli_version(
                "cursor-agent",
                "cursor_version.json",
                CURSOR_FALLBACK_VERSION
            )
        )
    })
    .as_str()
}

/// A usable Cursor session: the synthesized cookie header + the JWT expiry.
///
/// Shared with the session-data reader (`crate::session::cursor`), which reuses
/// the same cookie to reach the dashboard usage-events API.
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

/// Builds the `WorkosCursorSessionToken` cookie + reads `exp` from `auth.json`.
///
/// The cookie value is `<userID>::<accessToken>` with `::` percent-encoded,
/// where `userID` is the JWT `sub` claim after the final `|`.
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

/// Maps a `/api/usage-summary` body into a [`CursorQuotaSnapshot`] (pure).
///
/// # Errors
///
/// Returns an error if the body is not valid JSON in the expected shape.
pub fn map_cursor_usage(body: &str, now: i64) -> Result<CursorQuotaSnapshot> {
    let resp: CursorUsageSummary =
        serde_json::from_str(body).context("Failed to parse Cursor usage summary")?;

    let reset = resp.billing_cycle_end.as_deref().and_then(iso_to_unix_secs);
    // NOTE: despite the `*PercentUsed` names, Cursor reports these inversely to
    // absolute usage. Observed on a fresh free plan: `plan.used == 0` yet
    // `totalPercentUsed == 94`, so the field is *not* percent used. We invert
    // (100 - value) so a barely-used account reads as a near-empty gauge and
    // matches what cursor.com shows, consistent with the other panels. Do not
    // "simplify" this back to `p` — that regresses the gauge to show ~full for
    // an unused account.
    let win = |pct: Option<f64>| {
        pct.map(|p| QuotaWindow {
            used_percent: (100.0 - p).clamp(0.0, 100.0),
            resets_at_unix: reset,
        })
    };

    let plan = resp.individual_usage.as_ref().and_then(|u| u.plan.as_ref());
    let total = win(plan.and_then(|p| p.total_percent_used));
    let auto = win(plan.and_then(|p| p.auto_percent_used));
    let api = win(plan.and_then(|p| p.api_percent_used));

    // Prefer the individual on-demand spend; team/enterprise accounts bill it
    // under `teamUsage.onDemand` while the individual branch is disabled.
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
    /// 401/403 → the session is rejected; re-login.
    Unauthorized,
    /// Network / other non-success error → keep last-known-good.
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

/// Fetches the raw `/api/usage-summary` response for `vct fetch cursor`.
///
/// Synthesizes the session cookie from the stored JWT and sends one request. It
/// does **not** check the JWT `exp` (an expired token just 401s) and never
/// writes the file back. Returns `(status_code, body)`; a non-2xx status is
/// left for the caller to surface.
///
/// # Errors
///
/// Returns an error if `auth.json` is missing, has no usable session, or the
/// request cannot be sent.
pub(crate) fn fetch_cursor_raw(client: &Client) -> Result<(u16, String)> {
    fetch_cursor_raw_from(client, CURSOR_USAGE_URL, &get_cursor_auth_path()?)
}

/// The injectable core of [`fetch_cursor_raw`]: reads the session from an
/// explicit `auth.json` path and fetches the raw usage body from an explicit
/// `usage_url`. Production passes [`CURSOR_USAGE_URL`] + `~/.config/cursor/auth.json`;
/// tests point them at a local mock server and a temp auth file.
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
        // The file exists but yields no usable session (cleared on logout, no
        // `accessToken`, or a malformed JWT) — an auth failure, not a network
        // blip — so nudge login instead of silently showing "no Cursor quota".
        let session = match read_cursor_session(&body) {
            Some(s) => s,
            None => return QuotaOutcome::NeedsLogin,
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
        // API reports percent remaining; the gauge shows used (100 - remaining).
        assert_eq!(snap.total.as_ref().unwrap().used_percent, 6.0);
        assert_eq!(snap.auto.as_ref().unwrap().used_percent, 0.0);
        assert_eq!(snap.api.as_ref().unwrap().used_percent, 56.0);
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
    fn on_demand_falls_back_to_team_usage() {
        // Individual on-demand disabled, but the team pool carries the spend.
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
        // 0% remaining -> 100% used -> limit reached.
        let body =
            r#"{ "isUnlimited": false, "individualUsage": { "plan": { "totalPercentUsed": 0 } } }"#;
        let snap = map_cursor_usage(body, 1).unwrap();
        assert!(snap.limit_reached);
    }

    #[test]
    fn unlimited_never_flags_limit() {
        // Even at 0% remaining (fully used), the unlimited flag suppresses LIMIT.
        let body =
            r#"{ "isUnlimited": true, "individualUsage": { "plan": { "totalPercentUsed": 0 } } }"#;
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
            FetchResult::Ok(snap) => assert_eq!(snap.plan_type.as_deref(), Some("free")),
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
