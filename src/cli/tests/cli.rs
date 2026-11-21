// Integration tests for the `vct` binary:
// 1. CLI wiring and single-file behavior (checks stopping before dispatch run on the inherited environment; anything reaching dispatch uses an isolated child HOME because `run()` checks auto-update config).
// 2. Per-child HOME smoke tests for batch `usage`, `analysis`, `config`, and startup side effects.
// 3. Remote ledger export and SSH sync behavior.
//
// Isolation is applied per child process rather than mutating the test process environment, so tests run in parallel.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use vct_core::session::{ParseMode, parse_session_file_typed_with_mode};
use vct_core::{CARGO_VERSION, RUST_VERSION, VERSION, parse_session_file_typed};
use vct_test_support::{TempHome, fixture};

/// Returns a minimal pricing map for seeding the offline cache.
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

/// Returns a `vct` command isolated to `home` with network access disabled.
fn child_cmd(home: &TempHome) -> Command {
    let mut cmd = Command::cargo_bin("vibe_coding_tracker").unwrap();
    cmd.env("HOME", home.home())
        .env("USERPROFILE", home.home())
        .env("HERMES_HOME", home.home().join(".hermes"))
        .env("GROK_HOME", home.home().join(".grok"))
        .env("DSH_HOME", home.home().join(".dsh"))
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
    let home = TempHome::new();
    child_cmd(&home)
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
fn test_version_command_json_matches_build_constants() {
    let home = TempHome::new();
    let output = child_cmd(&home)
        .arg("version")
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // Assert the full object to pin both the JSON key set and that `vct version` matches the build constants.
    assert_eq!(
        json,
        json!({
            "Version": VERSION,
            "Rust Version": RUST_VERSION,
            "Cargo Version": CARGO_VERSION,
        })
    );
}

#[test]
fn test_version_command_text() {
    let home = TempHome::new();
    child_cmd(&home)
        .arg("version")
        .arg("--text")
        .assert()
        .success()
        .stdout(predicate::str::contains("Version"));
}

#[test]
fn analysis_file_json_matches_typed_parser_for_every_file_provider() {
    let home = TempHome::new();
    for fixture_name in [
        "sessions/claude_code.jsonl",
        "sessions/codex.jsonl",
        "sessions/copilot.jsonl",
        "sessions/gemini.jsonl",
        "sessions/grok/signals.json",
        "sessions/dsh/session.jsonl.zstd",
        "sessions/antigravity/agy/brain/5f0c2a9e-1b7d-4c3e-9a2f-6d8e4b1c7a30/.system_generated/logs/transcript_full.jsonl",
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
    let path = fixture("sessions/claude_code.jsonl");

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
fn analysis_detects_provider_by_content_not_extension() {
    let home = TempHome::new();
    let temp_dir = tempfile::TempDir::new().unwrap();

    let wrong_ext = temp_dir.path().join("session.txt");
    std::fs::copy(fixture("sessions/claude_code.jsonl"), &wrong_ext).unwrap();
    child_cmd(&home)
        .arg("analysis")
        .arg(&wrong_ext)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            r#""extensionName": "Claude-Code""#,
        ));

    let right_ext = temp_dir.path().join("session.jsonl");
    std::fs::write(&right_ext, "test content\n").unwrap();
    child_cmd(&home)
        .arg("analysis")
        .arg(&right_ext)
        .assert()
        .failure()
        .stderr(predicate::str::contains("Failed to parse JSON from file"));
}

#[test]
fn test_cli_handles_unicode_paths() {
    let home = TempHome::new();
    let temp_dir = tempfile::TempDir::new().unwrap();
    let unicode_path = temp_dir.path().join("測試_test_файл.jsonl");
    std::fs::copy(fixture("sessions/claude_code.jsonl"), &unicode_path).unwrap();

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
    std::fs::copy(fixture("sessions/claude_code.jsonl"), &space_path).unwrap();

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
fn test_removed_subcommands_are_rejected() {
    for name in ["monitor", "fetch"] {
        Command::cargo_bin("vibe_coding_tracker")
            .unwrap()
            .arg(name)
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}

#[test]
fn test_no_subcommand_off_a_terminal_prints_help() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Usage:"))
        .stderr(predicate::str::contains("quota"));
}

#[test]
fn test_quota_help() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("quota")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("claude"))
        .stdout(predicate::str::contains("codex"))
        .stdout(predicate::str::contains("copilot"))
        .stdout(predicate::str::contains("cursor"))
        .stdout(predicate::str::contains("grok"))
        .stdout(predicate::str::contains("antigravity"));
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
    let path = fixture("sessions/claude_code.jsonl");
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
    let path = fixture("sessions/claude_code.jsonl");

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
    let path = fixture("sessions/claude_code.jsonl");

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
        "sessions/claude_code.jsonl",
        "sessions/codex.jsonl",
        "sessions/copilot.jsonl",
        "sessions/gemini.jsonl",
        "sessions/grok/signals.json",
        "sessions/dsh/session.jsonl.zstd",
        "sessions/antigravity/agy/brain/5f0c2a9e-1b7d-4c3e-9a2f-6d8e4b1c7a30/.system_generated/logs/transcript_full.jsonl",
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

        let full_rows = vct_core::analysis::project_code_analysis(&full).rows;
        let compact_rows = vct_core::analysis::project_code_analysis(&compact).rows;
        assert_eq!(
            serde_json::to_value(full_rows).unwrap(),
            serde_json::to_value(compact_rows).unwrap(),
            "summary scalars drifted between parse modes for {fixture_name}"
        );
    }
}

#[test]
fn test_quota_format_requires_provider() {
    for flag in ["--text", "--table"] {
        Command::cargo_bin("vibe_coding_tracker")
            .unwrap()
            .arg("quota")
            .arg(flag)
            .assert()
            .code(2)
            .stderr(predicate::str::contains("name a provider"));
    }
}

#[test]
fn test_usage_by_day_requires_json() {
    let home = TempHome::new();
    for args in [
        &["usage", "--by-day"][..],
        &["usage", "--by-day", "--table"],
        &["usage", "--by-day", "--text"],
    ] {
        child_cmd(&home).args(args).assert().code(2);
    }
}

#[test]
fn test_quota_invalid_provider() {
    Command::cargo_bin("vibe_coding_tracker")
        .unwrap()
        .arg("quota")
        .arg("not-a-provider")
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid value"));
}

#[test]
fn test_quota_multiple_output_formats() {
    // clap rejects the combination before any network call is made.
    for combo in [
        ["--json", "--text"],
        ["--json", "--table"],
        ["--text", "--table"],
    ] {
        Command::cargo_bin("vibe_coding_tracker")
            .unwrap()
            .arg("quota")
            .arg("claude")
            .args(combo)
            .assert()
            .failure()
            .stderr(predicate::str::contains("cannot be used with"));
    }
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
        &vct_test_support::fixture_str("sessions/claude_code.jsonl"),
    );
    // Codex keeps usage nested internally, so include one to verify flat normalization below.
    home.put_codex_session(
        "2026/06/06/rollout-2026-06-06T10-00-00-019e4b75-8a4e-7801-a9ed-9723e77e0497.jsonl",
        &vct_test_support::fixture_str("sessions/codex.jsonl"),
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

    for row in arr {
        let usage = row["usage"]
            .as_object()
            .expect("usage --json rows carry a usage object");
        for key in [
            "input_tokens",
            "output_tokens",
            "reasoning_output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "total_tokens",
        ] {
            assert!(
                usage.get(key).is_some_and(serde_json::Value::is_i64),
                "usage --json row for {} must carry flat integer {key}",
                row["model"]
            );
        }
        assert!(
            usage.get("total_token_usage").is_none(),
            "nested Codex shape must not leak into usage --json"
        );
    }
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
        &vct_test_support::fixture_str("sessions/claude_code.jsonl"),
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
        &vct_test_support::fixture_str("sessions/claude_code.jsonl"),
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
        &vct_test_support::fixture_str("sessions/claude_code.jsonl"),
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
            &vct_test_support::fixture_str("sessions/claude_code.jsonl"),
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
    let home = TempHome::new();
    child_cmd(&home).arg("version").assert().success();
    assert!(
        !home.home().join(".vct/config.toml").exists(),
        "version must not create config.toml"
    );
    assert!(
        !home.home().join(".vct/version/vct.json").exists(),
        "offline startup must not create the update record"
    );
}

#[test]
fn startup_removes_the_flat_cache_layout() {
    const LEGACY: [&str; 12] = [
        "claude_usage.json",
        "codex_usage.json",
        "copilot_usage.json",
        "cursor_usage.json",
        "grok_usage.json",
        "claude_version.json",
        "codex_version.json",
        "copilot_version.json",
        "cursor_version.json",
        "grok_version.json",
        "version.json",
        "cursor_usage_events.json",
    ];
    let home = TempHome::new();
    for name in LEGACY {
        home.put(&format!(".vct/{name}"), "{}");
    }
    home.put(".vct/sessions/claude.json", "{}");

    child_cmd(&home).arg("version").assert().success();

    for name in LEGACY {
        assert!(
            !home.home().join(".vct").join(name).exists(),
            "{name} must be gone"
        );
    }
    assert!(
        home.home().join(".vct/sessions/claude.json").exists(),
        "the ledger is not part of the sweep"
    );
    assert!(!home.home().join(".vct/config.toml").exists());
}

#[test]
fn startup_reads_auto_update_preference_without_rewriting_config() {
    let home = TempHome::new();
    let original = "#:schema https://example.test/vct.schema.json\n[general]\nauto_update = false\nversion = 3\n";
    home.put(".vct/config.toml", original);

    child_cmd(&home)
        .arg("version")
        .arg("--json")
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(home.home().join(".vct/config.toml")).unwrap(),
        original
    );
    assert!(!home.home().join(".vct/version/vct.json").exists());
}

#[cfg(debug_assertions)]
#[test]
fn dev_build_skips_auto_update_before_network_or_cache_writes() {
    let home = TempHome::new();
    let output = child_cmd(&home)
        .env_remove("VCT_OFFLINE")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .arg("version")
        .arg("--json")
        .output()
        .unwrap();

    assert!(output.status.success());
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    assert!(output.stderr.is_empty());
    assert!(
        !home.home().join(".vct/version/vct.json").exists(),
        "a debug build must skip before claiming the daily check"
    );
}

// The dead proxy makes a regressed offline guard fail instead of downloading a release over the test binary.
#[test]
fn explicit_update_is_a_no_op_when_offline() {
    let home = TempHome::new();

    for args in [&["update"][..], &["update", "--force"][..]] {
        let output = child_cmd(&home)
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .args(args)
            .output()
            .unwrap();

        assert!(output.status.success(), "{args:?} must exit successfully");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "Checking for updates...\n",
            "{args:?} must stop before the GitHub request"
        );
    }
}

#[test]
fn analysis_file_does_not_create_config() {
    let home = TempHome::new();
    child_cmd(&home)
        .arg("analysis")
        .arg(fixture("sessions/claude_code.jsonl"))
        .assert()
        .success();
    assert!(
        !home.home().join(".vct/config.toml").exists(),
        "single-file analysis must not create config.toml"
    );
}

#[test]
fn config_path_prints_config_toml_location() {
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
        .stdout(predicate::str::contains("auto_update"))
        .stdout(predicate::str::contains("[providers]"))
        .stdout(predicate::str::contains("[usage.quota]"));

    // Update check bookkeeping must stay out of config.toml.
    let config = home.home().join(".vct/config.toml");
    assert!(config.exists());
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(!text.contains("[update]"));
}

#[test]
fn config_migrate_upgrades_a_legacy_file() {
    let home = TempHome::new();
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
    assert!(text.contains("auto_update = true"));
    assert!(!text.contains("quota_panels"));
    assert!(!text.contains("refresh_interval_secs"));

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

// ============================================================================
// Group 3: remote ledgers
// ============================================================================

#[test]
fn ledger_export_prints_one_line_of_totals_and_creates_no_config() {
    let home = TempHome::new();
    // Claude names a project directory after the working directory.
    home.put_claude_session(
        "-home-wei-repo-secret",
        "session.jsonl",
        &vct_test_support::fixture_str("sessions/claude_code.jsonl"),
    );

    let output = child_cmd(&home)
        .args(["ledger", "export"])
        .output()
        .expect("spawn vct");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1);
    let export: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(export["vct_ledger_export"], 1);
    let sessions = export["providers"]["claude"]["sessions"]
        .as_object()
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(!stdout.contains("-home-wei-repo") && !stdout.contains("session.jsonl"));
    assert!(!home.home().join(".vct/config.toml").exists());
    assert!(home.home().join(".vct/sessions/claude.json").exists());
}

/// Returns a [`child_cmd`] without `VCT_OFFLINE` so remote sync runs, with a dead proxy so any unexpected HTTP request fails locally (for example if a test straddles UTC midnight and misses the seeded pricing cache).
#[cfg(unix)]
fn online_cmd(home: &TempHome) -> Command {
    let mut cmd = child_cmd(home);
    cmd.env_remove("VCT_OFFLINE")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy");
    cmd
}

/// Creates a temporary directory with a stub `ssh` that runs `vct ledger export` against `remote`.
#[cfg(unix)]
fn fake_ssh_dir(remote: &TempHome) -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = format!(
        "#!/bin/sh\nHOME='{}' VCT_OFFLINE=1 exec '{}' ledger export\n",
        remote.home().display(),
        assert_cmd::cargo::cargo_bin("vibe_coding_tracker").display()
    );
    let path = dir.path().join("ssh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

#[cfg(unix)]
#[test]
fn usage_and_analysis_merge_a_configured_remote_and_drop_it_once_unconfigured() {
    let remote = TempHome::new();
    remote.put_claude_session(
        "proj",
        "session.jsonl",
        &vct_test_support::fixture_str("sessions/claude_code.jsonl"),
    );
    let ssh = fake_ssh_dir(&remote);
    let local = TempHome::new();
    // Seed pricing so `online_cmd` needs no HTTP request.
    local.seed_pricing_cache(&pricing_seed());
    local.put(
        ".vct/config.toml",
        "[general]\nauto_update = false\n[remotes]\nhosts = [\"devbox\"]\n",
    );
    let online = |args: &[&str]| {
        online_cmd(&local)
            .env("PATH", ssh.path())
            .args(args)
            .output()
            .expect("spawn vct")
    };

    let usage = online(&["usage", "--json"]);
    assert!(
        usage.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&usage.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&usage.stdout).unwrap();
    assert_eq!(rows[0]["model"], "claude-sonnet-4-20250514");
    assert!(
        rows[0]["usage"]["input_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
    );

    // Offline nothing is fetched, and the stored snapshot still counts.
    let offline = child_cmd(&local)
        .args(["usage", "--json"])
        .output()
        .unwrap();
    assert_eq!(offline.stdout, usage.stdout);

    // With the snapshot gone, analysis has to do its own fetch to show it.
    std::fs::remove_dir_all(local.home().join(".vct/remotes")).unwrap();
    let analysis = online(&["analysis", "--text"]);
    assert!(String::from_utf8_lossy(&analysis.stdout).contains("claude-sonnet-4-20250514"));

    // An unparseable config reads as no hosts; that is not the user removing one, so the snapshot stays and keeps counting.
    local.put(".vct/config.toml", "[remotes]\nhosts = \"devbox\"\n");
    let broken = online(&["usage", "--json"]);
    assert_eq!(broken.stdout, usage.stdout);
    assert!(local.home().join(".vct/remotes/devbox").exists());

    local.put(
        ".vct/config.toml",
        "[general]\nauto_update = false\n[remotes]\nhosts = []\n",
    );
    let unconfigured = online(&["usage", "--json"]);
    assert_eq!(String::from_utf8_lossy(&unconfigured.stdout).trim(), "[]");
    assert!(!local.home().join(".vct/remotes/devbox").exists());
}

#[cfg(unix)]
#[test]
fn an_unreachable_remote_warns_and_leaves_the_local_result_standing() {
    use std::os::unix::fs::PermissionsExt;
    let local = TempHome::new();
    local.put_claude_session(
        "proj",
        "session.jsonl",
        &vct_test_support::fixture_str("sessions/claude_code.jsonl"),
    );
    local.seed_pricing_cache(&pricing_seed());
    local.put(
        ".vct/config.toml",
        "[general]\nauto_update = false\n[remotes]\nhosts = [\"devbox\"]\n",
    );
    let ssh = tempfile::tempdir().unwrap();
    let path = ssh.path().join("ssh");
    std::fs::write(
        &path,
        "#!/bin/sh\necho 'ssh: connect to host devbox: timed out' >&2\nexit 255\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = online_cmd(&local)
        .env("PATH", ssh.path())
        .args(["usage", "--json"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("remote devbox") && stderr.contains("timed out"),
        "{stderr}"
    );
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows[0]["model"], "claude-sonnet-4-20250514");
}

#[test]
fn usage_json_by_day_matches_totals_reports_priced_and_remotes() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "priced.jsonl",
        &vct_test_support::fixture_str("sessions/claude_code.jsonl"),
    );
    home.put_claude_session(
        "proj",
        "unpriced.jsonl",
        concat!(
            r#"{"type":"assistant","parentUuid":"root","timestamp":"2026-06-06T12:00:00Z","message":{"model":"unpriced-custom-xyzzy","usage":{"input_tokens":100,"output_tokens":50},"content":[]}}"#,
            "\n"
        ),
    );
    home.put_codex_session(
        "2026/06/06/rollout-2026-06-06T10-00-00-019e4b75-8a4e-7801-a9ed-9723e77e0497.jsonl",
        &vct_test_support::fixture_str("sessions/codex.jsonl"),
    );
    home.seed_pricing_cache(&pricing_seed());
    // Configure `devbox` so `prune_remotes` retains its offline snapshot without spawning ssh.
    home.put(
        ".vct/config.toml",
        "[general]\nauto_update = false\n[remotes]\nhosts = [\"devbox\"]\n",
    );
    // Seed a remote snapshot directory directly so the test runs identically on every platform.
    home.put(
        ".vct/remotes/devbox/sessions/claude.json",
        &serde_json::to_string(&json!({
            "schema_version": 1,
            "provider": "claude",
            "sessions": {
                "0123456789abcdef0123456789abcdef": {
                    "days": {
                        "2020-01-15": {
                            "usage": {
                                "claude-sonnet-4-20250514": {
                                    "input_tokens": 1000,
                                    "output_tokens": 200,
                                    "cache_read_input_tokens": 0,
                                    "cache_creation_input_tokens": 0
                                }
                            },
                            "active": { "usage": true, "analysis": false }
                        }
                    }
                }
            }
        }))
        .unwrap(),
    );

    let totals_out = child_cmd(&home)
        .args(["usage", "--json", "--all"])
        .output()
        .expect("spawn vct usage --json");
    assert!(
        totals_out.status.success(),
        "{}",
        String::from_utf8_lossy(&totals_out.stderr)
    );
    let totals_rows: Vec<serde_json::Value> = serde_json::from_slice(&totals_out.stdout).unwrap();

    let by_day_out = child_cmd(&home)
        .args(["usage", "--json", "--by-day", "--all"])
        .output()
        .expect("spawn vct usage --json --by-day");
    assert!(
        by_day_out.status.success(),
        "{}",
        String::from_utf8_lossy(&by_day_out.stderr)
    );
    let payload: serde_json::Value = serde_json::from_slice(&by_day_out.stdout).unwrap();
    assert_eq!(payload["remotes"], json!(["devbox"]));

    let days = payload["days"].as_array().expect("days array");
    assert!(!days.is_empty());
    // Dates are sorted ascending.
    let dates: Vec<&str> = days.iter().map(|d| d["date"].as_str().unwrap()).collect();
    let mut sorted_dates = dates.clone();
    sorted_dates.sort_unstable();
    assert_eq!(dates, sorted_dates);
    assert!(dates.contains(&"2020-01-15"));

    // Sum per-day tokens and costs per model and compare against `usage --json`.
    let mut summed_tokens: std::collections::HashMap<String, i64> =
        std::collections::HashMap::new();
    let mut summed_costs: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let mut saw_priced = false;
    let mut saw_unpriced = false;

    for day in days {
        for row in day["models"].as_array().unwrap() {
            let model = row["model"].as_str().unwrap().to_string();
            let priced = row["priced"].as_bool().expect("boolean priced field");
            if model == "claude-sonnet-4-20250514" {
                assert!(priced);
                saw_priced = true;
            } else if model == "unpriced-custom-xyzzy" {
                assert!(!priced);
                saw_unpriced = true;
            }
            let total = row["usage"]["total_tokens"].as_i64().unwrap();
            let cost = row["cost_usd"].as_f64().unwrap();
            *summed_tokens.entry(model.clone()).or_insert(0) += total;
            *summed_costs.entry(model).or_insert(0.0) += cost;
        }
    }
    assert!(saw_priced && saw_unpriced);

    assert_eq!(summed_tokens.len(), totals_rows.len());
    for row in &totals_rows {
        let model = row["model"].as_str().unwrap();
        assert_eq!(
            summed_tokens[model],
            row["usage"]["total_tokens"].as_i64().unwrap(),
            "per-day token sum drifted for {model}"
        );
        assert!(
            (summed_costs[model] - row["cost_usd"].as_f64().unwrap()).abs() < 1e-9,
            "per-day cost sum drifted for {model}"
        );
    }

    // `--daily` composes with `--by-day` and excludes the older 2020-01-15 remote day.
    let daily_out = child_cmd(&home)
        .args(["usage", "--json", "--by-day", "--daily"])
        .output()
        .expect("spawn vct usage --json --by-day --daily");
    assert!(daily_out.status.success());
    let daily_payload: serde_json::Value = serde_json::from_slice(&daily_out.stdout).unwrap();
    let daily_dates: Vec<&str> = daily_payload["days"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["date"].as_str().unwrap())
        .collect();
    assert!(!daily_dates.contains(&"2020-01-15"));
}

#[test]
fn usage_json_merge_providers_applies_only_when_requested() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "variants.jsonl",
        concat!(
            r#"{"type":"assistant","parentUuid":"root","timestamp":"2026-06-06T12:00:00Z","message":{"model":"openai/claude-sonnet-4-20250514","usage":{"input_tokens":1000,"output_tokens":100},"content":[]}}"#,
            "\n",
            r#"{"type":"assistant","parentUuid":"root","timestamp":"2026-06-06T12:01:00Z","message":{"model":"claude-sonnet-4-20250514","usage":{"input_tokens":2000,"output_tokens":200},"content":[]}}"#,
            "\n"
        ),
    );
    home.seed_pricing_cache(&pricing_seed());

    // Default `usage --json` stays unmerged even though `usage.merge_models` defaults to true.
    let raw_out = child_cmd(&home)
        .args(["usage", "--json"])
        .output()
        .expect("spawn vct usage --json");
    assert!(raw_out.status.success());
    let raw_rows: Vec<serde_json::Value> = serde_json::from_slice(&raw_out.stdout).unwrap();
    assert_eq!(raw_rows.len(), 2);
    let raw_cost: f64 = raw_rows
        .iter()
        .map(|r| r["cost_usd"].as_f64().unwrap())
        .sum();

    // `--merge-providers` collapses both into the base model and sums costs verbatim.
    let merged_out = child_cmd(&home)
        .args(["usage", "--json", "--merge-providers"])
        .output()
        .expect("spawn vct usage --json --merge-providers");
    assert!(merged_out.status.success());
    let merged_rows: Vec<serde_json::Value> = serde_json::from_slice(&merged_out.stdout).unwrap();
    assert_eq!(merged_rows.len(), 1);
    assert_eq!(merged_rows[0]["model"], "claude-sonnet-4-20250514");
    assert_eq!(merged_rows[0]["usage"]["input_tokens"], json!(3000));
    assert_eq!(merged_rows[0]["usage"]["output_tokens"], json!(300));
    assert!((merged_rows[0]["cost_usd"].as_f64().unwrap() - raw_cost).abs() < 1e-9);

    // `--by-day --merge-providers` collapses within each day too.
    let daily_merged = child_cmd(&home)
        .args(["usage", "--json", "--by-day", "--merge-providers"])
        .output()
        .expect("spawn vct usage --json --by-day --merge-providers");
    assert!(daily_merged.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&daily_merged.stdout).unwrap();
    let models = payload["days"][0]["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["model"], "claude-sonnet-4-20250514");
    assert_eq!(models[0]["priced"], json!(true));
    assert!((models[0]["cost_usd"].as_f64().unwrap() - raw_cost).abs() < 1e-9);
}

#[test]
fn quota_json_empty_panels_returns_empty_array() {
    let home = TempHome::new();
    home.put(
        ".vct/config.toml",
        "[general]\nauto_update = false\n[usage.quota]\npanels = []\n",
    );

    let output = child_cmd(&home)
        .args(["quota", "--json"])
        .env_remove("ANTIGRAVITY_LS_ADDRESS")
        .env_remove("ANTIGRAVITY_CSRF_TOKEN")
        .env_remove("ANTIGRAVITY_APP_DATA_DIR")
        .env_remove("CLOUD_CODE_URL")
        .output()
        .expect("spawn vct quota --json");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cards: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(cards.is_empty());
}

#[test]
fn quota_json_fetches_normalized_cards_caches_and_isolates_auth_failure() {
    use httpmock::prelude::*;

    let server = MockServer::start();
    let ls_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary")
            .header("x-antigravity-csrf-token", "test-csrf");
        then.status(200).json_body(json!({
            "response": {
                "groups": [
                    {
                        "displayName": "Gemini Models",
                        "buckets": [
                            {
                                "displayName": "5h Limit",
                                "window": "5h",
                                "remainingFraction": 0.75,
                                "resetTime": "2026-12-01T00:00:00Z"
                            }
                        ]
                    }
                ]
            }
        }));
    });

    let home = TempHome::new();
    let future_reset = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 7200;
    // Codex session fallback (local-only, no `auth.json`).
    home.put_codex_session(
        "2026/06/06/rollout-2026-06-06T10-00-00-019e4b75-8a4e-7801-a9ed-9723e77e0497.jsonl",
        &format!(
            "{}\n",
            json!({
                "timestamp": "2026-06-06T10:00:00Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "rate_limits": {
                        "limit_id": "codex",
                        "plan_type": "plus",
                        "primary": {
                            "used_percent": 12.5,
                            "window_minutes": 300,
                            "resets_at": future_reset
                        },
                        "secondary": {
                            "used_percent": 4.0,
                            "window_minutes": 10080,
                            "resets_at": future_reset + 86400
                        }
                    }
                }
            })
        ),
    );
    // Invalid Cursor credentials file -> `needs_login: true` without any external HTTP request.
    home.put(".config/cursor/auth.json", r#"{"accessToken":"not-a-jwt"}"#);

    let output = child_cmd(&home)
        .args(["quota", "--json"])
        .env("ANTIGRAVITY_LS_ADDRESS", server.address().to_string())
        .env("ANTIGRAVITY_CSRF_TOKEN", "test-csrf")
        .env_remove("ANTIGRAVITY_APP_DATA_DIR")
        .env_remove("CLOUD_CODE_URL")
        .output()
        .expect("spawn vct quota --json");
    assert!(
        output.status.success(),
        "vct quota --json failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    ls_mock.assert();

    let cards: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    let providers: Vec<&str> = cards
        .iter()
        .map(|c| c["provider"].as_str().unwrap())
        .collect();
    assert_eq!(providers, vec!["codex", "cursor", "antigravity"]);

    let codex = &cards[0];
    assert_eq!(codex["source"], "session_fallback");
    assert_eq!(codex["plan"], "plus");
    assert_eq!(codex["needs_login"], false);
    assert!(codex["error"].is_null());
    assert_eq!(codex["buckets"].as_array().unwrap().len(), 2);
    assert_eq!(codex["buckets"][0]["window"], "5h");
    assert_eq!(codex["buckets"][0]["used_percent"], 12.5);

    let cursor = &cards[1];
    assert_eq!(cursor["needs_login"], true);
    assert!(
        cursor["error"]
            .as_str()
            .is_some_and(|e| e.contains("cursor-agent login"))
    );

    let agy = &cards[2];
    assert_eq!(agy["source"], "api");
    assert!(agy["plan"].is_null());
    assert_eq!(agy["needs_login"], false);
    assert!(agy["error"].is_null());
    assert_eq!(agy["buckets"][0]["label"], "Gemini Models");
    assert_eq!(agy["buckets"][0]["window"], "5h");
    assert!((agy["buckets"][0]["used_percent"].as_f64().unwrap() - 25.0).abs() < 1e-9);
    assert_eq!(agy["buckets"][0]["resets_at"], json!(1_796_083_200));

    // Per-provider caches are persisted, and `config.toml` is never created by `quota --json`.
    assert!(home.home().join(".vct/quota/antigravity.json").exists());
    assert!(home.home().join(".vct/quota/codex.json").exists());
    assert!(!home.home().join(".vct/config.toml").exists());
}
