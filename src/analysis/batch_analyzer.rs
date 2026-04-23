use crate::analysis::analyzer::analyze_session_file_typed_as;
use crate::analysis::common_state::AnalysisMode;
use crate::cache::global_cache;
use crate::cli::TimeRange;
use crate::constants::{FastHashMap, capacity};
use crate::models::{CodeAnalysis, ExtensionType, ProviderActiveDays};
use crate::utils::{
    collect_files_with_dates, is_claude_session_file, is_copilot_session_file,
    is_gemini_chat_file, is_json_file,
};
use anyhow::Result;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

/// Single row of aggregated metrics grouped by model
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregatedAnalysisRow {
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

/// Analysis results with provider active day counts for daily averages
pub struct AnalysisData {
    pub rows: Vec<AggregatedAnalysisRow>,
    pub provider_days: ProviderActiveDays,
}

/// Analyzes all session files across providers and aggregates file operation metrics
///
/// Scans Claude, Codex, Copilot, and Gemini session directories, aggregates tool call counts
/// and line counts by model, then returns sorted results with provider active day counts.
pub fn analyze_all_sessions(time_range: TimeRange) -> Result<AnalysisData> {
    let paths = crate::utils::resolve_paths()?;
    let mut aggregated: FastHashMap<String, AggregatedAnalysisRow> =
        FastHashMap::with_capacity(capacity::MODEL_COMBINATIONS);

    let mut claude_dates: HashSet<String> = HashSet::new();
    let mut codex_dates: HashSet<String> = HashSet::new();
    let mut copilot_dates: HashSet<String> = HashSet::new();
    let mut gemini_dates: HashSet<String> = HashSet::new();

    if paths.claude_session_dir.exists() {
        // Walks the projects tree recursively, so top-level `<session>.jsonl` logs
        // and `<session>/subagents/agent-*.jsonl` logs are both collected here.
        process_analysis_directory(
            &paths.claude_session_dir,
            ExtensionType::ClaudeCode,
            &mut aggregated,
            &mut claude_dates,
            is_claude_session_file,
            time_range,
        )?;
    }

    if paths.codex_session_dir.exists() {
        process_analysis_directory(
            &paths.codex_session_dir,
            ExtensionType::Codex,
            &mut aggregated,
            &mut codex_dates,
            is_json_file,
            time_range,
        )?;
    }

    if paths.copilot_session_dir.exists() {
        process_analysis_directory(
            &paths.copilot_session_dir,
            ExtensionType::Copilot,
            &mut aggregated,
            &mut copilot_dates,
            is_copilot_session_file,
            time_range,
        )?;
    }

    if paths.gemini_session_dir.exists() {
        process_analysis_directory(
            &paths.gemini_session_dir,
            ExtensionType::Gemini,
            &mut aggregated,
            &mut gemini_dates,
            is_gemini_chat_file,
            time_range,
        )?;
    }

    let mut all_dates: HashSet<&String> = HashSet::new();
    all_dates.extend(claude_dates.iter());
    all_dates.extend(codex_dates.iter());
    all_dates.extend(copilot_dates.iter());
    all_dates.extend(gemini_dates.iter());

    let provider_days = ProviderActiveDays {
        claude: claude_dates.len(),
        codex: codex_dates.len(),
        copilot: copilot_dates.len(),
        gemini: gemini_dates.len(),
        total: all_dates.len(),
    };

    let mut results: Vec<AggregatedAnalysisRow> = aggregated.into_values().collect();
    results.sort_unstable_by(|a, b| a.model.cmp(&b.model));

    Ok(AnalysisData {
        rows: results,
        provider_days,
    })
}

/// Complete CodeAnalysis results organized by AI provider.
///
/// Each record is stored as an `Arc<CodeAnalysis>` — the same typed struct
/// held by the global file cache — so no deep-clone happens to ferry results
/// out of the worker pool, and no intermediate `Value` is materialised.
/// serde's `rc` feature lets `Arc<CodeAnalysis>` serialise as the underlying
/// struct, so the emitted JSON is unchanged. The struct is output-only, which
/// is why it does not derive `Deserialize`.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderGroupedAnalysis {
    #[serde(rename = "Claude-Code")]
    pub claude: Vec<Arc<CodeAnalysis>>,
    #[serde(rename = "Codex")]
    pub codex: Vec<Arc<CodeAnalysis>>,
    #[serde(rename = "Copilot-CLI")]
    pub copilot: Vec<Arc<CodeAnalysis>>,
    #[serde(rename = "Gemini")]
    pub gemini: Vec<Arc<CodeAnalysis>>,
}

/// Analyzes all session files and returns complete records grouped by provider
///
/// Unlike `analyze_all_sessions()` which aggregates metrics, this function preserves
/// full CodeAnalysis records for each session file.
pub fn analyze_all_sessions_by_provider(time_range: TimeRange) -> Result<ProviderGroupedAnalysis> {
    let paths = crate::utils::resolve_paths()?;

    let mut claude_results: Vec<Arc<CodeAnalysis>> = Vec::new();
    let mut codex_results: Vec<Arc<CodeAnalysis>> = Vec::new();
    let mut copilot_results: Vec<Arc<CodeAnalysis>> = Vec::new();
    let mut gemini_results: Vec<Arc<CodeAnalysis>> = Vec::new();

    // Process Claude sessions (including subagents/ sublogs)
    if paths.claude_session_dir.exists() {
        process_full_analysis_directory(
            &paths.claude_session_dir,
            ExtensionType::ClaudeCode,
            &mut claude_results,
            is_claude_session_file,
            time_range,
        )?;
    }

    // Process Codex sessions
    if paths.codex_session_dir.exists() {
        process_full_analysis_directory(
            &paths.codex_session_dir,
            ExtensionType::Codex,
            &mut codex_results,
            is_json_file,
            time_range,
        )?;
    }

    // Process Copilot sessions
    if paths.copilot_session_dir.exists() {
        process_full_analysis_directory(
            &paths.copilot_session_dir,
            ExtensionType::Copilot,
            &mut copilot_results,
            is_copilot_session_file,
            time_range,
        )?;
    }

    // Process Gemini sessions
    if paths.gemini_session_dir.exists() {
        process_full_analysis_directory(
            &paths.gemini_session_dir,
            ExtensionType::Gemini,
            &mut gemini_results,
            is_gemini_chat_file,
            time_range,
        )?;
    }

    Ok(ProviderGroupedAnalysis {
        claude: claude_results,
        codex: codex_results,
        copilot: copilot_results,
        gemini: gemini_results,
    })
}

fn process_full_analysis_directory<P, F>(
    dir: P,
    provider: ExtensionType,
    results: &mut Vec<Arc<CodeAnalysis>>,
    filter_fn: F,
    time_range: TimeRange,
) -> Result<()>
where
    P: AsRef<Path>,
    F: Copy + Fn(&Path) -> bool + Sync + Send,
{
    let dir = dir.as_ref();
    let files = collect_files_with_dates(dir, filter_fn, time_range)?;

    // Parallel parse through the global cache. The provider is fixed by the
    // source directory, so the cache dispatches to the right analyzer without
    // re-inspecting the file's contents.
    let analyzed: Vec<Arc<CodeAnalysis>> = files
        .par_iter()
        .filter_map(
            |file_info| match global_cache().get_or_parse_as(&file_info.path, provider) {
                Ok(analysis_arc) => Some(analysis_arc),
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to analyze {}: {}",
                        file_info.path.display(),
                        e
                    );
                    None
                }
            },
        )
        .collect();

    results.extend(analyzed);
    Ok(())
}

fn process_analysis_directory<P, F>(
    dir: P,
    provider: ExtensionType,
    aggregated: &mut FastHashMap<String, AggregatedAnalysisRow>,
    unique_dates: &mut HashSet<String>,
    filter_fn: F,
    time_range: TimeRange,
) -> Result<()>
where
    P: AsRef<Path>,
    F: Copy + Fn(&Path) -> bool + Sync + Send,
{
    let dir = dir.as_ref();
    let files = collect_files_with_dates(dir, filter_fn, time_range)?;

    // Aggregated analysis only reads counters — no need for `write_file_details`
    // bodies or `edit_file_details` strings. Run in `UsageOnly` and skip the
    // global cache so each file's analysis drops as soon as we've scraped the
    // tool counts and usage totals. Provider is fixed by the source directory.
    let file_aggregations: Vec<(String, CodeAnalysis)> = files
        .par_iter()
        .filter_map(|file_info| {
            match analyze_session_file_typed_as(&file_info.path, provider, AnalysisMode::UsageOnly)
            {
                Ok(analysis) => Some((file_info.modified_date.clone(), analysis)),
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
    for (date, analysis) in file_aggregations {
        unique_dates.insert(date);
        aggregate_analysis_result(aggregated, &analysis);
    }

    Ok(())
}

fn aggregate_analysis_result(
    aggregated: &mut FastHashMap<String, AggregatedAnalysisRow>,
    analysis: &CodeAnalysis,
) {
    for record in &analysis.records {
        for model in record.conversation_usage.keys() {
            if model.contains("<synthetic>") {
                continue;
            }

            let entry = aggregated
                .entry(model.clone())
                .or_insert_with(|| AggregatedAnalysisRow {
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

            entry.edit_lines += record.total_edit_lines;
            entry.read_lines += record.total_read_lines;
            entry.write_lines += record.total_write_lines;

            entry.bash_count += record.tool_call_counts.bash;
            entry.edit_count += record.tool_call_counts.edit;
            entry.read_count += record.tool_call_counts.read;
            entry.todo_write_count += record.tool_call_counts.todo_write;
            entry.write_count += record.tool_call_counts.write;
        }
    }
}
