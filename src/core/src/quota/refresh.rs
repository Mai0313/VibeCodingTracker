//! Shared OAuth token-refresh primitives.
//!
//! Provides mtime-keyed refresh backoff ([`RefreshCooldown`]), proactive expiry checks ([`is_expiring`]), non-logging refresh HTTP dispatch ([`send_refresh`]), and mtime-guarded atomic JSON write-back ([`update_json_file_in_place`]) that preserves unknown keys.
//!
//! Functions in this module never log request bodies, response bodies, or tokens.

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::blocking::RequestBuilder;
use serde_json::Value;
use std::path::Path;
use std::time::SystemTime;

/// Duration in seconds that [`RefreshCooldown`] suppresses retries after a refresh failure.
pub const REFRESH_COOLDOWN_SECS: i64 = 300;

/// Skew applied to proactive expiry checks, to absorb a slightly-fast clock.
pub const EXPIRY_SKEW_SECS: i64 = 60;

/// Per-provider refresh backoff keyed on the credential file's modification time.
///
/// Once armed, the cooldown stays active for [`REFRESH_COOLDOWN_SECS`] seconds while the file's mtime is unchanged; any mtime change ends suppression immediately.
#[derive(Default)]
pub struct RefreshCooldown {
    until: i64,
    mtime: Option<SystemTime>,
}

impl RefreshCooldown {
    /// Returns whether the cooldown is active at `now` (Unix seconds) for `cur_mtime`.
    pub fn active(&self, now: i64, cur_mtime: Option<SystemTime>) -> bool {
        now < self.until && self.mtime == cur_mtime
    }

    /// Arms the cooldown for [`REFRESH_COOLDOWN_SECS`] seconds from `now` (Unix seconds), bound to `mtime`.
    pub fn arm(&mut self, now: i64, mtime: Option<SystemTime>) {
        self.until = now + REFRESH_COOLDOWN_SECS;
        self.mtime = mtime;
    }

    /// Clears the cooldown.
    pub fn clear(&mut self) {
        self.until = 0;
        self.mtime = None;
    }
}

/// Returns a file's modification time, or `None` if it cannot be read.
pub fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Returns whether `expires_at_secs` is known and within `skew_secs` of `now_secs`.
///
/// Returns `false` when `expires_at_secs` is `None`.
pub fn is_expiring(expires_at_secs: Option<i64>, now_secs: i64, skew_secs: i64) -> bool {
    expires_at_secs.is_some_and(|exp| exp - now_secs <= skew_secs)
}

/// Sends `req` and returns `(status, body)` without logging the response body.
///
/// Non-2xx statuses are returned as `Ok`; unreadable response bodies yield an empty string.
///
/// # Errors
///
/// Returns an error if sending the HTTP request fails.
pub fn send_refresh(req: RequestBuilder) -> Result<(StatusCode, String)> {
    let resp = req.send().context("token refresh request failed")?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    Ok((status, text))
}

/// Reads the JSON file at `path`, applies `mutate` to its [`Value`], and atomically writes the pretty-printed result back while preserving untouched keys.
///
/// Compares the file's mtime against `expected_mtime` before reading and again before writing; returns `Ok(false)` without writing if the mtime changed, or `Ok(true)` when the write succeeds.
///
/// # Errors
///
/// Returns an error if reading or parsing `path` fails, `mutate` returns an error, or the atomic write fails.
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
    // Re-check before writing to avoid clobbering a concurrent credential rotation by an official CLI (shrinks the TOCTOU window to serialize+rename without file locking).
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
        assert!(!is_expiring(Some(1061), 1000, 60));
        assert!(is_expiring(Some(1060), 1000, 60));
        assert!(is_expiring(Some(900), 1000, 60));
    }

    #[test]
    fn cooldown_respects_mtime_change() {
        let mt0 = SystemTime::UNIX_EPOCH;
        let mt1 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(5);
        let mut cd = RefreshCooldown::default();
        cd.arm(1000, Some(mt0));
        assert!(cd.active(1100, Some(mt0)));
        assert!(!cd.active(1100, Some(mt1)));
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
        assert_eq!(v["auth_mode"], "chatgpt");
        assert_eq!(v["extra"], 123);
        assert!(v.get("OPENAI_API_KEY").is_some());

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
        assert_eq!(v["claudeAiOauth"]["expiresAt"], 1783108188604i64);
        assert!(v["claudeAiOauth"]["expiresAt"].is_number());
        assert_eq!(v["claudeAiOauth"]["subscriptionType"], "max");
        assert_eq!(v["claudeAiOauth"]["rateLimitTier"], "t");
        assert_eq!(v["designOauth"]["accessToken"], "design");
    }
}
