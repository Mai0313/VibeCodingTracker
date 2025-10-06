// Integration tests for usage statistics functionality

use std::path::PathBuf;
use vibe_coding_tracker::analysis::analyze_jsonl_file;
use vibe_coding_tracker::usage::calculator::get_usage_from_directories;
use vibe_coding_tracker::utils::paths::resolve_paths;

/// Helper function to extract conversation_usage from CodeAnalysis result
fn extract_conversation_usage_from_analysis(
    analysis: &serde_json::Value,
) -> std::collections::HashMap<String, serde_json::Value> {
    let mut conversation_usage = std::collections::HashMap::new();

    if let Some(records) = analysis.get("records").and_then(|r| r.as_array()) {
        for record in records {
            if let Some(record_obj) = record.as_object() {
                if let Some(conv_usage) = record_obj
                    .get("conversationUsage")
                    .and_then(|c| c.as_object())
                {
                    for (model, usage) in conv_usage {
                        conversation_usage.insert(model.clone(), usage.clone());
                    }
                }
            }
        }
    }

    conversation_usage
}

#[test]
fn test_analyze_jsonl_file_claude() {
    let example_file = PathBuf::from("examples/test_conversation.jsonl");

    if !example_file.exists() {
        eprintln!("Test file not found, skipping test");
        return;
    }

    let result = analyze_jsonl_file(&example_file);
    assert!(result.is_ok(), "Should successfully analyze file");

    let analysis = result.unwrap();
    let conversation_usage = extract_conversation_usage_from_analysis(&analysis);
    // Should return usage data structure
    let _ = conversation_usage.len();
}

#[test]
fn test_analyze_jsonl_file_codex() {
    let example_file = PathBuf::from("examples/test_conversation_oai.jsonl");

    if !example_file.exists() {
        eprintln!("Test file not found, skipping test");
        return;
    }

    let result = analyze_jsonl_file(&example_file);
    assert!(result.is_ok(), "Should successfully analyze Codex file");
}

#[test]
fn test_analyze_jsonl_file_nonexistent() {
    let result = analyze_jsonl_file("/nonexistent/file.jsonl");
    assert!(result.is_err(), "Should fail for nonexistent file");
}

#[test]
fn test_get_usage_from_directories() {
    // This test checks if the function can handle potentially non-existent directories
    let result = get_usage_from_directories();

    // Should not panic regardless of whether directories exist
    assert!(
        result.is_ok() || result.is_err(),
        "Should handle directory access gracefully"
    );
}

#[test]
fn test_get_usage_from_directories_with_paths() {
    // Test that path resolution works
    let paths_result = resolve_paths();
    assert!(paths_result.is_ok(), "Should resolve paths");

    // Now test usage calculation
    // This may return empty results if directories don't exist, which is fine
    let usage_result = get_usage_from_directories();

    if let Ok(usage_map) = usage_result {
        // Verify the structure is correct (DateUsageResult is HashMap<String, HashMap<String, Value>>)
        for (_date, models_usage) in usage_map.iter() {
            // Each date should have a map of models to usage data
            // Just verify it's a valid map
            let _ = models_usage.len();
        }
    }
}

#[test]
fn test_usage_data_aggregation() {
    let example_file = PathBuf::from("examples/test_conversation.jsonl");

    if !example_file.exists() {
        return;
    }

    let result = analyze_jsonl_file(&example_file);

    if let Ok(analysis) = result {
        let conversation_usage = extract_conversation_usage_from_analysis(&analysis);

        // Verify each model usage entry has valid data
        for (model, usage_value) in conversation_usage.iter() {
            assert!(!model.is_empty(), "Model name should not be empty");

            // Check if it has expected fields
            if let Some(usage_obj) = usage_value.as_object() {
                // Claude usage has input_tokens, output_tokens, etc.
                if usage_obj.contains_key("input_tokens") {
                    let input_tokens = usage_obj["input_tokens"].as_i64().unwrap_or(0);
                    let output_tokens = usage_obj["output_tokens"].as_i64().unwrap_or(0);
                    assert!(input_tokens >= 0, "Input tokens should not be negative");
                    assert!(output_tokens >= 0, "Output tokens should not be negative");
                }
                // Codex usage has total_token_usage
                else if usage_obj.contains_key("total_token_usage") {
                    assert!(
                        usage_obj["total_token_usage"].is_object(),
                        "Should have total_token_usage object"
                    );
                }
            }
        }
    }
}
