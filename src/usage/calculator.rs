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

    if let Some(records) = analysis.get("records").and_then(|r| r.as_array()) {
        for record in records {
            if let Some(record_obj) = record.as_object() {
                if let Some(conv_usage) = record_obj
                    .get("conversationUsage")
                    .and_then(|c| c.as_object())
                {
                    for (model, usage) in conv_usage {
                        if let Some(existing_usage) = conversation_usage.get_mut(model) {
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

    if paths.claude_session_dir.exists() {
        process_usage_directory(&paths.claude_session_dir, &mut result, is_json_file)?;
    }

    if paths.codex_session_dir.exists() {
        process_usage_directory(&paths.codex_session_dir, &mut result, is_json_file)?;
    }

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
        match analyze_jsonl_file(&file_info.path) {
            Ok(analysis) => {
                let conversation_usage = extract_conversation_usage_from_analysis(&analysis);
                let date_entry = result.entry(file_info.modified_date).or_default();

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
    use crate::utils::{accumulate_i64_fields, accumulate_nested_object};

    if let (Some(existing_obj), Some(new_obj)) = (existing.as_object_mut(), new.as_object()) {
        // Handle Claude/Gemini format (has input_tokens)
        if existing_obj.contains_key("input_tokens") {
            accumulate_i64_fields(
                existing_obj,
                new_obj,
                &[
                    "input_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                    "output_tokens",
                    "thoughts_tokens",
                    "tool_tokens",
                    "total_tokens",
                ],
            );

            if let Some(new_cache) = new_obj.get("cache_creation").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "cache_creation", new_cache);
            }
        }
        // Handle Codex format (has total_token_usage)
        else if existing_obj.contains_key("total_token_usage") {
            if let Some(new_total) = new_obj.get("total_token_usage").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "total_token_usage", new_total);
            }
        }
    }
}
