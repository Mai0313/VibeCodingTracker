//! Integration test for the Codex session-log quota fallback.
//!
//! Drops a fixture rollout into a `TempHome`'s Codex sessions dir and calls the
//! path-injected resolver directly, so the test needs no `HOME` mutation and no
//! `#[serial]` — it runs in parallel and reads no machine files.

mod common;

use common::{TempHome, fixture_str};
use vibe_coding_tracker::models::QuotaSource;
use vibe_coding_tracker::quota::codex_session::latest_session_rate_limits_in;

#[test]
fn session_fallback_picks_newest_rate_limits() {
    let home = TempHome::new();
    home.put_codex_session(
        "2026/06/09/rollout-2026-06-09T21-00-00-test.jsonl",
        &fixture_str("codex_session_rate_limits.jsonl"),
    );

    let snap = latest_session_rate_limits_in(&home.paths.codex_session_dir)
        .unwrap()
        .expect("should find a rate_limits snapshot");

    assert_eq!(snap.source, QuotaSource::SessionFallback);
    // Newest line wins (42%, not the earlier 10%).
    assert_eq!(snap.primary.as_ref().unwrap().used_percent, 42.0);
    assert_eq!(snap.secondary.as_ref().unwrap().used_percent, 69.0);
    assert_eq!(snap.plan_type.as_deref(), Some("plus"));
}

#[test]
fn missing_sessions_dir_is_none() {
    let home = TempHome::new();
    // No sessions written: the resolver returns Ok(None), never an error.
    let result = latest_session_rate_limits_in(&home.paths.codex_session_dir).unwrap();
    assert!(result.is_none());
}
