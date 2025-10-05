use crate::models::ExtensionType;
use serde_json::Value;

/// Detect whether the log is from Claude Code, Codex, or Gemini
pub fn detect_extension_type(data: &[Value]) -> ExtensionType {
    if data.is_empty() {
        return ExtensionType::Codex;
    }

    // Check for Gemini specific fields (single session object)
    if data.len() == 1 {
        if let Some(obj) = data[0].as_object() {
            if obj.contains_key("sessionId")
                && obj.contains_key("projectHash")
                && obj.contains_key("messages")
            {
                return ExtensionType::Gemini;
            }
        }
    }

    // Check for Claude Code specific fields
    for record in data {
        if let Some(obj) = record.as_object() {
            if obj.contains_key("parentUuid") {
                return ExtensionType::ClaudeCode;
            }
        }
    }

    ExtensionType::Codex
}
