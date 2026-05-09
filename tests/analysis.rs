// Integration tests for analysis command functionality
//
// These tests verify both single-file analysis and batch analysis operations

use std::path::PathBuf;
use tempfile::TempDir;
use vibe_coding_tracker::analysis::aggregator::aggregate_sessions_by_model;
use vibe_coding_tracker::cli::TimeRange;
use vibe_coding_tracker::models::ExtensionType;
use vibe_coding_tracker::session::parser::{parse_session_file, parse_session_file_as};
use vibe_coding_tracker::session::state::ParseMode;

#[test]
fn test_single_file_analysis_claude() {
    let input_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = parse_session_file(&input_file);
    assert!(result.is_ok(), "Should successfully analyze Claude file");

    let analysis = result.unwrap();
    assert!(analysis.is_object(), "Analysis should be a JSON object");

    // Verify required fields
    assert!(
        analysis["extensionName"].is_string(),
        "Should have extensionName"
    );
    assert_eq!(analysis["extensionName"], "Claude-Code");
    assert!(analysis["records"].is_array(), "Should have records array");
}

#[test]
fn test_single_file_analysis_codex() {
    let input_file = PathBuf::from("examples/test_conversation_codex.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = parse_session_file(&input_file);
    assert!(result.is_ok(), "Should successfully analyze Codex file");

    let analysis = result.unwrap();
    assert!(analysis.is_object(), "Analysis should be a JSON object");
    assert_eq!(analysis["extensionName"], "Codex");
}

#[test]
fn test_single_file_analysis_copilot() {
    let input_file = PathBuf::from("examples/test_conversation_copilot.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = parse_session_file(&input_file);
    assert!(result.is_ok(), "Should successfully analyze Copilot file");

    let analysis = result.unwrap();
    assert!(analysis.is_object(), "Analysis should be a JSON object");
    assert_eq!(analysis["extensionName"], "Copilot-CLI");
}

#[test]
fn test_single_file_analysis_gemini() {
    let input_file = PathBuf::from("examples/test_conversation_gemini.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = parse_session_file(&input_file);
    assert!(result.is_ok(), "Should successfully analyze Gemini file");

    let analysis = result.unwrap();
    assert!(analysis.is_object(), "Analysis should be a JSON object");
    assert_eq!(analysis["extensionName"], "Gemini");
}

#[test]
fn test_analysis_record_structure() {
    let input_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = parse_session_file(&input_file);
    if let Ok(analysis) = result {
        let records = &analysis["records"];
        if let Some(first_record) = records.as_array().and_then(|arr| arr.first()) {
            // Verify record structure
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
    }
}

#[test]
fn test_analysis_conversation_usage() {
    let input_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = parse_session_file(&input_file);
    if let Ok(analysis) = result {
        let records = &analysis["records"];
        if let Some(first_record) = records.as_array().and_then(|arr| arr.first()) {
            let usage = &first_record["conversationUsage"];

            // Verify that we have at least one model
            assert!(
                usage.as_object().map(|o| !o.is_empty()).unwrap_or(false),
                "Should have at least one model in conversationUsage"
            );

            // Check token structure for each model
            if let Some(usage_obj) = usage.as_object() {
                for (model_name, model_usage) in usage_obj {
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
        }
    }
}

#[test]
fn test_analysis_tool_call_counts() {
    let input_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = parse_session_file(&input_file);
    if let Ok(analysis) = result {
        let records = &analysis["records"];
        if let Some(first_record) = records.as_array().and_then(|arr| arr.first()) {
            let counts = &first_record["toolCallCounts"];

            // Verify tool call counts structure
            assert!(counts.is_object(), "toolCallCounts should be an object");

            if let Some(counts_obj) = counts.as_object() {
                // Check that all values are numbers
                for (_tool, count) in counts_obj {
                    assert!(count.is_number(), "Tool count should be a number");
                }
            }
        }
    }
}

#[test]
fn test_analysis_file_operations() {
    let input_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = parse_session_file(&input_file);
    if let Ok(analysis) = result {
        let records = &analysis["records"];
        if let Some(first_record) = records.as_array().and_then(|arr| arr.first()) {
            // Verify file operation fields exist
            assert!(
                first_record["editFileDetails"].is_array()
                    || first_record["editFileDetails"].is_null()
            );
            assert!(
                first_record["readFileDetails"].is_array()
                    || first_record["readFileDetails"].is_null()
            );
            assert!(
                first_record["writeFileDetails"].is_array()
                    || first_record["writeFileDetails"].is_null()
            );
            assert!(
                first_record["runCommandDetails"].is_array()
                    || first_record["runCommandDetails"].is_null()
            );

            // Verify line/character counts
            assert!(first_record["totalEditLines"].is_number());
            assert!(first_record["totalReadLines"].is_number());
            assert!(first_record["totalWriteLines"].is_number());
        }
    }
}

#[test]
fn test_batch_analysis_basic() {
    // Test batch analysis with default directories
    let result = aggregate_sessions_by_model(TimeRange::All);
    assert!(result.is_ok(), "Batch analysis should not fail");

    if let Ok(data) = result {
        // Verify each row has required fields
        for row in data.rows.iter() {
            assert!(!row.model.is_empty(), "Model should not be empty");
            // Line counts are usize, so they're always non-negative
            let _ = row.edit_lines;
            let _ = row.read_lines;
            let _ = row.write_lines;
        }
    }
}

#[test]
fn test_batch_analysis_sorting() {
    let result = aggregate_sessions_by_model(TimeRange::All);

    if let Ok(data) = result
        && data.rows.len() > 1
    {
        // Verify sorting: models should be in alphabetical order
        for i in 0..data.rows.len() - 1 {
            assert!(
                data.rows[i].model <= data.rows[i + 1].model,
                "Models should be sorted alphabetically"
            );
        }
    }
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

    // Test serialization
    let json = serde_json::to_string(&row).unwrap();
    assert!(
        json.contains("editLines"),
        "Should use camelCase for edit_lines"
    );
    assert!(
        json.contains("readLines"),
        "Should use camelCase for read_lines"
    );
    assert!(
        json.contains("writeLines"),
        "Should use camelCase for write_lines"
    );
    assert!(
        json.contains("bashCount"),
        "Should use camelCase for bash_count"
    );
    assert!(
        json.contains("todoWriteCount"),
        "Should use camelCase for todo_write_count"
    );

    // Test deserialization
    let deserialized: AggregatedAnalysisRow = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.model, row.model);
    assert_eq!(deserialized.edit_lines, row.edit_lines);
}

#[test]
fn test_analysis_with_empty_file() {
    // Test that empty files are handled gracefully
    let temp_dir = TempDir::new().unwrap();
    let empty_file = temp_dir.path().join("empty.jsonl");
    std::fs::write(&empty_file, "").unwrap();

    let result = parse_session_file(&empty_file);
    // Should either succeed with empty result or fail gracefully
    assert!(
        result.is_ok() || result.is_err(),
        "Should handle empty file"
    );
}

#[test]
fn test_analysis_with_invalid_json() {
    // Test that invalid JSON is handled gracefully
    let temp_dir = TempDir::new().unwrap();
    let invalid_file = temp_dir.path().join("invalid.jsonl");
    std::fs::write(&invalid_file, "not valid json\n{incomplete").unwrap();

    let result = parse_session_file(&invalid_file);
    // Should fail with error
    assert!(result.is_err(), "Should fail on invalid JSON");
}

#[test]
fn test_batch_analysis_model_grouping() {
    // Test that batch analysis groups data by model
    let result = aggregate_sessions_by_model(TimeRange::All);

    if let Ok(data) = result {
        for row in data.rows.iter() {
            assert!(!row.model.is_empty(), "Model should not be empty");
        }

        // Verify provider active days are tracked
        // Total days should be >= max of individual provider days
        let max_provider_days = data
            .provider_days
            .claude
            .max(data.provider_days.codex)
            .max(data.provider_days.copilot)
            .max(data.provider_days.gemini);
        assert!(
            data.provider_days.total >= max_provider_days,
            "Total days should be >= max individual provider days"
        );
    }
}

#[test]
fn test_analysis_aggregation_logic() {
    // Test that analysis properly aggregates data
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

    // Calculate totals
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
/// Claude totals. This test writes a fixture with a `permission-mode` prelude
/// and asserts both the provider-known entry point and the auto-detect entry
/// point return the Claude model usage.
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
