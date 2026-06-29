//! Integration test for the Codex session-log quota fallback.
//!
//! Points `$HOME` at a temp dir holding a fixture rollout and (no `auth.json`)
//! asserts the resolver finds the newest `rate_limits` as a session fallback.

use serial_test::serial;
use std::fs;
use tempfile::tempdir;
use vibe_coding_tracker::models::QuotaSource;
use vibe_coding_tracker::quota::codex_session::latest_session_rate_limits;

#[test]
#[serial]
fn session_fallback_picks_newest_rate_limits() {
    let tmp = tempdir().unwrap();
    let home = tmp.path();

    let day = home.join(".codex/sessions/2026/06/09");
    fs::create_dir_all(&day).unwrap();
    let fixture = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/codex_session_rate_limits.jsonl"
    ))
    .unwrap();
    fs::write(day.join("rollout-2026-06-09T21-00-00-test.jsonl"), fixture).unwrap();

    let prev = std::env::var_os("HOME");
    // SAFETY: guarded by `#[serial]`, restored immediately after the call.
    unsafe { std::env::set_var("HOME", home) };
    let result = latest_session_rate_limits();
    match prev {
        Some(v) => unsafe { std::env::set_var("HOME", v) },
        None => unsafe { std::env::remove_var("HOME") },
    }

    let snap = result.unwrap().expect("should find a rate_limits snapshot");
    assert_eq!(snap.source, QuotaSource::SessionFallback);
    // Newest line wins (42%, not the earlier 10%).
    assert_eq!(snap.primary.as_ref().unwrap().used_percent, 42.0);
    assert_eq!(snap.secondary.as_ref().unwrap().used_percent, 69.0);
    assert_eq!(snap.plan_type.as_deref(), Some("plus"));
}
