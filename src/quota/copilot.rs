//! GitHub Copilot quota fetcher.
//!
//! Reads the long-lived GitHub OAuth token (`gho_...`) from the Copilot CLI's
//! `~/.copilot/config.json`, calls the internal usage API
//! (`GET /copilot_internal/user`), and maps the premium-interactions quota into
//! the shared snapshot shape. The token is long-lived and the file carries no
//! refresh token, so there is **no** token refresh here: a 401/403 means the
//! account is logged out and surfaces a `copilot login` hint.
//!
//! The request impersonates the Copilot CLI (the client the token belongs to):
//! a `GitHubCopilotCLI/<version>` User-Agent (version detected from
//! `copilot --version`, cached for the day) plus `Copilot-Integration-Id:
//! copilot-cli`. These identity headers are camouflage; the endpoint answers a
//! bare bearer token.

use crate::models::{CopilotQuotaSnapshot, CopilotUserResponse, QuotaSource, QuotaWindow};
use crate::quota::http::{detect_cli_version, iso_to_unix_secs};
use crate::quota::provider::QuotaOutcome;
use crate::utils::get_copilot_config_path;
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use std::sync::OnceLock;

/// Path of the Copilot internal usage endpoint (the host is derived per account
/// so GHE data-residency logins reach `api.<host>` instead of api.github.com).
const COPILOT_USAGE_PATH: &str = "/copilot_internal/user";
/// Fallback Copilot CLI version for the User-Agent when `copilot --version`
/// cannot be resolved (CLI absent or unreadable). Bump occasionally.
const COPILOT_FALLBACK_VERSION: &str = "1.0.68";
/// The Copilot CLI's integration id (confirmed from the CLI bundle).
const COPILOT_INTEGRATION_ID: &str = "copilot-cli";
/// GitHub API version pinned by the request.
const COPILOT_API_VERSION: &str = "2025-04-01";
/// Login hint shown when the Copilot token is rejected.
pub const COPILOT_LOGIN_HINT: &str = "run: copilot login";

/// Builds the Copilot CLI's request User-Agent, e.g. `GitHubCopilotCLI/1.0.68`.
///
/// The version is detected from the installed CLI (see
/// [`crate::quota::http::detect_cli_version`]) so the UA tracks the real client
/// rather than drifting from a hardcoded constant.
fn copilot_ua() -> &'static str {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        format!(
            "GitHubCopilotCLI/{}",
            detect_cli_version("copilot", "copilot_version.json", COPILOT_FALLBACK_VERSION)
        )
    })
    .as_str()
}

/// Strips `//` line comments and `/* */` block comments from a JSONC string,
/// respecting string context so a `//` inside a value (e.g. a
/// `"https://github.com:login"` key) is never removed.
fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                // Skip to end of line, preserving the newline for line counting.
                for nc in chars.by_ref() {
                    if nc == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for nc in chars.by_ref() {
                    if prev == '*' && nc == '/' {
                        break;
                    }
                    prev = nc;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// A resolved Copilot credential: the entitlement API URL + the OAuth token.
struct CopilotCreds {
    api_url: String,
    token: String,
}

/// Reads the `gho_...` GitHub token from the (JSONC) Copilot config and derives
/// the entitlement API host from the account's login host.
///
/// Prefers the token for `lastLoggedInUser` (`<host>:<login>`) so a config that
/// still holds several accounts queries the one the user is actually on, then
/// falls back to the first `https://github.com` entry.
fn read_copilot_creds(body: &str) -> Option<CopilotCreds> {
    let stripped = strip_jsonc_comments(body);
    let root: serde_json::Value = serde_json::from_str(&stripped).ok()?;
    let tokens = root.get("copilotTokens")?.as_object()?;

    let entry_token =
        |v: &serde_json::Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_string);

    // The `copilotTokens` keys are `<host>:<login>`; match the last-logged-in
    // account first, then fall back to the first GitHub entry.
    let preferred = root.get("lastLoggedInUser").and_then(|user| {
        let host = user.get("host")?.as_str()?;
        let login = user.get("login")?.as_str()?;
        let key = format!("{host}:{login}");
        let token = tokens.get(&key).and_then(entry_token)?;
        Some((key, token))
    });

    let (key, token) = match preferred {
        Some(pair) => pair,
        None => {
            let (k, v) = tokens
                .iter()
                .find(|(k, _)| k.starts_with("https://github.com"))?;
            (k.clone(), entry_token(v)?)
        }
    };

    Some(CopilotCreds {
        api_url: copilot_api_url(&key),
        token,
    })
}

/// Derives the entitlement API URL from a `copilotTokens` key (`<host>:<login>`).
///
/// `https://github.com:me` -> `https://api.github.com/copilot_internal/user`; a
/// GHE data-residency host (`https://acme.ghe.com:me`) maps to
/// `https://api.acme.ghe.com/...` so those tokens reach the correct host.
fn copilot_api_url(key: &str) -> String {
    let host = key.rsplit_once(':').map(|(h, _)| h).unwrap_or(key);
    let domain = host
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    format!("https://api.{domain}{COPILOT_USAGE_PATH}")
}

/// Maps a `/copilot_internal/user` body into a [`CopilotQuotaSnapshot`] (pure).
///
/// # Errors
///
/// Returns an error if the body is not valid JSON in the expected shape.
pub fn map_copilot_user(body: &str, now: i64) -> Result<CopilotQuotaSnapshot> {
    let resp: CopilotUserResponse =
        serde_json::from_str(body).context("Failed to parse Copilot user response")?;

    // Prefer the UTC instant; fall back to the date-only field (midnight UTC)
    // so the gauge still shows a reset countdown when only that is returned.
    let reset = resp
        .quota_reset_date_utc
        .as_deref()
        .and_then(iso_to_unix_secs)
        .or_else(|| {
            resp.quota_reset_date
                .as_deref()
                .and_then(|d| iso_to_unix_secs(&format!("{d}T00:00:00Z")))
        });

    let snaps = resp.quota_snapshots.as_ref();
    let premium_entry = snaps.and_then(|s| s.premium_interactions.as_ref());

    let premium_unlimited = premium_entry.and_then(|e| e.unlimited).unwrap_or(false);
    let premium_remaining = premium_entry.and_then(|e| e.remaining).map(|v| v as i64);
    let premium_entitlement = premium_entry.and_then(|e| e.entitlement).map(|v| v as i64);

    // Derive the used-percent from `percent_remaining` (preferred) or the
    // remaining/entitlement ratio. A zero-entitlement placeholder (common for
    // token-based / business seats) carries no real gauge, so drop it rather
    // than render a misleading empty bar.
    let placeholder = matches!((premium_remaining, premium_entitlement), (Some(0), Some(0)));
    let premium = if premium_unlimited || placeholder {
        None
    } else {
        premium_entry.and_then(|e| {
            let used = match e.percent_remaining {
                Some(pr) => 100.0 - pr,
                None => match (e.remaining, e.entitlement) {
                    (Some(r), Some(t)) if t > 0.0 => (1.0 - r / t) * 100.0,
                    _ => return None,
                },
            };
            Some(QuotaWindow {
                used_percent: used.clamp(0.0, 100.0),
                resets_at_unix: reset,
            })
        })
    };

    let chat_unlimited = snaps
        .and_then(|s| s.chat.as_ref())
        .and_then(|e| e.unlimited)
        .unwrap_or(false);
    let completions_unlimited = snaps
        .and_then(|s| s.completions.as_ref())
        .and_then(|e| e.unlimited)
        .unwrap_or(false);

    let limit_reached = !premium_unlimited
        && premium
            .as_ref()
            .map(|w| w.used_percent >= 100.0)
            .unwrap_or(false);

    Ok(CopilotQuotaSnapshot {
        source: QuotaSource::Api,
        fetched_at: now,
        plan_type: resp.copilot_plan.filter(|s| !s.is_empty()),
        premium,
        premium_remaining,
        premium_entitlement,
        premium_unlimited,
        chat_unlimited,
        completions_unlimited,
        limit_reached,
        needs_login: false,
    })
}

/// Outcome of a single usage request.
enum FetchResult {
    Ok(CopilotQuotaSnapshot),
    /// 401/403 → the token is rejected; there is no refresh, so re-login.
    Unauthorized,
    /// Network / other non-success error → keep last-known-good.
    Transient,
}

/// Calls the Copilot usage API with `token`, impersonating the Copilot CLI.
fn fetch_copilot_user(client: &Client, api_url: &str, token: &str, now: i64) -> FetchResult {
    let resp = match client
        .get(api_url)
        .header(reqwest::header::AUTHORIZATION, format!("token {token}"))
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, copilot_ua())
        .header("Copilot-Integration-Id", COPILOT_INTEGRATION_ID)
        .header("X-GitHub-Api-Version", COPILOT_API_VERSION)
        .send()
    {
        Ok(r) => r,
        Err(_) => return FetchResult::Transient,
    };
    let status = resp.status();
    // Copilot has no refresh; a logged-out account answers 401 or 403.
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return FetchResult::Unauthorized;
    }
    if !status.is_success() {
        return FetchResult::Transient;
    }
    match resp.text() {
        Ok(text) => match map_copilot_user(&text, now) {
            Ok(snap) => FetchResult::Ok(snap),
            Err(_) => FetchResult::Transient,
        },
        Err(_) => FetchResult::Transient,
    }
}

/// Per-worker Copilot state. Copilot's token is long-lived with no refresh, so
/// there is no in-memory token cache or refresh backoff to keep.
#[derive(Default)]
pub struct CopilotState;

impl CopilotState {
    /// One worker tick: read the token, call the usage API.
    pub fn resolve(&mut self, client: &Client) -> QuotaOutcome<CopilotQuotaSnapshot> {
        let now = chrono::Local::now().timestamp();
        let path = match get_copilot_config_path() {
            Ok(p) => p,
            Err(_) => return QuotaOutcome::Transient,
        };
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(_) => return QuotaOutcome::Transient,
        };
        let creds = match read_copilot_creds(&body) {
            Some(c) => c,
            None => return QuotaOutcome::NeedsLogin,
        };
        match fetch_copilot_user(client, &creds.api_url, &creds.token, now) {
            FetchResult::Ok(snap) => QuotaOutcome::Data(snap),
            FetchResult::Unauthorized => QuotaOutcome::NeedsLogin,
            FetchResult::Transient => QuotaOutcome::Transient,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real (redacted) config.json shape: JSONC with leading comments and a
    // URL key whose value contains `//`.
    const CONFIG: &str = r#"// User settings belong in settings.json.
// This file is managed automatically.
{
  "firstLaunchAt": "2026-04-27T16:16:13.673Z",
  "copilotTokens": {
    "https://github.com:octocat": "gho_EXAMPLETOKEN"
  }
}"#;

    #[test]
    fn strips_comments_without_eating_url_slashes() {
        let out = strip_jsonc_comments(CONFIG);
        assert!(!out.contains("// User settings"));
        // The `//` inside the string value must survive.
        assert!(out.contains("https://github.com:octocat"));
        let creds = read_copilot_creds(CONFIG).unwrap();
        assert_eq!(creds.token, "gho_EXAMPLETOKEN");
        assert_eq!(creds.api_url, "https://api.github.com/copilot_internal/user");
    }

    #[test]
    fn block_comments_are_stripped_outside_strings() {
        let src = r#"{ /* c */ "a": "x /* not a comment */ y" }"#;
        let out = strip_jsonc_comments(src);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v.get("a").unwrap().as_str().unwrap(),
            "x /* not a comment */ y"
        );
    }

    #[test]
    fn no_token_returns_none() {
        assert!(read_copilot_creds(r#"{ "copilotTokens": {} }"#).is_none());
        assert!(read_copilot_creds(r#"{}"#).is_none());
    }

    #[test]
    fn prefers_last_logged_in_account_token() {
        let cfg = r#"{
            "copilotTokens": {
                "https://github.com:alice": "gho_ALICE",
                "https://github.com:bob": "gho_BOB"
            },
            "lastLoggedInUser": { "host": "https://github.com", "login": "bob" }
        }"#;
        assert_eq!(read_copilot_creds(cfg).unwrap().token, "gho_BOB");
    }

    #[test]
    fn falls_back_to_a_github_token_without_last_user() {
        let cfg = r#"{ "copilotTokens": { "https://github.com:alice": "gho_ALICE" } }"#;
        assert_eq!(read_copilot_creds(cfg).unwrap().token, "gho_ALICE");
    }

    #[test]
    fn derives_api_host_from_login_host() {
        assert_eq!(
            copilot_api_url("https://github.com:me"),
            "https://api.github.com/copilot_internal/user"
        );
        // GHE data-residency host keeps its subdomain.
        assert_eq!(
            copilot_api_url("https://acme.ghe.com:me"),
            "https://api.acme.ghe.com/copilot_internal/user"
        );
    }

    #[test]
    fn ghe_host_creds_target_the_ghe_api() {
        let cfg = r#"{
            "copilotTokens": { "https://acme.ghe.com:me": "gho_GHE" },
            "lastLoggedInUser": { "host": "https://acme.ghe.com", "login": "me" }
        }"#;
        let creds = read_copilot_creds(cfg).unwrap();
        assert_eq!(creds.token, "gho_GHE");
        assert_eq!(creds.api_url, "https://api.acme.ghe.com/copilot_internal/user");
    }

    const USER: &str = r#"{
      "copilot_plan": "individual",
      "quota_reset_date": "2026-08-01",
      "quota_reset_date_utc": "2026-08-01T00:00:00.000Z",
      "quota_snapshots": {
        "premium_interactions": { "percent_remaining": 97.6, "remaining": 1464, "entitlement": 1500, "unlimited": false },
        "chat": { "unlimited": true },
        "completions": { "unlimited": true }
      }
    }"#;

    #[test]
    fn maps_copilot_user() {
        let snap = map_copilot_user(USER, 1_000_000).unwrap();
        assert_eq!(snap.source, QuotaSource::Api);
        assert_eq!(snap.plan_type.as_deref(), Some("individual"));
        let prem = snap.premium.as_ref().unwrap();
        // 100 - 97.6 = 2.4 used.
        assert!((prem.used_percent - 2.4).abs() < 1e-6);
        assert!(prem.resets_at_unix.unwrap() > 0);
        assert_eq!(snap.premium_remaining, Some(1464));
        assert_eq!(snap.premium_entitlement, Some(1500));
        assert!(snap.chat_unlimited);
        assert!(snap.completions_unlimited);
        assert!(!snap.limit_reached);
        assert!(!snap.needs_login);
    }

    #[test]
    fn derives_used_from_ratio_when_percent_absent() {
        let body = r#"{ "quota_snapshots": { "premium_interactions": { "remaining": 750, "entitlement": 1500 } } }"#;
        let snap = map_copilot_user(body, 1).unwrap();
        assert!((snap.premium.unwrap().used_percent - 50.0).abs() < 1e-6);
    }

    #[test]
    fn drops_zero_entitlement_placeholder() {
        let body = r#"{ "quota_snapshots": { "premium_interactions": { "remaining": 0, "entitlement": 0, "percent_remaining": 100 } } }"#;
        let snap = map_copilot_user(body, 1).unwrap();
        assert!(
            snap.premium.is_none(),
            "0/0 placeholder is not a real gauge"
        );
        assert!(!snap.limit_reached);
    }

    #[test]
    fn flags_limit_when_exhausted() {
        let body = r#"{ "quota_snapshots": { "premium_interactions": { "percent_remaining": 0, "remaining": 0, "entitlement": 1500 } } }"#;
        let snap = map_copilot_user(body, 1).unwrap();
        assert_eq!(snap.premium.as_ref().unwrap().used_percent, 100.0);
        assert!(snap.limit_reached);
    }

    #[test]
    fn missing_snapshots_is_tolerated() {
        let snap = map_copilot_user(r#"{ "copilot_plan": "business" }"#, 1).unwrap();
        assert!(snap.premium.is_none());
        assert_eq!(snap.plan_type.as_deref(), Some("business"));
        assert!(!snap.limit_reached);
    }

    #[test]
    fn reset_falls_back_to_date_only_field() {
        // Only the date-only field is present (no UTC instant).
        let body = r#"{
          "quota_reset_date": "2026-08-01",
          "quota_snapshots": { "premium_interactions": { "percent_remaining": 50, "remaining": 750, "entitlement": 1500 } }
        }"#;
        let snap = map_copilot_user(body, 1).unwrap();
        assert!(
            snap.premium.as_ref().unwrap().resets_at_unix.unwrap() > 0,
            "date-only reset should still yield a timestamp"
        );
    }
}
