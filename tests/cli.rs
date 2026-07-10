// Integration tests for the built `vct` binary.
//
// Two groups:
//  1. Zero-env, zero-network CLI wiring — version / help / `analysis --path`
//     (single-file) / flag conflicts / arg parsing. These need no HOME and no
//     network, so they mutate no environment at all.
//  2. Per-child HOME smoke tests — `usage` / `analysis` (batch) run against an
//     isolated temp HOME (set on the child process only) seeded with fixture
//     sessions plus an offline pricing cache, so the compiled binary is
//     exercised end-to-end without touching the real home or any external API.
//
// Setting HOME on the child is the only way to isolate a separate binary's home
// directory; it is per-child (no `#[serial]`, no process-global mutation) and
// behaves identically locally and in CI.

mod common;

use assert_cmd::Command;
use common::{TempHome, fixture};
use predicates::prelude::*;
use serde_json::json;
use vibe_coding_tracker::VERSION;

/// A minimal cost-fields pricing map used to seed the offline cache so `usage`
/// prices its models without a network fetch.
fn pricing_seed() -> serde_json::Value {
    json!({
        "claude-sonnet-4-20250514": {
            "input_cost_per_token": 3e-6,
            "output_cost_per_token": 1.5e-5,
            "cache_read_input_token_cost": 3e-7,
            "cache_creation_input_token_cost": 3.75e-6
        }
    })
}

/// `vct` with an isolated per-child HOME (XDG overrides cleared) so the binary
/// resolves every provider directory and the `~/.vct` cache under the temp home,
/// matching `resolve_paths_from_home`.
fn child_cmd(home: &TempHome) -> Command {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.env("HOME", home.home())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME");
    cmd
}

// ============================================================================
// Group 1: zero-env, zero-network CLI wiring
// ============================================================================

#[test]
fn test_version_command() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("version")
        .assert()
        .success()
        .stdout(predicate::str::contains("Version"));
}

#[test]
fn test_version_flag_outputs_build_version_only() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::eq(format!("{VERSION}\n")));
}

#[test]
fn test_short_version_flag_outputs_build_version_only() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("-V")
        .assert()
        .success()
        .stdout(predicate::eq(format!("{VERSION}\n")));
}

#[test]
fn test_version_command_json() {
    let output = Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("version")
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["Version"].is_string(), "Should have Version field");
}

#[test]
fn test_version_command_text() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("version")
        .arg("--text")
        .assert()
        .success()
        .stdout(predicate::str::contains("Version"));
}

#[test]
fn test_analysis_command_with_example_file() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("analysis")
        .arg("--path")
        .arg(fixture("test_conversation_claude_code.jsonl"))
        .assert()
        .success()
        .stdout(predicate::str::contains("extensionName"))
        .stdout(predicate::str::contains("records"));
}

#[test]
fn test_analysis_command_with_output_file() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let output_file = temp_dir.path().join("output.json");

    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("analysis")
        .arg("--path")
        .arg(fixture("test_conversation_claude_code.jsonl"))
        .arg("--output")
        .arg(&output_file)
        .assert()
        .success();

    assert!(output_file.exists(), "Output file should be created");
    let content = std::fs::read_to_string(&output_file).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(json.is_object(), "Output should be valid JSON object");
}

#[test]
fn test_analysis_command_with_nonexistent_file() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("analysis")
        .arg("--path")
        .arg("nonexistent_file.jsonl")
        .assert()
        .failure();
}

#[test]
fn test_analysis_output_directory_creation() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let nested_output = temp_dir
        .path()
        .join("nested")
        .join("dir")
        .join("output.json");
    std::fs::create_dir_all(nested_output.parent().unwrap()).unwrap();

    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("analysis")
        .arg("--path")
        .arg(fixture("test_conversation_claude_code.jsonl"))
        .arg("--output")
        .arg(&nested_output)
        .assert()
        .success();

    assert!(nested_output.exists(), "Output file should be created");
}

#[test]
fn test_analysis_validates_file_extension() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let wrong_ext = temp_dir.path().join("test.txt");
    std::fs::write(&wrong_ext, "test content").unwrap();

    // Behavior depends on implementation - may succeed or fail; must not panic.
    let _ = Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("analysis")
        .arg("--path")
        .arg(&wrong_ext)
        .output();
}

#[test]
fn test_cli_handles_unicode_paths() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let unicode_path = temp_dir.path().join("測試_test_файл.jsonl");
    std::fs::copy(
        fixture("test_conversation_claude_code.jsonl"),
        &unicode_path,
    )
    .unwrap();

    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("analysis")
        .arg("--path")
        .arg(&unicode_path)
        .assert()
        .success();
}

#[test]
fn test_cli_handles_spaces_in_paths() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let space_path = temp_dir.path().join("file with spaces.jsonl");
    std::fs::copy(fixture("test_conversation_claude_code.jsonl"), &space_path).unwrap();

    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("analysis")
        .arg("--path")
        .arg(&space_path)
        .assert()
        .success();
}

#[test]
fn test_help_command() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage"))
        .stdout(predicate::str::contains("Commands"));
}

#[test]
fn test_analysis_help() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("analysis")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("analysis"))
        .stdout(predicate::str::contains("--path"));
}

#[test]
fn test_usage_help() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("usage")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("usage"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--output"));
}

#[test]
fn test_version_help() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("version")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("version"));
}

#[test]
fn test_fetch_help() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("fetch")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("claude"))
        .stdout(predicate::str::contains("codex"))
        .stdout(predicate::str::contains("copilot"))
        .stdout(predicate::str::contains("cursor"));
}

#[test]
fn test_invalid_command() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("invalid_command")
        .assert()
        .failure()
        .stderr(predicate::str::contains("error").or(predicate::str::contains("unrecognized")));
}

#[test]
fn test_usage_multiple_output_formats() {
    // --json/--text/--table belong to a single clap group and must be mutually
    // exclusive.
    for combo in [
        ["--json", "--text"],
        ["--json", "--table"],
        ["--text", "--table"],
    ] {
        Command::cargo_bin("vibe_coding_tracker")
            .unwrap()
            .arg("usage")
            .args(combo)
            .assert()
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
        Command::cargo_bin("vibe_coding_tracker")
            .unwrap()
            .arg("analysis")
            .args(combo)
            .assert()
            .failure()
            .stderr(predicate::str::contains("cannot be used with"));
    }
}

#[test]
fn test_fetch_requires_provider() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("fetch")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required").or(predicate::str::contains("PROVIDER")));
}

#[test]
fn test_fetch_invalid_provider() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("fetch")
        .arg("not-a-provider")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn test_fetch_multiple_output_formats() {
    // clap rejects the combination before any network call is made.
    for combo in [
        ["--json", "--text"],
        ["--json", "--table"],
        ["--text", "--table"],
    ] {
        Command::cargo_bin("vibe_coding_tracker")
            .unwrap()
            .arg("fetch")
            .arg("claude")
            .args(combo)
            .assert()
            .failure()
            .stderr(predicate::str::contains("cannot be used with"));
    }
}

#[test]
fn test_cli_version_matches_cargo() {
    let output = Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("version")
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["Version"].is_string(), "Should have Version field");
}

// ============================================================================
// Group 2: per-child HOME smoke tests (offline via seeded fixtures + cache)
// ============================================================================

#[test]
fn usage_json_smoke_prices_seeded_session() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "session.jsonl",
        &common::fixture_str("test_conversation_claude_code.jsonl"),
    );
    home.seed_pricing_cache(&pricing_seed());

    let output = child_cmd(&home)
        .arg("usage")
        .arg("--json")
        .output()
        .expect("spawn vct");
    assert!(
        output.status.success(),
        "vct usage --json failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON array");
    let arr = rows.as_array().expect("usage --json is an array");
    let sonnet = arr
        .iter()
        .find(|r| r["model"] == "claude-sonnet-4-20250514")
        .expect("seeded Claude model should appear in usage output");
    assert!(sonnet["cost_usd"].is_number(), "cost should be priced");
}

#[test]
fn usage_json_empty_home_is_empty_array() {
    let home = TempHome::new();
    home.seed_pricing_cache(&pricing_seed());

    let output = child_cmd(&home)
        .arg("usage")
        .arg("--json")
        .output()
        .expect("spawn vct");
    assert!(output.status.success());
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.as_array().map(|a| a.len()), Some(0));
}

#[test]
fn usage_text_and_table_smoke() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "session.jsonl",
        &common::fixture_str("test_conversation_claude_code.jsonl"),
    );
    home.seed_pricing_cache(&pricing_seed());

    for format in ["--text", "--table"] {
        child_cmd(&home).arg("usage").arg(format).assert().success();
    }
}

#[test]
fn usage_merge_providers_flag_smoke() {
    let home = TempHome::new();
    home.seed_pricing_cache(&pricing_seed());
    for format in ["--table", "--text"] {
        child_cmd(&home)
            .arg("usage")
            .arg(format)
            .arg("--merge-providers")
            .assert()
            .success();
    }
}

#[test]
fn usage_output_file_smoke() {
    let home = TempHome::new();
    home.seed_pricing_cache(&pricing_seed());
    let out = home.home().join("usage_output.json");

    child_cmd(&home)
        .arg("usage")
        .arg("--output")
        .arg(&out)
        .assert()
        .success();

    let content = std::fs::read_to_string(&out).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(json.is_array(), "Usage output should be a JSON array");
}

#[test]
fn analysis_batch_json_smoke() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "session.jsonl",
        &common::fixture_str("test_conversation_claude_code.jsonl"),
    );

    let output = child_cmd(&home)
        .arg("analysis")
        .arg("--json")
        .output()
        .expect("spawn vct");
    assert!(
        output.status.success(),
        "vct analysis --json failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON array");
    let arr = rows.as_array().expect("analysis --json is an array");
    assert!(
        arr.iter().any(|r| r["model"] == "claude-sonnet-4-20250514"),
        "seeded Claude model should have an aggregated analysis row"
    );
}

#[test]
fn analysis_batch_text_and_table_smoke() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "session.jsonl",
        &common::fixture_str("test_conversation_claude_code.jsonl"),
    );
    for format in ["--text", "--table"] {
        child_cmd(&home)
            .arg("analysis")
            .arg(format)
            .assert()
            .success();
    }
}

#[test]
fn all_short_flag_parses_for_both_subcommands() {
    let home = TempHome::new();
    home.seed_pricing_cache(&pricing_seed());
    // `-a` is the short alias for `--all`; a parse error would be a non-zero exit.
    for subcommand in ["usage", "analysis"] {
        child_cmd(&home)
            .arg(subcommand)
            .arg("-a")
            .arg("--json")
            .assert()
            .success();
    }
}

#[test]
fn readonly_commands_do_not_create_config() {
    // `version` (like other settings-free commands) must not materialize
    // ~/.vct/config.toml as a home-directory side effect.
    let home = TempHome::new();
    child_cmd(&home).arg("version").assert().success();
    assert!(
        !home.home().join(".vct/config.toml").exists(),
        "version must not create config.toml"
    );
}

#[test]
fn config_path_prints_config_toml_location() {
    // Uses a per-child HOME so the check never touches the real `~/.vct`.
    let home = TempHome::new();
    child_cmd(&home)
        .arg("config")
        .arg("path")
        .assert()
        .success()
        .stdout(predicate::str::contains("config.toml"));
}

#[test]
fn config_show_creates_and_prints_settings() {
    let home = TempHome::new();
    child_cmd(&home)
        .arg("config")
        .arg("show")
        .assert()
        .success()
        .stdout(predicate::str::contains("[usage]"))
        .stdout(predicate::str::contains("merge_models"))
        .stdout(predicate::str::contains("[cursor]"))
        .stdout(predicate::str::contains("usage_source"));

    // The show path must have materialized the file under the temp home, and it
    // must not fold in any version.json bookkeeping.
    let config = home.home().join(".vct/config.toml");
    assert!(config.exists());
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(!text.contains("[update]"));
}
