// Unit tests for analysis/detector.rs
//
// Tests the AI provider format detection logic

use serde_json::{Value, json};
use vibe_coding_tracker::analysis::detector::detect_extension_type;
use vibe_coding_tracker::models::ExtensionType;

#[test]
fn test_detect_gemini_format() {
    // Legacy single-object export with sessionId, projectHash, and messages
    let data = vec![json!({
        "sessionId": "test-session",
        "projectHash": "abc123",
        "messages": []
    })];

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Gemini);
}

#[test]
fn test_detect_gemini_jsonl_meta_header() {
    // Modern Gemini CLI writes one event per line under `chats/`. The first
    // line is a pure session-meta record tagged with `kind` (no `messages`
    // array) — the detector must recognise it even when further event lines
    // follow in the same slice.
    let data = vec![
        json!({
            "sessionId": "0ab84937-9fe7-4284-986a-33c832af0b6a",
            "projectHash": "9da8b3dfb8655182ac1f0e66601c367e34f8d18447a29759eeba4d7e45dc60ea",
            "startTime": "2026-04-23T12:52:52.759Z",
            "lastUpdated": "2026-04-23T12:52:52.759Z",
            "kind": "main"
        }),
        json!({
            "id": "0cf1a565-3230-4426-bdfc-d4d7af19f867",
            "timestamp": "2026-04-23T12:53:02.597Z",
            "type": "info",
            "content": "Empty GEMINI.md created."
        }),
        json!({
            "id": "8828dd6a-d778-464f-8160-eb2e1604a122",
            "timestamp": "2026-04-23T12:53:05.283Z",
            "type": "gemini",
            "model": "gemini-3-flash-preview",
            "tokens": {
                "input": 13906,
                "output": 185,
                "cached": 0,
                "thoughts": 306,
                "tool": 0,
                "total": 14397
            }
        }),
    ];

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
        }),
    ];

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::ClaudeCode);
}

#[test]
fn test_detect_codex_format_default() {
    // Test Codex format detection (default when no distinctive markers found)
    let data = vec![json!({
        "timestamp": 1234567890,
        "model": "gpt-4",
        "usage": {}
    })];

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Codex);
}

#[test]
fn test_detect_claude_code_in_first_few_records() {
    // Test that detection works within first 5 records
    let mut data = vec![json!({"field": "value1"}), json!({"field": "value2"})];

    // Add Claude marker in third record
    data.push(json!({
        "parentUuid": "test-uuid",
        "content": "test"
    }));

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::ClaudeCode);
}

#[test]
fn test_detect_claude_code_past_fifth_record() {
    // Regression: the streaming auto-detect path buffers up to 8 records and
    // hands them all to `detect_extension_type`. Earlier versions capped the
    // scan at 5 records, so a Claude session with a 6+-line metadata prelude
    // (e.g. `permission-mode` followed by several `file-history-snapshot`
    // entries) silently fell through to Codex. The detector must now scan the
    // full slice the caller supplies.
    let mut data: Vec<Value> = (0..7)
        .map(|i| json!({"type": "file-history-snapshot", "idx": i}))
        .collect();
    data.push(json!({
        "parentUuid": "deep-uuid",
        "type": "user"
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
