use crate::analysis::analyzer::analyze_jsonl_file;
use crate::utils::{collect_files_with_dates, is_gemini_chat_file, is_json_file};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

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

/// Analyze all JSONL/JSON files from all directories and aggregate by date and model
pub fn analyze_all_sessions() -> Result<Vec<AggregatedAnalysisRow>> {
    let paths = crate::utils::resolve_paths()?;
    let mut aggregated: HashMap<String, AggregatedAnalysisRow> = HashMap::new();

    if paths.claude_session_dir.exists() {
        process_analysis_directory(&paths.claude_session_dir, &mut aggregated, is_json_file)?;
    }

    if paths.codex_session_dir.exists() {
        process_analysis_directory(&paths.codex_session_dir, &mut aggregated, is_json_file)?;
    }

    if paths.gemini_session_dir.exists() {
        process_analysis_directory(
            &paths.gemini_session_dir,
            &mut aggregated,
            is_gemini_chat_file,
        )?;
    }

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

/// Result structure for provider-grouped analysis with full records
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderGroupedAnalysis {
    #[serde(rename = "Claude-Code")]
    pub claude: Vec<Value>,
    #[serde(rename = "Codex")]
    pub codex: Vec<Value>,
    #[serde(rename = "Gemini")]
    pub gemini: Vec<Value>,
}

/// Analyze all JSONL/JSON files grouped by provider (claude/codex/gemini)
/// Returns full CodeAnalysis results for each provider
pub fn analyze_all_sessions_by_provider() -> Result<ProviderGroupedAnalysis> {
    let paths = crate::utils::resolve_paths()?;

    let mut claude_results: Vec<Value> = Vec::new();
    let mut codex_results: Vec<Value> = Vec::new();
    let mut gemini_results: Vec<Value> = Vec::new();

    // Process Claude sessions
    if paths.claude_session_dir.exists() {
        process_full_analysis_directory(&paths.claude_session_dir, &mut claude_results, is_json_file)?;
    }

    // Process Codex sessions
    if paths.codex_session_dir.exists() {
        process_full_analysis_directory(&paths.codex_session_dir, &mut codex_results, is_json_file)?;
    }

    // Process Gemini sessions
    if paths.gemini_session_dir.exists() {
        process_full_analysis_directory(
            &paths.gemini_session_dir,
            &mut gemini_results,
            is_gemini_chat_file,
        )?;
    }

    Ok(ProviderGroupedAnalysis {
        claude: claude_results,
        codex: codex_results,
        gemini: gemini_results,
    })
}

fn process_full_analysis_directory<P, F>(
    dir: P,
    results: &mut Vec<Value>,
    filter_fn: F,
) -> Result<()>
where
    P: AsRef<Path>,
    F: Copy + Fn(&Path) -> bool,
{
    let dir = dir.as_ref();
    let files = collect_files_with_dates(dir, filter_fn)?;

    for file_info in files {
        match analyze_jsonl_file(&file_info.path) {
            Ok(analysis) => {
                results.push(analysis);
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

fn process_analysis_directory<P, F>(
    dir: P,
    aggregated: &mut HashMap<String, AggregatedAnalysisRow>,
    filter_fn: F,
) -> Result<()>
where
    P: AsRef<Path>,
    F: Copy + Fn(&Path) -> bool,
{
    let dir = dir.as_ref();
    let files = collect_files_with_dates(dir, filter_fn)?;

    for file_info in files {
        match analyze_jsonl_file(&file_info.path) {
            Ok(analysis) => {
                aggregate_analysis_result(aggregated, &file_info.modified_date, &analysis);
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

fn aggregate_analysis_result(
    aggregated: &mut HashMap<String, AggregatedAnalysisRow>,
    date: &str,
    analysis: &Value,
) {
    if let Some(records) = analysis.get("records").and_then(|r| r.as_array()) {
        for record in records {
            if let Some(record_obj) = record.as_object() {
                if let Some(conv_usage) = record_obj
                    .get("conversationUsage")
                    .and_then(|c| c.as_object())
                {
                    for (model, _usage) in conv_usage {
                        if model.contains("<synthetic>") {
                            continue;
                        }

                        let key = format!("{}:{}", date, model);

                        let entry =
                            aggregated
                                .entry(key)
                                .or_insert_with(|| AggregatedAnalysisRow {
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
                                });

                        if let Some(edit_lines) =
                            record_obj.get("totalEditLines").and_then(|v| v.as_u64())
                        {
                            entry.edit_lines += edit_lines as usize;
                        }
                        if let Some(read_lines) =
                            record_obj.get("totalReadLines").and_then(|v| v.as_u64())
                        {
                            entry.read_lines += read_lines as usize;
                        }
                        if let Some(write_lines) =
                            record_obj.get("totalWriteLines").and_then(|v| v.as_u64())
                        {
                            entry.write_lines += write_lines as usize;
                        }

                        if let Some(tool_calls) =
                            record_obj.get("toolCallCounts").and_then(|t| t.as_object())
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
                            if let Some(write) = tool_calls.get("Write").and_then(|v| v.as_u64()) {
                                entry.write_count += write as usize;
                            }
                        }
                    }
                }
            }
        }
    }
}
