//! Content-based provider classification for session JSONL records.
//!
//! Given the parsed records from a session file, this module decides which
//! of the four supported assistants wrote it ([`ExtensionType`]). Two entry
//! points exist for two call shapes: [`detect_extension_type`] commits
//! eagerly on a fully-materialised slice (the `Vec<Value>` fallback path),
//! while [`classify_records`] returns `None` on indeterminate input so a
//! streaming caller can keep peeking lines until a marker appears.
use crate::models::ExtensionType;
use crate::session::grok::is_grok_signals;
use anyhow::{Result, bail};
use serde_json::Value;

/// Detects the AI provider format by analyzing distinctive fields in the
/// session data.
///
/// Thin eager wrapper over [`classify_records`]: returns whatever marker the
/// records carry, falling back to [`ExtensionType::Codex`] when none is
/// found (a marker-less JSONL stream is almost always a Codex log, whose
/// `type` discriminators just happen to be absent in this slice).
///
/// Detection strategy:
/// - Gemini: first line is a session-meta record with `sessionId` and
///   `projectHash` fields but *no* `messages` array (legacy single-object
///   Gemini exports are no longer supported)
/// - Copilot: first line is a `type == "session.start"` event whose
///   `data.producer` field identifies a Copilot agent (e.g.
///   `copilot-agent`, `copilot-cli`). Legacy single-object dumps under
///   `~/.copilot/history-session-state/` are no longer supported.
/// - Claude Code: contains `parentUuid` field in log entries
/// - Codex: contains a record whose `type` is one of `session_meta`,
///   `turn_context`, `event_msg`, or `response_item` — **or** as a final
///   fallback when no other marker is present
///
/// Callers walking a streaming source should prefer
/// [`classify_records`] instead: it returns `None` when the records seen
/// so far are indeterminate, letting the caller decide whether to read more
/// before committing to a provider (see `parser::stream_parse_autodetect`).
///
/// # Errors
///
/// Returns an error when `data` is empty — an empty slice carries no marker
/// to classify and is treated as a caller bug rather than silently defaulted.
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use vibe_coding_tracker::session::detector::detect_extension_type;
/// use vibe_coding_tracker::ExtensionType;
///
/// let records = [json!({ "parentUuid": "abc", "type": "user" })];
/// assert_eq!(detect_extension_type(&records).unwrap(), ExtensionType::ClaudeCode);
/// ```
pub fn detect_extension_type(data: &[Value]) -> Result<ExtensionType> {
    if data.is_empty() {
        bail!("Cannot detect extension type from empty data");
    }

    Ok(classify_records(data).unwrap_or(ExtensionType::Codex))
}

/// Streaming-friendly classifier that only commits to a provider when the
/// records carry a distinctive marker.
///
/// Returns `None` when every record seen so far is indeterminate (a Claude
/// metadata preamble, an empty record, a record without any recognised
/// `type` discriminator, …). The streaming auto-detect path uses the
/// `None` signal to decide whether to peek one more JSONL line before
/// falling back to the default.
///
/// Why this matters: the previous design buffered a fixed `AUTODETECT_PEEK_LINES`
/// (8) records and then called [`detect_extension_type`], which silently
/// committed to Codex (the default) once that buffer was exhausted. A Claude
/// session whose `parentUuid`-bearing record sat past a long metadata
/// prelude could then be mis-classified as Codex and have its usage
/// silently dropped. With this function the caller can keep reading until
/// a positive signal appears, so there is no arbitrary limit to the
/// preamble length we tolerate.
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use vibe_coding_tracker::session::detector::classify_records;
/// use vibe_coding_tracker::ExtensionType;
///
/// // A marker-less Claude metadata preamble stays indeterminate...
/// let preamble = [json!({ "type": "file-history-snapshot" })];
/// assert!(classify_records(&preamble).is_none());
///
/// // ...until a `parentUuid`-bearing record arrives.
/// let with_marker = [
///     json!({ "type": "file-history-snapshot" }),
///     json!({ "parentUuid": "abc", "type": "user" }),
/// ];
/// assert_eq!(classify_records(&with_marker), Some(ExtensionType::ClaudeCode));
/// ```
pub fn classify_records(data: &[Value]) -> Option<ExtensionType> {
    if data.first().is_some_and(is_grok_signals) {
        return Some(ExtensionType::Grok);
    }

    // JSONL stream: Gemini session-meta header line.
    //
    // Gemini CLI writes one event per line under `chats/`; the very first
    // line is a pure session-meta record carrying `sessionId` +
    // `projectHash`. The header does *not* include a `messages` array (an
    // inline `messages[]` marked the obsolete single-object export shape,
    // which we deliberately reject here to avoid mis-classifying it).
    if let Some(first) = data.first().and_then(|v| v.as_object())
        && first.contains_key("sessionId")
        && first.contains_key("projectHash")
        && !first.contains_key("messages")
    {
        return Some(ExtensionType::Gemini);
    }

    // JSONL stream: Copilot CLI `events.jsonl` session-start record.
    //
    // The modern Copilot CLI writes one event per line under
    // `session-state/<sessionId>/events.jsonl`. The very first line is a
    // `type == "session.start"` event whose `data.producer` field reads
    // `"copilot-agent"` — distinct enough from Codex / Claude event
    // streams to use as a classification key. Accept any of the typical
    // producer tags (some dev builds emit `"copilot-cli"`) so we stay
    // forward-compatible across minor CLI releases.
    if let Some(first) = data.first().and_then(|v| v.as_object())
        && first
            .get("type")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t == "session.start")
    {
        let producer = first
            .get("data")
            .and_then(|d| d.get("producer"))
            .and_then(|p| p.as_str())
            .unwrap_or("");
        if producer.starts_with("copilot") {
            return Some(ExtensionType::Copilot);
        }
    }

    // Walk every record looking for Claude Code / Codex positive markers.
    // Either one counts — whichever shows up first wins. This scan has no
    // arbitrary upper bound: the caller decides how much data to supply,
    // and the streaming auto-detect path keeps peeking lines until a
    // marker is seen or the file ends.
    for record in data {
        let Some(obj) = record.as_object() else {
            continue;
        };

        // Claude Code sessions key off the `parentUuid` field, which Claude
        // writes on every assistant / user record but nothing else in this
        // codebase relies on.
        if obj.contains_key("parentUuid") {
            return Some(ExtensionType::ClaudeCode);
        }

        // Codex rollout logs use a `type` field with one of a small set of
        // enum-like values. Matching any of these is a definitive signal —
        // none of them overlap with Claude / Gemini / Copilot shapes.
        if let Some(t) = obj.get("type").and_then(|v| v.as_str())
            && matches!(
                t,
                "session_meta" | "turn_context" | "event_msg" | "response_item"
            )
        {
            return Some(ExtensionType::Codex);
        }
    }

    None
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
    fn test_detect_copilot_rejects_legacy_single_object() {
        // Older Copilot CLI releases wrote a single-object dump with
        // `sessionId` + `startTime` + `timeline`. We no longer support that
        // shape — the detector should fall through to the default (Codex)
        // rather than mis-routing the file to the JSONL analyzer, which
        // would silently produce an empty analysis.
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
        // Unknown extra fields on the Copilot `session.start` event must not
        // stop detection — the classifier only relies on
        // `type == "session.start"` + `data.producer` starting with `copilot`.
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
        // A `session.start` event without a copilot-flavoured producer must
        // not be classified as Copilot — guards against false positives when
        // other providers ever adopt the same discriminator.
        let data = vec![json!({
            "type": "session.start",
            "data": {
                "sessionId": "test"
                // no `producer` field at all
            }
        })];

        let result = detect_extension_type(&data).unwrap();
        assert_eq!(result, ExtensionType::Codex); // Should default to Codex
    }
}
