//! Shared OAuth token-refresh primitives used by the Claude / Codex quota
//! fetchers.
//!
//! The provider-specific request/response shapes live in each fetcher; this
//! module owns the cross-cutting concerns:
//!
//! - a per-provider refresh **backoff** so a revoked token cannot make the
//!   worker hammer the token endpoint every tick (`RefreshCooldown`),
//! - **write-back that preserves unknown fields** by mutating a whole
//!   `serde_json::Value` and re-checking the file mtime just before the
//!   atomic write (TOCTOU guard against a concurrently-running official CLI),
//! - the near-expiry check and the RFC3339 timestamp formats the provider
//!   CLIs write.
//!
//! No function here ever logs a request/response body or a token; callers only
//! surface the HTTP status.

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::blocking::RequestBuilder;
use serde_json::Value;
use std::path::Path;
use std::time::SystemTime;

/// Cooldown after a refresh failure, so a revoked/rotated-away refresh token
/// cannot make the 10s worker retry the token endpoint every tick (B1).
pub const REFRESH_COOLDOWN_SECS: i64 = 300;

/// Skew applied to proactive expiry checks, to absorb a slightly-fast clock.
pub const EXPIRY_SKEW_SECS: i64 = 60;

/// Per-provider refresh backoff keyed on the credential file's mtime.
///
/// After a refresh failure the worker arms the cooldown; while it is active a
/// refresh is skipped (the panel shows the login hint) — *unless* the
/// credential file's mtime changes, which means the user re-logged in or the
/// official CLI rotated the token, so a retry is worthwhile immediately.
#[derive(Default)]
pub struct RefreshCooldown {
    until: i64,
    mtime: Option<SystemTime>,
}

impl RefreshCooldown {
    /// Whether a refresh should be skipped right now.
    pub fn active(&self, now: i64, cur_mtime: Option<SystemTime>) -> bool {
        now < self.until && self.mtime == cur_mtime
    }

    /// Arms the cooldown after a refresh failure.
    pub fn arm(&mut self, now: i64, mtime: Option<SystemTime>) {
        self.until = now + REFRESH_COOLDOWN_SECS;
        self.mtime = mtime;
    }

    /// Clears the cooldown after a success.
    pub fn clear(&mut self) {
        self.until = 0;
        self.mtime = None;
    }
}

/// Returns a file's modification time, or `None` if it cannot be read.
pub fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// True when `expires_at_secs` is known and within `skew_secs` of `now_secs`.
///
/// An unknown expiry (`None`) returns `false` — we never refresh blindly, which
/// would spin the token endpoint (see B1).
pub fn is_expiring(expires_at_secs: Option<i64>, now_secs: i64, skew_secs: i64) -> bool {
    expires_at_secs.is_some_and(|exp| exp - now_secs <= skew_secs)
}

/// Sends a refresh request and returns `(status, body)` without ever logging
/// the body (which echoes tokens). The caller checks the status.
///
/// # Errors
///
/// Returns an error only if the request could not be sent (network).
pub fn send_refresh(req: RequestBuilder) -> Result<(StatusCode, String)> {
    let resp = req.send().context("token refresh request failed")?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    Ok((status, text))
}

/// Reads a credential file, applies `mutate` to only the fields it touches, and
/// writes the whole `Value` back atomically (pretty), preserving every other
/// key.
///
/// Re-checks the file's mtime against `expected_mtime` just before writing and
/// **aborts the write** (returns `Ok(false)`) if it changed — a concurrently
/// running official CLI may have rotated the token, and clobbering it would log
/// that CLI out. Returns `Ok(true)` when the write happened.
///
/// # Errors
///
/// Returns an error if the file cannot be read or parsed, or the write fails.
pub fn update_json_file_in_place(
    path: &Path,
    expected_mtime: Option<SystemTime>,
    mutate: impl FnOnce(&mut Value) -> Result<()>,
) -> Result<bool> {
    // TOCTOU guard: bail before reading if the file changed under us.
    if file_mtime(path) != expected_mtime {
        return Ok(false);
    }
    let body = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let mut root: Value = serde_json::from_str(&body).context("Failed to parse credential file")?;
    mutate(&mut root)?;
    // Re-check right before writing: a concurrent official CLI may have rotated
    // the credential during the read/mutate above. This shrinks the TOCTOU
    // window to the serialize+rename below (fully closing it would need file
    // locking).
    if file_mtime(path) != expected_mtime {
        return Ok(false);
    }
    crate::utils::write_json_atomic_pretty(path, &root)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_expiring_unknown_is_never_expiring() {
        assert!(!is_expiring(None, 1000, 60));
    }

    #[test]
    fn is_expiring_boundaries() {
        // Expires in 61s, skew 60 → not expiring yet.
        assert!(!is_expiring(Some(1061), 1000, 60));
        // Expires in exactly 60s → expiring.
        assert!(is_expiring(Some(1060), 1000, 60));
        // Already expired → expiring.
        assert!(is_expiring(Some(900), 1000, 60));
    }

    #[test]
    fn cooldown_respects_mtime_change() {
        let mt0 = SystemTime::UNIX_EPOCH;
        let mt1 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(5);
        let mut cd = RefreshCooldown::default();
        cd.arm(1000, Some(mt0));
        // Within cooldown, same mtime → active (skip refresh).
        assert!(cd.active(1100, Some(mt0)));
        // Within cooldown but mtime changed → not active (retry).
        assert!(!cd.active(1100, Some(mt1)));
        // Past cooldown → not active.
        assert!(!cd.active(2000, Some(mt0)));
    }

    #[test]
    fn update_in_place_preserves_unknown_and_aborts_on_mtime_change() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            r#"{{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{{"access_token":"old","refresh_token":"oldr"}},"extra":123}}"#
        )
        .unwrap();
        drop(f);

        let mtime = file_mtime(&path);
        let wrote = update_json_file_in_place(&path, mtime, |root| {
            let t = root
                .get_mut("tokens")
                .and_then(|v| v.as_object_mut())
                .unwrap();
            t.insert("access_token".into(), serde_json::json!("new"));
            t.insert("refresh_token".into(), serde_json::json!("newr"));
            Ok(())
        })
        .unwrap();
        assert!(wrote);

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["tokens"]["access_token"], "new");
        assert_eq!(v["tokens"]["refresh_token"], "newr");
        // Unknown fields survive.
        assert_eq!(v["auth_mode"], "chatgpt");
        assert_eq!(v["extra"], 123);
        assert!(v.get("OPENAI_API_KEY").is_some());

        // A stale expected_mtime aborts the write.
        let wrote2 = update_json_file_in_place(&path, Some(SystemTime::UNIX_EPOCH), |root| {
            root["tokens"]["access_token"] = serde_json::json!("should-not-write");
            Ok(())
        })
        .unwrap();
        assert!(!wrote2);
        let v2: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v2["tokens"]["access_token"], "new");
    }

    #[test]
    fn claude_write_back_preserves_design_oauth_and_ms_expiry() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".credentials.json");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            r#"{{"claudeAiOauth":{{"accessToken":"old","refreshToken":"oldr","expiresAt":1,"scopes":["user:inference"],"subscriptionType":"max","rateLimitTier":"t"}},"designOauth":{{"accessToken":"design"}}}}"#
        )
        .unwrap();
        drop(f);

        let mtime = file_mtime(&path);
        let wrote = update_json_file_in_place(&path, mtime, |root| {
            let o = root
                .get_mut("claudeAiOauth")
                .and_then(|v| v.as_object_mut())
                .unwrap();
            o.insert("accessToken".into(), serde_json::json!("newacc"));
            o.insert("refreshToken".into(), serde_json::json!("newref"));
            o.insert("expiresAt".into(), serde_json::json!(1783108188604i64));
            Ok(())
        })
        .unwrap();
        assert!(wrote);

        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["claudeAiOauth"]["accessToken"], "newacc");
        assert_eq!(v["claudeAiOauth"]["refreshToken"], "newref");
        // expiresAt stays a NUMBER (ms), not a string.
        assert_eq!(v["claudeAiOauth"]["expiresAt"], 1783108188604i64);
        assert!(v["claudeAiOauth"]["expiresAt"].is_number());
        // Preserved siblings.
        assert_eq!(v["claudeAiOauth"]["subscriptionType"], "max");
        assert_eq!(v["claudeAiOauth"]["rateLimitTier"], "t");
        assert_eq!(v["designOauth"]["accessToken"], "design");
    }
}
