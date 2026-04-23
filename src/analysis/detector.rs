use crate::models::ExtensionType;
use anyhow::{Result, bail};
use serde_json::Value;

/// Detects the AI provider format by analyzing distinctive fields in the session data
///
/// Detection strategy:
/// - Gemini: Single object with `sessionId`, `projectHash`, and `messages` fields
/// - Copilot: Single object with `sessionId`, `startTime`, and `timeline` fields
/// - Claude Code: Contains `parentUuid` field in log entries
/// - Codex: Default fallback if no other markers found
pub fn detect_extension_type(data: &[Value]) -> Result<ExtensionType> {
    if data.is_empty() {
        bail!("Cannot detect extension type from empty data");
    }

    // Quick check for single object formats (Gemini or Copilot)
    if data.len() == 1
        && let Some(obj) = data[0].as_object()
    {
        // Check for Gemini format
        if obj.contains_key("sessionId")
            && obj.contains_key("projectHash")
            && obj.contains_key("messages")
        {
            return Ok(ExtensionType::Gemini);
        }

        // Check for Copilot CLI format
        if obj.contains_key("sessionId")
            && obj.contains_key("startTime")
            && obj.contains_key("timeline")
        {
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
