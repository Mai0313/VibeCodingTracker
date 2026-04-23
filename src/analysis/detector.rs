use crate::models::ExtensionType;
use anyhow::{Result, bail};
use serde_json::Value;

/// Detects the AI provider format by analyzing distinctive fields in the session data
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
/// before committing to a provider (see `analyzer::stream_analyze_autodetect`).
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
pub fn classify_records(data: &[Value]) -> Option<ExtensionType> {
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
