use crate::models::ExtensionType;
use anyhow::{Result, bail};
use serde_json::Value;

/// Detects the AI provider format by analyzing distinctive fields in the session data
///
/// Detection strategy:
/// - Gemini (legacy single-object): object with `sessionId`, `projectHash`,
///   and `messages` fields
/// - Gemini (current JSONL stream): first line is a session-meta record with
///   `sessionId`, `projectHash`, and `kind` fields (no `messages`)
/// - Copilot (legacy single-object): object with `sessionId`, `startTime`,
///   and `timeline` fields
/// - Copilot (current JSONL stream): first line is a
///   `type == "session.start"` event whose `data.producer` field identifies
///   a Copilot agent (e.g. `copilot-agent`, `copilot-cli`)
/// - Claude Code: contains `parentUuid` field in log entries
/// - Codex: default fallback if no other markers found
pub fn detect_extension_type(data: &[Value]) -> Result<ExtensionType> {
    if data.is_empty() {
        bail!("Cannot detect extension type from empty data");
    }

    // Single-object formats: Gemini legacy export or Copilot CLI legacy dump.
    if data.len() == 1
        && let Some(obj) = data[0].as_object()
    {
        // Legacy Gemini single-object export.
        if obj.contains_key("sessionId")
            && obj.contains_key("projectHash")
            && obj.contains_key("messages")
        {
            return Ok(ExtensionType::Gemini);
        }

        // Legacy Copilot CLI single-object dump.
        if obj.contains_key("sessionId")
            && obj.contains_key("startTime")
            && obj.contains_key("timeline")
        {
            return Ok(ExtensionType::Copilot);
        }
    }

    // JSONL stream: Gemini session-meta header line.
    //
    // The modern Gemini CLI writes one event per line under `chats/`; the
    // very first line is a pure session-meta record tagged with `kind`
    // (typically `"main"`), carrying `sessionId` + `projectHash` but *no*
    // `messages` array. If we see that shape at index 0 we can classify
    // the whole stream as Gemini.
    if let Some(first) = data.first().and_then(|v| v.as_object())
        && first.contains_key("sessionId")
        && first.contains_key("projectHash")
        && first.contains_key("kind")
        && !first.contains_key("messages")
    {
        return Ok(ExtensionType::Gemini);
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
            return Ok(ExtensionType::Copilot);
        }
    }

    // Single-pass detection for Claude Code or Codex.
    //
    // The caller decides how much to pass in (the streaming auto-detect path
    // buffers up to 8 records), so we scan the whole slice here — previously
    // this was capped at 5 records, which silently missed Claude sessions
    // whose `parentUuid`-bearing record sat past a 6+-line metadata prelude
    // (e.g. `permission-mode` followed by several `file-history-snapshot`
    // records).
    for record in data {
        if let Some(obj) = record.as_object() {
            // Claude Code has parentUuid field
            if obj.contains_key("parentUuid") {
                return Ok(ExtensionType::ClaudeCode);
            }
        }
    }

    // Default to Codex if no distinctive markers found
    Ok(ExtensionType::Codex)
}
