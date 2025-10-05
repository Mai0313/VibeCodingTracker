use crate::models::ExtensionType;
use anyhow::{bail, Result};
use serde_json::Value;

/// Detect whether the log is from Claude Code, Codex, or Gemini
pub fn detect_extension_type(data: &[Value]) -> Result<ExtensionType> {
    if data.is_empty() {
        bail!("Cannot detect extension type from empty data");
    }

    // Check for Gemini specific fields (single session object)
    if data.len() == 1 {
        if let Some(obj) = data[0].as_object() {
            if obj.contains_key("sessionId")
                && obj.contains_key("projectHash")
                && obj.contains_key("messages")
            {
                return Ok(ExtensionType::Gemini);
            }
        }
    }

    // Check for Claude Code specific fields
    for record in data {
        if let Some(obj) = record.as_object() {
            if obj.contains_key("parentUuid") {
                return Ok(ExtensionType::ClaudeCode);
            }
        }
    }

    // Default to Codex if no specific markers found
    Ok(ExtensionType::Codex)
}
