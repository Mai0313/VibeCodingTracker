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

/// The Copilot internal usage endpoint.
const COPILOT_USAGE_URL: &str = "https://api.github.com/copilot_internal/user";
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

/// Reads the first `gho_...` GitHub token from the (JSONC) Copilot config.
///
/// Picks the first `copilotTokens` entry whose key names a `https://github.com`
/// host, matching what the Copilot CLI itself uses.
fn read_copilot_token(body: &str) -> Option<String> {
    let stripped = strip_jsonc_comments(body);
    let root: serde_json::Value = serde_json::from_str(&stripped).ok()?;
    let tokens = root.get("copilotTokens")?.as_object()?;
    tokens
        .iter()
        .find(|(k, _)| k.starts_with("https://github.com"))
        .and_then(|(_, v)| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Maps a `/copilot_internal/user` body into a [`CopilotQuotaSnapshot`] (pure).
///
/// # Errors
///
/// Returns an error if the body is not valid JSON in the expected shape.
pub fn map_copilot_user(body: &str, now: i64) -> Result<CopilotQuotaSnapshot> {
    let resp: CopilotUserResponse =
        serde_json::from_str(body).context("Failed to parse Copilot user response")?;

    let reset = resp
        .quota_reset_date_utc
        .as_deref()
        .and_then(iso_to_unix_secs);

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
fn fetch_copilot_user(client: &Client, token: &str, now: i64) -> FetchResult {
    let resp = match client
        .get(COPILOT_USAGE_URL)
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
        let token = match read_copilot_token(&body) {
            Some(t) => t,
            None => return QuotaOutcome::NeedsLogin,
        };
        match fetch_copilot_user(client, &token, now) {
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
        let token = read_copilot_token(CONFIG).unwrap();
        assert_eq!(token, "gho_EXAMPLETOKEN");
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
        assert!(read_copilot_token(r#"{ "copilotTokens": {} }"#).is_none());
        assert!(read_copilot_token(r#"{}"#).is_none());
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
}
