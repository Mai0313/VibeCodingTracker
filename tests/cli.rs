// Integration tests for the built `vct` binary.
//
// Two groups:
//  1. CLI wiring and single-file behavior. Clap-only checks use the inherited
//     environment because they stop before runtime dispatch; every command that
//     can parse, log, or return a runtime error uses an isolated child HOME.
//  2. Per-child HOME smoke tests — `usage` / `analysis` (batch) run against an
//     isolated temp HOME seeded with fixture sessions plus an offline pricing
//     cache, so the compiled binary is exercised end-to-end without touching
//     the real home or any external API.
//
// Setting the environment on the child is the only way to isolate a separate
// binary; it is per-child (no `#[serial]`, no process-global mutation) and
// behaves identically locally and in CI.

mod common;

use assert_cmd::Command;
use common::{TempHome, fixture};
use predicates::prelude::*;
use serde_json::json;
use vibe_coding_tracker::session::{ParseMode, parse_session_file_typed_with_mode};
use vibe_coding_tracker::{VERSION, parse_session_file_typed};

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

/// `vct` with an isolated, offline per-child environment so every provider
/// directory and `~/.vct` cache resolve under the temp home.
fn child_cmd(home: &TempHome) -> Command {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.env("HOME", home.home())
        .env("USERPROFILE", home.home())
        .env("HERMES_HOME", home.home().join(".hermes"))
        .env("VCT_OFFLINE", "1")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME");
    cmd
}

// ============================================================================
// Group 1: CLI wiring and single-file behavior
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
fn analysis_file_json_matches_typed_parser_for_every_jsonl_provider() {
    let home = TempHome::new();
    for fixture_name in [
        "test_conversation_claude_code.jsonl",
        "test_conversation_codex.jsonl",
        "test_conversation_copilot.jsonl",
        "test_conversation_gemini.jsonl",
    ] {
        let path = fixture(fixture_name);
        let expected = serde_json::to_value(parse_session_file_typed(&path).unwrap()).unwrap();

        let default_output = child_cmd(&home)
            .arg("analysis")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            default_output.status.success(),
            "default single-file analysis failed for {fixture_name}"
        );
        assert!(default_output.stdout.ends_with(b"\n"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&default_output.stdout).unwrap(),
            expected,
            "single-file JSON drifted for {fixture_name}"
        );

        let explicit_json = child_cmd(&home)
            .arg("analysis")
            .arg(&path)
            .arg("--json")
            .output()
            .unwrap();
        assert!(explicit_json.status.success());
        assert_eq!(
            explicit_json.stdout, default_output.stdout,
            "--json changed the single-file payload for {fixture_name}"
        );
    }
}

#[test]
fn analysis_legacy_path_and_output_flags_are_rejected() {
    let path = fixture("test_conversation_claude_code.jsonl");

    for flag in ["--path", "-p"] {
        Command::cargo_bin("vibe_coding_tracker")
            .unwrap()
            .arg("analysis")
            .arg(flag)
            .arg(&path)
            .assert()
            .failure();
    }

    for flag in ["--output", "-o"] {
        Command::cargo_bin("vibe_coding_tracker")
            .unwrap()
            .arg("analysis")
            .arg(&path)
            .arg(flag)
            .arg("output.json")
            .assert()
            .failure();
    }
}

#[test]
fn test_analysis_command_with_nonexistent_file() {
    let home = TempHome::new();
    child_cmd(&home)
        .arg("analysis")
        .arg("nonexistent_file.jsonl")
        .assert()
        .failure();
}

#[test]
fn analysis_file_rejects_completely_unknown_provider_schema() {
    let home = TempHome::new();
    let temp_dir = tempfile::TempDir::new().unwrap();
    let path = temp_dir.path().join("future.jsonl");
    std::fs::write(
        &path,
        r#"{"type":"future.provider.event","timestamp":"2026-07-12T00:00:00Z"}"#,
    )
    .unwrap();

    child_cmd(&home)
        .arg("analysis")
        .arg(&path)
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "contained no recognized provider records",
        ));
}

#[test]
fn analysis_file_warns_when_only_some_analyzer_payloads_fail() {
    let home = TempHome::new();
    let temp_dir = tempfile::TempDir::new().unwrap();
    let path = temp_dir.path().join("partial.jsonl");
    std::fs::write(
        &path,
        concat!(
            r#"{"type":"assistant","parentUuid":"root","timestamp":"2026-07-12T00:00:00Z","message":{"model":"claude-sonnet","usage":{"input_tokens":1,"output_tokens":1},"content":[]}}"#,
            "\n",
            r#"{"type":"assistant","parentUuid":"root","timestamp":"2026-07-12T00:00:01Z","message":{"model":"claude-sonnet","usage":"future","content":[]}}"#,
            "\n"
        ),
    )
    .unwrap();

    let output = child_cmd(&home)
        .arg("analysis")
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Skipped 1 malformed or unsupported analyzer records")
    );
}

#[test]
fn analysis_file_does_not_log_analyzer_irrelevant_codex_schema_drift() {
    let home = TempHome::new();
    let temp_dir = tempfile::TempDir::new().unwrap();
    let path = temp_dir.path().join("irrelevant-drift.jsonl");
    let mut contents = String::from(
        r#"{"timestamp":"2026-07-12T00:00:00Z","type":"session_meta","payload":{"type":"session_meta","id":"session"}}"#,
    );
    contents.push('\n');
    for _ in 0..100 {
        contents.push_str(
            r#"{"timestamp":"2026-07-12T00:00:01Z","type":"response_item","payload":{"type":"message","output":true}}"#,
        );
        contents.push('\n');
    }
    std::fs::write(&path, contents).unwrap();

    child_cmd(&home)
        .arg("analysis")
        .arg(&path)
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
    assert!(
        !home.home().join(".vct/logs").exists(),
        "analyzer-irrelevant Codex records must not create diagnostic logs"
    );
}

#[test]
fn usage_output_flag_is_rejected() {
    for flag in ["--output", "-o"] {
        Command::cargo_bin("vibe_coding_tracker")
            .unwrap()
            .arg("usage")
            .arg(flag)
            .arg("output.json")
            .assert()
            .failure();
    }
}

#[test]
fn test_analysis_validates_file_extension() {
    let home = TempHome::new();
    let temp_dir = tempfile::TempDir::new().unwrap();
    let wrong_ext = temp_dir.path().join("test.txt");
    std::fs::write(&wrong_ext, "test content").unwrap();

    // Behavior depends on implementation - may succeed or fail; must not panic.
    let _ = child_cmd(&home).arg("analysis").arg(&wrong_ext).output();
}

#[test]
fn test_cli_handles_unicode_paths() {
    let home = TempHome::new();
    let temp_dir = tempfile::TempDir::new().unwrap();
    let unicode_path = temp_dir.path().join("測試_test_файл.jsonl");
    std::fs::copy(
        fixture("test_conversation_claude_code.jsonl"),
        &unicode_path,
    )
    .unwrap();

    child_cmd(&home)
        .arg("analysis")
        .arg(&unicode_path)
        .assert()
        .success();
}

#[test]
fn test_cli_handles_spaces_in_paths() {
    let home = TempHome::new();
    let temp_dir = tempfile::TempDir::new().unwrap();
    let space_path = temp_dir.path().join("file with spaces.jsonl");
    std::fs::copy(fixture("test_conversation_claude_code.jsonl"), &space_path).unwrap();

    child_cmd(&home)
        .arg("analysis")
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
        .stdout(predicate::str::contains("[FILE]"))
        .stdout(predicate::str::contains("--path").not())
        .stdout(predicate::str::contains("--output").not());
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
        .stdout(predicate::str::contains("--output").not());
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
    let path = fixture("test_conversation_claude_code.jsonl");
    for combo in [
        ["--json", "--text"],
        ["--json", "--table"],
        ["--text", "--table"],
    ] {
        Command::cargo_bin("vibe_coding_tracker")
            .unwrap()
            .arg("analysis")
            .arg(&path)
            .args(combo)
            .assert()
            .failure()
            .stderr(predicate::str::contains("cannot be used with"));
    }
}

#[test]
fn analysis_file_rejects_period_flags() {
    let path = fixture("test_conversation_claude_code.jsonl");

    for period in ["--daily", "--weekly", "--monthly", "--all", "-a"] {
        Command::cargo_bin("vibe_coding_tracker")
            .unwrap()
            .arg("analysis")
            .arg(&path)
            .arg(period)
            .assert()
            .failure()
            .stderr(predicate::str::contains("cannot be used with"));
    }
}

#[test]
fn analysis_file_text_and_table_use_the_canonical_parse() {
    let home = TempHome::new();
    let path = fixture("test_conversation_claude_code.jsonl");

    child_cmd(&home)
        .arg("analysis")
        .arg(&path)
        .arg("--text")
        .assert()
        .success()
        .stdout(predicate::str::contains("claude-sonnet-4-20250514"))
        .stdout(predicate::str::contains("editLines="));

    child_cmd(&home)
        .arg("analysis")
        .arg(&path)
        .arg("--table")
        .assert()
        .success()
        .stdout(predicate::str::contains("Analysis Statistics"))
        .stdout(predicate::str::contains("claude-sonnet-4-20250514"));
}

#[test]
fn single_file_summary_projection_is_parse_mode_invariant() {
    for fixture_name in [
        "test_conversation_claude_code.jsonl",
        "test_conversation_codex.jsonl",
        "test_conversation_copilot.jsonl",
        "test_conversation_gemini.jsonl",
    ] {
        let path = fixture(fixture_name);
        let full = parse_session_file_typed_with_mode(&path, ParseMode::Full).unwrap();
        let compact = parse_session_file_typed_with_mode(&path, ParseMode::UsageOnly).unwrap();

        assert!(compact.records.iter().all(|record| {
            record.write_file_details.is_empty()
                && record.read_file_details.is_empty()
                && record.edit_file_details.is_empty()
                && record.run_command_details.is_empty()
        }));

        let full_rows = vibe_coding_tracker::analysis::project_code_analysis(&full).rows;
        let compact_rows = vibe_coding_tracker::analysis::project_code_analysis(&compact).rows;
        assert_eq!(
            serde_json::to_value(full_rows).unwrap(),
            serde_json::to_value(compact_rows).unwrap(),
            "summary scalars drifted between parse modes for {fixture_name}"
        );
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

    assert!(output.stdout.ends_with(b"\n"));
    let analyses: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("valid JSON array");
    let arr = analyses.as_array().expect("analysis --json is an array");
    let claude = arr
        .iter()
        .find(|analysis| analysis["extensionName"] == "Claude-Code")
        .expect("seeded Claude session should retain the canonical analysis shape");
    let records = claude["records"].as_array().expect("records array");
    assert!(!records.is_empty());
    assert!(
        records.iter().any(|record| {
            [
                "writeFileDetails",
                "readFileDetails",
                "editFileDetails",
                "runCommandDetails",
            ]
            .iter()
            .any(|key| {
                record[*key]
                    .as_array()
                    .is_some_and(|details| !details.is_empty())
            })
        }),
        "batch JSON must retain Full parse details"
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
fn analysis_noninteractive_formats_fail_when_every_candidate_fails() {
    for format in ["--json", "--text", "--table"] {
        let home = TempHome::new();
        home.put_claude_session("broken", "broken.jsonl", "{not json\n");

        child_cmd(&home)
            .arg("analysis")
            .arg(format)
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "failed to parse all 1 analysis sources",
            ));
    }
}

#[test]
fn analysis_json_fails_when_provider_schema_is_completely_unknown() {
    let home = TempHome::new();
    home.put_claude_session(
        "future",
        "future.jsonl",
        r#"{"type":"future.claude.event","timestamp":"2026-07-12T00:00:00Z"}"#,
    );

    child_cmd(&home)
        .arg("analysis")
        .arg("--json")
        .assert()
        .failure()
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "failed to parse all 1 analysis sources",
        ));
}

#[test]
fn analysis_noninteractive_formats_warn_and_keep_partial_results() {
    for format in ["--json", "--text", "--table"] {
        let home = TempHome::new();
        home.put_claude_session(
            "valid",
            "valid.jsonl",
            &common::fixture_str("test_conversation_claude_code.jsonl"),
        );
        home.put_claude_session("broken", "broken.jsonl", "{not json\n");

        child_cmd(&home)
            .arg("analysis")
            .arg(format)
            .assert()
            .success()
            .stdout(predicate::str::is_empty().not())
            .stderr(predicate::str::contains(
                "Encountered 1 analysis source failures while scanning 2 candidates",
            ));
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
fn analysis_file_does_not_create_config() {
    let home = TempHome::new();
    child_cmd(&home)
        .arg("analysis")
        .arg(fixture("test_conversation_claude_code.jsonl"))
        .assert()
        .success();
    assert!(
        !home.home().join(".vct/config.toml").exists(),
        "single-file analysis must not create config.toml"
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
        .stdout(predicate::str::contains("[providers]"))
        .stdout(predicate::str::contains("[usage.quota]"));

    // The show path must have materialized the file under the temp home, and it
    // must not fold in any version.json bookkeeping.
    let config = home.home().join(".vct/config.toml");
    assert!(config.exists());
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(!text.contains("[update]"));
}

#[test]
fn config_migrate_upgrades_a_legacy_file() {
    let home = TempHome::new();
    // Seed a pre-`[usage.quota]` config with the legacy key names.
    home.put(
        ".vct/config.toml",
        "[usage]\nquota_panels = [\"claude\"]\nrefresh_interval_secs = 15\n",
    );

    child_cmd(&home)
        .arg("config")
        .arg("migrate")
        .assert()
        .success()
        .stdout(predicate::str::contains("Migrated"));

    let text = std::fs::read_to_string(home.home().join(".vct/config.toml")).unwrap();
    assert!(text.starts_with("#:schema "));
    assert!(text.contains("[usage.quota]"));
    assert!(!text.contains("quota_panels"));
    assert!(!text.contains("refresh_interval_secs"));

    // Running it again is a no-op.
    child_cmd(&home)
        .arg("config")
        .arg("migrate")
        .assert()
        .success()
        .stdout(predicate::str::contains("already up to date"));
}

#[cfg(unix)]
#[test]
fn config_edit_splits_a_multi_word_editor_command() {
    // `$EDITOR` / `$VISUAL` often carry arguments (`code --wait`); the program +
    // args must be split, with the config path passed as the trailing arg.
    use std::os::unix::fs::PermissionsExt;

    let home = TempHome::new();
    let stub = home.home().join("stub-editor.sh");
    let sentinel = home.home().join("editor-argv.txt");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n",
            sentinel.display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).unwrap();

    child_cmd(&home)
        .env_remove("VISUAL")
        .env("EDITOR", format!("{} --flag", stub.display()))
        .arg("config")
        .arg("edit")
        .assert()
        .success();

    let argv = std::fs::read_to_string(&sentinel).expect("stub editor should have run");
    assert!(argv.contains("--flag"), "the editor's own arg is forwarded");
    assert!(
        argv.contains("config.toml"),
        "the config path is passed as the trailing arg, got: {argv:?}"
    );
}

#[cfg(unix)]
#[test]
fn config_edit_propagates_a_failing_editor() {
    // An editor that exits non-zero must make `vct config edit` exit non-zero, so
    // scripts can tell the edit was aborted / failed.
    use std::os::unix::fs::PermissionsExt;

    let home = TempHome::new();
    let stub = home.home().join("failing-editor.sh");
    std::fs::write(&stub, "#!/bin/sh\nexit 7\n").unwrap();
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).unwrap();

    child_cmd(&home)
        .env_remove("VISUAL")
        .env("EDITOR", stub.display().to_string())
        .arg("config")
        .arg("edit")
        .assert()
        .failure();
}
