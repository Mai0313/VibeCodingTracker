use crate::cli::TimeRange;
use crate::config::ProvidersConfig;
use crate::constants::{FastHashMap, capacity};
use crate::models::{
    CodeAnalysis, CodeAnalysisRecord, CodeAnalysisToolCalls, ExtensionType, ProviderActiveDays,
};
use crate::session::cursor::{
    discover_cursor_store_dbs, load_conversation_model_snapshot,
    read_cursor_analysis_with_diagnostics, read_store_analysis,
};
use crate::session::diagnostics::{
    AnalysisFact, AnalysisFactEffect, AnalysisMetrics, DatabaseAnalysisRow, ToolFactStatus,
    UsageFact, fallback_facts,
};
use crate::session::opencode::read_opencode_analysis_with_diagnostics;
use crate::session::parser::{
    SessionFileParseDiagnostics, parse_session_file_as_with_diagnostics,
    parse_session_file_with_facts_and_diagnostics,
};
use crate::session::sqlite::is_cacheable_sqlite_failure;
use crate::session::state::ParseMode;
use crate::summary_cache::{
    CachedSourceSummary, CompactSourceSummary, SourceFingerprint, SummaryCacheKey, SummaryKind,
    SummaryScanCache, provider_scan_rank,
};
use crate::usage::merge_usage_values;
use crate::utils::directory::{FileInfo, collect_files_with_max_depth_diagnostics};
use crate::utils::{
    COPILOT_SESSION_MAX_DEPTH, GROK_SESSION_MAX_DEPTH, HelperPaths, get_current_user,
    get_machine_id, is_claude_session_file, is_codex_session_file, is_copilot_session_file,
    is_gemini_session_file, is_grok_session_file,
};
use anyhow::Result;
use rayon::prelude::*;
use serde::{Deserialize, Serialize, Serializer, ser::SerializeSeq};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Single row of aggregated file-operation metrics for one model.
///
/// Counts are summed across every session that used the model in the active
/// time range. The `*_lines` fields total the lines touched by edit/read/write
/// operations; the `*_count` fields total how many times each tool was called.
/// Serializes with camelCase field names for library callers that persist a
/// compact projection. The CLI's `analysis --json` output uses
/// [`AnalysisDataset`] instead.
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
#[derive(Debug, Clone)]
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

/// A compact summary plus diagnostics from the source scan that produced it.
///
/// The legacy aggregation entry points return only [`AnalysisData`] for TUI
/// callers that intentionally operate on best-effort data. Noninteractive
/// callers can use the `*_with_diagnostics` variants and reject an all-failed
/// scan or surface partial failures before rendering `data`.
pub struct AnalysisAggregation {
    /// Successfully parsed metrics, even when some other sources failed.
    pub data: AnalysisData,
    /// Candidate, success, and failure information for the scan.
    pub diagnostics: AnalysisCollectionDiagnostics,
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
    /// Rows from the Grok CLI session directory.
    pub grok: Vec<AggregatedAnalysisRow>,
    /// Rows from the OpenCode database.
    pub opencode: Vec<AggregatedAnalysisRow>,
    /// Rows from the Cursor chat stores.
    pub cursor: Vec<AggregatedAnalysisRow>,
}

/// One parsed session in the canonical batch-analysis dataset.
///
/// `provider` and `date` retain the source provenance needed by the summary
/// projection. They are intentionally not part of the public JSON shape; the
/// nested [`CodeAnalysis`] is the same object emitted by single-file analysis.
#[derive(Debug, Clone)]
pub struct AnalysisSession {
    /// Provider selected from the source directory or database.
    pub provider: ExtensionType,
    /// Local `YYYY-MM-DD` date used by the active-day summary.
    pub date: String,
    /// Complete normalized parser result for this session.
    pub analysis: CodeAnalysis,
}

/// One independently readable analysis source that could not be collected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisCollectionFailure {
    /// Provider whose file, database, or store collection failed.
    pub provider: ExtensionType,
    /// File or collection root passed to the parser or database reader.
    pub source: PathBuf,
    /// Parser or reader error, or the reason a parsed result was rejected.
    pub error: String,
}

/// Diagnostics retained alongside a batch analysis result.
///
/// A candidate is the smallest source this layer can read independently. Each
/// JSONL file and each Cursor store is one candidate. OpenCode's database is one
/// candidate. `parsed` counts candidates read successfully, not the number of
/// sessions emitted by a database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalysisCollectionDiagnostics {
    /// Number of independently readable sources discovered.
    pub candidates: usize,
    /// Number of candidates parsed or read successfully.
    pub parsed: usize,
    /// Failures in deterministic provider and source order.
    pub failures: Vec<AnalysisCollectionFailure>,
}

impl AnalysisCollectionDiagnostics {
    /// Returns whether at least one candidate failed.
    pub fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }

    /// Returns whether candidates existed but none could be parsed.
    pub fn all_failed(&self) -> bool {
        self.candidates > 0 && self.parsed == 0
    }

    /// Returns whether the scan contains both successful and failed sources.
    pub fn partially_failed(&self) -> bool {
        self.parsed > 0 && self.has_failures()
    }
}

/// Canonical batch-analysis dataset before any display-specific projection.
///
/// The in-memory entries retain provider and date provenance. Serialization is
/// deliberately transparent: the JSON value is an array of [`CodeAnalysis`]
/// objects, so every element has exactly the same schema as a single-file
/// golden result.
#[derive(Debug, Clone, Default)]
pub struct AnalysisDataset {
    /// Sessions in deterministic provider and source order.
    pub sessions: Vec<AnalysisSession>,
    /// Candidate, success, and failure information from collection.
    ///
    /// The custom [`Serialize`] implementation deliberately omits this field
    /// so canonical batch JSON remains a transparent `CodeAnalysis[]`.
    pub diagnostics: AnalysisCollectionDiagnostics,
}

impl AnalysisDataset {
    /// Creates a dataset from already-normalized sessions and diagnostics.
    pub fn new(sessions: Vec<AnalysisSession>, diagnostics: AnalysisCollectionDiagnostics) -> Self {
        Self {
            sessions,
            diagnostics,
        }
    }

    /// Returns whether the dataset contains no parsed sessions.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Returns the number of parsed sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Projects this canonical dataset into compact display summaries.
    ///
    /// A public `CodeAnalysis` record with several conversation models no
    /// longer carries invocation-level ownership, so its nonzero metrics are
    /// grouped under `unknown`. Use the `aggregate_sessions_*` entry points
    /// when exact event-level model attribution is required.
    pub fn summarize(&self) -> AnalysisData {
        project_analysis_dataset(self)
    }

    /// Projects the dataset while retaining its collection diagnostics.
    pub fn summarize_with_diagnostics(&self) -> AnalysisAggregation {
        AnalysisAggregation {
            data: self.summarize(),
            diagnostics: self.diagnostics.clone(),
        }
    }
}

impl Serialize for AnalysisDataset {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.sessions.len()))?;
        for session in &self.sessions {
            sequence.serialize_element(&session.analysis)?;
        }
        sequence.end()
    }
}

/// Aggregate file-operation metrics across every provider's session files,
/// keyed by model.
///
/// Scans every enabled analysis provider's session files or database, sums
/// tool-call counts and line counts by model within `time_range`, and returns
/// rows sorted by model name alongside per-provider active-day counts. Parsed
/// sessions are folded directly into the compact summary in
/// [`ParseMode::UsageOnly`], so this path never retains a cross-provider
/// [`AnalysisDataset`]. Missing provider directories are skipped, and
/// individual source failures are logged rather than aborting the scan.
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
    aggregate_sessions_by_model_with(time_range, ProvidersConfig::default())
}

/// [`aggregate_sessions_by_model`] with explicit per-provider toggles (from
/// `~/.vct/config.toml`). A disabled provider is skipped entirely.
pub fn aggregate_sessions_by_model_with(
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<AnalysisData> {
    Ok(aggregate_sessions_by_model_with_diagnostics(time_range, providers)?.data)
}

/// Streaming counterpart of [`aggregate_sessions_by_model_with`] that also
/// returns source diagnostics for noninteractive callers.
///
/// Parsed sessions are added to the summary as each provider completes. Only
/// one provider's parallel parse results are temporarily retained at a time.
pub fn aggregate_sessions_by_model_with_diagnostics(
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<AnalysisAggregation> {
    aggregate_sessions_by_model_from_paths_with_diagnostics(
        &crate::utils::resolve_paths()?,
        time_range,
        providers,
    )
}

/// Aggregates file-operation metrics from provider session directories rooted at
/// an explicit [`HelperPaths`].
///
/// The env-free, injectable counterpart of [`aggregate_sessions_by_model`]:
/// every provider path comes from `paths` rather than the resolved home
/// directory, so tests can point them at a temp tree and exercise the real
/// aggregation without mutating process-global `HOME`.
pub fn aggregate_sessions_by_model_from_paths(
    paths: &HelperPaths,
    time_range: TimeRange,
) -> Result<AnalysisData> {
    aggregate_sessions_by_model_from_paths_with(paths, time_range, ProvidersConfig::default())
}

/// [`aggregate_sessions_by_model_from_paths`] with explicit provider toggles.
pub fn aggregate_sessions_by_model_from_paths_with(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<AnalysisData> {
    Ok(aggregate_sessions_by_model_from_paths_with_diagnostics(paths, time_range, providers)?.data)
}

/// Env-free streaming aggregation with source diagnostics.
pub fn aggregate_sessions_by_model_from_paths_with_diagnostics(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<AnalysisAggregation> {
    let mut cache = SummaryScanCache::default();
    aggregate_sessions_by_model_from_paths_with_cache(paths, time_range, providers, &mut cache)
}

/// Collects the canonical batch-analysis dataset from the current user's home.
///
/// Providers are always appended in this order: Claude, Codex, Copilot,
/// Gemini, Grok, OpenCode, Cursor. `mode` controls only detail retention; every
/// scalar counter remains available to downstream projections.
pub fn collect_analysis_sessions_with(
    time_range: TimeRange,
    providers: ProvidersConfig,
    mode: ParseMode,
) -> Result<AnalysisDataset> {
    collect_analysis_sessions_from_paths_with(
        &crate::utils::resolve_paths()?,
        time_range,
        providers,
        mode,
    )
}

/// Collects the canonical batch-analysis dataset from explicit provider paths.
///
/// File-backed providers are ordered by path before parallel parsing. Database
/// results are ordered by date and their database source identity. Together with the
/// fixed provider order this makes serialized batch JSON deterministic.
pub fn collect_analysis_sessions_from_paths_with(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    mode: ParseMode,
) -> Result<AnalysisDataset> {
    let mut sources = Vec::new();
    let diagnostics = visit_analysis_sessions_from_paths_with(
        paths,
        time_range,
        providers,
        mode,
        &mut |source| sources.push(source),
    )?;
    let sessions = materialize_analysis_sources(sources, time_range, mode);
    Ok(AnalysisDataset {
        sessions,
        diagnostics,
    })
}

/// Visits parsed sessions in deterministic provider and source order.
///
/// The canonical collector passes a `Vec::push` visitor and retains every
/// session. Summary aggregation passes an [`AnalysisProjection`] visitor and
/// drops each parsed session immediately after folding it. This keeps source
/// discovery, diagnostics, and ordering identical across both paths.
fn visit_analysis_sessions_from_paths_with<F>(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    mode: ParseMode,
    visitor: &mut F,
) -> Result<AnalysisCollectionDiagnostics>
where
    F: FnMut(CollectedAnalysisSource),
{
    let mut diagnostics = AnalysisCollectionDiagnostics::default();

    if providers.claude {
        visit_file_sessions(
            &paths.claude_session_dir,
            ExtensionType::ClaudeCode,
            is_claude_session_file,
            time_range,
            None,
            mode,
            &mut diagnostics,
            visitor,
        )?;
    }

    if providers.codex {
        visit_file_sessions(
            &paths.codex_session_dir,
            ExtensionType::Codex,
            is_codex_session_file,
            time_range,
            None,
            mode,
            &mut diagnostics,
            visitor,
        )?;
    }

    if providers.copilot {
        visit_file_sessions(
            &paths.copilot_session_dir,
            ExtensionType::Copilot,
            is_copilot_session_file,
            time_range,
            Some(COPILOT_SESSION_MAX_DEPTH),
            mode,
            &mut diagnostics,
            visitor,
        )?;
    }

    if providers.gemini {
        visit_file_sessions(
            &paths.gemini_session_dir,
            ExtensionType::Gemini,
            is_gemini_session_file,
            time_range,
            None,
            mode,
            &mut diagnostics,
            visitor,
        )?;
    }

    if providers.grok {
        visit_file_sessions(
            &paths.grok_session_dir,
            ExtensionType::Grok,
            is_grok_session_file,
            time_range,
            Some(GROK_SESSION_MAX_DEPTH),
            mode,
            &mut diagnostics,
            visitor,
        )?;
    }

    if providers.opencode && paths.opencode_db.exists() {
        diagnostics.candidates += 1;
        match read_opencode_analysis_with_diagnostics(&paths.opencode_db, TimeRange::All, mode) {
            Ok(result) => {
                if result.expected_records > 0 && result.parsed_records == 0 {
                    record_failure(
                        &mut diagnostics,
                        ExtensionType::OpenCode,
                        &paths.opencode_db,
                        format!(
                            "none of {} analysis records used a recognized schema",
                            result.expected_records
                        ),
                    );
                } else {
                    diagnostics.parsed += 1;
                    let failed_payloads = result
                        .expected_records
                        .saturating_sub(result.parsed_records)
                        + result.failed_tool_parts;
                    if failed_payloads > 0 {
                        record_failure(
                            &mut diagnostics,
                            ExtensionType::OpenCode,
                            &paths.opencode_db,
                            format!(
                                "{failed_payloads} analysis payloads used an unsupported schema"
                            ),
                        );
                    }
                }
                visit_database_sessions(
                    ExtensionType::OpenCode,
                    &paths.opencode_db,
                    result.rows,
                    visitor,
                );
            }
            Err(err) => record_failure(
                &mut diagnostics,
                ExtensionType::OpenCode,
                &paths.opencode_db,
                err.to_string(),
            ),
        }
    }

    if providers.cursor && paths.cursor_chats_dir.exists() {
        let result = read_cursor_analysis_with_diagnostics(
            &paths.cursor_chats_dir,
            &paths.cursor_tracking_db,
            TimeRange::All,
            mode,
        );
        diagnostics.candidates += result.candidates;
        diagnostics.parsed += result.parsed;
        for failure in result.failures {
            record_failure(
                &mut diagnostics,
                ExtensionType::Cursor,
                &failure.path,
                failure.error,
            );
        }
        visit_database_sessions(
            ExtensionType::Cursor,
            &paths.cursor_chats_dir,
            result.rows,
            visitor,
        );
    }

    Ok(diagnostics)
}

/// Incremental compact analysis scan rooted at the current user's paths.
pub fn aggregate_sessions_by_model_with_cache(
    time_range: TimeRange,
    providers: ProvidersConfig,
    cache: &mut SummaryScanCache,
) -> Result<AnalysisAggregation> {
    aggregate_sessions_by_model_from_paths_with_cache(
        &crate::utils::resolve_paths()?,
        time_range,
        providers,
        cache,
    )
}

/// Incremental compact analysis scan rooted at explicit provider paths.
///
/// File sources share the same compact cache shape as the usage collector.
/// Database entries retain only model counters, dates, and source diagnostics.
pub fn aggregate_sessions_by_model_from_paths_with_cache(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    cache: &mut SummaryScanCache,
) -> Result<AnalysisAggregation> {
    cache.begin_scan();
    let mut projection = AnalysisProjection::with_time_range(time_range);
    let mut diagnostics = AnalysisCollectionDiagnostics::default();
    let mut seen = HashSet::new();

    if providers.claude {
        scan_analysis_files(
            &paths.claude_session_dir,
            ExtensionType::ClaudeCode,
            is_claude_session_file,
            time_range,
            None,
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
        )?;
    }
    if providers.codex {
        scan_analysis_files(
            &paths.codex_session_dir,
            ExtensionType::Codex,
            is_codex_session_file,
            time_range,
            None,
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
        )?;
    }
    if providers.copilot {
        scan_analysis_files(
            &paths.copilot_session_dir,
            ExtensionType::Copilot,
            is_copilot_session_file,
            time_range,
            Some(COPILOT_SESSION_MAX_DEPTH),
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
        )?;
    }
    if providers.gemini {
        scan_analysis_files(
            &paths.gemini_session_dir,
            ExtensionType::Gemini,
            is_gemini_session_file,
            time_range,
            None,
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
        )?;
    }
    if providers.grok {
        scan_analysis_files(
            &paths.grok_session_dir,
            ExtensionType::Grok,
            is_grok_session_file,
            time_range,
            Some(GROK_SESSION_MAX_DEPTH),
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
        )?;
    }

    if providers.opencode && paths.opencode_db.exists() {
        scan_opencode_analysis(
            paths,
            time_range,
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
        );
    }
    if providers.cursor && paths.cursor_chats_dir.exists() {
        scan_cursor_analysis(
            paths,
            time_range,
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
        );
    }

    cache.retain_kinds(&seen, &[SummaryKind::File, SummaryKind::AnalysisDatabase]);
    diagnostics.failures.sort_by(|left, right| {
        provider_scan_rank(left.provider)
            .cmp(&provider_scan_rank(right.provider))
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.error.cmp(&right.error))
    });
    Ok(AnalysisAggregation {
        data: projection.finish(),
        diagnostics,
    })
}

#[allow(clippy::too_many_arguments)]
fn scan_analysis_files<F>(
    dir: &Path,
    provider: ExtensionType,
    filter: F,
    time_range: TimeRange,
    max_depth: Option<usize>,
    cache: &mut SummaryScanCache,
    seen: &mut HashSet<SummaryCacheKey>,
    projection: &mut AnalysisProjection,
    diagnostics: &mut AnalysisCollectionDiagnostics,
) -> Result<()>
where
    F: Copy + Fn(&Path) -> bool + Sync + Send,
{
    let discovery =
        collect_files_with_max_depth_diagnostics(dir, filter, TimeRange::All, max_depth);
    if !discovery.failures.is_empty() {
        cache.preserve_provider_keys(seen, SummaryKind::File, provider);
    }
    diagnostics.candidates += discovery.failures.len();
    for failure in discovery.failures {
        record_failure(diagnostics, provider, &failure.path, failure.error);
    }

    let mut files = discovery.files;
    files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    diagnostics.candidates += files.len();
    let mut misses = Vec::new();

    for file in files {
        let key = SummaryCacheKey::new(SummaryKind::File, provider, &file.path, time_range);
        seen.insert(key.clone());
        match SourceFingerprint::file(&file.path, provider) {
            Ok(fingerprint) => {
                if let Some(cached) = cache.get(&key, &fingerprint) {
                    fold_cached_analysis(provider, &file.path, cached, projection, diagnostics);
                } else {
                    misses.push((file, key, fingerprint));
                }
            }
            Err(error) => record_failure(diagnostics, provider, &file.path, error.to_string()),
        }
    }

    let loaded: Vec<_> = misses
        .into_par_iter()
        .map(|(file, key, fingerprint)| {
            let result = load_analysis_file(&file, provider);
            (file.path, key, fingerprint, result)
        })
        .collect();
    for (source, key, fingerprint, result) in loaded {
        cache.record_parse();
        match result {
            Ok(loaded) => {
                fold_loaded_analysis(provider, &source, &loaded, projection, diagnostics);
                cache.insert(
                    key,
                    fingerprint,
                    loaded.summary,
                    loaded.parsed,
                    loaded.failure,
                );
            }
            Err(error) => record_failure(diagnostics, provider, &source, error.to_string()),
        }
    }
    Ok(())
}

fn load_analysis_file(file: &FileInfo, provider: ExtensionType) -> Result<CachedAnalysisLoad> {
    let parsed =
        parse_session_file_as_with_diagnostics(&file.path, provider, ParseMode::UsageOnly)?;
    if parsed.diagnostics.is_complete_failure() {
        let error = if parsed.diagnostics.recognized_records == 0 {
            "source contained no recognized provider records".to_string()
        } else {
            format!(
                "none of {} analyzer-relevant provider records used a supported schema",
                parsed.diagnostics.relevant_records
            )
        };
        return Ok(CachedAnalysisLoad {
            summary: CompactSourceSummary::default(),
            parsed: false,
            failure: Some(error),
        });
    }
    let emit = parsed.diagnostics.should_emit_session();
    if emit && parsed.analysis.records.is_empty() {
        return Ok(CachedAnalysisLoad {
            summary: CompactSourceSummary::default(),
            parsed: false,
            failure: Some("normalized source produced no analysis records".to_string()),
        });
    }
    let partial = parsed.diagnostics.partial_failure_count();
    Ok(CachedAnalysisLoad {
        summary: CompactSourceSummary::from_parsed(parsed, emit),
        parsed: true,
        failure: (partial > 0)
            .then(|| format!("skipped {partial} malformed or unsupported analyzer records")),
    })
}

fn scan_opencode_analysis(
    paths: &HelperPaths,
    time_range: TimeRange,
    cache: &mut SummaryScanCache,
    seen: &mut HashSet<SummaryCacheKey>,
    projection: &mut AnalysisProjection,
    diagnostics: &mut AnalysisCollectionDiagnostics,
) {
    let provider = ExtensionType::OpenCode;
    let source = &paths.opencode_db;
    diagnostics.candidates += 1;
    let key = SummaryCacheKey::new(SummaryKind::AnalysisDatabase, provider, source, time_range);
    seen.insert(key.clone());
    let fingerprint = match SourceFingerprint::sqlite(source, &[]) {
        Ok(value) => value,
        Err(error) => {
            record_failure(diagnostics, provider, source, error.to_string());
            return;
        }
    };
    if let Some(cached) = cache.get(&key, &fingerprint) {
        fold_cached_analysis(provider, source, cached, projection, diagnostics);
        return;
    }

    cache.record_parse();
    match read_opencode_analysis_with_diagnostics(source, TimeRange::All, ParseMode::UsageOnly) {
        Ok(result) => {
            let complete_failure = result.expected_records > 0 && result.parsed_records == 0;
            let failed = result
                .expected_records
                .saturating_sub(result.parsed_records)
                + result.failed_tool_parts;
            let failure = if complete_failure {
                Some(format!(
                    "none of {} analysis records used a recognized schema",
                    result.expected_records
                ))
            } else if failed > 0 {
                Some(format!(
                    "{failed} analysis payloads used an unsupported schema"
                ))
            } else {
                None
            };
            let mut summary = CompactSourceSummary::default();
            for row in result.rows {
                summary.add_analysis(row.analysis, true, row.analysis_facts);
            }
            let loaded = CachedAnalysisLoad {
                summary,
                parsed: !complete_failure,
                failure,
            };
            fold_loaded_analysis(provider, source, &loaded, projection, diagnostics);
            cache.insert(
                key,
                fingerprint,
                loaded.summary,
                loaded.parsed,
                loaded.failure,
            );
        }
        Err(error) => {
            let failure = error.to_string();
            record_failure(diagnostics, provider, source, failure.clone());
            if is_cacheable_sqlite_failure(&error) {
                cache.insert(
                    key,
                    fingerprint,
                    CompactSourceSummary::default(),
                    false,
                    Some(failure),
                );
            }
        }
    }
}

fn scan_cursor_analysis(
    paths: &HelperPaths,
    time_range: TimeRange,
    cache: &mut SummaryScanCache,
    seen: &mut HashSet<SummaryCacheKey>,
    projection: &mut AnalysisProjection,
    diagnostics: &mut AnalysisCollectionDiagnostics,
) {
    let provider = ExtensionType::Cursor;
    let source = &paths.cursor_chats_dir;
    let discovery = discover_cursor_store_dbs(source);
    if !discovery.failures.is_empty() {
        cache.preserve_provider_keys(seen, SummaryKind::AnalysisDatabase, provider);
    }
    for failure in discovery.failures {
        diagnostics.candidates += 1;
        record_failure(diagnostics, provider, &failure.path, failure.error);
    }

    let tracking_db = &paths.cursor_tracking_db;
    let (conv_models, tracking_fingerprint, tracking_ok) =
        match load_conversation_model_snapshot(tracking_db) {
            Ok(snapshot) => (snapshot.models, snapshot.fingerprint, true),
            Err(error) => {
                record_failure(diagnostics, provider, tracking_db, error.to_string());
                (FastHashMap::default(), None, false)
            }
        };
    let user = get_current_user();
    let machine = get_machine_id().to_string();

    for store in discovery.stores {
        diagnostics.candidates += 1;
        let key = SummaryCacheKey::new(SummaryKind::AnalysisDatabase, provider, &store, time_range);
        seen.insert(key.clone());
        let fingerprint = if tracking_ok {
            SourceFingerprint::sqlite_with_dependency(
                &store,
                tracking_db,
                tracking_fingerprint.as_ref(),
            )
        } else {
            SourceFingerprint::sqlite(&store, &[])
        };
        let fingerprint = match fingerprint {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                record_failure(diagnostics, provider, &store, error.to_string());
                continue;
            }
        };
        if tracking_ok && let Some(cached) = cache.get(&key, &fingerprint) {
            fold_cached_analysis(provider, &store, cached, projection, diagnostics);
            continue;
        }

        cache.record_parse();
        match read_store_analysis(
            &store,
            &conv_models,
            TimeRange::All,
            ParseMode::UsageOnly,
            &user,
            &machine,
        ) {
            Ok(result) => {
                let complete_failure =
                    result.normalized_messages == 0 && result.failed_payloads > 0;
                let failure = if complete_failure {
                    Some(format!(
                        "none of {} analyzer payloads used a supported schema",
                        result.failed_payloads
                    ))
                } else if result.failed_payloads > 0 {
                    Some(format!(
                        "{} analyzer payloads used an unsupported schema",
                        result.failed_payloads
                    ))
                } else {
                    None
                };
                let mut summary = CompactSourceSummary::default();
                for row in result.rows {
                    summary.add_analysis(row.analysis, true, row.analysis_facts);
                }
                let loaded = CachedAnalysisLoad {
                    summary,
                    parsed: !complete_failure,
                    failure,
                };
                fold_loaded_analysis(provider, &store, &loaded, projection, diagnostics);
                if tracking_ok {
                    cache.insert(
                        key,
                        fingerprint,
                        loaded.summary,
                        loaded.parsed,
                        loaded.failure,
                    );
                }
            }
            Err(error) => {
                let failure = error.to_string();
                record_failure(diagnostics, provider, &store, failure.clone());
                if tracking_ok && is_cacheable_sqlite_failure(&error) {
                    cache.insert(
                        key,
                        fingerprint,
                        CompactSourceSummary::default(),
                        false,
                        Some(failure),
                    );
                }
            }
        }
    }
}
struct CachedAnalysisLoad {
    summary: CompactSourceSummary,
    parsed: bool,
    failure: Option<String>,
}

fn fold_cached_analysis(
    provider: ExtensionType,
    source: &Path,
    cached: &CachedSourceSummary,
    projection: &mut AnalysisProjection,
    diagnostics: &mut AnalysisCollectionDiagnostics,
) {
    if cached.parsed {
        diagnostics.parsed += 1;
        projection.add_compact(provider, source, &cached.summary);
    }
    if let Some(error) = &cached.failure {
        diagnostics.failures.push(AnalysisCollectionFailure {
            provider,
            source: source.to_path_buf(),
            error: error.clone(),
        });
    }
}

fn fold_loaded_analysis(
    provider: ExtensionType,
    source: &Path,
    loaded: &CachedAnalysisLoad,
    projection: &mut AnalysisProjection,
    diagnostics: &mut AnalysisCollectionDiagnostics,
) {
    if loaded.parsed {
        diagnostics.parsed += 1;
        projection.add_compact(provider, source, &loaded.summary);
    }
    if let Some(error) = &loaded.failure {
        diagnostics.failures.push(AnalysisCollectionFailure {
            provider,
            source: source.to_path_buf(),
            error: error.clone(),
        });
    }
}

/// Projects a canonical dataset into the compact model/provider summaries used
/// by the TUI, text, and table renderers.
pub fn project_analysis_dataset(dataset: &AnalysisDataset) -> AnalysisData {
    let mut projection = AnalysisProjection::new();
    for session in &dataset.sessions {
        projection.add_session(session);
    }
    projection.finish()
}

/// Projects one complete parser result into the same summary shape as a batch.
///
/// This is the single-file seam for `analysis FILE --text` and `--table`; it
/// deliberately shares the batch projection instead of duplicating counters in
/// CLI wiring.
pub fn project_code_analysis(analysis: &CodeAnalysis) -> AnalysisData {
    let provider = extension_type_from_name(&analysis.extension_name);
    let mut projection = AnalysisProjection::new();
    projection.add_analysis(provider, analysis);

    let mut dates = HashSet::new();
    for record in &analysis.records {
        if record.timestamp > 0
            && let Some(date) = local_date_from_millis(record.timestamp)
        {
            dates.insert(date);
        }
    }
    if dates.is_empty() && !analysis.records.is_empty() {
        dates.insert("single".to_string());
    }
    for date in dates {
        projection.add_date(provider, date);
    }

    projection.finish()
}

/// Parses one session file and projects its private event facts into the
/// compact summary used by non-JSON single-file output.
///
/// The returned [`CodeAnalysis`] keeps the public JSON contract unchanged;
/// stable tool ids and lifecycle facts are consumed only by the projection.
#[doc(hidden)]
pub fn project_session_file<P: AsRef<Path>>(
    path: P,
    mode: ParseMode,
) -> Result<(CodeAnalysis, AnalysisData, SessionFileParseDiagnostics)> {
    let path = path.as_ref();
    let (mut parsed, diagnostics) = parse_session_file_with_facts_and_diagnostics(path, mode)?;
    let provider = extension_type_from_name(&parsed.analysis.extension_name);
    let mut projection = AnalysisProjection::new();
    if let Some(provider) = provider {
        projection.add_usage_presence(provider, path, &parsed.usage_facts);
        let analysis_facts = if parsed.analysis_facts.is_empty() {
            fallback_facts(&parsed.analysis).1
        } else {
            std::mem::take(&mut parsed.analysis_facts)
        };
        projection.add_facts(provider, path, analysis_facts);
    } else {
        projection.add_analysis(None, &parsed.analysis);
    }
    if !parsed.analysis.records.is_empty()
        && parsed.analysis.records.iter().all(|record| {
            record.timestamp <= 0 || local_date_from_millis(record.timestamp).is_none()
        })
    {
        projection.add_date(provider, "single".to_string());
    }
    Ok((parsed.analysis, projection.finish(), diagnostics))
}

/// Drains a model-keyed map into a `Vec` sorted by model name.
fn into_sorted_rows(map: FastHashMap<String, AggregatedAnalysisRow>) -> Vec<AggregatedAnalysisRow> {
    let mut v: Vec<AggregatedAnalysisRow> = map.into_values().collect();
    v.sort_unstable_by(|a, b| a.model.cmp(&b.model));
    v
}

struct CollectedAnalysisSource {
    source: PathBuf,
    session: AnalysisSession,
    usage_facts: Vec<UsageFact>,
    analysis_facts: Vec<AnalysisFact>,
}

type FileSessionOutcome = std::result::Result<
    (
        Option<CollectedAnalysisSource>,
        Option<AnalysisCollectionFailure>,
    ),
    AnalysisCollectionFailure,
>;

/// Visits one file-backed provider in deterministic path order.
#[allow(clippy::too_many_arguments)]
fn visit_file_sessions<F, V>(
    dir: &Path,
    provider: ExtensionType,
    filter_fn: F,
    _time_range: TimeRange,
    max_depth: Option<usize>,
    mode: ParseMode,
    diagnostics: &mut AnalysisCollectionDiagnostics,
    visitor: &mut V,
) -> Result<()>
where
    F: Copy + Fn(&Path) -> bool + Sync + Send,
    V: FnMut(CollectedAnalysisSource),
{
    let discovery =
        collect_files_with_max_depth_diagnostics(dir, filter_fn, TimeRange::All, max_depth);
    diagnostics.candidates += discovery.failures.len();
    for failure in discovery.failures {
        record_failure(diagnostics, provider, &failure.path, failure.error);
    }

    let mut files = discovery.files;
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    diagnostics.candidates += files.len();
    // `Vec::into_par_iter` is indexed, so collecting retains the sorted source
    // order while moving each path/date directly into its outcome.
    let outcomes: Vec<FileSessionOutcome> = files
        .into_par_iter()
        .map(|file_info| {
            let FileInfo { path, .. } = file_info;
            match parse_session_file_as_with_diagnostics(&path, provider, mode) {
                Ok(parsed) if parsed.diagnostics.is_complete_failure() => {
                    let error = if parsed.diagnostics.recognized_records == 0 {
                        "source contained no recognized provider records".to_string()
                    } else {
                        format!(
                            "none of {} analyzer-relevant provider records used a supported schema",
                            parsed.diagnostics.relevant_records
                        )
                    };
                    Err(AnalysisCollectionFailure {
                        provider,
                        source: path,
                        error,
                    })
                }
                Ok(parsed)
                    if parsed.diagnostics.should_emit_session()
                        && parsed.analysis.records.is_empty() =>
                {
                    Err(AnalysisCollectionFailure {
                        provider,
                        source: path,
                        error: "normalized source produced no analysis records".to_string(),
                    })
                }
                Ok(parsed) => {
                    let partial_failure_count = parsed.diagnostics.partial_failure_count();
                    let partial_failure = (partial_failure_count > 0).then_some(
                        AnalysisCollectionFailure {
                            provider,
                            source: path.clone(),
                            error: format!(
                                "skipped {partial_failure_count} malformed or unsupported analyzer records"
                            ),
                        },
                    );
                    let event_date = parsed
                        .analysis
                        .records
                        .iter()
                        .filter_map(|record| local_date_from_millis(record.timestamp))
                        .max();
                    let (fallback_usage_facts, fallback_analysis_facts) =
                        fallback_facts(&parsed.analysis);
                    let usage_facts = if parsed.usage_facts.is_empty() {
                        fallback_usage_facts
                    } else {
                        parsed.usage_facts
                    };
                    let analysis_facts = if parsed.analysis_facts.is_empty() {
                        fallback_analysis_facts
                    } else {
                        parsed.analysis_facts
                    };
                    let session = parsed.diagnostics.should_emit_session().then_some(
                        CollectedAnalysisSource {
                            source: path.clone(),
                            session: AnalysisSession {
                                provider,
                                date: event_date.unwrap_or_default(),
                                analysis: parsed.analysis,
                            },
                            usage_facts,
                            analysis_facts,
                        },
                    );
                    Ok((session, partial_failure))
                }
                Err(err) => Err(AnalysisCollectionFailure {
                    provider,
                    source: path,
                    error: err.to_string(),
                }),
            }
        })
        .collect();

    for outcome in outcomes {
        match outcome {
            Ok((session, partial_failure)) => {
                diagnostics.parsed += 1;
                if let Some(session) = session {
                    visitor(session);
                }
                if let Some(failure) = partial_failure {
                    push_failure(diagnostics, failure);
                }
            }
            Err(failure) => push_failure(diagnostics, failure),
        }
    }
    Ok(())
}

fn record_failure(
    diagnostics: &mut AnalysisCollectionDiagnostics,
    provider: ExtensionType,
    source: &Path,
    error: String,
) {
    push_failure(
        diagnostics,
        AnalysisCollectionFailure {
            provider,
            source: source.to_path_buf(),
            error,
        },
    );
}

fn push_failure(
    diagnostics: &mut AnalysisCollectionDiagnostics,
    failure: AnalysisCollectionFailure,
) {
    log::warn!(
        "failed to collect {} analysis from {}: {}",
        failure.provider,
        failure.source.display(),
        failure.error
    );
    diagnostics.failures.push(failure);
}

fn visit_database_sessions<F>(
    provider: ExtensionType,
    database: &Path,
    mut rows: Vec<DatabaseAnalysisRow>,
    visitor: &mut F,
) where
    F: FnMut(CollectedAnalysisSource),
{
    rows.sort_unstable_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then_with(|| a.source_id.cmp(&b.source_id))
    });
    for row in rows {
        let (usage_facts, fallback_analysis_facts) = fallback_facts(&row.analysis);
        let analysis_facts = if row.analysis_facts.is_empty() {
            fallback_analysis_facts
        } else {
            row.analysis_facts
        };
        visitor(CollectedAnalysisSource {
            source: PathBuf::from(format!("{}#{}", database.display(), row.source_id)),
            session: AnalysisSession {
                provider,
                date: row.date,
                analysis: row.analysis,
            },
            usage_facts,
            analysis_facts,
        });
    }
}

#[derive(Clone)]
struct SourcedUsageFact {
    provider: ExtensionType,
    source: PathBuf,
    source_index: usize,
    fact: UsageFact,
}

struct MaterializedRecord {
    record: CodeAnalysisRecord,
    unique_files: HashSet<String>,
    unknown_unique_files: usize,
    aggregate_facts: HashSet<usize>,
    conversation_models: HashSet<String>,
    advisor_models: HashSet<String>,
    activity: bool,
    max_timestamp: Option<i64>,
}

impl MaterializedRecord {
    fn new(source: &CollectedAnalysisSource) -> Self {
        let original = &source.session.analysis.records[0];
        Self {
            record: CodeAnalysisRecord {
                total_unique_files: 0,
                total_write_lines: 0,
                total_read_lines: 0,
                total_edit_lines: 0,
                total_write_characters: 0,
                total_read_characters: 0,
                total_edit_characters: 0,
                write_file_details: Vec::new(),
                read_file_details: Vec::new(),
                edit_file_details: Vec::new(),
                run_command_details: Vec::new(),
                tool_call_counts: CodeAnalysisToolCalls::default(),
                conversation_usage: FastHashMap::default(),
                advisor_usage: FastHashMap::default(),
                task_id: original.task_id.clone(),
                timestamp: 0,
                folder_path: original.folder_path.clone(),
                git_remote_url: original.git_remote_url.clone(),
            },
            unique_files: HashSet::new(),
            unknown_unique_files: 0,
            aggregate_facts: HashSet::new(),
            conversation_models: original.conversation_usage.keys().cloned().collect(),
            advisor_models: original.advisor_usage.keys().cloned().collect(),
            activity: false,
            max_timestamp: None,
        }
    }

    fn observe_timestamp(&mut self, timestamp_ms: Option<i64>) {
        if let Some(timestamp_ms) = timestamp_ms {
            self.max_timestamp = Some(
                self.max_timestamp
                    .map_or(timestamp_ms, |current| current.max(timestamp_ms)),
            );
        }
    }

    fn add_usage_fact(&mut self, fact: UsageFact) {
        self.activity = true;
        self.observe_timestamp(fact.observed_at_ms.or(fact.timestamp_ms));
        for unit in fact.units {
            let advisor_only = self.advisor_models.contains(&unit.model)
                && !self.conversation_models.contains(&unit.model);
            let target = if advisor_only {
                &mut self.record.advisor_usage
            } else {
                &mut self.record.conversation_usage
            };
            target
                .entry(unit.model)
                .and_modify(|existing| merge_usage_values(existing, unit.usage.as_ref()))
                .or_insert_with(|| unit.usage.as_ref().clone());
        }
    }

    fn add_analysis_fact(&mut self, fact: AnalysisFact) {
        if fact.effect.as_ref().is_some_and(|effect| effect.aggregate)
            && !self.aggregate_facts.insert(fact.source_order)
        {
            return;
        }
        self.activity = true;
        self.observe_timestamp(fact.observed_at_ms.or(fact.timestamp_ms));
        if !fact.model.is_empty() {
            self.record
                .conversation_usage
                .entry(fact.model.clone())
                .or_insert_with(|| serde_json::Value::Object(Default::default()));
        }
        self.record.total_edit_lines += fact.metrics.edit_lines;
        self.record.total_read_lines += fact.metrics.read_lines;
        self.record.total_write_lines += fact.metrics.write_lines;
        self.record.tool_call_counts.bash += fact.metrics.bash_count;
        self.record.tool_call_counts.edit += fact.metrics.edit_count;
        self.record.tool_call_counts.read += fact.metrics.read_count;
        self.record.tool_call_counts.todo_write += fact.metrics.todo_write_count;
        self.record.tool_call_counts.write += fact.metrics.write_count;
        if fact.status == ToolFactStatus::Succeeded
            && let Some(effect) = fact.effect
        {
            self.add_effect(effect);
        }
    }

    fn add_effect(&mut self, mut effect: AnalysisFactEffect) {
        self.unique_files.extend(effect.unique_files.drain(..));
        self.unknown_unique_files += effect.unknown_unique_files;
        self.record.total_write_characters += effect.write_characters;
        self.record.total_read_characters += effect.read_characters;
        self.record.total_edit_characters += effect.edit_characters;
        self.record
            .write_file_details
            .append(&mut effect.write_file_details);
        self.record
            .read_file_details
            .append(&mut effect.read_file_details);
        self.record
            .edit_file_details
            .append(&mut effect.edit_file_details);
        self.record
            .run_command_details
            .append(&mut effect.run_command_details);
    }

    fn finish(mut self) -> CodeAnalysisRecord {
        self.record.total_unique_files = self.unique_files.len() + self.unknown_unique_files;
        self.record.timestamp = self.max_timestamp.unwrap_or_default();
        self.record
    }
}

fn materialize_analysis_sources(
    mut sources: Vec<CollectedAnalysisSource>,
    time_range: TimeRange,
    _mode: ParseMode,
) -> Vec<AnalysisSession> {
    sources.retain(|source| {
        let keep = !source.session.analysis.records.is_empty();
        if !keep {
            log::warn!(
                "{} analysis source {} produced no records",
                source.session.provider,
                source.source.display()
            );
        }
        keep
    });
    let cutoff = time_range
        .cutoff_date()
        .map(|date| date.format("%Y-%m-%d").to_string());
    let mut builders: Vec<_> = sources.iter().map(MaterializedRecord::new).collect();
    if cutoff.is_none() {
        for (builder, source) in builders.iter_mut().zip(&sources) {
            builder.observe_timestamp(
                (source.session.analysis.records[0].timestamp > 0)
                    .then_some(source.session.analysis.records[0].timestamp),
            );
        }
    }
    let mut usage_facts = Vec::new();
    let mut analysis_facts = Vec::new();
    let mut sources_without_facts = Vec::new();

    for (source_index, source) in sources.iter_mut().enumerate() {
        if source.usage_facts.is_empty() && source.analysis_facts.is_empty() {
            sources_without_facts.push(source_index);
        }
        usage_facts.extend(source.usage_facts.drain(..).map(|fact| SourcedUsageFact {
            provider: source.session.provider,
            source: source.source.clone(),
            source_index,
            fact,
        }));
        analysis_facts.extend(
            source
                .analysis_facts
                .drain(..)
                .map(|fact| SourcedAnalysisFact {
                    provider: source.session.provider,
                    source: source.source.clone(),
                    source_index,
                    fact,
                }),
        );
    }

    for sourced in reduce_batch_usage_facts(usage_facts) {
        if fact_is_in_range(
            sourced.fact.timestamp_ms,
            cutoff.as_deref(),
            sourced.provider,
            &sourced.source,
            "usage",
        ) {
            builders[sourced.source_index].add_usage_fact(sourced.fact);
        }
    }
    for sourced in reduce_analysis_facts(analysis_facts) {
        if fact_is_in_range(
            sourced.fact.timestamp_ms,
            cutoff.as_deref(),
            sourced.provider,
            &sourced.source,
            "analysis",
        ) {
            builders[sourced.source_index].add_analysis_fact(sourced.fact);
        }
    }

    let preserve_without_facts: HashSet<_> = if cutoff.is_none() {
        sources_without_facts.into_iter().collect()
    } else {
        HashSet::new()
    };
    sources
        .into_iter()
        .zip(builders)
        .enumerate()
        .filter_map(|(source_index, (mut source, builder))| {
            if !builder.activity {
                return preserve_without_facts
                    .contains(&source_index)
                    .then_some(source.session);
            }
            let record = builder.finish();
            source.session.date = local_date_from_millis(record.timestamp).unwrap_or_default();
            source.session.analysis.records = vec![record];
            Some(source.session)
        })
        .collect()
}

fn fact_is_in_range(
    timestamp_ms: Option<i64>,
    cutoff: Option<&str>,
    provider: ExtensionType,
    source: &Path,
    kind: &str,
) -> bool {
    let date = timestamp_ms.and_then(local_date_from_millis);
    if timestamp_ms.is_none() {
        log::warn!(
            "{provider} {kind} fact from {} has no event timestamp{}",
            source.display(),
            if cutoff.is_some() {
                " and was excluded from the selected time range"
            } else {
                ""
            }
        );
    }
    match (cutoff, date.as_deref()) {
        (None, _) => true,
        (Some(cutoff), Some(date)) => date >= cutoff,
        (Some(_), None) => false,
    }
}

fn reduce_batch_usage_facts(facts: Vec<SourcedUsageFact>) -> Vec<SourcedUsageFact> {
    let mut anonymous = Vec::new();
    let mut stable: FastHashMap<String, Vec<SourcedUsageFact>> = FastHashMap::default();
    for sourced in facts {
        if let Some(id) = &sourced.fact.stable_id {
            stable
                .entry(format!("{}\0{id}", sourced.provider))
                .or_default()
                .push(sourced);
        } else {
            anonymous.push(sourced);
        }
    }
    for mut group in stable.into_values() {
        let canonical_timestamp = group
            .iter()
            .filter_map(|sourced| sourced.fact.timestamp_ms)
            .min();
        group.sort_unstable_by(|left, right| {
            left.fact
                .observed_at_ms
                .unwrap_or_default()
                .cmp(&right.fact.observed_at_ms.unwrap_or_default())
                .then_with(|| left.source.cmp(&right.source))
                .then_with(|| left.fact.source_order.cmp(&right.fact.source_order))
        });
        if let Some(mut winner) = group.pop() {
            winner.fact.timestamp_ms = canonical_timestamp;
            anonymous.push(winner);
        }
    }
    anonymous.sort_unstable_by(|left, right| {
        provider_scan_rank(left.provider)
            .cmp(&provider_scan_rank(right.provider))
            .then_with(|| left.fact.timestamp_ms.cmp(&right.fact.timestamp_ms))
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.fact.source_order.cmp(&right.fact.source_order))
    });
    anonymous
}

/// Mutable accumulator shared by batch and single-file projections.
struct AnalysisProjection {
    cutoff: Option<String>,
    usage_presence: Vec<SourcedUsageFact>,
    facts: Vec<SourcedAnalysisFact>,
    all: FastHashMap<String, AggregatedAnalysisRow>,
    claude: FastHashMap<String, AggregatedAnalysisRow>,
    codex: FastHashMap<String, AggregatedAnalysisRow>,
    copilot: FastHashMap<String, AggregatedAnalysisRow>,
    gemini: FastHashMap<String, AggregatedAnalysisRow>,
    grok: FastHashMap<String, AggregatedAnalysisRow>,
    opencode: FastHashMap<String, AggregatedAnalysisRow>,
    cursor: FastHashMap<String, AggregatedAnalysisRow>,
    all_dates: HashSet<String>,
    claude_dates: HashSet<String>,
    codex_dates: HashSet<String>,
    copilot_dates: HashSet<String>,
    gemini_dates: HashSet<String>,
    grok_dates: HashSet<String>,
    opencode_dates: HashSet<String>,
    cursor_dates: HashSet<String>,
    hermes_dates: HashSet<String>,
}

#[derive(Clone)]
struct SourcedAnalysisFact {
    provider: ExtensionType,
    source: PathBuf,
    source_index: usize,
    fact: AnalysisFact,
}

impl AnalysisProjection {
    fn new() -> Self {
        Self {
            cutoff: None,
            usage_presence: Vec::new(),
            facts: Vec::new(),
            all: FastHashMap::with_capacity(capacity::MODEL_COMBINATIONS),
            claude: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            codex: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            copilot: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            gemini: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            grok: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            opencode: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            cursor: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            all_dates: HashSet::new(),
            claude_dates: HashSet::new(),
            codex_dates: HashSet::new(),
            copilot_dates: HashSet::new(),
            gemini_dates: HashSet::new(),
            grok_dates: HashSet::new(),
            opencode_dates: HashSet::new(),
            cursor_dates: HashSet::new(),
            hermes_dates: HashSet::new(),
        }
    }

    fn with_time_range(time_range: TimeRange) -> Self {
        let mut projection = Self::new();
        projection.cutoff = time_range
            .cutoff_date()
            .map(|date| date.format("%Y-%m-%d").to_string());
        projection
    }

    fn add_session(&mut self, session: &AnalysisSession) {
        self.add_analysis(Some(session.provider), &session.analysis);
        self.add_date(Some(session.provider), session.date.clone());
    }

    fn add_analysis(&mut self, provider: Option<ExtensionType>, analysis: &CodeAnalysis) {
        aggregate_analysis_result(&mut self.all, analysis);
        let provider_rows = match provider {
            Some(ExtensionType::ClaudeCode) => Some(&mut self.claude),
            Some(ExtensionType::Codex) => Some(&mut self.codex),
            Some(ExtensionType::Copilot) => Some(&mut self.copilot),
            Some(ExtensionType::Gemini) => Some(&mut self.gemini),
            Some(ExtensionType::Grok) => Some(&mut self.grok),
            Some(ExtensionType::OpenCode) => Some(&mut self.opencode),
            Some(ExtensionType::Cursor) => Some(&mut self.cursor),
            Some(ExtensionType::Hermes) | None => None,
        };
        if let Some(rows) = provider_rows {
            aggregate_analysis_result(rows, analysis);
        }
    }

    fn add_compact(
        &mut self,
        provider: ExtensionType,
        source: &Path,
        summary: &CompactSourceSummary,
    ) {
        self.add_usage_presence(provider, source, &summary.usage_facts);
        self.add_facts(provider, source, summary.analysis_facts.iter().cloned());
    }

    fn add_usage_presence(
        &mut self,
        provider: ExtensionType,
        source: &Path,
        usage_facts: &[UsageFact],
    ) {
        self.usage_presence
            .extend(usage_facts.iter().cloned().map(|fact| SourcedUsageFact {
                provider,
                source: source.to_path_buf(),
                source_index: 0,
                fact,
            }));
    }

    fn add_facts(
        &mut self,
        provider: ExtensionType,
        source: &Path,
        facts: impl IntoIterator<Item = AnalysisFact>,
    ) {
        self.facts
            .extend(facts.into_iter().map(|fact| SourcedAnalysisFact {
                provider,
                source: source.to_path_buf(),
                source_index: 0,
                fact,
            }));
    }

    fn add_date(&mut self, provider: Option<ExtensionType>, date: String) {
        if date.is_empty() {
            return;
        }
        self.all_dates.insert(date.clone());
        match provider {
            Some(ExtensionType::ClaudeCode) => {
                self.claude_dates.insert(date);
            }
            Some(ExtensionType::Codex) => {
                self.codex_dates.insert(date);
            }
            Some(ExtensionType::Copilot) => {
                self.copilot_dates.insert(date);
            }
            Some(ExtensionType::Gemini) => {
                self.gemini_dates.insert(date);
            }
            Some(ExtensionType::Grok) => {
                self.grok_dates.insert(date);
            }
            Some(ExtensionType::OpenCode) => {
                self.opencode_dates.insert(date);
            }
            Some(ExtensionType::Cursor) => {
                self.cursor_dates.insert(date);
            }
            Some(ExtensionType::Hermes) => {
                self.hermes_dates.insert(date);
            }
            None => {}
        }
    }

    fn finish(mut self) -> AnalysisData {
        for sourced in reduce_batch_usage_facts(std::mem::take(&mut self.usage_presence)) {
            let date = sourced.fact.timestamp_ms.and_then(local_date_from_millis);
            let included = match (&self.cutoff, &date) {
                (None, _) => true,
                (Some(cutoff), Some(date)) => date >= cutoff,
                (Some(_), None) => false,
            };
            if sourced.fact.timestamp_ms.is_none() {
                log::warn!(
                    "{} usage-presence fact from {} has no event timestamp{}",
                    sourced.provider,
                    sourced.source.display(),
                    if self.cutoff.is_some() {
                        " and was excluded from the selected time range"
                    } else {
                        ""
                    }
                );
            }
            if !included {
                continue;
            }

            let mut present = false;
            for unit in sourced.fact.units {
                if !unit.analysis_presence || unit.model.contains("<synthetic>") {
                    continue;
                }
                present = true;
                merge_analysis_metrics(&mut self.all, &unit.model, AnalysisMetrics::default());
                if let Some(rows) = self.provider_rows_mut(sourced.provider) {
                    merge_analysis_metrics(rows, &unit.model, AnalysisMetrics::default());
                }
            }
            if present && let Some(date) = date {
                self.add_date(Some(sourced.provider), date);
            }
        }

        for sourced in reduce_analysis_facts(std::mem::take(&mut self.facts)) {
            let date = sourced.fact.timestamp_ms.and_then(local_date_from_millis);
            let included = match (&self.cutoff, &date) {
                (None, _) => true,
                (Some(cutoff), Some(date)) => date >= cutoff,
                (Some(_), None) => false,
            };
            if sourced.fact.timestamp_ms.is_none() {
                log::warn!(
                    "{} analysis fact from {} has no invocation timestamp{}",
                    sourced.provider,
                    sourced.source.display(),
                    if self.cutoff.is_some() {
                        " and was excluded from the selected time range"
                    } else {
                        ""
                    }
                );
            }
            if !included {
                continue;
            }
            let model = if sourced.fact.model.is_empty() {
                log::warn!(
                    "{} tool fact from {} has no invocation model",
                    sourced.provider,
                    sourced.source.display()
                );
                "unknown".to_string()
            } else {
                sourced.fact.model
            };
            merge_analysis_metrics(&mut self.all, &model, sourced.fact.metrics);
            if let Some(rows) = self.provider_rows_mut(sourced.provider) {
                merge_analysis_metrics(rows, &model, sourced.fact.metrics);
            }
            if let Some(date) = date {
                self.add_date(Some(sourced.provider), date);
            }
        }
        let provider_days = ProviderActiveDays {
            claude: self.claude_dates.len(),
            codex: self.codex_dates.len(),
            copilot: self.copilot_dates.len(),
            gemini: self.gemini_dates.len(),
            grok: self.grok_dates.len(),
            opencode: self.opencode_dates.len(),
            cursor: self.cursor_dates.len(),
            hermes: self.hermes_dates.len(),
            total: self.all_dates.len(),
        };
        AnalysisData {
            rows: into_sorted_rows(self.all),
            per_provider: PerProviderAnalysisRows {
                claude: into_sorted_rows(self.claude),
                codex: into_sorted_rows(self.codex),
                copilot: into_sorted_rows(self.copilot),
                gemini: into_sorted_rows(self.gemini),
                grok: into_sorted_rows(self.grok),
                opencode: into_sorted_rows(self.opencode),
                cursor: into_sorted_rows(self.cursor),
            },
            provider_days,
        }
    }

    fn provider_rows_mut(
        &mut self,
        provider: ExtensionType,
    ) -> Option<&mut FastHashMap<String, AggregatedAnalysisRow>> {
        match provider {
            ExtensionType::ClaudeCode => Some(&mut self.claude),
            ExtensionType::Codex => Some(&mut self.codex),
            ExtensionType::Copilot => Some(&mut self.copilot),
            ExtensionType::Gemini => Some(&mut self.gemini),
            ExtensionType::Grok => Some(&mut self.grok),
            ExtensionType::OpenCode => Some(&mut self.opencode),
            ExtensionType::Cursor => Some(&mut self.cursor),
            ExtensionType::Hermes => None,
        }
    }
}

fn merge_analysis_metrics(
    target: &mut FastHashMap<String, AggregatedAnalysisRow>,
    model: &str,
    metrics: AnalysisMetrics,
) {
    let entry = target
        .entry(model.to_string())
        .or_insert_with(|| AnalysisMetrics::default().into_row(model.to_string()));
    entry.edit_lines += metrics.edit_lines;
    entry.read_lines += metrics.read_lines;
    entry.write_lines += metrics.write_lines;
    entry.bash_count += metrics.bash_count;
    entry.edit_count += metrics.edit_count;
    entry.read_count += metrics.read_count;
    entry.todo_write_count += metrics.todo_write_count;
    entry.write_count += metrics.write_count;
}

fn reduce_analysis_facts(facts: Vec<SourcedAnalysisFact>) -> Vec<SourcedAnalysisFact> {
    let mut anonymous = Vec::new();
    let mut stable: FastHashMap<String, Vec<SourcedAnalysisFact>> = FastHashMap::default();
    for sourced in facts {
        if let Some(id) = &sourced.fact.stable_id {
            stable
                .entry(format!("{}\0{id}", sourced.provider))
                .or_default()
                .push(sourced);
        } else {
            anonymous.push(sourced);
        }
    }
    for group in stable.into_values() {
        let canonical_timestamp = group
            .iter()
            .filter_map(|sourced| sourced.fact.timestamp_ms)
            .min();
        let invocation = group
            .iter()
            .filter(|sourced| has_invocation_count(sourced.fact.metrics))
            .min_by(|left, right| compare_invocations(left, right));
        let Some(invocation) = invocation else {
            continue;
        };
        let terminal = group
            .iter()
            .filter(|sourced| sourced.fact.status != ToolFactStatus::Pending)
            .max_by(|left, right| compare_outcomes(left, right));

        let mut winner = invocation.clone();
        winner.fact.timestamp_ms = canonical_timestamp;
        if winner.fact.model.is_empty()
            && let Some(model) = group
                .iter()
                .filter(|sourced| !sourced.fact.model.is_empty())
                .min_by(|left, right| compare_invocations(left, right))
                .map(|sourced| sourced.fact.model.clone())
        {
            winner.fact.model = model;
        }
        winner.fact.metrics = invocation_counts(invocation.fact.metrics);
        if let Some(terminal) = terminal {
            winner.fact.status = terminal.fact.status;
            winner.fact.observed_at_ms = terminal.fact.observed_at_ms;
            if terminal.fact.status == ToolFactStatus::Succeeded {
                winner
                    .fact
                    .metrics
                    .add_assign(success_effects(terminal.fact.metrics));
                winner.fact.effect = terminal.fact.effect.clone();
            } else {
                winner.fact.effect = None;
            }
        } else {
            winner.fact.status = ToolFactStatus::Pending;
            winner.fact.effect = None;
        }
        anonymous.push(winner);
    }
    for sourced in &mut anonymous {
        if sourced.fact.stable_id.is_none() && sourced.fact.status != ToolFactStatus::Succeeded {
            sourced.fact.metrics = invocation_counts(sourced.fact.metrics);
        }
    }
    anonymous.sort_unstable_by(|left, right| {
        provider_scan_rank(left.provider)
            .cmp(&provider_scan_rank(right.provider))
            .then_with(|| left.fact.timestamp_ms.cmp(&right.fact.timestamp_ms))
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.fact.source_order.cmp(&right.fact.source_order))
    });
    anonymous
}

fn has_invocation_count(metrics: AnalysisMetrics) -> bool {
    metrics.bash_count > 0
        || metrics.edit_count > 0
        || metrics.read_count > 0
        || metrics.todo_write_count > 0
        || metrics.write_count > 0
}

fn invocation_counts(metrics: AnalysisMetrics) -> AnalysisMetrics {
    AnalysisMetrics {
        bash_count: metrics.bash_count,
        edit_count: metrics.edit_count,
        read_count: metrics.read_count,
        todo_write_count: metrics.todo_write_count,
        write_count: metrics.write_count,
        ..AnalysisMetrics::default()
    }
}

fn success_effects(metrics: AnalysisMetrics) -> AnalysisMetrics {
    AnalysisMetrics {
        edit_lines: metrics.edit_lines,
        read_lines: metrics.read_lines,
        write_lines: metrics.write_lines,
        ..AnalysisMetrics::default()
    }
}

fn compare_invocations(
    left: &SourcedAnalysisFact,
    right: &SourcedAnalysisFact,
) -> std::cmp::Ordering {
    left.fact
        .timestamp_ms
        .unwrap_or(i64::MAX)
        .cmp(&right.fact.timestamp_ms.unwrap_or(i64::MAX))
        .then_with(|| left.source.cmp(&right.source))
        .then_with(|| left.fact.source_order.cmp(&right.fact.source_order))
}

fn compare_outcomes(left: &SourcedAnalysisFact, right: &SourcedAnalysisFact) -> std::cmp::Ordering {
    left.fact
        .status
        .cmp(&right.fact.status)
        .then_with(|| {
            left.fact
                .observed_at_ms
                .or(left.fact.timestamp_ms)
                .unwrap_or(i64::MIN)
                .cmp(
                    &right
                        .fact
                        .observed_at_ms
                        .or(right.fact.timestamp_ms)
                        .unwrap_or(i64::MIN),
                )
        })
        .then_with(|| left.source.cmp(&right.source))
        .then_with(|| left.fact.source_order.cmp(&right.fact.source_order))
}

fn extension_type_from_name(name: &str) -> Option<ExtensionType> {
    match name {
        "Claude-Code" => Some(ExtensionType::ClaudeCode),
        "Codex" => Some(ExtensionType::Codex),
        "Copilot-CLI" => Some(ExtensionType::Copilot),
        "Gemini" => Some(ExtensionType::Gemini),
        "Grok" => Some(ExtensionType::Grok),
        "OpenCode" => Some(ExtensionType::OpenCode),
        "Cursor" => Some(ExtensionType::Cursor),
        "Hermes" => Some(ExtensionType::Hermes),
        _ => None,
    }
}

fn local_date_from_millis(timestamp: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp).map(|datetime| {
        datetime
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string()
    })
}

/// Folds one parsed session's per-model counters into `aggregated`.
///
/// A record with one conversation model can be attributed directly. A legacy
/// aggregate carrying several models has already lost invocation-level model
/// ownership, so its metrics are kept under `unknown` instead of being copied
/// to every model. Event-aware callers use [`AnalysisProjection::add_facts`]
/// and retain exact attribution.
fn aggregate_analysis_result(
    aggregated: &mut FastHashMap<String, AggregatedAnalysisRow>,
    analysis: &CodeAnalysis,
) {
    for record in &analysis.records {
        let metrics = AnalysisMetrics::from_record(record);
        let models: Vec<_> = record
            .conversation_usage
            .keys()
            .filter(|model| !model.contains("<synthetic>"))
            .collect();
        match models.as_slice() {
            [model] => merge_analysis_metrics(aggregated, model, metrics),
            [] if metrics.has_activity() => merge_analysis_metrics(aggregated, "unknown", metrics),
            [] => {}
            _ if metrics.has_activity() => {
                merge_analysis_metrics(aggregated, "unknown", metrics);
            }
            _ => {
                for model in models {
                    merge_analysis_metrics(aggregated, model, AnalysisMetrics::default());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{CodeAnalysisRecord, CodeAnalysisToolCalls};
    use serde_json::json;

    fn analysis_with_advisor() -> CodeAnalysis {
        let mut conversation_usage = FastHashMap::default();
        conversation_usage.insert("claude-haiku-4-5".to_string(), json!({ "input_tokens": 4 }));
        let mut advisor_usage = FastHashMap::default();
        advisor_usage.insert(
            "claude-opus-4-8".to_string(),
            json!({ "input_tokens": 47579 }),
        );

        let record = CodeAnalysisRecord {
            total_unique_files: 1,
            total_write_lines: 10,
            total_read_lines: 20,
            total_edit_lines: 5,
            total_write_characters: 0,
            total_read_characters: 0,
            total_edit_characters: 0,
            write_file_details: vec![],
            read_file_details: vec![],
            edit_file_details: vec![],
            run_command_details: vec![],
            tool_call_counts: CodeAnalysisToolCalls {
                read: 4,
                write: 1,
                edit: 2,
                todo_write: 1,
                bash: 3,
            },
            conversation_usage,
            advisor_usage,
            task_id: String::new(),
            timestamp: 0,
            folder_path: String::new(),
            git_remote_url: String::new(),
        };

        CodeAnalysis {
            user: String::new(),
            extension_name: String::new(),
            insights_version: String::new(),
            machine_id: String::new(),
            records: vec![record],
        }
    }

    #[test]
    fn advisor_model_is_not_credited_with_file_operations() {
        // Regression guard: advisor-message usage lives in `advisor_usage`, not
        // `conversation_usage`, so the aggregator must not create a row for the
        // advisor model or credit it with the main model's tool / line counts.
        let analysis = analysis_with_advisor();
        let mut aggregated = FastHashMap::default();
        aggregate_analysis_result(&mut aggregated, &analysis);

        let main = aggregated
            .get("claude-haiku-4-5")
            .expect("main model row must exist");
        assert_eq!(main.read_lines, 20);
        assert_eq!(main.bash_count, 3);

        assert!(
            aggregated.get("claude-opus-4-8").is_none(),
            "advisor model must not be credited with the main model's file operations"
        );
    }

    #[test]
    fn legacy_multi_model_record_is_not_credited_to_every_model() {
        let mut analysis = analysis_with_advisor();
        analysis.records[0]
            .conversation_usage
            .insert("model-b".to_string(), json!({ "input_tokens": 1 }));
        let mut aggregated = FastHashMap::default();

        aggregate_analysis_result(&mut aggregated, &analysis);

        assert_eq!(aggregated.len(), 1);
        let unknown = aggregated
            .get("unknown")
            .expect("unattributable aggregate must remain visible");
        assert_eq!(unknown.read_lines, 20);
        assert_eq!(unknown.bash_count, 3);
    }

    #[test]
    fn successful_tool_outcome_wins_over_a_later_failed_replay() {
        let sourced = |status, observed_at_ms, metrics, effect| SourcedAnalysisFact {
            provider: ExtensionType::ClaudeCode,
            source: PathBuf::from(format!("{observed_at_ms}.jsonl")),
            source_index: 0,
            fact: AnalysisFact {
                stable_id: Some("shared-tool".to_string()),
                timestamp_ms: Some(1),
                observed_at_ms: Some(observed_at_ms),
                source_order: 0,
                model: "claude-test".to_string(),
                status,
                metrics,
                effect,
            },
        };
        let invocation = sourced(
            ToolFactStatus::Pending,
            1,
            AnalysisMetrics {
                read_count: 1,
                ..AnalysisMetrics::default()
            },
            None,
        );
        let succeeded = sourced(
            ToolFactStatus::Succeeded,
            2,
            AnalysisMetrics {
                read_lines: 2,
                ..AnalysisMetrics::default()
            },
            Some(AnalysisFactEffect::default()),
        );
        let later_failed = sourced(ToolFactStatus::Failed, 3, AnalysisMetrics::default(), None);

        let reduced = reduce_analysis_facts(vec![invocation, succeeded, later_failed]);

        assert_eq!(reduced.len(), 1);
        assert_eq!(reduced[0].fact.status, ToolFactStatus::Succeeded);
        assert_eq!(reduced[0].fact.metrics.read_count, 1);
        assert_eq!(reduced[0].fact.metrics.read_lines, 2);
        assert!(reduced[0].fact.effect.is_some());
    }
}
