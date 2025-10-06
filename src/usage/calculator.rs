use crate::analysis::analyze_jsonl_file;
use crate::models::DateUsageResult;
use crate::utils::{collect_files_with_dates, is_gemini_chat_file, is_json_file, resolve_paths};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Extract conversation usage from CodeAnalysis result
fn extract_conversation_usage_from_analysis(analysis: &Value) -> HashMap<String, Value> {
    let mut conversation_usage = HashMap::new();

    // Extract records array from CodeAnalysis
    if let Some(records) = analysis.get("records").and_then(|r| r.as_array()) {
        for record in records {
            if let Some(record_obj) = record.as_object() {
                // Extract conversationUsage object
                if let Some(conv_usage) = record_obj
                    .get("conversationUsage")
                    .and_then(|c| c.as_object())
                {
                    // Merge all models into the result
                    for (model, usage) in conv_usage {
                        if let Some(existing_usage) = conversation_usage.get_mut(model) {
                            // Merge usage values
                            merge_usage_values(existing_usage, usage);
                        } else {
                            conversation_usage.insert(model.clone(), usage.clone());
                        }
                    }
                }
            }
        }
    }

    conversation_usage
}

/// Calculate usage from all directories
pub fn get_usage_from_directories() -> Result<DateUsageResult> {
    let paths = resolve_paths()?;
    let mut result = DateUsageResult::new();

    // Process Claude directory
    if paths.claude_session_dir.exists() {
        process_usage_directory(&paths.claude_session_dir, &mut result, is_json_file)?;
    }

    // Process Codex directory
    if paths.codex_session_dir.exists() {
        process_usage_directory(&paths.codex_session_dir, &mut result, is_json_file)?;
    }

    // Process Gemini directory (special structure: ~/.gemini/tmp/<hash>/chats/*.json)
    if paths.gemini_session_dir.exists() {
        process_usage_directory(&paths.gemini_session_dir, &mut result, is_gemini_chat_file)?;
    }

    Ok(result)
}


fn process_usage_directory<P, F>(dir: P, result: &mut DateUsageResult, filter_fn: F) -> Result<()>
where
    P: AsRef<Path>,
    F: Copy + Fn(&Path) -> bool,
{
    let dir = dir.as_ref();
    let files = collect_files_with_dates(dir, filter_fn)?;

    for file_info in files {
        // Analyze file using unified parser
        match analyze_jsonl_file(&file_info.path) {
            Ok(analysis) => {
                // Extract conversation usage from CodeAnalysis result
                let conversation_usage = extract_conversation_usage_from_analysis(&analysis);

                // Initialize date entry if it doesn't exist
                let date_entry = result.entry(file_info.modified_date).or_default();

                // Merge usage data
                for (model, usage_value) in conversation_usage {
                    if let Some(existing) = date_entry.get_mut(&model) {
                        merge_usage_values(existing, &usage_value);
                    } else {
                        date_entry.insert(model, usage_value);
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: Failed to analyze {}: {}",
                    file_info.path.display(),
                    e
                );
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
