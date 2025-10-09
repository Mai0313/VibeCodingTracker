use crate::models::*;
use crate::utils::{get_git_remote_url, parse_iso_timestamp, process_gemini_usage};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;

/// Analyze Gemini conversations
pub fn analyze_gemini_conversations(mut data: Vec<Value>) -> Result<CodeAnalysis> {
    if data.is_empty() {
        return Ok(CodeAnalysis {
            user: String::new(),
            extension_name: String::new(),
            insights_version: String::new(),
            machine_id: String::new(),
            records: vec![],
        });
    }

    // Parse the Gemini session
    let session: GeminiSession = serde_json::from_value(data.remove(0))?;

    // Pre-allocate HashMap with typical capacity (1-3 models per conversation)
    let mut conversation_usage: HashMap<String, Value> = HashMap::with_capacity(3);
    let mut last_timestamp = 0i64;
    let folder_path = String::new();

    // Process messages to extract token usage
    for message in &session.messages {
        let ts = parse_iso_timestamp(&message.timestamp);
        if ts > last_timestamp {
            last_timestamp = ts;
        }

        // Only process gemini messages (not user messages)
        if message.message_type == "gemini" {
            if let (Some(tokens), Some(model)) = (&message.tokens, &message.model) {
                process_gemini_usage(&mut conversation_usage, model, tokens);
            }
        }
    }

    // Try to get git remote URL from current directory
    let git_remote_url = get_git_remote_url(&folder_path);

    let tool_counts = CodeAnalysisToolCalls::default();

    let record = CodeAnalysisRecord {
        total_unique_files: 0,
        total_write_lines: 0,
        total_read_lines: 0,
        total_read_characters: 0,
        total_write_characters: 0,
        total_edit_characters: 0,
        total_edit_lines: 0,
        write_file_details: vec![],
        read_file_details: vec![],
        edit_file_details: vec![],
        run_command_details: vec![],
        tool_call_counts: tool_counts,
        conversation_usage,
        task_id: session.session_id,  // Consume session instead of cloning
        timestamp: last_timestamp,
        folder_path,
        git_remote_url,
    };

    Ok(CodeAnalysis {
        user: String::new(),
        extension_name: String::new(),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    })
}
