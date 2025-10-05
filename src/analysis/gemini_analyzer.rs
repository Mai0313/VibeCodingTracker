use crate::models::*;
use crate::utils::{get_git_remote_url, parse_iso_timestamp};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;

/// Analyze Gemini conversations
pub fn analyze_gemini_conversations(data: &[Value]) -> Result<CodeAnalysis> {
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
    let session: GeminiSession = serde_json::from_value(data[0].clone())?;

    let mut conversation_usage: HashMap<String, Value> = HashMap::new();
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
        task_id: session.session_id.clone(),
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

/// Process Gemini token usage
fn process_gemini_usage(
    conversation_usage: &mut HashMap<String, Value>,
    model: &str,
    tokens: &GeminiTokens,
) {
    // Get or create usage entry
    let existing = conversation_usage
        .entry(model.to_string())
        .or_insert_with(|| {
            serde_json::json!({
                "input_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "output_tokens": 0,
                "thoughts_tokens": 0,
                "tool_tokens": 0,
                "total_tokens": 0,
            })
        });

    let existing_obj = existing.as_object_mut().unwrap();

    // Add input tokens
    let current_input = existing_obj
        .get("input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "input_tokens".to_string(),
        (current_input + tokens.input).into(),
    );

    // Add cached tokens as cache_read_input_tokens
    let current_cached = existing_obj
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "cache_read_input_tokens".to_string(),
        (current_cached + tokens.cached).into(),
    );

    // Add output tokens
    let current_output = existing_obj
        .get("output_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "output_tokens".to_string(),
        (current_output + tokens.output).into(),
    );

    // Add thoughts tokens (Gemini-specific)
    let current_thoughts = existing_obj
        .get("thoughts_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "thoughts_tokens".to_string(),
        (current_thoughts + tokens.thoughts).into(),
    );

    // Add tool tokens (Gemini-specific)
    let current_tool = existing_obj
        .get("tool_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "tool_tokens".to_string(),
        (current_tool + tokens.tool).into(),
    );

    // Add total tokens
    let current_total = existing_obj
        .get("total_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "total_tokens".to_string(),
        (current_total + tokens.total).into(),
    );
}
