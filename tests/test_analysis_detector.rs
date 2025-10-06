// Tests for analysis::detector module

use serde_json::json;
use vibe_coding_tracker::analysis::detector::detect_extension_type;
use vibe_coding_tracker::models::ExtensionType;

#[test]
fn test_detect_extension_type_empty_data() {
    let data = vec![];
    let result = detect_extension_type(&data);
    assert!(result.is_err(), "Empty data should return an error");
}

#[test]
fn test_detect_extension_type_claude_code() {
    let data = vec![
        json!({
            "parentUuid": null,
            "sessionId": "test-session",
            "type": "user",
            "message": {"role": "user", "content": "test"}
        }),
        json!({
            "parentUuid": "some-uuid",
            "sessionId": "test-session",
            "type": "assistant"
        }),
    ];

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(
        result,
        ExtensionType::ClaudeCode,
        "Data with parentUuid should be detected as Claude Code"
    );
}

#[test]
fn test_detect_extension_type_codex() {
    let data = vec![
        json!({
            "timestamp": "2025-08-28T12:00:00Z",
            "type": "user_message",
            "content": "test message"
        }),
        json!({
            "timestamp": "2025-08-28T12:00:01Z",
            "type": "assistant_message",
            "content": "response"
        }),
    ];

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(
        result,
        ExtensionType::Codex,
        "Data without parentUuid should be detected as Codex"
    );
}

#[test]
fn test_detect_extension_type_mixed_data() {
    // First record without parentUuid, second with
    let data = vec![
        json!({
            "timestamp": "2025-08-28T12:00:00Z",
            "type": "user"
        }),
        json!({
            "parentUuid": "some-uuid",
            "type": "assistant"
        }),
    ];

    let result = detect_extension_type(&data).unwrap();
    // Should detect as Claude Code because at least one record has parentUuid
    assert_eq!(result, ExtensionType::ClaudeCode);
}

#[test]
fn test_detect_extension_type_single_claude_record() {
    let data = vec![json!({
        "parentUuid": null,
        "sessionId": "test",
        "message": "test"
    })];

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::ClaudeCode);
}

#[test]
fn test_detect_extension_type_non_object_records() {
    // Test with non-object records (should default to Codex)
    let data = vec![json!("string value"), json!(123), json!([1, 2, 3])];

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Codex);
}

#[test]
fn test_detect_extension_type_claude_code_in_large_dataset() {
    // Test that Claude Code is detected even with many records
    // (optimization: only checks first 5 records)
    let mut data = vec![
        json!({"timestamp": "2025-08-28T12:00:00Z", "type": "user"}),
        json!({"timestamp": "2025-08-28T12:00:01Z", "type": "user"}),
        json!({"parentUuid": "some-uuid", "type": "assistant"}), // Claude marker at position 2
    ];

    // Add more records to exceed sample size
    for i in 3..20 {
        data.push(json!({
            "timestamp": format!("2025-08-28T12:00:{:02}Z", i),
            "type": "assistant"
        }));
    }

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(
        result,
        ExtensionType::ClaudeCode,
        "Should detect Claude Code when parentUuid appears within first 5 records"
    );
}

#[test]
fn test_detect_extension_type_optimization_sample_size() {
    // Test that detection works efficiently with large dataset
    // by only checking first few records
    let mut data = vec![];

    // Add 100 records without parentUuid
    for i in 0..100 {
        data.push(json!({
            "timestamp": format!("2025-08-28T12:00:{:02}Z", i % 60),
            "type": "user"
        }));
    }

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(
        result,
        ExtensionType::Codex,
        "Should default to Codex when no Claude markers found in sample"
    );
}

#[test]
fn test_detect_extension_type_gemini_format() {
    // Test Gemini format detection (single session object)
    let data = vec![json!({
        "sessionId": "test-session-123",
        "projectHash": "abc123",
        "messages": [
            {"role": "user", "content": "test"},
            {"role": "assistant", "content": "response"}
        ]
    })];

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(
        result,
        ExtensionType::Gemini,
        "Should detect Gemini format with sessionId, projectHash, and messages"
    );
}
