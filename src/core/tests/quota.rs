//! Integration test for the Codex session-log quota fallback.
//!
//! Drops a fixture rollout into a `TempHome`'s Codex session roots and calls the
//! path-injected resolver directly, so the test needs no `HOME` mutation and no
//! `#[serial]` — it runs in parallel and reads no machine files.

use vct_core::models::QuotaSource;
use vct_core::quota::codex_session::latest_session_rate_limits_in;
use vct_test_support::{TempHome, fixture_str};

/// Rewrites the fixture's percentages so a second rollout is distinguishable.
fn fixture_with_percent(primary: &str, secondary: &str) -> String {
    fixture_str("quota/codex_session_rate_limits.jsonl")
        .replace("42.0", primary)
        .replace("69.0", secondary)
}

#[test]
fn session_fallback_picks_newest_rate_limits() {
    let home = TempHome::new();
    home.put_codex_session(
        "2026/06/09/rollout-2026-06-09T21-00-00-test.jsonl",
        &fixture_str("quota/codex_session_rate_limits.jsonl"),
    );

    let snap = latest_session_rate_limits_in(&home.paths.codex_session_dirs())
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
    let result = latest_session_rate_limits_in(&home.paths.codex_session_dirs()).unwrap();
    assert!(result.is_none());
}

#[test]
fn archived_only_home_still_yields_a_snapshot() {
    let home = TempHome::new();
    // The user archived every session: the active root does not even exist.
    home.put_codex_archived_session(
        "rollout-2026-06-09T21-00-00-test.jsonl",
        &fixture_str("quota/codex_session_rate_limits.jsonl"),
    );

    let snap = latest_session_rate_limits_in(&home.paths.codex_session_dirs())
        .unwrap()
        .expect("archived rollout carries the snapshot");

    assert_eq!(snap.primary.as_ref().unwrap().used_percent, 42.0);
}

#[test]
fn newest_snapshot_wins_across_roots() {
    let home = TempHome::new();
    let stale = home.put_codex_session(
        "2026/06/09/rollout-2026-06-09T09-00-00-old.jsonl",
        &fixture_with_percent("11.0", "22.0"),
    );
    let fresh = home.put_codex_archived_session(
        "rollout-2026-06-09T21-00-00-new.jsonl",
        &fixture_with_percent("77.0", "88.0"),
    );
    // mtime ranks across roots, and users archive same-day, so the freshest
    // snapshot on disk is frequently the archived one.
    let base = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(base)
        .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&fresh)
        .unwrap()
        .set_modified(base + std::time::Duration::from_secs(3600))
        .unwrap();

    let snap = latest_session_rate_limits_in(&home.paths.codex_session_dirs())
        .unwrap()
        .expect("should find a rate_limits snapshot");

    assert_eq!(snap.primary.as_ref().unwrap().used_percent, 77.0);
    assert_eq!(snap.secondary.as_ref().unwrap().used_percent, 88.0);
}
