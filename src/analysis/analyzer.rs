use crate::analysis::claude_analyzer::analyze_claude_conversations;
use crate::analysis::codex_analyzer::analyze_codex_conversations;
use crate::analysis::detector::detect_extension_type;
use crate::models::{CodexLog, ExtensionType};
use crate::utils::{get_current_user, get_machine_id, read_jsonl};
use crate::VERSION;
use anyhow::Result;
use serde_json::Value;
use std::path::Path;

/// Analyze a JSONL file and return CodeAnalysis result
pub fn analyze_jsonl_file<P: AsRef<Path>>(path: P) -> Result<Value> {
    let data = read_jsonl(path)?;

    if data.is_empty() {
        return Ok(serde_json::json!({}));
    }

    let ext_type = detect_extension_type(&data);
    let analysis = analyze_record_set(&data, ext_type)?;

    Ok(analysis)
}

/// Analyze a set of records and return structured result
fn analyze_record_set(data: &[Value], ext_type: ExtensionType) -> Result<Value> {
    let mut analysis = match ext_type {
        ExtensionType::ClaudeCode => analyze_claude_conversations(data)?,
        ExtensionType::Codex => {
            let logs: Vec<CodexLog> = data
                .iter()
                .filter_map(|v| serde_json::from_value(v.clone()).ok())
                .collect();
            analyze_codex_conversations(&logs)?
        }
    };

    // Fill in metadata
    analysis.user = get_current_user();
    analysis.extension_name = ext_type.to_string();
    analysis.machine_id = get_machine_id();
    analysis.insights_version = VERSION.to_string();

    // Convert to JSON Value
    let result = serde_json::to_value(&analysis)?;

    Ok(result)
}
