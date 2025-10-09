use crate::analysis::analyze_jsonl_file;
use crate::models::DateUsageResult;
use crate::utils::{collect_files_with_dates, is_gemini_chat_file, is_json_file, resolve_paths};
use anyhow::Result;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Extract conversation usage from CodeAnalysis result
fn extract_conversation_usage_from_analysis(analysis: &Value) -> HashMap<String, Value> {
    let Some(records) = analysis.get("records").and_then(|r| r.as_array()) else {
        return HashMap::new();
    };

    // Pre-allocate HashMap with estimated capacity (typical: 1-3 models per session)
    let mut conversation_usage = HashMap::with_capacity(3);

    for record in records {
        let Some(record_obj) = record.as_object() else {
            continue;
        };

        let Some(conv_usage) = record_obj
            .get("conversationUsage")
            .and_then(|c| c.as_object())
        else {
            continue;
        };

        for (model, usage) in conv_usage {
            // Use entry API to avoid double lookup
            conversation_usage
                .entry(model.clone())
                .and_modify(|existing_usage| merge_usage_values(existing_usage, usage))
                .or_insert_with(|| usage.clone());
        }
    }

    conversation_usage
}

/// Calculate usage from all directories
pub fn get_usage_from_directories() -> Result<DateUsageResult> {
    let paths = resolve_paths()?;
    // Use BTreeMap for automatic chronological sorting by date
    let mut result = BTreeMap::new();

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

                // Use entry API to avoid double lookup
                let date_entry = result
                    .entry(file_info.modified_date)
                    .or_insert_with(|| HashMap::with_capacity(3)); // typical: 1-3 models per date

                for (model, usage_value) in conversation_usage {
                    // Use entry API to avoid double lookup
                    date_entry
                        .entry(model)
                        .and_modify(|existing| merge_usage_values(existing, &usage_value))
                        .or_insert(usage_value);
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
