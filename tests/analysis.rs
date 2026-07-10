// Integration tests for analysis functionality.
//
// Single-file parsing reads the in-repo `examples/` fixtures via
// `common::fixture` (an absolute, machine-stable path). Batch aggregation drives
// `aggregate_sessions_by_model_from_paths` against a `TempHome`, so it reads no
// real machine session directories and mutates no environment.

mod common;

use common::{TempHome, fixture, fixture_str};
use tempfile::TempDir;
use vibe_coding_tracker::analysis::aggregator::{
    aggregate_sessions_by_model_from_paths, aggregate_sessions_by_model_from_paths_with,
};
use vibe_coding_tracker::cli::TimeRange;
use vibe_coding_tracker::config::ProvidersConfig;
use vibe_coding_tracker::models::ExtensionType;
use vibe_coding_tracker::session::parser::{parse_session_file, parse_session_file_as};
use vibe_coding_tracker::session::state::ParseMode;

#[test]
fn test_single_file_analysis_claude() {
    let analysis = parse_session_file(fixture("test_conversation_claude_code.jsonl"))
        .expect("should successfully analyze Claude file");

    assert!(analysis.is_object(), "Analysis should be a JSON object");
    assert_eq!(analysis["extensionName"], "Claude-Code");
    assert!(analysis["records"].is_array(), "Should have records array");
}

#[test]
fn test_single_file_analysis_codex() {
    let analysis = parse_session_file(fixture("test_conversation_codex.jsonl"))
        .expect("should successfully analyze Codex file");
    assert_eq!(analysis["extensionName"], "Codex");
}

#[test]
fn test_single_file_analysis_copilot() {
    let analysis = parse_session_file(fixture("test_conversation_copilot.jsonl"))
        .expect("should successfully analyze Copilot file");
    assert_eq!(analysis["extensionName"], "Copilot-CLI");
}

#[test]
fn test_single_file_analysis_gemini() {
    let analysis = parse_session_file(fixture("test_conversation_gemini.jsonl"))
        .expect("should successfully analyze Gemini file");
    assert_eq!(analysis["extensionName"], "Gemini");
}

#[test]
fn test_analysis_record_structure() {
    let analysis = parse_session_file(fixture("test_conversation_claude_code.jsonl")).unwrap();
    let records = &analysis["records"];
    let first_record = records
        .as_array()
        .and_then(|arr| arr.first())
        .expect("fixture has at least one record");

    assert!(
        first_record["conversationUsage"].is_object(),
        "Should have conversationUsage"
    );
    assert!(
        first_record["toolCallCounts"].is_object(),
        "Should have toolCallCounts"
    );
    assert!(first_record["taskId"].is_string(), "Should have taskId");
    assert!(
        first_record["timestamp"].is_number(),
        "Should have timestamp"
    );
}

#[test]
fn test_analysis_conversation_usage() {
    let analysis = parse_session_file(fixture("test_conversation_claude_code.jsonl")).unwrap();
    let records = &analysis["records"];
    let first_record = records.as_array().and_then(|arr| arr.first()).unwrap();
    let usage = &first_record["conversationUsage"];

    assert!(
        usage.as_object().map(|o| !o.is_empty()).unwrap_or(false),
        "Should have at least one model in conversationUsage"
    );

    for (model_name, model_usage) in usage.as_object().unwrap() {
        assert!(!model_name.is_empty(), "Model name should not be empty");
        assert!(
            model_usage["input_tokens"].is_number(),
            "Should have input_tokens"
        );
        assert!(
            model_usage["output_tokens"].is_number(),
            "Should have output_tokens"
        );
    }
}

#[test]
fn test_analysis_tool_call_counts() {
    let analysis = parse_session_file(fixture("test_conversation_claude_code.jsonl")).unwrap();
    let records = &analysis["records"];
    let first_record = records.as_array().and_then(|arr| arr.first()).unwrap();
    let counts = &first_record["toolCallCounts"];

    assert!(counts.is_object(), "toolCallCounts should be an object");
    for (_tool, count) in counts.as_object().unwrap() {
        assert!(count.is_number(), "Tool count should be a number");
    }
}

#[test]
fn test_analysis_file_operations() {
    let analysis = parse_session_file(fixture("test_conversation_claude_code.jsonl")).unwrap();
    let records = &analysis["records"];
    let first_record = records.as_array().and_then(|arr| arr.first()).unwrap();

    assert!(
        first_record["editFileDetails"].is_array() || first_record["editFileDetails"].is_null()
    );
    assert!(
        first_record["readFileDetails"].is_array() || first_record["readFileDetails"].is_null()
    );
    assert!(
        first_record["writeFileDetails"].is_array() || first_record["writeFileDetails"].is_null()
    );
    assert!(
        first_record["runCommandDetails"].is_array() || first_record["runCommandDetails"].is_null()
    );

    assert!(first_record["totalEditLines"].is_number());
    assert!(first_record["totalReadLines"].is_number());
    assert!(first_record["totalWriteLines"].is_number());
}

#[test]
fn disabled_provider_is_dropped_from_analysis_rollup() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "session.jsonl",
        &fixture_str("test_conversation_claude_code.jsonl"),
    );
    home.put_gemini_session(
        "proj-hash",
        "chat.jsonl",
        &fixture_str("test_conversation_gemini.jsonl"),
    );

    // Turn Gemini off in `[providers]`: it must be skipped entirely.
    let providers = ProvidersConfig {
        gemini: false,
        ..ProvidersConfig::default()
    };
    let data = aggregate_sessions_by_model_from_paths_with(&home.paths, TimeRange::All, providers)
        .expect("aggregate with gemini disabled");

    assert!(
        data.rows
            .iter()
            .any(|r| r.model == "claude-sonnet-4-20250514"),
        "the enabled Claude provider is still aggregated"
    );
    assert!(
        !data.rows.iter().any(|r| r.model.starts_with("gemini-3")),
        "the disabled Gemini provider must not appear, got: {:?}",
        data.rows.iter().map(|r| &r.model).collect::<Vec<_>>()
    );
}

#[test]
fn batch_analysis_from_paths_groups_by_model() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "session.jsonl",
        &fixture_str("test_conversation_claude_code.jsonl"),
    );
    home.put_gemini_session(
        "proj-hash",
        "chat.jsonl",
        &fixture_str("test_conversation_gemini.jsonl"),
    );

    let data = aggregate_sessions_by_model_from_paths(&home.paths, TimeRange::All)
        .expect("batch aggregation should succeed");

    // Every row has a non-empty model name and rows are sorted.
    for row in &data.rows {
        assert!(!row.model.is_empty(), "Model should not be empty");
    }
    for i in 1..data.rows.len() {
        assert!(
            data.rows[i - 1].model <= data.rows[i].model,
            "Models should be sorted alphabetically"
        );
    }

    // The Claude fixture's model is grouped and attributed to the Claude bucket.
    assert!(
        data.rows
            .iter()
            .any(|r| r.model == "claude-sonnet-4-20250514"),
        "Claude fixture model should have a row, got: {:?}",
        data.rows.iter().map(|r| &r.model).collect::<Vec<_>>()
    );
    assert!(
        data.per_provider
            .claude
            .iter()
            .any(|r| r.model == "claude-sonnet-4-20250514")
    );
    assert!(
        data.per_provider
            .gemini
            .iter()
            .any(|r| r.model.starts_with("gemini-3"))
    );

    let max_provider_days = data
        .provider_days
        .claude
        .max(data.provider_days.codex)
        .max(data.provider_days.copilot)
        .max(data.provider_days.gemini);
    assert!(data.provider_days.total >= max_provider_days);
    assert!(data.provider_days.claude >= 1 && data.provider_days.gemini >= 1);
}

#[test]
fn batch_analysis_from_empty_paths_is_empty() {
    let home = TempHome::new();
    let data = aggregate_sessions_by_model_from_paths(&home.paths, TimeRange::All).unwrap();
    assert!(data.rows.is_empty(), "no sessions -> no rows");
    assert_eq!(data.provider_days.total, 0);
}

#[test]
fn test_batch_analysis_serialization() {
    use vibe_coding_tracker::analysis::aggregator::AggregatedAnalysisRow;

    let row = AggregatedAnalysisRow {
        model: "claude-sonnet-4".to_string(),
        edit_lines: 100,
        read_lines: 200,
        write_lines: 50,
        bash_count: 10,
        edit_count: 20,
        read_count: 30,
        todo_write_count: 5,
        write_count: 8,
    };

    let json = serde_json::to_string(&row).unwrap();
    assert!(json.contains("editLines"));
    assert!(json.contains("readLines"));
    assert!(json.contains("writeLines"));
    assert!(json.contains("bashCount"));
    assert!(json.contains("todoWriteCount"));

    let deserialized: AggregatedAnalysisRow = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.model, row.model);
    assert_eq!(deserialized.edit_lines, row.edit_lines);
}

#[test]
fn test_analysis_with_empty_file() {
    let temp_dir = TempDir::new().unwrap();
    let empty_file = temp_dir.path().join("empty.jsonl");
    std::fs::write(&empty_file, "").unwrap();

    let result = parse_session_file(&empty_file);
    assert!(
        result.is_ok() || result.is_err(),
        "Should handle empty file without panicking"
    );
}

#[test]
fn test_analysis_with_invalid_json() {
    let temp_dir = TempDir::new().unwrap();
    let invalid_file = temp_dir.path().join("invalid.jsonl");
    std::fs::write(&invalid_file, "not valid json\n{incomplete").unwrap();

    let result = parse_session_file(&invalid_file);
    assert!(result.is_err(), "Should fail on invalid JSON");
}

#[test]
fn test_analysis_aggregation_logic() {
    use vibe_coding_tracker::analysis::aggregator::AggregatedAnalysisRow;

    let rows = [
        AggregatedAnalysisRow {
            model: "claude-sonnet-4".to_string(),
            edit_lines: 50,
            read_lines: 100,
            write_lines: 25,
            bash_count: 5,
            edit_count: 10,
            read_count: 15,
            todo_write_count: 2,
            write_count: 3,
        },
        AggregatedAnalysisRow {
            model: "claude-sonnet-4".to_string(),
            edit_lines: 50,
            read_lines: 100,
            write_lines: 25,
            bash_count: 5,
            edit_count: 10,
            read_count: 15,
            todo_write_count: 3,
            write_count: 5,
        },
    ];

    let total_edit_lines: usize = rows.iter().map(|r| r.edit_lines).sum();
    let total_read_lines: usize = rows.iter().map(|r| r.read_lines).sum();
    let total_write_lines: usize = rows.iter().map(|r| r.write_lines).sum();

    assert_eq!(total_edit_lines, 100);
    assert_eq!(total_read_lines, 200);
    assert_eq!(total_write_lines, 50);
}

/// Regression for the silent usage drop that happened when a Claude session
/// started with a metadata sentinel (`permission-mode`, `file-history-snapshot`,
/// `queue-operation`). Those records don't carry `parentUuid`, so the old
/// streaming detector — which only looked at the first line — classified the
/// whole file as Codex and the assistant `usage` entries never landed in the
/// Claude totals. This test writes a fixture with such a prelude and asserts both
/// the provider-known entry point and the auto-detect entry point return the
/// Claude model usage.
fn write_claude_fixture_with_sentinel_prelude(path: &std::path::Path, sentinel_type: &str) {
    let sentinel = match sentinel_type {
        "permission-mode" => {
            r#"{"type":"permission-mode","permissionMode":"default","sessionId":"sess-1"}"#
        }
        "file-history-snapshot" => {
            r#"{"type":"file-history-snapshot","messageId":"m1","isSnapshotUpdate":false,"snapshot":{}}"#
        }
        "queue-operation" => {
            r#"{"type":"queue-operation","operation":"enqueue","sessionId":"sess-1","content":"x","timestamp":"2026-04-23T00:00:00.000Z"}"#
        }
        _ => unreachable!(),
    };

    // Minimal assistant message with the fields the analyzer reads:
    // model + usage. No <synthetic> — those are intentionally skipped.
    let assistant = r#"{"type":"assistant","sessionId":"sess-1","parentUuid":"pu","timestamp":"2026-04-23T00:00:00.000Z","message":{"model":"claude-opus-4-7","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":10,"cache_read_input_tokens":20,"service_tier":"standard","cache_creation":{"ephemeral_5m_input_tokens":10}},"content":[]}}"#;

    let body = format!("{sentinel}\n{assistant}\n");
    std::fs::write(path, body).unwrap();
}

fn usage_input_tokens_for_model(analysis: &serde_json::Value, model: &str) -> i64 {
    analysis["records"]
        .as_array()
        .and_then(|records| records.first())
        .and_then(|r| r.get("conversationUsage"))
        .and_then(|cu| cu.get(model))
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(-1)
}

#[test]
fn test_provider_known_extracts_usage_when_first_line_is_permission_mode() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("session.jsonl");
    write_claude_fixture_with_sentinel_prelude(&file, "permission-mode");

    let analysis = parse_session_file_as(&file, ExtensionType::ClaudeCode, ParseMode::UsageOnly)
        .expect("provider-known path should accept the sentinel prelude");

    assert_eq!(analysis.extension_name, "Claude-Code");
    assert_eq!(analysis.records.len(), 1);

    let record = &analysis.records[0];
    let usage = record
        .conversation_usage
        .get("claude-opus-4-7")
        .expect("claude-opus-4-7 usage should be recorded despite the permission-mode prelude");
    assert_eq!(usage["input_tokens"], 100);
    assert_eq!(usage["output_tokens"], 50);
    assert_eq!(usage["cache_creation_input_tokens"], 10);
    assert_eq!(usage["cache_read_input_tokens"], 20);
}

#[test]
fn test_provider_known_extracts_usage_when_first_line_is_file_history_snapshot() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("session.jsonl");
    write_claude_fixture_with_sentinel_prelude(&file, "file-history-snapshot");

    let analysis = parse_session_file_as(&file, ExtensionType::ClaudeCode, ParseMode::UsageOnly)
        .expect("provider-known path should accept the sentinel prelude");

    let record = &analysis.records[0];
    assert!(
        record.conversation_usage.contains_key("claude-opus-4-7"),
        "claude-opus-4-7 usage should be recorded even when first line is file-history-snapshot"
    );
}

#[test]
fn test_provider_known_extracts_usage_when_first_line_is_queue_operation() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("session.jsonl");
    write_claude_fixture_with_sentinel_prelude(&file, "queue-operation");

    let analysis = parse_session_file_as(&file, ExtensionType::ClaudeCode, ParseMode::UsageOnly)
        .expect("provider-known path should accept the sentinel prelude");

    let record = &analysis.records[0];
    assert!(
        record.conversation_usage.contains_key("claude-opus-4-7"),
        "claude-opus-4-7 usage should be recorded even when first line is queue-operation"
    );
}

#[test]
fn test_autodetect_sees_past_queue_operation_prelude() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("session.jsonl");
    write_claude_fixture_with_sentinel_prelude(&file, "queue-operation");

    let analysis = parse_session_file(&file).expect("auto-detect should handle the prelude");
    assert_eq!(analysis["extensionName"], "Claude-Code");
    assert_eq!(
        usage_input_tokens_for_model(&analysis, "claude-opus-4-7"),
        100,
    );
}

#[test]
fn test_autodetect_sees_past_sentinel_prelude() {
    // The auto-detect path (used by the CLI `vct analysis <file>` form) should
    // peek enough records to spot the Claude-shaped assistant row sitting
    // behind the metadata preamble.
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("session.jsonl");
    write_claude_fixture_with_sentinel_prelude(&file, "permission-mode");

    let analysis = parse_session_file(&file).expect("auto-detect should handle the prelude");

    assert_eq!(analysis["extensionName"], "Claude-Code");
    assert_eq!(
        usage_input_tokens_for_model(&analysis, "claude-opus-4-7"),
        100,
        "auto-detect should extract the assistant record's usage, not drop the whole file"
    );
}
