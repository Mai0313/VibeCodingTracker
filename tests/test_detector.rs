// Unit tests for analysis/detector.rs
//
// Tests the AI provider format detection logic

use serde_json::{Value, json};
use vibe_coding_tracker::analysis::detector::{classify_records, detect_extension_type};
use vibe_coding_tracker::models::ExtensionType;

#[test]
fn test_detect_gemini_jsonl_meta_header() {
    // Gemini CLI writes one event per line under `chats/`. The first
    // line is a pure session-meta record carrying `sessionId` +
    // `projectHash` and *no* `messages` array — the detector must
    // recognise it even when further event lines follow in the same slice.
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
fn test_detect_gemini_rejects_legacy_single_object() {
    // Legacy Gemini single-object exports used to be detected as Gemini, but
    // the analyzer no longer supports that shape. We explicitly guard
    // against mis-classifying a record with an inline `messages` array as
    // Gemini so it falls through to Codex (and fails clearly) instead of
    // silently producing an empty analysis.
    let data = vec![json!({
        "sessionId": "test-session",
        "projectHash": "abc123",
        "messages": []
    })];

    let result = detect_extension_type(&data).unwrap();
    assert_ne!(result, ExtensionType::Gemini);
}

#[test]
fn test_detect_copilot_format() {
    // Legacy single-object dump with sessionId, startTime, and timeline
    let data = vec![json!({
        "sessionId": "test-session",
        "startTime": 1234567890,
        "timeline": []
    })];

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Copilot);
}

#[test]
fn test_detect_copilot_jsonl_session_start() {
    // Modern Copilot CLI writes one event per line; the first event is always
    // `type == "session.start"` with `data.producer == "copilot-agent"`.
    let data = vec![
        json!({
            "type": "session.start",
            "data": {
                "sessionId": "d2e098d0-e0d6-4d6b-914b-c4c5543b17e3",
                "version": 1,
                "producer": "copilot-agent",
                "copilotVersion": "1.0.34",
                "startTime": "2026-04-23T12:56:32.850Z",
                "context": {
                    "cwd": "/home/wei/repo/VibeCodingTracker",
                    "gitRoot": "/home/wei/repo/VibeCodingTracker",
                    "branch": "main",
                    "repository": "Mai0313/VibeCodingTracker",
                    "hostType": "github",
                    "repositoryHost": "github.com"
                }
            },
            "id": "eac2d9cb-d62b-4c32-9178-ac8e83d5dfad",
            "timestamp": "2026-04-23T12:56:32.876Z",
            "parentId": null
        }),
        json!({
            "type": "session.mode_changed",
            "data": {"previousMode": "interactive", "newMode": "autopilot"}
        }),
    ];

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::Copilot);
}

#[test]
fn test_detect_copilot_jsonl_rejects_non_copilot_producer() {
    // A `session.start` event without a copilot producer tag should not
    // trigger the Copilot branch — guards against false positives if
    // another provider ever adopts the same discriminator name.
    let data = vec![json!({
        "type": "session.start",
        "data": {
            "sessionId": "abc",
            "producer": "some-other-tool"
        }
    })];

    let result = detect_extension_type(&data).unwrap();
    assert_ne!(result, ExtensionType::Copilot);
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
fn test_detect_claude_code_past_long_preamble() {
    // Regression guard: Claude Code sessions can carry an arbitrary
    // number of metadata preamble records (`permission-mode`,
    // `file-history-snapshot`, `queue-operation`, …) before the first
    // `parentUuid`-bearing line. The detector has no upper bound — it
    // scans the entire slice the caller hands it. A previous
    // implementation capped the scan at 5 records and silently
    // mis-classified long-preamble sessions as Codex.
    let mut data: Vec<Value> = (0..50)
        .map(|i| json!({"type": "file-history-snapshot", "idx": i}))
        .collect();
    data.push(json!({
        "parentUuid": "deep-uuid",
        "type": "user"
    }));

    let result = detect_extension_type(&data).unwrap();
    assert_eq!(result, ExtensionType::ClaudeCode);
}

// ============================================================================
// classify_records — the streaming-friendly variant
// ============================================================================

#[test]
fn test_classify_returns_none_on_indeterminate_records() {
    // A Claude metadata preamble with no `parentUuid` yet has no
    // distinctive marker on any provider — `classify_records` must return
    // `None` so the streaming auto-detect loop keeps reading more lines
    // instead of committing to a default too early.
    let preamble: Vec<Value> = (0..5)
        .map(|i| json!({"type": "file-history-snapshot", "idx": i}))
        .collect();
    assert!(classify_records(&preamble).is_none());
}

#[test]
fn test_classify_commits_when_claude_marker_arrives() {
    // Streaming behaviour: once the caller appends a record containing
    // `parentUuid`, classification flips from `None` to `Some(ClaudeCode)`.
    let mut buffer: Vec<Value> = (0..3)
        .map(|i| json!({"type": "file-history-snapshot", "idx": i}))
        .collect();
    assert!(classify_records(&buffer).is_none());

    buffer.push(json!({"parentUuid": "abc-123", "type": "user"}));
    assert_eq!(classify_records(&buffer), Some(ExtensionType::ClaudeCode));
}

#[test]
fn test_classify_commits_on_codex_type_marker() {
    // Codex rollout logs use a small set of `type` enum values on each
    // record — any one of them is a positive signal.
    for codex_type in ["session_meta", "turn_context", "event_msg", "response_item"] {
        let data = vec![json!({
            "type": codex_type,
            "timestamp": "2026-04-23T00:00:00Z",
            "payload": {}
        })];
        assert_eq!(
            classify_records(&data),
            Some(ExtensionType::Codex),
            "type={} should classify as Codex",
            codex_type
        );
    }
}

#[test]
fn test_classify_gemini_meta_header_first_line() {
    // Gemini's first-line meta record is enough to commit without needing
    // any subsequent event lines.
    let data = vec![json!({
        "sessionId": "s",
        "projectHash": "p",
        "kind": "main"
    })];
    assert_eq!(classify_records(&data), Some(ExtensionType::Gemini));
}

#[test]
fn test_classify_copilot_jsonl_first_line() {
    // Modern Copilot CLI's first line is `type: "session.start"` with a
    // copilot producer — one line is enough.
    let data = vec![json!({
        "type": "session.start",
        "data": {"sessionId": "s", "producer": "copilot-agent"}
    })];
    assert_eq!(classify_records(&data), Some(ExtensionType::Copilot));
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
    // Unknown extra fields on the Gemini JSONL meta-header must not stop
    // detection — the analyzer relies on `sessionId` + `projectHash` + the
    // absence of a `messages` array and ignores everything else.
    let data = vec![json!({
        "sessionId": "test",
        "projectHash": "hash",
        "startTime": "2026-04-23T00:00:00Z",
        "kind": "main",
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
    // A record missing either `sessionId` or `projectHash` must not be
    // classified as Gemini even when it looks superficially similar.
    let without_project_hash = vec![json!({
        "sessionId": "test"
    })];
    let result = detect_extension_type(&without_project_hash).unwrap();
    assert_eq!(result, ExtensionType::Codex);

    let without_session_id = vec![json!({
        "projectHash": "hash"
    })];
    let result = detect_extension_type(&without_session_id).unwrap();
    assert_eq!(result, ExtensionType::Codex);
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
