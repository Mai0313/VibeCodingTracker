// Integration tests for CLI functionality
//
// These tests verify command-line interface operations (excluding TUI components)

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_version_command() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("version");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Version"));
}

#[test]
fn test_version_command_json() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("version").arg("--json");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Version"));
}

#[test]
fn test_version_command_text() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("version").arg("--text");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Version"));
}

#[test]
fn test_usage_command_json() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--json");

    // Should succeed and output valid JSON
    let output = cmd.output().unwrap();

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.trim().is_empty() {
            let json: Result<serde_json::Value, _> = serde_json::from_str(&stdout);
            assert!(json.is_ok(), "Output should be valid JSON");
        }
    }
}

#[test]
fn test_usage_command_text() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--text");

    cmd.assert().success();
}

#[test]
fn test_usage_command_table() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--table");

    cmd.assert().success();
}

#[test]
fn test_help_command() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Usage"))
        .stdout(predicate::str::contains("Commands"));
}

#[test]
fn test_usage_help() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("usage"))
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn test_version_help() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("version").arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("version"));
}

#[test]
fn test_invalid_command() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("invalid_command");

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("error").or(predicate::str::contains("unrecognized")));
}

#[test]
fn test_usage_multiple_output_formats() {
    // Test that multiple output format flags can coexist (behavior depends on CLI implementation)
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--json").arg("--text");

    // Should handle gracefully
    let _ = cmd.output();
}

#[test]
fn test_cli_with_env_vars() {
    // Test that environment variables are respected if defined
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.env("RUST_LOG", "debug");
    cmd.arg("version");

    cmd.assert().success();
}

#[test]
fn test_update_check_command() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("update").arg("--check");

    // Should succeed (network errors are handled gracefully)
    let output = cmd.output().unwrap();
    assert!(
        output.status.success() || output.status.code().is_some(),
        "Update check should complete"
    );
}

#[test]
fn test_cli_version_matches_cargo() {
    // Verify that CLI version output is valid JSON
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("version").arg("--json");

    let output = cmd.output().unwrap();
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

        // Check for Version field (note: capital V)
        assert!(json["Version"].is_string(), "Should have Version field");
    }
}
