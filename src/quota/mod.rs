//! Codex quota orchestration: resolve precedence (API → session fallback),
//! the background refresh worker, shared state, and cache loading. Also exposes
//! the tiny Claude rate-limits cache reader used directly by the TUI.

pub mod cache;
pub mod codex_session;
pub mod wham;

pub use cache::{load_codex_cache, save_codex_cache};

use crate::models::{ClaudeRateLimitsCache, CodexQuotaSnapshot, QuotaSource};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the worker refreshes (matches the usage TUI refresh cadence).
const REFRESH_SECS: u64 = 10;

/// How long a last-known-good snapshot is kept once no source resolves.
///
/// Within this window a `None` result is treated as a transient failure and the
/// previous value is preserved; past it the snapshot is cleared so a removed
/// `auth.json` (or an expired token with no session fallback) stops showing
/// stale quota forever.
const STALE_AFTER_SECS: i64 = 90;

/// Loads the Claude rate-limits cache written by `vct statusline ingest`.
///
/// Returns `None` if absent or corrupt. This is a sub-millisecond local read,
/// so the TUI calls it on the main thread (no worker needed).
pub fn load_claude_rate_limits() -> Option<ClaudeRateLimitsCache> {
    let path = crate::utils::get_claude_rate_limits_path().ok()?;
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

/// Resolves the Codex quota with API-first, session-fallback precedence.
///
/// 1. `~/.codex/auth.json` exists and wham succeeds → [`QuotaSource::Api`].
/// 2. else newest session `rate_limits` → [`QuotaSource::SessionFallback`].
/// 3. else an empty snapshot ([`QuotaSource::None`]).
pub fn resolve_codex_quota(client: &reqwest::blocking::Client) -> CodexQuotaSnapshot {
    if let Ok(paths) = crate::utils::resolve_paths() {
        let auth = paths.codex_dir.join("auth.json");
        if auth.exists() {
            match wham::fetch_codex_usage(&auth, client) {
                Ok(snap) => return snap,
                Err(e) => log::warn!("codex wham/usage failed: {e}; using session fallback"),
            }
        }
    }
    match codex_session::latest_session_rate_limits() {
        Ok(Some(snap)) => snap,
        _ => CodexQuotaSnapshot::default(),
    }
}

/// Whether a preserved last-known-good snapshot should be dropped because no
/// source resolved and it has aged past `max_age_secs`.
///
/// A snapshot that is already [`QuotaSource::None`] needs no clearing.
fn should_clear_stale(current: &CodexQuotaSnapshot, now: i64, max_age_secs: i64) -> bool {
    current.source != QuotaSource::None && now - current.fetched_at > max_age_secs
}

/// Spawns a detached background worker that refreshes the Codex quota snapshot
/// into `shared` (and the on-disk cache) every ~10s until `shutdown` is set.
///
/// The worker is panic-isolated (`catch_unwind`) and holds the mutex only for
/// the assignment, so it can never poison the lock. A resolved snapshot with
/// [`QuotaSource::None`] keeps the last-known-good value through a transient
/// failure, but a stale snapshot (older than [`STALE_AFTER_SECS`]) is dropped so
/// the panel stops showing quota for a source that is gone. It is not joined on
/// quit — `shutdown` is set as a courtesy and the OS reclaims the thread on
/// process exit.
pub fn spawn_codex_quota_worker(
    shared: Arc<Mutex<CodexQuotaSnapshot>>,
    shutdown: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let client = match wham::build_client() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("codex quota worker: failed to build HTTP client: {e}");
                return;
            }
        };
        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            match catch_unwind(AssertUnwindSafe(|| resolve_codex_quota(&client))) {
                Ok(snap) if snap.source != QuotaSource::None => {
                    if let Ok(mut guard) = shared.lock() {
                        *guard = snap.clone();
                    }
                    let _ = cache::save_codex_cache(&snap);
                }
                Ok(_) => {
                    // No source resolved. Keep the last-known-good value through
                    // a transient failure, but drop it once stale so a removed
                    // source stops showing quota indefinitely.
                    let now = chrono::Local::now().timestamp();
                    let mut cleared = false;
                    if let Ok(mut guard) = shared.lock()
                        && should_clear_stale(&guard, now, STALE_AFTER_SECS)
                    {
                        *guard = CodexQuotaSnapshot::default();
                        cleared = true;
                    }
                    if cleared {
                        let _ = cache::save_codex_cache(&CodexQuotaSnapshot::default());
                    }
                }
                Err(_) => log::warn!("codex quota worker panicked; keeping last snapshot"),
            }
            // Sleep in 200ms slices so shutdown stays responsive.
            for _ in 0..(REFRESH_SECS * 5) {
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    })
}

/// Pure precedence decision used by [`resolve_codex_quota`], factored out for
/// testing without any I/O.
///
/// Returns the source that *would* be selected given whether auth exists, the
/// API call succeeded, and a session snapshot was found.
#[cfg(test)]
fn choose_source(auth_exists: bool, api_ok: bool, session_some: bool) -> QuotaSource {
    if auth_exists && api_ok {
        QuotaSource::Api
    } else if session_some {
        QuotaSource::SessionFallback
    } else {
        QuotaSource::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_api_first() {
        assert_eq!(choose_source(true, true, true), QuotaSource::Api);
        assert_eq!(choose_source(true, true, false), QuotaSource::Api);
    }

    #[test]
    fn precedence_falls_back_to_session() {
        assert_eq!(
            choose_source(false, false, true),
            QuotaSource::SessionFallback
        );
        assert_eq!(
            choose_source(true, false, true),
            QuotaSource::SessionFallback
        );
    }

    #[test]
    fn precedence_none_when_nothing() {
        assert_eq!(choose_source(false, false, false), QuotaSource::None);
        assert_eq!(choose_source(true, false, false), QuotaSource::None);
    }

    #[test]
    fn keeps_fresh_snapshot_when_no_source() {
        let snap = CodexQuotaSnapshot {
            source: QuotaSource::Api,
            fetched_at: 1000,
            ..Default::default()
        };
        // 30s old, within the window: transient failure, keep last-known-good.
        assert!(!should_clear_stale(&snap, 1030, 90));
    }

    #[test]
    fn clears_stale_snapshot_when_no_source() {
        let snap = CodexQuotaSnapshot {
            source: QuotaSource::Api,
            fetched_at: 1000,
            ..Default::default()
        };
        // 200s old: source is gone, drop the stale snapshot.
        assert!(should_clear_stale(&snap, 1200, 90));
    }

    #[test]
    fn never_clears_already_empty_snapshot() {
        let snap = CodexQuotaSnapshot::default(); // source == None
        assert!(!should_clear_stale(&snap, i64::MAX, 90));
    }
}
