//! Quota orchestration for the `usage` panels.
//!
//! Each provider (Claude / Codex) runs its own background worker
//! ([`provider::spawn_quota_worker`]) that refreshes a shared snapshot every
//! ~10s, seeded from an on-disk cache. The provider-specific fetch + token
//! refresh lives in [`claude`] and [`wham`] (+ this module's [`CodexState`]);
//! the shared HTTP + refresh primitives live in [`http`] and [`refresh`].

pub mod cache;
pub mod claude;
pub mod codex_session;
pub mod copilot;
pub mod cursor;
pub mod fetch;
pub mod http;
pub mod provider;
pub mod refresh;
pub mod wham;

pub use cache::{
    load_claude_cache, load_codex_cache, load_copilot_cache, load_cursor_cache, save_claude_cache,
    save_codex_cache, save_copilot_cache, save_cursor_cache,
};
pub use claude::{CLAUDE_LOGIN_HINT, ClaudeState};
pub use copilot::{COPILOT_LOGIN_HINT, CopilotState};
pub use cursor::{CURSOR_LOGIN_HINT, CursorState};
pub use provider::{QuotaOutcome, QuotaSnapshot, spawn_quota_worker};

use crate::models::CodexQuotaSnapshot;
use crate::quota::refresh::{RefreshCooldown, file_mtime};
use crate::quota::wham::{ResetCreditsResult, WhamResult};
use std::time::SystemTime;

/// Login hint shown when Codex refresh fails.
pub const CODEX_LOGIN_HINT: &str = "run: codex auth login";

/// Outcome of a Codex wham fetch (with reactive refresh).
enum CodexFetch {
    Ok(CodexQuotaSnapshot),
    /// Token rejected and refresh failed → show login hint (keep session data).
    NeedsLogin,
    /// Network / non-auth error → fall back to session logs.
    Transient,
}

/// Per-worker Codex state: an in-memory access token + refresh backoff.
///
/// Codex `auth.json` carries no explicit expiry, so refresh is **reactive**:
/// the stored (or in-memory) access token is used until a Codex quota endpoint
/// 401s, then it is refreshed and the rejected request is retried once.
#[derive(Default)]
pub struct CodexState {
    /// Cached access token + the `auth.json` mtime it came from, so a re-login /
    /// account switch (which rewrites the file) drops the stale token.
    token: Option<(String, Option<SystemTime>)>,
    cooldown: RefreshCooldown,
    /// Separate backoff for a details-only 401, which must never blank a valid
    /// usage snapshot or turn into a login hint.
    reset_credits_cooldown: RefreshCooldown,
}

impl CodexState {
    /// One worker tick: wham API with reactive refresh, else session fallback.
    pub fn resolve(
        &mut self,
        client: &reqwest::blocking::Client,
    ) -> QuotaOutcome<CodexQuotaSnapshot> {
        let now = chrono::Local::now().timestamp();
        let auth = crate::utils::resolve_paths()
            .ok()
            .map(|p| p.codex_dir.join("auth.json"));

        if let Some(auth) = &auth
            && auth.exists()
        {
            match self.fetch_with_refresh(
                client,
                auth,
                now,
                wham::WHAM_URL,
                wham::WHAM_RESET_CREDITS_URL,
                wham::CODEX_TOKEN_URL,
            ) {
                CodexFetch::Ok(snap) => return QuotaOutcome::Data(snap),
                CodexFetch::NeedsLogin => {
                    // Keep any session-fallback data, flag the login hint (S3).
                    let mut snap = codex_session::latest_session_rate_limits()
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    snap.needs_login = true;
                    snap.fetched_at = now;
                    return QuotaOutcome::Data(snap);
                }
                CodexFetch::Transient => { /* fall through to session logs */ }
            }
        }

        match codex_session::latest_session_rate_limits() {
            Ok(Some(snap)) => QuotaOutcome::Data(snap),
            _ => QuotaOutcome::Transient,
        }
    }

    /// wham call with a reactive 401 → refresh → retry-once.
    ///
    /// The URL parameters are the usage, reset-credit details, and token
    /// endpoints. Production passes the [`wham`] module constants; tests point
    /// them at a local mock server so the full refresh path runs offline.
    fn fetch_with_refresh(
        &mut self,
        client: &reqwest::blocking::Client,
        auth: &std::path::Path,
        now: i64,
        wham_url: &str,
        reset_credits_url: &str,
        token_url: &str,
    ) -> CodexFetch {
        let body = match std::fs::read_to_string(auth) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("codex quota: failed to read {}: {e}", auth.display());
                return CodexFetch::Transient;
            }
        };
        let (file_token, account_id) = match wham::parse_auth(&body) {
            Ok(x) => x,
            Err(e) => {
                log::warn!("codex quota: failed to parse auth.json: {e}");
                return CodexFetch::Transient;
            }
        };
        let cur_mtime = file_mtime(auth);
        // Reuse the cached token only if auth.json hasn't changed since we cached
        // it; a re-login / account switch rewrites the file and must be picked up.
        let token = match &self.token {
            Some((t, m)) if *m == cur_mtime => t.clone(),
            _ => file_token,
        };

        match wham::call_wham(client, &token, account_id.as_deref(), now, wham_url) {
            WhamResult::Ok(snap) => self.finish_with_reset_credits(
                client,
                auth,
                snap,
                token,
                cur_mtime,
                account_id.as_deref(),
                now,
                reset_credits_url,
                token_url,
                true,
            ),
            WhamResult::Unauthorized => {
                if self.cooldown.active(now, cur_mtime) {
                    self.token = None;
                    return CodexFetch::NeedsLogin;
                }
                match wham::refresh_codex(client, auth, token_url) {
                    Ok(new_tok) => {
                        self.cooldown.clear();
                        // The successful refresh just rewrote auth.json; key the
                        // cache + cooldown on the post-write mtime (a stale one
                        // would never suppress the next tick).
                        let post_mtime = file_mtime(auth);
                        self.token = Some((new_tok.clone(), post_mtime));
                        match wham::call_wham(
                            client,
                            &new_tok,
                            account_id.as_deref(),
                            now,
                            wham_url,
                        ) {
                            WhamResult::Ok(snap) => self.finish_with_reset_credits(
                                client,
                                auth,
                                snap,
                                new_tok,
                                post_mtime,
                                account_id.as_deref(),
                                now,
                                reset_credits_url,
                                token_url,
                                false,
                            ),
                            // A transient retry error keeps the fresh token and
                            // falls back to session data; only a 401 means login.
                            WhamResult::Transient => CodexFetch::Transient,
                            WhamResult::Unauthorized => {
                                self.cooldown.arm(now, post_mtime);
                                self.token = None;
                                CodexFetch::NeedsLogin
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!("codex token refresh failed: {e}");
                        self.cooldown.arm(now, file_mtime(auth));
                        self.token = None;
                        CodexFetch::NeedsLogin
                    }
                }
            }
            WhamResult::Transient => CodexFetch::Transient,
        }
    }

    /// Enriches a successful usage snapshot and optionally refreshes once when
    /// only the reset-credit details endpoint rejects the token.
    #[allow(clippy::too_many_arguments)]
    fn finish_with_reset_credits(
        &mut self,
        client: &reqwest::blocking::Client,
        auth: &std::path::Path,
        mut snap: CodexQuotaSnapshot,
        token: String,
        token_mtime: Option<SystemTime>,
        account_id: Option<&str>,
        now: i64,
        reset_credits_url: &str,
        token_url: &str,
        refresh_on_unauthorized: bool,
    ) -> CodexFetch {
        self.cooldown.clear();
        self.token = Some((token.clone(), token_mtime));

        match wham::call_reset_credit_details(client, &token, account_id, reset_credits_url) {
            ResetCreditsResult::Ok {
                available_count,
                expirations,
            } => {
                self.reset_credits_cooldown.clear();
                snap.reset_credits_available = Some(available_count);
                snap.reset_credit_expirations = Some(expirations);
            }
            ResetCreditsResult::Transient => {}
            ResetCreditsResult::Unauthorized => {
                if !refresh_on_unauthorized {
                    self.reset_credits_cooldown.arm(now, token_mtime);
                    return CodexFetch::Ok(snap);
                }
                if self.reset_credits_cooldown.active(now, token_mtime) {
                    return CodexFetch::Ok(snap);
                }

                let new_token = match wham::refresh_codex(client, auth, token_url) {
                    Ok(new_token) => new_token,
                    Err(e) => {
                        log::warn!("codex token refresh for reset-credit details failed: {e}");
                        self.reset_credits_cooldown.arm(now, file_mtime(auth));
                        return CodexFetch::Ok(snap);
                    }
                };
                let post_mtime = file_mtime(auth);
                self.token = Some((new_token.clone(), post_mtime));
                match wham::call_reset_credit_details(
                    client,
                    &new_token,
                    account_id,
                    reset_credits_url,
                ) {
                    ResetCreditsResult::Ok {
                        available_count,
                        expirations,
                    } => {
                        self.reset_credits_cooldown.clear();
                        snap.reset_credits_available = Some(available_count);
                        snap.reset_credit_expirations = Some(expirations);
                    }
                    ResetCreditsResult::Unauthorized => {
                        self.reset_credits_cooldown.arm(now, post_mtime);
                    }
                    ResetCreditsResult::Transient => {}
                }
            }
        }

        CodexFetch::Ok(snap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    /// The reactive 401 → refresh → retry loop, end-to-end against a mock server.
    ///
    /// The first wham call carries the stale token and 401s; the loop then hits
    /// the (mock) token endpoint, writes the rotated token back to auth.json, and
    /// retries wham with the fresh token, which succeeds. The two wham mocks are
    /// distinguished by their `Authorization` header so the ordering is asserted
    /// structurally rather than by call sequence.
    #[test]
    fn fetch_with_refresh_recovers_from_401() {
        let server = MockServer::start();
        let stale = server.mock(|when, then| {
            when.method(GET)
                .path("/wham")
                .header("authorization", "Bearer stale");
            then.status(401);
        });
        let fresh = server.mock(|when, then| {
            when.method(GET)
                .path("/wham")
                .header("authorization", "Bearer new-access");
            then.status(200).body(r#"{"plan_type":"plus"}"#);
        });
        let reset_credits = server.mock(|when, then| {
            when.method(GET)
                .path("/reset-credits")
                .header("authorization", "Bearer new-access");
            then.status(200)
                .body(r#"{"credits":[],"available_count":0}"#);
        });
        let token = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200).json_body(serde_json::json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh"
            }));
        });

        let dir = tempfile::tempdir().unwrap();
        let auth = dir.path().join("auth.json");
        std::fs::write(
            &auth,
            r#"{"tokens":{"access_token":"stale","refresh_token":"rt","account_id":"acct"}}"#,
        )
        .unwrap();

        let client = crate::quota::http::build_client().unwrap();
        let mut state = CodexState::default();
        let result = state.fetch_with_refresh(
            &client,
            &auth,
            1_000_000,
            &server.url("/wham"),
            &server.url("/reset-credits"),
            &server.url("/token"),
        );

        stale.assert();
        token.assert();
        fresh.assert();
        reset_credits.assert();
        match result {
            CodexFetch::Ok(snap) => assert_eq!(snap.plan_type.as_deref(), Some("plus")),
            _ => panic!("expected a recovered snapshot after refresh"),
        }

        // The rotated token was persisted back to auth.json.
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&auth).unwrap()).unwrap();
        assert_eq!(written["tokens"]["access_token"], "new-access");
    }

    /// A 401 from only the optional details endpoint refreshes once, retries
    /// details with the new token, and keeps the already-successful usage body.
    #[test]
    fn fetch_with_refresh_recovers_from_reset_credit_401() {
        let server = MockServer::start();
        let usage = server.mock(|when, then| {
            when.method(GET)
                .path("/wham")
                .header("authorization", "Bearer stale");
            then.status(200)
                .body(r#"{"plan_type":"plus","rate_limit_reset_credits":{"available_count":2}}"#);
        });
        let stale_details = server.mock(|when, then| {
            when.method(GET)
                .path("/reset-credits")
                .header("authorization", "Bearer stale");
            then.status(401);
        });
        let token = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200).json_body(serde_json::json!({
                "access_token": "new-access",
                "refresh_token": "new-refresh"
            }));
        });
        let fresh_details = server.mock(|when, then| {
            when.method(GET)
                .path("/reset-credits")
                .header("authorization", "Bearer new-access");
            then.status(200).body(
                r#"{"credits":[{"id":"credit-1","reset_type":"codex_rate_limits","status":"available","granted_at":"2026-07-01T00:00:00Z","expires_at":"2026-08-01T00:00:00Z"}],"available_count":1}"#,
            );
        });

        let dir = tempfile::tempdir().unwrap();
        let auth = dir.path().join("auth.json");
        std::fs::write(
            &auth,
            r#"{"tokens":{"access_token":"stale","refresh_token":"rt","account_id":"acct"}}"#,
        )
        .unwrap();

        let client = crate::quota::http::build_client().unwrap();
        let mut state = CodexState::default();
        let result = state.fetch_with_refresh(
            &client,
            &auth,
            1_000_000,
            &server.url("/wham"),
            &server.url("/reset-credits"),
            &server.url("/token"),
        );

        usage.assert();
        stale_details.assert();
        token.assert();
        fresh_details.assert();
        match result {
            CodexFetch::Ok(snap) => {
                assert_eq!(snap.plan_type.as_deref(), Some("plus"));
                assert_eq!(snap.reset_credits_available, Some(1));
                assert_eq!(snap.reset_credit_expirations.unwrap().len(), 1);
            }
            _ => panic!("expected a recovered snapshot after details refresh"),
        }
    }

    /// When refresh itself fails (token endpoint 400), the loop reports
    /// `NeedsLogin` rather than looping or succeeding.
    #[test]
    fn fetch_with_refresh_needs_login_when_refresh_fails() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/wham");
            then.status(401);
        });
        server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(400)
                .json_body(serde_json::json!({ "error": "invalid_grant" }));
        });

        let dir = tempfile::tempdir().unwrap();
        let auth = dir.path().join("auth.json");
        std::fs::write(
            &auth,
            r#"{"tokens":{"access_token":"stale","refresh_token":"rt"}}"#,
        )
        .unwrap();

        let client = crate::quota::http::build_client().unwrap();
        let mut state = CodexState::default();
        let result = state.fetch_with_refresh(
            &client,
            &auth,
            1_000_000,
            &server.url("/wham"),
            &server.url("/reset-credits"),
            &server.url("/token"),
        );
        assert!(matches!(result, CodexFetch::NeedsLogin));
    }
}
