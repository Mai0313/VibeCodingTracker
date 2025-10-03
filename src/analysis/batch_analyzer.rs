use crate::analysis::analyzer::analyze_jsonl_file;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use walkdir::WalkDir;

/// Aggregated analysis result grouped by date and model
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregatedAnalysisRow {
    pub date: String,
    pub model: String,
    pub edit_lines: usize,
    pub read_lines: usize,
    pub write_lines: usize,
    pub bash_count: usize,
    pub edit_count: usize,
    pub read_count: usize,
    pub todo_write_count: usize,
    pub write_count: usize,
}

/// Analyze all JSONL files from both directories and aggregate by date and model
pub fn analyze_all_sessions() -> Result<Vec<AggregatedAnalysisRow>> {
    let paths = crate::utils::resolve_paths()?;
    let mut aggregated: HashMap<String, AggregatedAnalysisRow> = HashMap::new();

    // Process Claude directory
    if paths.claude_session_dir.exists() {
        process_directory_for_analysis(&paths.claude_session_dir, &mut aggregated)?;
    }

    // Process Codex directory
    if paths.codex_session_dir.exists() {
        process_directory_for_analysis(&paths.codex_session_dir, &mut aggregated)?;
    }

    // Convert HashMap to sorted Vec
    let mut results: Vec<AggregatedAnalysisRow> = aggregated.into_values().collect();
    results.sort_by(|a, b| {
        let date_cmp = a.date.cmp(&b.date);
        if date_cmp == std::cmp::Ordering::Equal {
            a.model.cmp(&b.model)
        } else {
            date_cmp
        }
    });

    Ok(results)
}

fn process_directory_for_analysis<P: AsRef<Path>>(
    dir: P,
    aggregated: &mut HashMap<String, AggregatedAnalysisRow>,
) -> Result<()> {
    if !dir.as_ref().exists() {
        return Ok(());
    }

    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        if let Some(ext) = path.extension() {
            if ext == "jsonl" {
                // Get file modification time for date grouping
                if let Ok(metadata) = std::fs::metadata(path) {
                    if let Ok(modified) = metadata.modified() {
                        let datetime: chrono::DateTime<chrono::Utc> = modified.into();
                        let date_key = datetime.format("%Y-%m-%d").to_string();

                        // Analyze the file
                        if let Ok(analysis) = analyze_jsonl_file(path) {
                            aggregate_analysis_result(aggregated, &date_key, &analysis);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn aggregate_analysis_result(
    aggregated: &mut HashMap<String, AggregatedAnalysisRow>,
    date: &str,
    analysis: &Value,
) {
    // Extract records array
    if let Some(records) = analysis
        .get("records")
        .and_then(|r| r.as_array())
    {
        for record in records {
            if let Some(record_obj) = record.as_object() {
                // Extract model from conversation_usage
                if let Some(conv_usage) = record_obj
                    .get("conversationUsage")
                    .and_then(|c| c.as_object())
                {
                    for (model, _usage) in conv_usage {
                        // Create unique key for date + model
                        let key = format!("{}:{}", date, model);

                        let entry = aggregated.entry(key).or_insert_with(|| {
                            AggregatedAnalysisRow {
                                date: date.to_string(),
                                model: model.clone(),
                                edit_lines: 0,
                                read_lines: 0,
                                write_lines: 0,
                                bash_count: 0,
                                edit_count: 0,
                                read_count: 0,
                                todo_write_count: 0,
                                write_count: 0,
                            }
                        });

                        // Aggregate line counts
                        if let Some(edit_lines) = record_obj
                            .get("totalEditLines")
                            .and_then(|v| v.as_u64())
                        {
                            entry.edit_lines += edit_lines as usize;
                        }
                        if let Some(read_lines) = record_obj
                            .get("totalReadLines")
                            .and_then(|v| v.as_u64())
                        {
                            entry.read_lines += read_lines as usize;
                        }
                        if let Some(write_lines) = record_obj
                            .get("totalWriteLines")
                            .and_then(|v| v.as_u64())
                        {
                            entry.write_lines += write_lines as usize;
                        }

                        // Aggregate tool call counts
                        if let Some(tool_calls) = record_obj
                            .get("toolCallCounts")
                            .and_then(|t| t.as_object())
                        {
                            if let Some(bash) = tool_calls.get("Bash").and_then(|v| v.as_u64()) {
                                entry.bash_count += bash as usize;
                            }
                            if let Some(edit) = tool_calls.get("Edit").and_then(|v| v.as_u64()) {
                                entry.edit_count += edit as usize;
                            }
                            if let Some(read) = tool_calls.get("Read").and_then(|v| v.as_u64()) {
                                entry.read_count += read as usize;
                            }
                            if let Some(todo_write) =
                                tool_calls.get("TodoWrite").and_then(|v| v.as_u64())
                            {
                                entry.todo_write_count += todo_write as usize;
                            }
                            if let Some(write) = tool_calls.get("Write").and_then(|v| v.as_u64())
                            {
                                entry.write_count += write as usize;
                            }
                        }
                    }
                }
            }
        }
    }
}
