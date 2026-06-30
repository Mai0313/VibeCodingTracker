//! End-to-end tests for the `statusline` subcommand via the built binary.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use serial_test::serial;
use tempfile::tempdir;

fn fixture() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/claude_statusline_input.json"
    ))
    .unwrap()
}

#[test]
#[serial]
fn ingest_writes_cache_silently() {
    let tmp = tempdir().unwrap();
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .args(["statusline", "ingest"])
        .env("HOME", tmp.path())
        .write_stdin(fixture())
        .assert()
        .success()
        .stdout("");

    let cache = tmp
        .path()
        .join(".vibe_coding_tracker/claude_rate_limits.json");
    let body = std::fs::read_to_string(cache).expect("cache should exist");
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["five_hour"]["used_percent"], 16.0);
    assert_eq!(parsed["seven_day"]["used_percent"], 28.0);
    assert_eq!(parsed["five_hour"]["resets_at_unix"], 1782765000i64);
}

#[test]
#[serial]
fn ingest_garbage_exits_zero_without_output() {
    let tmp = tempdir().unwrap();
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .args(["statusline", "ingest"])
        .env("HOME", tmp.path())
        .write_stdin("not json at all")
        .assert()
        .success()
        .stdout("");
}

#[test]
#[serial]
fn statusline_default_prints_one_line() {
    let tmp = tempdir().unwrap();
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("statusline")
        .env("HOME", tmp.path())
        .write_stdin(fixture())
        .assert()
        .success()
        .stdout(predicates::str::contains("5h 16%").and(predicates::str::contains("7d 28%")));
}
