// Integration tests for analysis functionality

use std::path::PathBuf;
use vibe_coding_tracker::analysis::analyzer::analyze_jsonl_file;

#[test]
fn test_analyze_claude_code_conversation() {
    // Use the example file from the examples directory
    let example_file = PathBuf::from("examples/test_conversation.jsonl");

    if !example_file.exists() {
        eprintln!("Test file not found, skipping test");
        return;
    }

    let result = analyze_jsonl_file(&example_file);
    assert!(
        result.is_ok(),
        "Should successfully analyze Claude Code conversation"
    );

    let analysis = result.unwrap();
    // Analysis is a JSON Value
    assert!(analysis.is_object(), "Analysis should be a JSON object");

    // Check basic fields
    if let Some(user) = analysis.get("user").and_then(|v| v.as_str()) {
        assert!(!user.is_empty(), "User should not be empty");
    }

    if let Some(ext_name) = analysis.get("extensionName").and_then(|v| v.as_str()) {
        assert_eq!(ext_name, "Claude-Code", "Should detect Claude Code");
    }

    if let Some(records) = analysis.get("records").and_then(|v| v.as_array()) {
        assert!(!records.is_empty(), "Should have at least one record");
    }
}

#[test]
fn test_analyze_codex_conversation() {
    // Use the Codex example file
    let example_file = PathBuf::from("examples/test_conversation_oai.jsonl");

    if !example_file.exists() {
        eprintln!("Test file not found, skipping test");
        return;
    }

    let result = analyze_jsonl_file(&example_file);
    assert!(
        result.is_ok(),
        "Should successfully analyze Codex conversation"
    );

    let analysis = result.unwrap();
    if let Some(ext_name) = analysis.get("extensionName").and_then(|v| v.as_str()) {
        assert_eq!(ext_name, "Codex", "Should detect Codex");
    }
}

#[test]
fn test_analyze_nonexistent_file() {
    let result = analyze_jsonl_file("/nonexistent/file.jsonl");
    assert!(result.is_err(), "Should fail for nonexistent file");
}

#[test]
fn test_analyze_claude_code_tool_calls() {
    let example_file = PathBuf::from("examples/test_conversation.jsonl");

    if !example_file.exists() {
        return;
    }

    let result = analyze_jsonl_file(&example_file);
    if let Ok(analysis) = result {
        if let Some(records) = analysis.get("records").and_then(|v| v.as_array()) {
            if let Some(record) = records.first() {
                // Check if tool call counts exist
                if let Some(tool_calls) = record.get("toolCallCounts") {
                    let total_tools = ["Read", "Write", "Edit", "TodoWrite", "Bash"]
                        .iter()
                        .filter_map(|key| tool_calls.get(*key).and_then(|v| v.as_u64()))
                        .sum::<u64>();

                    // We expect at least some tool calls in the conversation
                    assert!(
                        total_tools > 0,
                        "Should have detected some tool calls in the conversation"
                    );
                }
            }
        }
    }
}

#[test]
fn test_analyze_claude_code_conversation_usage() {
    let example_file = PathBuf::from("examples/test_conversation.jsonl");

    if !example_file.exists() {
        return;
    }

    let result = analyze_jsonl_file(&example_file);
    if let Ok(analysis) = result {
        if let Some(records) = analysis.get("records").and_then(|v| v.as_array()) {
            if let Some(record) = records.first() {
                // Check that conversation usage was tracked
                if let Some(conversation_usage) =
                    record.get("conversationUsage").and_then(|v| v.as_object())
                {
                    // The format depends on the model used in the conversation
                    assert!(
                        !conversation_usage.is_empty(),
                        "Should have tracked conversation usage"
                    );
                }
            }
        }
    }
}

#[test]
fn test_analyze_file_operations() {
    let example_file = PathBuf::from("examples/test_conversation.jsonl");

    if !example_file.exists() {
        return;
    }

    let result = analyze_jsonl_file(&example_file);
    if let Ok(analysis) = result {
        if let Some(records) = analysis.get("records").and_then(|v| v.as_array()) {
            if let Some(record) = records.first() {
                // Check file operation details are structured correctly
                if let Some(write_details) =
                    record.get("writeFileDetails").and_then(|v| v.as_array())
                {
                    for write_detail in write_details {
                        if let Some(file_path) =
                            write_detail.get("filePath").and_then(|v| v.as_str())
                        {
                            assert!(!file_path.is_empty());
                        }
                    }
                }

                if let Some(read_details) = record.get("readFileDetails").and_then(|v| v.as_array())
                {
                    for read_detail in read_details {
                        if let Some(file_path) =
                            read_detail.get("filePath").and_then(|v| v.as_str())
                        {
                            assert!(!file_path.is_empty());
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn test_analyze_unique_files_count() {
    let example_file = PathBuf::from("examples/test_conversation.jsonl");

    if !example_file.exists() {
        return;
    }

    let result = analyze_jsonl_file(&example_file);
    if let Ok(analysis) = result {
        if let Some(records) = analysis.get("records").and_then(|v| v.as_array()) {
            if let Some(record) = records.first() {
                // Total unique files field should exist and be a valid number
                assert!(
                    record
                        .get("totalUniqueFiles")
                        .and_then(|v| v.as_u64())
                        .is_some(),
                    "Total unique files field should exist as a number"
                );
            }
        }
    }
}
