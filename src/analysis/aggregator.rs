use crate::cli::TimeRange;
use crate::constants::{FastHashMap, capacity};
use crate::models::{CodeAnalysis, ExtensionType, ProviderActiveDays};
use crate::session::parser::parse_session_file_as;
use crate::session::read_opencode_analysis;
use crate::session::state::ParseMode;
use crate::utils::{
    COPILOT_SESSION_MAX_DEPTH, collect_files_with_max_depth, is_claude_session_file,
    is_codex_session_file, is_copilot_session_file, is_gemini_session_file,
};
use anyhow::Result;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Single row of aggregated file-operation metrics for one model.
///
/// Counts are summed across every session that used the model in the active
/// time range. The `*_lines` fields total the lines touched by edit/read/write
/// operations; the `*_count` fields total how many times each tool was called.
/// Serializes with camelCase field names to match the `analysis` JSON output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregatedAnalysisRow {
    /// Model name the metrics are grouped under.
    pub model: String,
    /// Total lines changed by `Edit`/`MultiEdit` operations.
    pub edit_lines: usize,
    /// Total lines returned by `Read` operations.
    pub read_lines: usize,
    /// Total lines emitted by `Write` operations.
    pub write_lines: usize,
    /// Number of `Bash` tool calls.
    pub bash_count: usize,
    /// Number of `Edit` tool calls.
    pub edit_count: usize,
    /// Number of `Read` tool calls.
    pub read_count: usize,
    /// Number of `TodoWrite` tool calls.
    pub todo_write_count: usize,
    /// Number of `Write` tool calls.
    pub write_count: usize,
}

/// Bundle of aggregated analysis rows plus the per-provider active-day counts
/// the display layer needs for daily averages.
pub struct AnalysisData {
    /// Rows aggregated across *all* providers, keyed by model name.
    ///
    /// Drives the main per-model table. Same-named models from different
    /// providers (e.g. Copilot CLI + Claude Code both using
    /// `claude-sonnet-4-6`) share a single row here.
    pub rows: Vec<AggregatedAnalysisRow>,
    /// Same aggregation, but partitioned by **source directory** rather
    /// than by model name. Drives the per-provider summary footer so
    /// Copilot-originated sessions cannot be mis-attributed to Claude Code
    /// just because their model name starts with `claude-`.
    pub per_provider: PerProviderAnalysisRows,
    /// Distinct active-day count per provider, used to derive daily averages.
    pub provider_days: ProviderActiveDays,
}

/// Aggregated analysis rows partitioned by the source directory they came from.
///
/// Attribution is by provider directory, not by model name, so a model that
/// appears under more than one provider (e.g. `claude-sonnet-4-6` recorded by
/// both Claude Code and Copilot CLI) lands in the correct bucket.
#[derive(Debug, Default, Clone)]
pub struct PerProviderAnalysisRows {
    /// Rows from the Claude Code session directory.
    pub claude: Vec<AggregatedAnalysisRow>,
    /// Rows from the Codex session directory.
    pub codex: Vec<AggregatedAnalysisRow>,
    /// Rows from the Copilot CLI session directory.
    pub copilot: Vec<AggregatedAnalysisRow>,
    /// Rows from the Gemini CLI session directory.
    pub gemini: Vec<AggregatedAnalysisRow>,
    /// Rows from the OpenCode database.
    pub opencode: Vec<AggregatedAnalysisRow>,
}

/// Aggregate file-operation metrics across every provider's session files,
/// keyed by model.
///
/// Scans the Claude, Codex, Copilot, and Gemini session directories, sums
/// tool-call counts and line counts by model within `time_range`, and returns
/// rows sorted by model name alongside per-provider active-day counts. Each
/// file is parsed in [`ParseMode::UsageOnly`] and dropped immediately, so the
/// global file cache is bypassed. Missing provider directories are skipped, and
/// files that fail to parse are logged to stderr rather than aborting the scan.
///
/// # Errors
///
/// Returns an error if the provider paths cannot be resolved. Directory
/// traversal and metadata errors are currently skipped by the walker rather
/// than propagated.
///
/// # Examples
///
/// ```no_run
/// use vibe_coding_tracker::analysis::aggregate_sessions_by_model;
/// use vibe_coding_tracker::TimeRange;
///
/// let data = aggregate_sessions_by_model(TimeRange::All)?;
/// for row in &data.rows {
///     println!("{}: {} edit lines", row.model, row.edit_lines);
/// }
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn aggregate_sessions_by_model(time_range: TimeRange) -> Result<AnalysisData> {
    let paths = crate::utils::resolve_paths()?;
    let mut aggregated: FastHashMap<String, AggregatedAnalysisRow> =
        FastHashMap::with_capacity(capacity::MODEL_COMBINATIONS);

    let mut claude_aggregated: FastHashMap<String, AggregatedAnalysisRow> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);
    let mut codex_aggregated: FastHashMap<String, AggregatedAnalysisRow> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);
    let mut copilot_aggregated: FastHashMap<String, AggregatedAnalysisRow> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);
    let mut gemini_aggregated: FastHashMap<String, AggregatedAnalysisRow> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);

    let mut opencode_aggregated: FastHashMap<String, AggregatedAnalysisRow> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);

    let mut claude_dates: HashSet<String> = HashSet::new();
    let mut codex_dates: HashSet<String> = HashSet::new();
    let mut copilot_dates: HashSet<String> = HashSet::new();
    let mut gemini_dates: HashSet<String> = HashSet::new();
    let mut opencode_dates: HashSet<String> = HashSet::new();

    if paths.claude_session_dir.exists() {
        // Walks the projects tree recursively, so top-level `<session>.jsonl` logs
        // and `<session>/subagents/agent-*.jsonl` logs are both collected here.
        aggregate_sessions_in_directory(
            &paths.claude_session_dir,
            ExtensionType::ClaudeCode,
            &mut aggregated,
            &mut claude_aggregated,
            &mut claude_dates,
            is_claude_session_file,
            time_range,
            None,
        )?;
    }

    if paths.codex_session_dir.exists() {
        aggregate_sessions_in_directory(
            &paths.codex_session_dir,
            ExtensionType::Codex,
            &mut aggregated,
            &mut codex_aggregated,
            &mut codex_dates,
            is_codex_session_file,
            time_range,
            None,
        )?;
    }

    if paths.copilot_session_dir.exists() {
        // `events.jsonl` always lives exactly two levels under
        // `session-state/`; see the rationale in `usage::calculator`.
        aggregate_sessions_in_directory(
            &paths.copilot_session_dir,
            ExtensionType::Copilot,
            &mut aggregated,
            &mut copilot_aggregated,
            &mut copilot_dates,
            is_copilot_session_file,
            time_range,
            Some(COPILOT_SESSION_MAX_DEPTH),
        )?;
    }

    if paths.gemini_session_dir.exists() {
        aggregate_sessions_in_directory(
            &paths.gemini_session_dir,
            ExtensionType::Gemini,
            &mut aggregated,
            &mut gemini_aggregated,
            &mut gemini_dates,
            is_gemini_session_file,
            time_range,
            None,
        )?;
    }

    // OpenCode lives in a single SQLite database rather than a session
    // directory, so it is read directly instead of walked.
    if paths.opencode_db.exists() {
        aggregate_opencode_sessions(
            &paths.opencode_db,
            &mut aggregated,
            &mut opencode_aggregated,
            &mut opencode_dates,
            time_range,
        )?;
    }

    let mut all_dates: HashSet<&String> = HashSet::new();
    all_dates.extend(claude_dates.iter());
    all_dates.extend(codex_dates.iter());
    all_dates.extend(copilot_dates.iter());
    all_dates.extend(gemini_dates.iter());
    all_dates.extend(opencode_dates.iter());

    let provider_days = ProviderActiveDays {
        claude: claude_dates.len(),
        codex: codex_dates.len(),
        copilot: copilot_dates.len(),
        gemini: gemini_dates.len(),
        opencode: opencode_dates.len(),
        total: all_dates.len(),
    };

    let mut results: Vec<AggregatedAnalysisRow> = aggregated.into_values().collect();
    results.sort_unstable_by(|a, b| a.model.cmp(&b.model));

    let per_provider = PerProviderAnalysisRows {
        claude: into_sorted_rows(claude_aggregated),
        codex: into_sorted_rows(codex_aggregated),
        copilot: into_sorted_rows(copilot_aggregated),
        gemini: into_sorted_rows(gemini_aggregated),
        opencode: into_sorted_rows(opencode_aggregated),
    };

    Ok(AnalysisData {
        rows: results,
        per_provider,
        provider_days,
    })
}

/// Drains a model-keyed map into a `Vec` sorted by model name.
fn into_sorted_rows(map: FastHashMap<String, AggregatedAnalysisRow>) -> Vec<AggregatedAnalysisRow> {
    let mut v: Vec<AggregatedAnalysisRow> = map.into_values().collect();
    v.sort_unstable_by(|a, b| a.model.cmp(&b.model));
    v
}

/// Parses every session file under one provider directory and folds its
/// per-model metrics into both the cross-provider `aggregated` map and the
/// provider-scoped `provider_aggregated` map.
///
/// `filter_fn` selects which files belong to `provider`, `time_range` bounds
/// which files are considered, and `max_depth` caps the directory walk (used to
/// skip Copilot per-session snapshot subtrees). Each parsed file's modified
/// date is recorded in `unique_dates` to feed active-day counts. Files that
/// fail to parse are logged to stderr and skipped, not treated as errors.
///
/// # Errors
///
/// Returns an error only if the candidate-file collector returns one. The
/// current collector skips traversal and metadata errors.
#[allow(clippy::too_many_arguments)] // per-provider helper; struct-wrapping the args would hurt readability
fn aggregate_sessions_in_directory<P, F>(
    dir: P,
    provider: ExtensionType,
    aggregated: &mut FastHashMap<String, AggregatedAnalysisRow>,
    provider_aggregated: &mut FastHashMap<String, AggregatedAnalysisRow>,
    unique_dates: &mut HashSet<String>,
    filter_fn: F,
    time_range: TimeRange,
    max_depth: Option<usize>,
) -> Result<()>
where
    P: AsRef<Path>,
    F: Copy + Fn(&Path) -> bool + Sync + Send,
{
    let dir = dir.as_ref();
    let files = collect_files_with_max_depth(dir, filter_fn, time_range, max_depth)?;

    // Aggregated analysis only reads counters — no need for `write_file_details`
    // bodies or `edit_file_details` strings. Run in `UsageOnly` and skip the
    // global cache so each file's analysis drops as soon as we've scraped the
    // tool counts and usage totals. Provider is fixed by the source directory.
    let file_aggregations: Vec<(String, CodeAnalysis)> = files
        .par_iter()
        .filter_map(|file_info| {
            match parse_session_file_as(&file_info.path, provider, ParseMode::UsageOnly) {
                Ok(analysis) => Some((file_info.modified_date.clone(), analysis)),
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to parse {}: {}",
                        file_info.path.display(),
                        e
                    );
                    None
                }
            }
        })
        .collect();

    // Merge parallel results sequentially (this part is fast). Every
    // per-model row is accumulated into *both* the cross-provider
    // `aggregated` map (drives the main table) and the per-provider
    // `provider_aggregated` map (drives the per-provider footer) so
    // the display layer does not have to infer provenance from the
    // model name.
    for (date, analysis) in file_aggregations {
        unique_dates.insert(date);
        aggregate_analysis_result(aggregated, &analysis);
        aggregate_analysis_result(provider_aggregated, &analysis);
    }

    Ok(())
}

/// Reads OpenCode's SQLite database and folds each session's metrics into both
/// the cross-provider and OpenCode-scoped aggregation maps.
///
/// Mirrors [`aggregate_sessions_in_directory`] but sources sessions from the
/// database (via [`read_opencode_analysis`] in [`ParseMode::UsageOnly`]) instead
/// of a directory walk. Each session's `time_updated` date is recorded in
/// `unique_dates` for the active-day count.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or queried.
fn aggregate_opencode_sessions(
    db_path: &Path,
    aggregated: &mut FastHashMap<String, AggregatedAnalysisRow>,
    provider_aggregated: &mut FastHashMap<String, AggregatedAnalysisRow>,
    unique_dates: &mut HashSet<String>,
    time_range: TimeRange,
) -> Result<()> {
    let sessions = read_opencode_analysis(db_path, time_range, ParseMode::UsageOnly)?;

    for (date, analysis) in sessions {
        unique_dates.insert(date);
        aggregate_analysis_result(aggregated, &analysis);
        aggregate_analysis_result(provider_aggregated, &analysis);
    }

    Ok(())
}

/// Folds one parsed session's per-model counters into `aggregated`.
///
/// Each model in the session's `conversation_usage` gets (or creates) a row,
/// and that record's line and tool-call counts are added in. Synthetic models
/// (model name containing `<synthetic>`) are skipped so placeholder usage does
/// not pollute the per-model breakdown.
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
