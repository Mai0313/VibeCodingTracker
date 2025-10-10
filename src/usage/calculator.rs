use crate::cache::global_cache;
use crate::constants::{FastHashMap, capacity};
use crate::models::DateUsageResult;
use crate::utils::{collect_files_with_dates, is_gemini_chat_file, is_json_file, resolve_paths};
use anyhow::Result;
use rayon::prelude::*;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Extracts token usage data from CodeAnalysis records
fn extract_conversation_usage_from_analysis(analysis: &Value) -> FastHashMap<String, Value> {
    let Some(records) = analysis.get("records").and_then(|r| r.as_array()) else {
        return FastHashMap::default();
    };

    // Pre-allocate HashMap using centralized capacity constant
    let mut conversation_usage = FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);

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

/// Aggregates token usage from all AI provider session directories
///
/// Scans Claude Code, Codex, and Gemini session files, extracts token usage,
/// and aggregates by date and model. Returns a BTreeMap sorted chronologically.
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
    F: Copy + Fn(&Path) -> bool + Sync + Send,
{
    let dir = dir.as_ref();
    let files = collect_files_with_dates(dir, filter_fn)?;

    // Process files in parallel with caching for better performance
    let file_results: Vec<(String, FastHashMap<String, Value>)> = files
        .par_iter()
        .filter_map(|file_info| {
            match global_cache().get_or_parse(&file_info.path) {
                Ok(analysis_arc) => {
                    // Use Arc to avoid deep cloning the entire analysis
                    let conversation_usage =
                        extract_conversation_usage_from_analysis(&analysis_arc);
                    Some((file_info.modified_date.clone(), conversation_usage))
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to analyze {}: {}",
                        file_info.path.display(),
                        e
                    );
                    None
                }
            }
        })
        .collect();

    // Merge parallel results sequentially (this part is fast)
    for (date, conversation_usage) in file_results {
        // Use entry API to avoid double lookup
        let date_entry = result
            .entry(date)
            .or_insert_with(|| FastHashMap::with_capacity(capacity::MODELS_PER_SESSION));

        for (model, usage_value) in conversation_usage {
            // Use entry API to avoid double lookup
            date_entry
                .entry(model)
                .and_modify(|existing| merge_usage_values(existing, &usage_value))
                .or_insert(usage_value);
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
