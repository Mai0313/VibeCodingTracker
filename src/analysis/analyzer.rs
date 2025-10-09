use crate::analysis::claude_analyzer::analyze_claude_conversations;
use crate::analysis::codex_analyzer::analyze_codex_conversations;
use crate::analysis::detector::detect_extension_type;
use crate::analysis::gemini_analyzer::analyze_gemini_conversations;
use crate::models::{CodexLog, ExtensionType};
use crate::utils::{get_current_user, get_machine_id, read_json, read_jsonl};
use crate::VERSION;
use anyhow::Result;
use serde_json::Value;
use std::path::Path;

/// Analyze a JSONL or JSON file and return CodeAnalysis result
pub fn analyze_jsonl_file<P: AsRef<Path>>(path: P) -> Result<Value> {
    let data = match read_jsonl(&path) {
        Ok(data) => data,
        Err(_) => read_json(&path)?,
    };

    if data.is_empty() {
        return Ok(serde_json::json!({}));
    }

    let ext_type = detect_extension_type(&data)?;
    let analysis = analyze_record_set(data, ext_type)?;

    Ok(analysis)
}

fn analyze_record_set(data: Vec<Value>, ext_type: ExtensionType) -> Result<Value> {
    let mut analysis = match ext_type {
        ExtensionType::ClaudeCode => analyze_claude_conversations(data)?,
        ExtensionType::Codex => {
            let logs: Vec<CodexLog> = data
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            analyze_codex_conversations(&logs)?
        }
        ExtensionType::Gemini => analyze_gemini_conversations(data)?,
    };

    analysis.user = get_current_user();
    analysis.extension_name = ext_type.to_string();
    analysis.machine_id = get_machine_id().to_string();
    analysis.insights_version = VERSION.to_string();

    let result = serde_json::to_value(&analysis)?;
    Ok(result)
}
