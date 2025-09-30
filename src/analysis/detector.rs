use crate::models::ExtensionType;
use serde_json::Value;

/// Detect whether the log is from Claude Code or Codex
pub fn detect_extension_type(data: &[Value]) -> ExtensionType {
    if data.is_empty() {
        return ExtensionType::Codex;
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
