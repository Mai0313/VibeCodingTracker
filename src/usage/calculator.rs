use crate::analysis::detector::detect_extension_type;
use crate::models::{DateUsageResult, ExtensionType, GeminiSession, UsageResult};
use crate::utils::{
    collect_files_with_dates, is_gemini_chat_file, is_json_file, process_claude_usage,
    process_codex_usage, process_gemini_usage, read_json, read_jsonl, resolve_paths,
};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Calculate usage from a single JSONL or JSON file
pub fn calculate_usage_from_jsonl<P: AsRef<Path>>(file_path: P) -> Result<UsageResult> {
    // Try reading as JSONL first, then as JSON
    let data = match read_jsonl(&file_path) {
        Ok(data) => data,
        Err(_) => read_json(&file_path)?,
    };

    if data.is_empty() {
        return Ok(UsageResult {
            tool_call_counts: HashMap::new(),
            conversation_usage: HashMap::new(),
        });
    }

    let ext_type = detect_extension_type(&data)?;

    match ext_type {
        ExtensionType::ClaudeCode => calculate_claude_usage(&data),
        ExtensionType::Gemini => calculate_gemini_usage(&data),
        ExtensionType::Codex => calculate_codex_usage(&data),
    }
}

/// Calculate usage from all directories
pub fn get_usage_from_directories() -> Result<DateUsageResult> {
    let paths = resolve_paths()?;
    let mut result = DateUsageResult::new();

    // Process Claude directory
    if paths.claude_session_dir.exists() {
        process_directory(&paths.claude_session_dir, &mut result)?;
    }

    // Process Codex directory
    if paths.codex_session_dir.exists() {
        process_directory(&paths.codex_session_dir, &mut result)?;
    }

    // Process Gemini directory (special structure: ~/.gemini/tmp/<hash>/chats/*.json)
    if paths.gemini_session_dir.exists() {
        process_gemini_directory(&paths.gemini_session_dir, &mut result)?;
    }

    Ok(result)
}

fn calculate_claude_usage(data: &[Value]) -> Result<UsageResult> {
    let mut conversation_usage: HashMap<String, Value> = HashMap::new();
    let mut tool_counts: HashMap<String, usize> = HashMap::new();

    for record in data {
        if let Some(obj) = record.as_object() {
            if let Some(log_type) = obj.get("type").and_then(|v| v.as_str()) {
                if log_type == "assistant" {
                    if let Some(message) = obj.get("message").and_then(|v| v.as_object()) {
                        // Process usage
                        if let (Some(model), Some(usage)) =
                            (message.get("model"), message.get("usage"))
                        {
                            if let Some(model_str) = model.as_str() {
                                process_claude_usage(&mut conversation_usage, model_str, usage);
                            }
                        }

                        // Count tool calls
                        if let Some(content_array) =
                            message.get("content").and_then(|c| c.as_array())
                        {
                            for item in content_array {
                                if let Some(item_obj) = item.as_object() {
                                    if let Some(item_type) =
                                        item_obj.get("type").and_then(|t| t.as_str())
                                    {
                                        if item_type == "tool_use" {
                                            if let Some(name) =
                                                item_obj.get("name").and_then(|n| n.as_str())
                                            {
                                                *tool_counts
                                                    .entry(name.to_string())
                                                    .or_insert(0) += 1;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(UsageResult {
        tool_call_counts: tool_counts,
        conversation_usage,
    })
}

fn calculate_codex_usage(data: &[Value]) -> Result<UsageResult> {
    let mut conversation_usage: HashMap<String, Value> = HashMap::new();
    let mut tool_counts: HashMap<String, usize> = HashMap::new();
    let mut current_model = String::new();

    for record in data {
        if let Some(obj) = record.as_object() {
            if let Some(log_type) = obj.get("type").and_then(|v| v.as_str()) {
                // Extract model from turn_context
                if log_type == "turn_context" {
                    if let Some(payload) = obj.get("payload").and_then(|p| p.as_object()) {
                        if let Some(model) = payload.get("model").and_then(|m| m.as_str()) {
                            current_model = model.to_string();
                        }
                    }
                }

                // Extract usage from event_msg
                if log_type == "event_msg" {
                    if let Some(payload) = obj.get("payload").and_then(|p| p.as_object()) {
                        if let Some(payload_type) = payload.get("type").and_then(|t| t.as_str()) {
                            if payload_type == "token_count" && !current_model.is_empty() {
                                if let Some(info) = payload.get("info") {
                                    process_codex_usage(
                                        &mut conversation_usage,
                                        &current_model,
                                        info,
                                    );
                                }
                            }
                        }
                    }
                }

                // Count shell tool calls
                if log_type == "response_item" {
                    if let Some(payload) = obj.get("payload").and_then(|p| p.as_object()) {
                        if let Some(payload_type) = payload.get("type").and_then(|t| t.as_str()) {
                            if payload_type == "function_call" {
                                if let Some(name) = payload.get("name").and_then(|n| n.as_str()) {
                                    if name == "shell" {
                                        *tool_counts.entry("Bash".to_string()).or_insert(0) += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(UsageResult {
        tool_call_counts: tool_counts,
        conversation_usage,
    })
}

fn calculate_gemini_usage(data: &[Value]) -> Result<UsageResult> {
    let mut conversation_usage: HashMap<String, Value> = HashMap::new();
    let tool_counts: HashMap<String, usize> = HashMap::new();

    if data.is_empty() {
        return Ok(UsageResult {
            tool_call_counts: tool_counts,
            conversation_usage,
        });
    }

    // Parse the Gemini session
    let session: GeminiSession = serde_json::from_value(data[0].clone())?;

    // Process messages to extract token usage
    for message in &session.messages {
        // Only process gemini messages (not user messages)
        if message.message_type == "gemini" {
            if let (Some(tokens), Some(model)) = (&message.tokens, &message.model) {
                process_gemini_usage(&mut conversation_usage, model, tokens);
            }
        }
    }

    Ok(UsageResult {
        tool_call_counts: tool_counts,
        conversation_usage,
    })
}

fn process_directory<P: AsRef<Path>>(dir: P, result: &mut DateUsageResult) -> Result<()> {
    let files = collect_files_with_dates(&dir, is_json_file)?;

    for file_info in files {
        // Calculate usage for this file
        if let Ok(usage) = calculate_usage_from_jsonl(&file_info.path) {
            // Initialize date entry if it doesn't exist
            let date_entry = result.entry(file_info.modified_date).or_default();

            // Merge usage data
            for (model, usage_value) in usage.conversation_usage {
                if let Some(existing) = date_entry.get_mut(&model) {
                    merge_usage_values(existing, &usage_value);
                } else {
                    date_entry.insert(model, usage_value);
                }
            }
        }
    }

    Ok(())
}

/// Process Gemini directory structure: ~/.gemini/tmp/<hash>/chats/*.json
fn process_gemini_directory<P: AsRef<Path>>(dir: P, result: &mut DateUsageResult) -> Result<()> {
    let files = collect_files_with_dates(&dir, is_gemini_chat_file)?;

    for file_info in files {
        // Calculate usage for this file
        if let Ok(usage) = calculate_usage_from_jsonl(&file_info.path) {
            // Initialize date entry if it doesn't exist
            let date_entry = result.entry(file_info.modified_date).or_default();

            // Merge usage data
            for (model, usage_value) in usage.conversation_usage {
                if let Some(existing) = date_entry.get_mut(&model) {
                    merge_usage_values(existing, &usage_value);
                } else {
                    date_entry.insert(model, usage_value);
                }
            }
        }
    }

    Ok(())
}

fn merge_usage_values(existing: &mut Value, new: &Value) {
    if let (Some(existing_obj), Some(new_obj)) = (existing.as_object_mut(), new.as_object()) {
        // Check if it's Claude/Gemini usage or Codex usage
        if existing_obj.contains_key("input_tokens") {
            // Claude or Gemini usage
            for field in &[
                "input_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
                "output_tokens",
                "thoughts_tokens",
                "tool_tokens",
                "total_tokens",
            ] {
                if let Some(new_value) = new_obj.get(*field).and_then(|v| v.as_i64()) {
                    let current = existing_obj
                        .get(*field)
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    existing_obj.insert(field.to_string(), (current + new_value).into());
                }
            }

            // Merge cache_creation
            if let Some(new_cache) = new_obj.get("cache_creation").and_then(|v| v.as_object()) {
                let existing_cache = existing_obj
                    .entry("cache_creation".to_string())
                    .or_insert_with(|| serde_json::json!({}));

                if let Some(existing_cache_obj) = existing_cache.as_object_mut() {
                    for (key, value) in new_cache {
                        if let Some(v) = value.as_i64() {
                            let current = existing_cache_obj
                                .get(key)
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0);
                            existing_cache_obj.insert(key.clone(), (current + v).into());
                        }
                    }
                }
            }
        } else if existing_obj.contains_key("total_token_usage") {
            // Codex usage
            if let Some(new_total) = new_obj.get("total_token_usage").and_then(|v| v.as_object()) {
                let existing_total = existing_obj
                    .entry("total_token_usage".to_string())
                    .or_insert_with(|| serde_json::json!({}));

                if let Some(existing_total_obj) = existing_total.as_object_mut() {
                    for (key, value) in new_total {
                        if let Some(v) = value.as_i64() {
                            let current = existing_total_obj
                                .get(key)
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0);
                            existing_total_obj.insert(key.clone(), (current + v).into());
                        }
                    }
                }
            }
        }
    }
}
