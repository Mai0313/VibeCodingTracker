// Integration tests for main.rs CLI commands
use assert_cmd::Command;
use std::fs;
use std::io::Write;
use tempfile::TempDir;

#[test]
fn test_version_command_default() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("version");
    cmd.assert().success();
}

#[test]
fn test_version_command_json() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("version").arg("--json");
    let output = cmd.assert().success();

    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(stdout.contains("Version"));
    assert!(stdout.contains("Rust Version"));
    assert!(stdout.contains("Cargo Version"));
}

#[test]
fn test_version_command_text() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("version").arg("--text");
    let output = cmd.assert().success();

    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(stdout.contains("Version:"));
    assert!(stdout.contains("Rust Version:"));
    assert!(stdout.contains("Cargo Version:"));
}

#[test]
fn test_analysis_command_with_file() {
    let temp_dir = TempDir::new().unwrap();
    let input_file = temp_dir.path().join("test.jsonl");

    // Create a minimal Claude Code format JSONL file
    let mut file = fs::File::create(&input_file).unwrap();
    writeln!(file, r#"{{"type":"conversation","message":{{"model":"claude-3","usage":{{"input_tokens":100,"output_tokens":50}}}}}}"#).unwrap();

    let output_file = temp_dir.path().join("output.json");

    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis")
        .arg("--path")
        .arg(input_file)
        .arg("--output")
        .arg(&output_file);

    cmd.assert().success();

    // Verify output file was created
    assert!(output_file.exists(), "Output file should be created");
}

#[test]
fn test_analysis_command_with_invalid_file() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis")
        .arg("--path")
        .arg("/nonexistent/file.jsonl");

    cmd.assert().failure();
}

#[test]
fn test_usage_command_json() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--json");

    // This should succeed even if no session files exist
    let output = cmd.assert().success();

    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    // Should output valid JSON (empty object or actual data)
    assert!(stdout.starts_with("{") || stdout.starts_with("["));
}

#[test]
fn test_usage_command_text() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--text");

    // Should succeed even if no data
    cmd.assert().success();
}

#[test]
fn test_usage_command_table() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--table");

    // Should succeed even if no data
    cmd.assert().success();
}

#[test]
fn test_help_command() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("--help");

    let output = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);

    assert!(stdout.contains("usage") || stdout.contains("Usage"));
    assert!(stdout.contains("analysis") || stdout.contains("Analysis"));
    assert!(stdout.contains("version") || stdout.contains("Version"));
}

#[test]
fn test_analysis_help() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis").arg("--help");

    let output = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);

    assert!(stdout.contains("path") || stdout.contains("Path"));
    assert!(stdout.contains("output") || stdout.contains("Output"));
}

#[test]
fn test_usage_help() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--help");

    let output = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);

    assert!(stdout.contains("json") || stdout.contains("JSON"));
}

#[test]
fn test_version_help() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("version").arg("--help");

    let output = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);

    assert!(stdout.contains("json") || stdout.contains("JSON") || stdout.contains("text"));
}
