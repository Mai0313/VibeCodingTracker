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
use crate::quota::wham::WhamResult;
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
/// the stored (or in-memory) access token is used until the wham endpoint 401s,
/// then it is refreshed (rotating + writing back the refresh token) and retried.
#[derive(Default)]
pub struct CodexState {
    /// Cached access token + the `auth.json` mtime it came from, so a re-login /
    /// account switch (which rewrites the file) drops the stale token.
    token: Option<(String, Option<SystemTime>)>,
    cooldown: RefreshCooldown,
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
            match self.fetch_with_refresh(client, auth, now) {
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
    fn fetch_with_refresh(
        &mut self,
        client: &reqwest::blocking::Client,
        auth: &std::path::Path,
        now: i64,
    ) -> CodexFetch {
        let body = match std::fs::read_to_string(auth) {
            Ok(b) => b,
            Err(_) => return CodexFetch::Transient,
        };
        let (file_token, account_id) = match wham::parse_auth(&body) {
            Ok(x) => x,
            Err(_) => return CodexFetch::Transient,
        };
        let cur_mtime = file_mtime(auth);
        // Reuse the cached token only if auth.json hasn't changed since we cached
        // it; a re-login / account switch rewrites the file and must be picked up.
        let token = match &self.token {
            Some((t, m)) if *m == cur_mtime => t.clone(),
            _ => file_token,
        };

        match wham::call_wham(client, &token, account_id.as_deref(), now) {
            WhamResult::Ok(snap) => {
                self.cooldown.clear();
                self.token = Some((token, cur_mtime));
                CodexFetch::Ok(snap)
            }
            WhamResult::Unauthorized => {
                if self.cooldown.active(now, cur_mtime) {
                    self.token = None;
                    return CodexFetch::NeedsLogin;
                }
                match wham::refresh_codex(client, auth) {
                    Ok(new_tok) => {
                        self.cooldown.clear();
                        // The successful refresh just rewrote auth.json; key the
                        // cache + cooldown on the post-write mtime (a stale one
                        // would never suppress the next tick).
                        let post_mtime = file_mtime(auth);
                        self.token = Some((new_tok.clone(), post_mtime));
                        match wham::call_wham(client, &new_tok, account_id.as_deref(), now) {
                            WhamResult::Ok(snap) => CodexFetch::Ok(snap),
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
}
