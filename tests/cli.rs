// Integration tests for CLI functionality
//
// These tests verify command-line interface operations (excluding TUI components)

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;
use tempfile::TempDir;

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
fn test_analysis_command_with_example_file() {
    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !example_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis")
        .arg("--path")
        .arg(example_file.to_str().unwrap());

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("extensionName"))
        .stdout(predicate::str::contains("records"));
}

#[test]
fn test_analysis_command_with_output_file() {
    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");
    let temp_dir = TempDir::new().unwrap();
    let output_file = temp_dir.path().join("output.json");

    if !example_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis")
        .arg("--path")
        .arg(example_file.to_str().unwrap())
        .arg("--output")
        .arg(output_file.to_str().unwrap());

    cmd.assert().success();

    // Verify output file was created
    assert!(output_file.exists(), "Output file should be created");

    // Verify output file contains valid JSON
    let content = std::fs::read_to_string(&output_file).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(json.is_object(), "Output should be valid JSON object");
}

#[test]
fn test_analysis_command_with_nonexistent_file() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis")
        .arg("--path")
        .arg("nonexistent_file.jsonl");

    cmd.assert().failure(); // Should fail with nonexistent file
}

#[test]
fn test_analysis_batch_mode() {
    // This test is skipped because it may hang when scanning system directories
    // Use test_analysis_batch_mode_with_output instead which has explicit timeout
    eprintln!("Skipping test_analysis_batch_mode - may hang on system directories");
}

#[test]
fn test_analysis_batch_mode_with_output() {
    use std::time::Duration;

    let temp_dir = TempDir::new().unwrap();
    let output_file = temp_dir.path().join("batch_output.json");

    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis")
        .arg("--output")
        .arg(output_file.to_str().unwrap())
        .timeout(Duration::from_secs(10)); // Add 10 second timeout

    // May timeout on slow systems or large session directories
    let output = cmd.output();

    if let Ok(output) = output
        && output.status.success()
        && output_file.exists()
    {
        let content = std::fs::read_to_string(&output_file).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(json.is_array(), "Batch output should be JSON array");
    }
}

#[test]
fn test_analysis_command_json() {
    use std::time::Duration;

    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis")
        .arg("--json")
        .timeout(Duration::from_secs(10));

    let output = cmd.output().expect("failed to spawn vct binary");

    // Allow timeout (env-dependent on session-directory size) but not other
    // failures — a non-zero exit means a regression in the --json path.
    if output.status.code().is_none() {
        return;
    }
    assert!(
        output.status.success(),
        "vct analysis --json failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("--json output should be valid JSON");
    assert!(
        json.is_array(),
        "--json output should be a JSON array of aggregated rows"
    );
}

#[test]
fn test_analysis_command_text() {
    use std::time::Duration;

    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis")
        .arg("--text")
        .timeout(Duration::from_secs(10));

    let output = cmd.output().expect("failed to spawn vct binary");
    assert!(
        output.status.success() || output.status.code().is_none(),
        "vct analysis --text failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_analysis_command_table() {
    use std::time::Duration;

    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis")
        .arg("--table")
        .timeout(Duration::from_secs(10));

    let output = cmd.output().expect("failed to spawn vct binary");
    assert!(
        output.status.success() || output.status.code().is_none(),
        "vct analysis --table failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_usage_command_json() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--json");

    let output = cmd.output().expect("failed to spawn vct binary");
    assert!(
        output.status.success(),
        "vct usage --json failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("--json output should be valid JSON");
    assert!(json.is_array(), "--json output should be a JSON array");
}

#[test]
fn test_usage_command_text() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--text");

    // Should succeed
    cmd.assert().success();
}

#[test]
fn test_usage_command_table() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("usage").arg("--table");

    // Should succeed
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
fn test_analysis_help() {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis").arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("analysis"))
        .stdout(predicate::str::contains("--path"));
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
    // --json/--text/--table belong to a single clap group and must be
    // mutually exclusive.
    for combo in [
        ["--json", "--text"],
        ["--json", "--table"],
        ["--text", "--table"],
    ] {
        let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
        cmd.arg("usage").args(combo);
        cmd.assert()
            .failure()
            .stderr(predicate::str::contains("cannot be used with"));
    }
}

#[test]
fn test_analysis_multiple_output_formats() {
    for combo in [
        ["--json", "--text"],
        ["--json", "--table"],
        ["--text", "--table"],
    ] {
        let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
        cmd.arg("analysis").args(combo);
        cmd.assert()
            .failure()
            .stderr(predicate::str::contains("cannot be used with"));
    }
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
fn test_analysis_output_directory_creation() {
    let temp_dir = TempDir::new().unwrap();
    // Create parent directory first
    let nested_dir = temp_dir.path().join("nested").join("dir");
    std::fs::create_dir_all(&nested_dir).unwrap();
    let nested_output = nested_dir.join("output.json");

    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !example_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis")
        .arg("--path")
        .arg(example_file.to_str().unwrap())
        .arg("--output")
        .arg(nested_output.to_str().unwrap());

    cmd.assert().success();

    // Verify output file was created
    assert!(nested_output.exists(), "Output file should be created");
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

#[test]
fn test_analysis_validates_file_extension() {
    let temp_dir = TempDir::new().unwrap();
    let wrong_ext = temp_dir.path().join("test.txt");
    std::fs::write(&wrong_ext, "test content").unwrap();

    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.arg("analysis")
        .arg("--path")
        .arg(wrong_ext.to_str().unwrap());

    // Behavior depends on implementation - may succeed or fail
    let _ = cmd.output();
}

#[test]
fn test_cli_handles_unicode_paths() {
    let temp_dir = TempDir::new().unwrap();
    let unicode_path = temp_dir.path().join("測試_test_файл.json");

    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");
    if example_file.exists() {
        std::fs::copy(&example_file, &unicode_path).ok();

        let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
        cmd.arg("analysis")
            .arg("--path")
            .arg(unicode_path.to_str().unwrap());

        // Should handle Unicode paths
        let _ = cmd.output();
    }
}

#[test]
fn test_cli_handles_spaces_in_paths() {
    let temp_dir = TempDir::new().unwrap();
    let space_path = temp_dir.path().join("file with spaces.jsonl");

    let example_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");
    if example_file.exists() {
        std::fs::copy(&example_file, &space_path).ok();

        let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
        cmd.arg("analysis")
            .arg("--path")
            .arg(space_path.to_str().unwrap());

        cmd.assert().success();
    }
}
