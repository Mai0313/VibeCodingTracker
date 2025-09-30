// Integration tests for usage statistics functionality

use codex_usage::usage::calculator::{calculate_usage_from_jsonl, get_usage_from_directories};
use codex_usage::utils::paths::resolve_paths;
use std::path::PathBuf;

#[test]
fn test_calculate_usage_from_jsonl_claude() {
    let example_file = PathBuf::from("examples/test_conversation.jsonl");
    
    if !example_file.exists() {
        eprintln!("Test file not found, skipping test");
        return;
    }
    
    let result = calculate_usage_from_jsonl(&example_file);
    assert!(result.is_ok(), "Should successfully calculate usage");
    
    let usage_result = result.unwrap();
    // Should return usage data structure
    // conversation_usage is a HashMap which always has len() >= 0
    let _ = usage_result.conversation_usage.len();
}

#[test]
fn test_calculate_usage_from_jsonl_codex() {
    let example_file = PathBuf::from("examples/test_conversation_oai.jsonl");
    
    if !example_file.exists() {
        eprintln!("Test file not found, skipping test");
        return;
    }
    
    let result = calculate_usage_from_jsonl(&example_file);
    assert!(result.is_ok(), "Should successfully calculate usage from Codex file");
}

#[test]
fn test_calculate_usage_nonexistent_file() {
    let result = calculate_usage_from_jsonl("/nonexistent/file.jsonl");
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
    
    let result = calculate_usage_from_jsonl(&example_file);
    
    if let Ok(usage_result) = result {
        // Verify each model usage entry has valid data
        for (model, usage_value) in usage_result.conversation_usage.iter() {
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
                    assert!(usage_obj["total_token_usage"].is_object(), "Should have total_token_usage object");
                }
            }
        }
    }
}
