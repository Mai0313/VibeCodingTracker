use crate::analysis::{AnalysisMode, analyze_session_file_typed_as};
use crate::cli::TimeRange;
use crate::constants::{FastHashMap, capacity};
use crate::models::{
    CodeAnalysis, ExtensionType, PerProviderUsage, Provider, ProviderActiveDays, UsageResult,
};
use crate::utils::{
    collect_files_with_dates, is_claude_session_file, is_copilot_session_file, is_gemini_chat_file,
    is_json_file, resolve_paths,
};
use anyhow::Result;
use rayon::prelude::*;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

/// Usage results with provider active day counts for daily averages
pub struct UsageData {
    /// Tokens aggregated across *all* providers, keyed by model name.
    ///
    /// Drives the per-model summary table where, e.g., `claude-sonnet-4-6`
    /// tokens from Claude Code and Copilot CLI share a single row.
    pub models: UsageResult,
    /// Tokens kept separate per source directory, keyed by provider → model.
    ///
    /// Drives the per-provider totals in the summary footer. Keeping this
    /// split at aggregation time avoids the display layer from having to
    /// guess a model's provider from its name, which broke once Copilot CLI
    /// started emitting real (Claude / OpenAI / …) model names.
    pub per_provider: PerProviderUsage,
    pub provider_days: ProviderActiveDays,
}

/// Extracts token usage data from a typed `CodeAnalysis`.
///
/// Reads directly from the typed `conversation_usage` map instead of walking
/// `Value` via `.get(...)`, so no intermediate `serde_json::Value` tree is
/// built or retained here.
fn extract_conversation_usage_from_analysis(analysis: &CodeAnalysis) -> FastHashMap<String, Value> {
    let mut conversation_usage = FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);

    for record in &analysis.records {
        for (model, usage) in &record.conversation_usage {
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
/// Scans Claude Code, Codex, Copilot, and Gemini session files, extracts token usage,
/// and aggregates by model. Returns usage data with provider active day counts.
pub fn get_usage_from_directories(time_range: TimeRange) -> Result<UsageData> {
    let paths = resolve_paths()?;
    let mut result = FastHashMap::with_capacity(capacity::MODEL_COMBINATIONS);
    let mut per_provider = PerProviderUsage::default();

    let mut claude_dates: HashSet<String> = HashSet::new();
    let mut codex_dates: HashSet<String> = HashSet::new();
    let mut copilot_dates: HashSet<String> = HashSet::new();
    let mut gemini_dates: HashSet<String> = HashSet::new();

    if paths.claude_session_dir.exists() {
        // Walks the projects tree recursively, so top-level `<session>.jsonl` logs
        // and `<session>/subagents/agent-*.jsonl` logs are both collected here.
        process_usage_directory(
            &paths.claude_session_dir,
            ExtensionType::ClaudeCode,
            &mut result,
            &mut per_provider.claude,
            &mut claude_dates,
            is_claude_session_file,
            time_range,
        )?;
    }

    if paths.codex_session_dir.exists() {
        process_usage_directory(
            &paths.codex_session_dir,
            ExtensionType::Codex,
            &mut result,
            &mut per_provider.codex,
            &mut codex_dates,
            is_json_file,
            time_range,
        )?;
    }

    if paths.copilot_session_dir.exists() {
        process_usage_directory(
            &paths.copilot_session_dir,
            ExtensionType::Copilot,
            &mut result,
            &mut per_provider.copilot,
            &mut copilot_dates,
            is_copilot_session_file,
            time_range,
        )?;
    }

    if paths.gemini_session_dir.exists() {
        process_usage_directory(
            &paths.gemini_session_dir,
            ExtensionType::Gemini,
            &mut result,
            &mut per_provider.gemini,
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

    Ok(UsageData {
        models: result,
        per_provider,
        provider_days,
    })
}

fn process_usage_directory<P, F>(
    dir: P,
    provider: ExtensionType,
    global_result: &mut UsageResult,
    provider_result: &mut UsageResult,
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

    // Parse each file directly in `UsageOnly` mode, extract the small
    // per-model usage map, then drop the analysis. The provider is fixed by
    // the source directory — we do not re-detect from file contents, which
    // would mis-classify Claude sessions whose first line is a metadata
    // sentinel (`permission-mode`, `file-history-snapshot`) and silently drop
    // their usage. We also deliberately bypass the global file cache here:
    // the `usage` path never needs the heavy `write_file_details` /
    // `edit_file_details` payloads, so caching the full analysis would waste
    // the memory win from `UsageOnly`.
    let file_results: Vec<(String, FastHashMap<String, Value>)> = files
        .par_iter()
        .filter_map(|file_info| {
            match analyze_session_file_typed_as(&file_info.path, provider, AnalysisMode::UsageOnly)
            {
                Ok(analysis) => {
                    let conversation_usage = extract_conversation_usage_from_analysis(&analysis);
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

    // Merge parallel results sequentially (this part is fast). Every
    // per-model usage value is merged into *both* maps:
    //   - `global_result` keeps the cross-provider view used by the main
    //     per-model table,
    //   - `provider_result` keeps the same tokens scoped to this provider
    //     so the summary footer can attribute them to the right source
    //     directory without having to guess from the model name.
    for (date, conversation_usage) in file_results {
        unique_dates.insert(date);

        for (model, usage_value) in conversation_usage {
            provider_result
                .entry(model.clone())
                .and_modify(|existing| merge_usage_values(existing, &usage_value))
                .or_insert_with(|| usage_value.clone());

            global_result
                .entry(model)
                .and_modify(|existing| merge_usage_values(existing, &usage_value))
                .or_insert(usage_value);
        }
    }

    Ok(())
}

impl UsageData {
    /// Returns the per-provider usage slice for `provider`, or an empty
    /// map when the provider is `Unknown` (the display layer's fallthrough
    /// bucket is fed by the global `models` map instead).
    pub fn provider_usage(&self, provider: Provider) -> Option<&UsageResult> {
        self.per_provider.get(provider)
    }
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
        else if existing_obj.contains_key("total_token_usage")
            && let Some(new_total) = new_obj.get("total_token_usage").and_then(|v| v.as_object())
        {
            accumulate_nested_object(existing_obj, "total_token_usage", new_total);
        }
    }
}
