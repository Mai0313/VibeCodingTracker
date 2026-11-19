//! Content-based provider classification for session JSON and JSONL records.
//!
//! [`classify_records`] returns `None` until a distinctive provider marker appears, supporting streaming callers with unbounded metadata preambles. [`detect_extension_type`] commits eagerly on a slice and defaults to [`ExtensionType::Codex`] when no marker matches.
use crate::models::ExtensionType;
use crate::session::antigravity::is_antigravity_step;
use crate::session::dsh::is_dsh_header;
use crate::session::grok::is_grok_signals;
use anyhow::{Result, bail};
use serde_json::Value;

#[cfg(test)]
std::thread_local! {
    static RECORD_INSPECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_record_inspections() {
    RECORD_INSPECTIONS.set(0);
}

#[cfg(test)]
pub(crate) fn record_inspections() -> usize {
    RECORD_INSPECTIONS.get()
}

/// Returns the [`ExtensionType`] detected from `data`, defaulting to [`ExtensionType::Codex`] when no marker matches.
///
/// Evaluates the same markers as [`classify_records`] in order, testing the first five against the first record only and the last two against every record:
/// - [`ExtensionType::Grok`]: `primaryModelId`, `contextTokensUsed`, and either `contextWindowTokens` or `toolsUsed`.
/// - [`ExtensionType::DeepSeek`]: uncompressed session header with `type == "session"`, numeric `version`, and string `id`.
/// - [`ExtensionType::Antigravity`]: transcript step with numeric `step_index` and string `source` and `type`.
/// - [`ExtensionType::Gemini`]: session-meta record with `sessionId` and `projectHash` and no `messages` field.
/// - [`ExtensionType::Copilot`]: `type == "session.start"` with `data.producer` starting with `"copilot"`.
/// - [`ExtensionType::ClaudeCode`]: any record containing `parentUuid`.
/// - [`ExtensionType::Codex`]: any record with `type` in `session_meta`, `turn_context`, `event_msg`, or `response_item`, or as the final fallback.
///
/// Streaming callers should use [`classify_records`] instead so indeterminate prefixes do not trigger the fallback early.
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use vct_core::session::detector::detect_extension_type;
/// use vct_core::ExtensionType;
///
/// let records = [json!({ "parentUuid": "abc", "type": "user" })];
/// assert_eq!(detect_extension_type(&records).unwrap(), ExtensionType::ClaudeCode);
/// ```
///
/// # Errors
///
/// Returns an error if `data` is empty.
pub fn detect_extension_type(data: &[Value]) -> Result<ExtensionType> {
    if data.is_empty() {
        bail!("Cannot detect extension type from empty data");
    }

    Ok(classify_records(data).unwrap_or(ExtensionType::Codex))
}

/// Returns the first [`ExtensionType`] positively identified in `data`, or `None` if every record is indeterminate.
///
/// Evaluates the marker rules documented on [`detect_extension_type`] without applying the [`ExtensionType::Codex`] fallback, allowing streaming callers to keep reading past arbitrarily long metadata preambles.
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use vct_core::session::detector::classify_records;
/// use vct_core::ExtensionType;
///
/// // Indeterminate metadata preambles return `None` until a marker arrives.
/// let preamble = [json!({ "type": "file-history-snapshot" })];
/// assert!(classify_records(&preamble).is_none());
///
/// let with_marker = [
///     json!({ "type": "file-history-snapshot" }),
///     json!({ "parentUuid": "abc", "type": "user" }),
/// ];
/// assert_eq!(classify_records(&with_marker), Some(ExtensionType::ClaudeCode));
/// ```
pub fn classify_records(data: &[Value]) -> Option<ExtensionType> {
    let mut classifier = RecordClassifier::default();
    data.iter().find_map(|record| classifier.push(record))
}

/// Stateful provider classifier that inspects each record in a stream once.
#[derive(Debug, Default)]
pub(crate) struct RecordClassifier {
    seen_first: bool,
    classified: Option<ExtensionType>,
}

impl RecordClassifier {
    /// Adds one record and returns the first confident provider verdict.
    pub(crate) fn push(&mut self, record: &Value) -> Option<ExtensionType> {
        if self.classified.is_some() {
            return self.classified;
        }

        #[cfg(test)]
        RECORD_INSPECTIONS.set(RECORD_INSPECTIONS.get() + 1);

        let first = !self.seen_first;
        self.seen_first = true;

        if first {
            if is_grok_signals(record) {
                self.classified = Some(ExtensionType::Grok);
                return self.classified;
            }

            // Only uncompressed `dsh` logs reach text classification; zstd logs route by frame magic earlier.
            if is_dsh_header(record) {
                self.classified = Some(ExtensionType::DeepSeek);
                return self.classified;
            }

            if is_antigravity_step(record) {
                self.classified = Some(ExtensionType::Antigravity);
                return self.classified;
            }

            if let Some(object) = record.as_object() {
                if object.contains_key("sessionId")
                    && object.contains_key("projectHash")
                    && !object.contains_key("messages")
                {
                    self.classified = Some(ExtensionType::Gemini);
                    return self.classified;
                }

                if object.get("type").and_then(Value::as_str) == Some("session.start")
                    && object
                        .get("data")
                        .and_then(|data| data.get("producer"))
                        .and_then(Value::as_str)
                        .is_some_and(|producer| producer.starts_with("copilot"))
                {
                    self.classified = Some(ExtensionType::Copilot);
                    return self.classified;
                }
            }
        }

        let object = record.as_object()?;

        if object.contains_key("parentUuid") {
            self.classified = Some(ExtensionType::ClaudeCode);
        } else if object
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|record_type| {
                matches!(
                    record_type,
                    "session_meta" | "turn_context" | "event_msg" | "response_item"
                )
            })
        {
            self.classified = Some(ExtensionType::Codex);
        }

        self.classified
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn test_detect_grok_signals() {
        let data = vec![json!({
            "primaryModelId": "grok-4.5",
            "contextTokensUsed": 12_345,
            "contextWindowTokens": 200_000,
            "toolsUsed": ["read_file"]
        })];

        assert_eq!(detect_extension_type(&data).unwrap(), ExtensionType::Grok);
    }

    #[test]
    fn test_detect_gemini_jsonl_meta_header() {
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
        let data = vec![json!({
            "sessionId": "test-session",
            "projectHash": "abc123",
            "messages": []
        })];

        let result = detect_extension_type(&data).unwrap();
        assert_ne!(result, ExtensionType::Gemini);
    }

    #[test]
    fn test_detect_copilot_rejects_legacy_single_object() {
        let data = vec![json!({
            "sessionId": "test-session",
            "startTime": 1234567890,
            "timeline": []
        })];

        let result = detect_extension_type(&data).unwrap();
        assert_ne!(result, ExtensionType::Copilot);
    }

    #[test]
    fn test_detect_copilot_jsonl_session_start() {
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
        let mut data = vec![json!({"field": "value1"}), json!({"field": "value2"})];

        data.push(json!({
            "parentUuid": "test-uuid",
            "content": "test"
        }));

        let result = detect_extension_type(&data).unwrap();
        assert_eq!(result, ExtensionType::ClaudeCode);
    }

    #[test]
    fn test_detect_claude_code_past_long_preamble() {
        // Preamble length is unbounded before the first `parentUuid` record.
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

    #[test]
    fn test_classify_returns_none_on_indeterminate_records() {
        let preamble: Vec<Value> = (0..5)
            .map(|i| json!({"type": "file-history-snapshot", "idx": i}))
            .collect();
        assert!(classify_records(&preamble).is_none());
    }

    #[test]
    fn test_classify_commits_when_claude_marker_arrives() {
        let mut buffer: Vec<Value> = (0..3)
            .map(|i| json!({"type": "file-history-snapshot", "idx": i}))
            .collect();
        assert!(classify_records(&buffer).is_none());

        buffer.push(json!({"parentUuid": "abc-123", "type": "user"}));
        assert_eq!(classify_records(&buffer), Some(ExtensionType::ClaudeCode));
    }

    #[test]
    fn test_classify_commits_on_codex_type_marker() {
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
        let data = vec![json!({
            "sessionId": "s",
            "projectHash": "p",
            "kind": "main"
        })];
        assert_eq!(classify_records(&data), Some(ExtensionType::Gemini));
    }

    #[test]
    fn test_classify_copilot_jsonl_first_line() {
        let data = vec![json!({
            "type": "session.start",
            "data": {"sessionId": "s", "producer": "copilot-agent"}
        })];
        assert_eq!(classify_records(&data), Some(ExtensionType::Copilot));
    }

    #[test]
    fn test_detect_empty_data_error() {
        let data: Vec<Value> = vec![];

        let result = detect_extension_type(&data);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty data"));
    }

    #[test]
    fn test_detect_multiple_objects_without_markers() {
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
        let data = vec![json!({
            "type": "session.start",
            "data": {
                "sessionId": "test",
                "producer": "copilot-agent",
                "extraField": "extra"
            },
            "id": "abc",
            "timestamp": "2026-04-23T00:00:00Z",
            "extraTop": 42
        })];

        let result = detect_extension_type(&data).unwrap();
        assert_eq!(result, ExtensionType::Copilot);
    }

    #[test]
    fn test_detect_partial_gemini_fields() {
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
        let data = vec![json!({
            "type": "session.start",
            "data": {
                "sessionId": "test"
            }
        })];

        let result = detect_extension_type(&data).unwrap();
        assert_eq!(result, ExtensionType::Codex);
    }
}
