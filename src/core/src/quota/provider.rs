//! Generic background quota polling worker shared by all quota providers.
//!
//! Each provider runs its own panic-isolated thread executing a `resolve` closure on a fixed interval and applying each [`QuotaOutcome`] to the shared snapshot:
//!
//! - [`QuotaOutcome::Data`] — replaces the shared snapshot and persists it.
//! - [`QuotaOutcome::NeedsLogin`] — sets `needs_login` on the current snapshot without discarding existing data, then persists it.
//! - [`QuotaOutcome::Transient`] — leaves the current snapshot unchanged until a subsequent tick succeeds.

use crate::models::{
    AntigravityQuotaSnapshot, ClaudeQuotaSnapshot, CodexQuotaSnapshot, CopilotQuotaSnapshot,
    CursorQuotaSnapshot, GrokQuotaSnapshot, QuotaSource,
};
use serde::Serialize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Cooperative shutdown signal shared between a caller and its quota polling workers.
#[derive(Default)]
pub struct QuotaShutdown {
    stopped: Mutex<bool>,
    wake: Condvar,
}

impl QuotaShutdown {
    /// Creates an unsignaled shutdown handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks shutdown requested and wakes all sleeping workers immediately.
    pub fn signal(&self) {
        let mut stopped = self.stopped.lock().unwrap_or_else(|e| e.into_inner());
        *stopped = true;
        self.wake.notify_all();
    }

    /// Returns whether shutdown has been requested.
    pub fn is_shutdown(&self) -> bool {
        *self.stopped.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits up to `duration` for shutdown, returning `true` if shutdown was signaled.
    pub fn wait_timeout(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        let mut stopped = self.stopped.lock().unwrap_or_else(|e| e.into_inner());
        while !*stopped {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            if remaining.is_zero() {
                return false;
            }
            let (guard, result) = self
                .wake
                .wait_timeout(stopped, remaining)
                .unwrap_or_else(|e| e.into_inner());
            stopped = guard;
            if result.timed_out() {
                return *stopped;
            }
        }
        true
    }
}

/// A normalized quota snapshot managed by [`spawn_quota_worker`].
pub trait QuotaSnapshot: Clone + Default + Send + Serialize + 'static {
    /// Returns the Unix timestamp in seconds when this snapshot was produced.
    fn fetched_at(&self) -> i64;
    /// Returns whether this snapshot carries data or a login hint to display.
    fn is_present(&self) -> bool;
    /// Sets the `needs_login` flag without modifying other fields.
    fn set_needs_login(&mut self, value: bool);
}

impl QuotaSnapshot for ClaudeQuotaSnapshot {
    fn fetched_at(&self) -> i64 {
        self.fetched_at
    }
    fn is_present(&self) -> bool {
        self.five_hour.is_some()
            || self.seven_day.is_some()
            // Mirror the render gate: a scoped row requires both the window and its model label.
            || (self.scoped_weekly.is_some() && self.scoped_label.is_some())
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

impl QuotaSnapshot for CopilotQuotaSnapshot {
    fn fetched_at(&self) -> i64 {
        self.fetched_at
    }
    fn is_present(&self) -> bool {
        // Must match the panel's render gate.
        self.premium.is_some()
            || self.premium_unlimited
            || self.plan_type.is_some()
            || self.needs_login
    }
    fn set_needs_login(&mut self, value: bool) {
        self.needs_login = value;
    }
}

impl QuotaSnapshot for CursorQuotaSnapshot {
    fn fetched_at(&self) -> i64 {
        self.fetched_at
    }
    fn is_present(&self) -> bool {
        self.total.is_some()
            || self.auto.is_some()
            || self.api.is_some()
            || self.plan_type.is_some()
            || self.needs_login
    }
    fn set_needs_login(&mut self, value: bool) {
        self.needs_login = value;
    }
}

impl QuotaSnapshot for GrokQuotaSnapshot {
    fn fetched_at(&self) -> i64 {
        self.fetched_at
    }
    fn is_present(&self) -> bool {
        // Mirror the render gate; money fields are omitted because any mapped billing response sets `included`.
        self.included.is_some() || self.plan_type.is_some() || self.needs_login
    }
    fn set_needs_login(&mut self, value: bool) {
        self.needs_login = value;
    }
}

impl QuotaSnapshot for AntigravityQuotaSnapshot {
    fn fetched_at(&self) -> i64 {
        self.fetched_at
    }
    fn is_present(&self) -> bool {
        !self.buckets.is_empty() || self.plan_type.is_some() || self.needs_login
    }
    fn set_needs_login(&mut self, value: bool) {
        self.needs_login = value;
    }
}

/// Result of one provider poll tick.
pub enum QuotaOutcome<T> {
    /// Fresh snapshot to store and persist (may itself carry `needs_login`, e.g. Codex session fallback).
    Data(T),
    /// Authentication failed with no fallback snapshot; sets `needs_login` on the current snapshot without discarding its data.
    NeedsLogin,
    /// Transient failure (such as network error or rate limit); keeps the current snapshot unchanged.
    Transient,
}

/// Spawns a background thread that polls `resolve`, updates `shared`, and invokes `save` every `refresh_secs` seconds until `shutdown` is signaled.
///
/// `refresh_secs` is clamped to at least 1 second. Panics inside `resolve` are caught and logged under `label` while the lock is not held, preserving the last snapshot and avoiding mutex poisoning.
pub fn spawn_quota_worker<T, R, S>(
    label: &'static str,
    shared: Arc<Mutex<T>>,
    shutdown: Arc<QuotaShutdown>,
    refresh_secs: u64,
    mut resolve: R,
    save: S,
) -> JoinHandle<()>
where
    T: QuotaSnapshot,
    R: FnMut() -> QuotaOutcome<T> + Send + 'static,
    S: Fn(&T) + Send + 'static,
{
    let interval = Duration::from_secs(refresh_secs.max(1));
    std::thread::spawn(move || {
        loop {
            if shutdown.is_shutdown() {
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
                // Keep the last-known-good snapshot until a later tick succeeds.
                Ok(QuotaOutcome::Transient) => {}
                Err(_) => log::warn!("{label} quota worker panicked; keeping last snapshot"),
            }
            if shutdown.wait_timeout(interval) {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::CodexQuotaSnapshot;
    use std::sync::mpsc;

    #[test]
    fn needs_login_snapshot_is_present() {
        let snap = CodexQuotaSnapshot {
            needs_login: true,
            ..Default::default()
        };
        assert!(snap.is_present());
    }

    #[test]
    fn shutdown_signal_wakes_sleeping_worker_immediately() {
        let shared = Arc::new(Mutex::new(CodexQuotaSnapshot::default()));
        let shutdown = Arc::new(QuotaShutdown::new());
        let (tx, rx) = mpsc::channel();
        let handle = spawn_quota_worker(
            "test",
            shared,
            Arc::clone(&shutdown),
            60,
            move || {
                let _ = tx.send(());
                QuotaOutcome::Transient
            },
            |_| {},
        );

        rx.recv_timeout(Duration::from_secs(2))
            .expect("first poll should run immediately");
        let start = Instant::now();
        shutdown.signal();
        handle.join().expect("worker should exit cleanly");
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "worker should wake immediately on shutdown signal, took {:?}",
            start.elapsed()
        );
    }
}
