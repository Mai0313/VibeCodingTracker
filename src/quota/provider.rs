//! Generic background quota worker, shared by the Claude and Codex providers.
//!
//! Each provider gets its own thread (so one provider's slow HTTP never stalls
//! the others), but they all run the same panic-isolated 10s loop here. A
//! provider supplies a stateful `resolve` closure returning a [`QuotaOutcome`];
//! the worker applies it to the shared snapshot:
//!
//! - [`QuotaOutcome::Data`] — store the fresh snapshot + persist it.
//! - [`QuotaOutcome::NeedsLogin`] — flip `needs_login` on the *current*
//!   snapshot (keep its data), so a refresh failure never blanks out
//!   still-valid last-known-good numbers (S3).
//! - [`QuotaOutcome::Transient`] — keep last-known-good, dropping it only once
//!   it has aged past [`STALE_AFTER_SECS`].

use crate::models::{ClaudeQuotaSnapshot, CodexQuotaSnapshot, QuotaSource};
use serde::Serialize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Default worker refresh cadence (Codex). Claude uses a longer interval to
/// stay under its stricter usage-endpoint rate limit; see [`spawn_quota_worker`].
pub const REFRESH_SECS: u64 = 10;

/// Baseline for how long a last-known-good snapshot is kept once no source
/// resolves. Scaled up per worker for slower cadences so a single missed poll
/// never blanks a panel.
pub const STALE_AFTER_SECS: i64 = 90;

/// A normalized quota snapshot the worker can store, age out, and flag.
pub trait QuotaSnapshot: Clone + Default + Send + Serialize + 'static {
    /// Unix seconds when this snapshot was produced.
    fn fetched_at(&self) -> i64;
    /// Whether this snapshot carries anything to show (data or a login hint).
    ///
    /// A snapshot that is not present is treated as "nothing resolved" and may
    /// be cleared once stale.
    fn is_present(&self) -> bool;
    /// Sets the `needs_login` flag without touching the data.
    fn set_needs_login(&mut self, value: bool);
}

impl QuotaSnapshot for ClaudeQuotaSnapshot {
    fn fetched_at(&self) -> i64 {
        self.fetched_at
    }
    fn is_present(&self) -> bool {
        self.five_hour.is_some()
            || self.seven_day.is_some()
            || self.scoped_weekly.is_some()
            || self.needs_login
    }
    fn set_needs_login(&mut self, value: bool) {
        self.needs_login = value;
    }
}

impl QuotaSnapshot for CodexQuotaSnapshot {
    fn fetched_at(&self) -> i64 {
        self.fetched_at
    }
    fn is_present(&self) -> bool {
        self.source != QuotaSource::None || self.needs_login
    }
    fn set_needs_login(&mut self, value: bool) {
        self.needs_login = value;
    }
}

/// Whether a preserved last-known-good snapshot should be dropped because
/// nothing resolved and it has aged past `max_age_secs`.
fn should_clear_stale<T: QuotaSnapshot>(snap: &T, now: i64, max_age_secs: i64) -> bool {
    snap.is_present() && now - snap.fetched_at() > max_age_secs
}

/// The result of one provider `resolve` tick.
pub enum QuotaOutcome<T> {
    /// A fresh snapshot to store (may itself carry `needs_login`, e.g. Codex
    /// session-fallback data + login hint).
    Data(T),
    /// Auth failed and there is no fallback data: flag the current snapshot for
    /// re-login but keep whatever it is already showing.
    NeedsLogin,
    /// Transient failure (network): keep last-known-good, age out if stale.
    Transient,
}

/// Spawns a detached background worker that refreshes `shared` (and the on-disk
/// cache via `save`) every `refresh_secs` until `shutdown` is set.
///
/// The stale-drop threshold scales with `refresh_secs` (at least 3× the cadence)
/// so a single missed poll on a slow-polling provider never blanks the panel.
///
/// The worker is panic-isolated and holds the mutex only for the assignment, so
/// it can never poison the lock. It is not joined on quit — `shutdown` is set as
/// a courtesy and the OS reclaims the thread on process exit.
pub fn spawn_quota_worker<T, R, S>(
    label: &'static str,
    shared: Arc<Mutex<T>>,
    shutdown: Arc<AtomicBool>,
    refresh_secs: u64,
    mut resolve: R,
    save: S,
) -> JoinHandle<()>
where
    T: QuotaSnapshot,
    R: FnMut() -> QuotaOutcome<T> + Send + 'static,
    S: Fn(&T) + Send + 'static,
{
    let stale_after = STALE_AFTER_SECS.max(refresh_secs as i64 * 3);
    std::thread::spawn(move || {
        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            match catch_unwind(AssertUnwindSafe(&mut resolve)) {
                Ok(QuotaOutcome::Data(snap)) => {
                    if let Ok(mut guard) = shared.lock() {
                        *guard = snap.clone();
                    }
                    save(&snap);
                }
                Ok(QuotaOutcome::NeedsLogin) => {
                    let mut updated = None;
                    if let Ok(mut guard) = shared.lock() {
                        guard.set_needs_login(true);
                        updated = Some(guard.clone());
                    }
                    if let Some(snap) = updated {
                        save(&snap);
                    }
                }
                Ok(QuotaOutcome::Transient) => {
                    let now = chrono::Local::now().timestamp();
                    let mut cleared = None;
                    if let Ok(mut guard) = shared.lock()
                        && should_clear_stale(&*guard, now, stale_after)
                    {
                        *guard = T::default();
                        cleared = Some(T::default());
                    }
                    if let Some(snap) = cleared {
                        save(&snap);
                    }
                }
                Err(_) => log::warn!("{label} quota worker panicked; keeping last snapshot"),
            }
            // Sleep in 200ms slices so shutdown stays responsive.
            for _ in 0..(refresh_secs * 5) {
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{CodexQuotaSnapshot, QuotaSource};

    #[test]
    fn keeps_fresh_snapshot() {
        let snap = CodexQuotaSnapshot {
            source: QuotaSource::Api,
            fetched_at: 1000,
            ..Default::default()
        };
        assert!(!should_clear_stale(&snap, 1030, 90));
    }

    #[test]
    fn clears_stale_snapshot() {
        let snap = CodexQuotaSnapshot {
            source: QuotaSource::Api,
            fetched_at: 1000,
            ..Default::default()
        };
        assert!(should_clear_stale(&snap, 1200, 90));
    }

    #[test]
    fn never_clears_empty_snapshot() {
        let snap = CodexQuotaSnapshot::default();
        assert!(!should_clear_stale(&snap, i64::MAX, 90));
    }

    #[test]
    fn needs_login_snapshot_is_present() {
        let snap = CodexQuotaSnapshot {
            needs_login: true,
            ..Default::default()
        };
        assert!(snap.is_present());
    }
}
