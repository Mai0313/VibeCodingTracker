// Unit tests for analysis/detector.rs
//
// Tests the AI provider format detection logic

use vibe_coding_tracker::models::ExtensionType;
use vibe_coding_tracker::analysis::detector::detect_extension_type;
use serde_json::{json, Value};

#[test]
fn test_detect_gemini_format() {
    // Test Gemini format detection with sessionId, projectHash, and messages
    let data = vec![json!({
        "sessionId": "test-session",
        "projectHash": "abc123",
        "messages": []
    })];
    
    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Gemini);
}

#[test]
fn test_detect_copilot_format() {
    // Test Copilot format detection with sessionId, startTime, and timeline
    let data = vec![json!({
        "sessionId": "test-session",
        "startTime": 1234567890,
        "timeline": []
    })];
    
    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Copilot);
}

#[test]
fn test_detect_claude_code_format() {
    // Test Claude Code format detection with parentUuid field
    let data = vec![
        json!({
            "parentUuid": "parent-uuid",
            "type": "assistant_message",
            "content": "test"
        }),
        json!({
            "parentUuid": "parent-uuid-2",
            "type": "user_message"
        })
    ];
    
    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::ClaudeCode);
}

#[test]
fn test_detect_codex_format_default() {
    // Test Codex format detection (default when no distinctive markers found)
    let data = vec![
        json!({
            "timestamp": 1234567890,
            "model": "gpt-4",
            "usage": {}
        })
    ];
    
    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Codex);
}

#[test]
fn test_detect_claude_code_in_first_few_records() {
    // Test that detection works within first 5 records
    let mut data = vec![
        json!({"field": "value1"}),
        json!({"field": "value2"}),
    ];
    
    // Add Claude marker in third record
    data.push(json!({
        "parentUuid": "test-uuid",
        "content": "test"
    }));
    
    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::ClaudeCode);
}

#[test]
fn test_detect_empty_data_error() {
    // Test that empty data returns an error
    let data: Vec<Value> = vec![];
    
    let result = detect_extension_type(&data);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("empty data"));
}

#[test]
fn test_detect_multiple_objects_without_markers() {
    // Test that multiple objects without distinctive markers default to Codex
    let data = vec![
        json!({"timestamp": 123}),
        json!({"model": "gpt-4"}),
        json!({"usage": {}}),
    ];
    
    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Codex);
}

#[test]
fn test_detect_gemini_with_extra_fields() {
    // Test that Gemini detection works even with extra fields
    let data = vec![json!({
        "sessionId": "test",
        "projectHash": "hash",
        "messages": [],
        "extraField": "extra"
    })];
    
    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Gemini);
}

#[test]
fn test_detect_copilot_with_extra_fields() {
    // Test that Copilot detection works even with extra fields
    let data = vec![json!({
        "sessionId": "test",
        "startTime": 123,
        "timeline": [],
        "extraField": "extra"
    })];
    
    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Copilot);
}

#[test]
fn test_detect_partial_gemini_fields() {
    // Test that partial Gemini fields don't trigger false positive
    let data = vec![json!({
        "sessionId": "test",
        "projectHash": "hash"
        // missing "messages" field
    })];
    
    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Codex); // Should default to Codex
}

#[test]
fn test_detect_partial_copilot_fields() {
    // Test that partial Copilot fields don't trigger false positive
    let data = vec![json!({
        "sessionId": "test",
        "startTime": 123
        // missing "timeline" field
    })];
    
    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Codex); // Should default to Codex
}

