use crate::cli::TimeRange;
use crate::config::ProvidersConfig;
use crate::constants::{FastHashMap, capacity};
use crate::models::{CodeAnalysis, ExtensionType, ProviderActiveDays};
use crate::session::cursor::{
    discover_cursor_store_dbs, load_conversation_model_snapshot,
    read_cursor_analysis_with_diagnostics, read_store_analysis,
};
use crate::session::diagnostics::DatabaseAnalysisRow;
use crate::session::opencode::read_opencode_analysis_with_diagnostics;
use crate::session::parser::parse_session_file_as_with_diagnostics;
use crate::session::sqlite::is_cacheable_sqlite_failure;
use crate::session::state::ParseMode;
use crate::summary_cache::{
    CompactSourceSummary, SourceFingerprint, SummaryCacheKey, SummaryKind, SummaryScanCache,
};
use crate::utils::directory::{FileInfo, collect_files_with_max_depth_diagnostics};
use crate::utils::{
    COPILOT_SESSION_MAX_DEPTH, GROK_SESSION_MAX_DEPTH, HelperPaths, get_current_user,
    get_machine_id, is_claude_session_file, is_codex_session_file, is_copilot_session_file,
    is_gemini_session_file, is_grok_session_file,
};
use anyhow::Result;
use rayon::prelude::*;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::collections::HashSet;
use std::path::Path;

// `AggregatedAnalysisRow` is a neutral DTO shared with the scan cache, so it
// lives in `models`; re-exported here to keep the `analysis::AggregatedAnalysisRow`
// (and `analysis::aggregator::AggregatedAnalysisRow`) paths working.
pub use crate::models::AggregatedAnalysisRow;

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

// Usage and analysis share one unified scan-diagnostics type; these historical
// names are kept as aliases so `analysis::AnalysisCollectionDiagnostics` and the
// per-source failure constructors keep working.
pub use crate::scan::ScanDiagnostics as AnalysisCollectionDiagnostics;
pub use crate::scan::ScanFailure as AnalysisCollectionFailure;

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
    /// Returns whether the dataset contains no parsed sessions.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Returns the number of parsed sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Projects this canonical dataset into the compact display summaries.
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
    let mut projection = AnalysisProjection::new();
    let diagnostics = visit_analysis_sessions_from_paths_with(
        paths,
        time_range,
        providers,
        ParseMode::UsageOnly,
        &mut |session| projection.add_session(&session),
    )?;
    Ok(AnalysisAggregation {
        data: projection.finish(),
        diagnostics,
    })
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
    let mut sessions = Vec::new();
    let diagnostics = visit_analysis_sessions_from_paths_with(
        paths,
        time_range,
        providers,
        mode,
        &mut |session| sessions.push(session),
    )?;
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
    F: FnMut(AnalysisSession),
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
        match read_opencode_analysis_with_diagnostics(&paths.opencode_db, time_range, mode) {
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
                visit_database_sessions(ExtensionType::OpenCode, result.rows, visitor);
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
            time_range,
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
        visit_database_sessions(ExtensionType::Cursor, result.rows, visitor);
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
    let mut projection = AnalysisProjection::new();
    let mut diagnostics = AnalysisCollectionDiagnostics::default();
    let mut seen = HashSet::new();

    if providers.claude {
        crate::scan::scan_cached_files(
            &paths.claude_session_dir,
            ExtensionType::ClaudeCode,
            is_claude_session_file,
            time_range,
            None,
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
            None,
        )?;
    }
    if providers.codex {
        crate::scan::scan_cached_files(
            &paths.codex_session_dir,
            ExtensionType::Codex,
            is_codex_session_file,
            time_range,
            None,
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
            None,
        )?;
    }
    if providers.copilot {
        crate::scan::scan_cached_files(
            &paths.copilot_session_dir,
            ExtensionType::Copilot,
            is_copilot_session_file,
            time_range,
            Some(COPILOT_SESSION_MAX_DEPTH),
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
            None,
        )?;
    }
    if providers.gemini {
        crate::scan::scan_cached_files(
            &paths.gemini_session_dir,
            ExtensionType::Gemini,
            is_gemini_session_file,
            time_range,
            None,
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
            None,
        )?;
    }
    if providers.grok {
        crate::scan::scan_cached_files(
            &paths.grok_session_dir,
            ExtensionType::Grok,
            is_grok_session_file,
            time_range,
            Some(GROK_SESSION_MAX_DEPTH),
            cache,
            &mut seen,
            &mut projection,
            &mut diagnostics,
            None,
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
    diagnostics.finalize();
    Ok(AnalysisAggregation {
        data: projection.finish(),
        diagnostics,
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
        crate::scan::fold_cached(provider, source, cached, projection, diagnostics);
        return;
    }

    cache.record_parse();
    match read_opencode_analysis_with_diagnostics(source, time_range, ParseMode::UsageOnly) {
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
                summary.add_analysis(row.analysis, row.date, 0.0, true);
            }
            let loaded = crate::scan::LoadedCompactSummary {
                summary,
                parsed: !complete_failure,
                failure,
            };
            crate::scan::fold_loaded(provider, source, &loaded, projection, diagnostics);
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
            crate::scan::fold_cached(provider, &store, cached, projection, diagnostics);
            continue;
        }

        cache.record_parse();
        match read_store_analysis(
            &store,
            &conv_models,
            time_range,
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
                for (date, analysis) in result.rows {
                    summary.add_analysis(analysis, date, 0.0, true);
                }
                let loaded = crate::scan::LoadedCompactSummary {
                    summary,
                    parsed: !complete_failure,
                    failure,
                };
                crate::scan::fold_loaded(provider, &store, &loaded, projection, diagnostics);
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
        if let Some(date) = local_date_from_millis(record.timestamp) {
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

/// Drains a model-keyed map into a `Vec` sorted by model name.
fn into_sorted_rows(map: FastHashMap<String, AggregatedAnalysisRow>) -> Vec<AggregatedAnalysisRow> {
    let mut v: Vec<AggregatedAnalysisRow> = map.into_values().collect();
    v.sort_unstable_by(|a, b| a.model.cmp(&b.model));
    v
}

type FileSessionOutcome = std::result::Result<
    (Option<AnalysisSession>, Option<AnalysisCollectionFailure>),
    AnalysisCollectionFailure,
>;

/// Visits one file-backed provider in deterministic path order.
#[allow(clippy::too_many_arguments)]
fn visit_file_sessions<F, V>(
    dir: &Path,
    provider: ExtensionType,
    filter_fn: F,
    time_range: TimeRange,
    max_depth: Option<usize>,
    mode: ParseMode,
    diagnostics: &mut AnalysisCollectionDiagnostics,
    visitor: &mut V,
) -> Result<()>
where
    F: Copy + Fn(&Path) -> bool + Sync + Send,
    V: FnMut(AnalysisSession),
{
    let discovery = collect_files_with_max_depth_diagnostics(dir, filter_fn, time_range, max_depth);
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
            let FileInfo {
                path,
                modified_date,
            } = file_info;
            match parse_session_file_as_with_diagnostics(&path, provider, mode, None) {
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
                    let partial_failure =
                        (partial_failure_count > 0).then_some(AnalysisCollectionFailure {
                            provider,
                            source: path,
                            error: crate::session::diagnostics::partial_failure_reason(
                                partial_failure_count,
                            ),
                        });
                    let session = parsed.diagnostics.should_emit_session().then_some({
                        AnalysisSession {
                            provider,
                            date: modified_date,
                            analysis: parsed.analysis,
                        }
                    });
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
    // A partial parse keeps its recognized data; logging it as "failed to
    // collect" would read as a dropped source.
    if crate::session::diagnostics::is_partial_failure_reason(&failure.error) {
        log::warn!(
            "{} analysis from {}: {}",
            failure.provider,
            failure.source.display(),
            failure.error
        );
    } else {
        log::warn!(
            "failed to collect {} analysis from {}: {}",
            failure.provider,
            failure.source.display(),
            failure.error
        );
    }
    diagnostics.failures.push(failure);
}

fn visit_database_sessions<F>(
    provider: ExtensionType,
    mut rows: Vec<DatabaseAnalysisRow>,
    visitor: &mut F,
) where
    F: FnMut(AnalysisSession),
{
    rows.sort_unstable_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then_with(|| a.source_id.cmp(&b.source_id))
    });
    for row in rows {
        visitor(AnalysisSession {
            provider,
            date: row.date,
            analysis: row.analysis,
        });
    }
}

/// Mutable accumulator shared by batch and single-file projections.
struct AnalysisProjection {
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

impl crate::scan::CompactSink for AnalysisProjection {
    fn fold(&mut self, provider: ExtensionType, summary: &CompactSourceSummary) {
        self.add_compact(provider, summary);
    }
}

impl AnalysisProjection {
    fn new() -> Self {
        Self {
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

    fn add_compact(&mut self, provider: ExtensionType, summary: &CompactSourceSummary) {
        merge_compact_rows(&mut self.all, &summary.analysis);
        let provider_rows = match provider {
            ExtensionType::ClaudeCode => Some(&mut self.claude),
            ExtensionType::Codex => Some(&mut self.codex),
            ExtensionType::Copilot => Some(&mut self.copilot),
            ExtensionType::Gemini => Some(&mut self.gemini),
            ExtensionType::Grok => Some(&mut self.grok),
            ExtensionType::OpenCode => Some(&mut self.opencode),
            ExtensionType::Cursor => Some(&mut self.cursor),
            ExtensionType::Hermes => None,
        };
        if let Some(rows) = provider_rows {
            merge_compact_rows(rows, &summary.analysis);
        }

        self.all_dates
            .extend(summary.analysis_dates.iter().cloned());
        let dates = match provider {
            ExtensionType::ClaudeCode => Some(&mut self.claude_dates),
            ExtensionType::Codex => Some(&mut self.codex_dates),
            ExtensionType::Copilot => Some(&mut self.copilot_dates),
            ExtensionType::Gemini => Some(&mut self.gemini_dates),
            ExtensionType::Grok => Some(&mut self.grok_dates),
            ExtensionType::OpenCode => Some(&mut self.opencode_dates),
            ExtensionType::Cursor => Some(&mut self.cursor_dates),
            ExtensionType::Hermes => Some(&mut self.hermes_dates),
        };
        if let Some(dates) = dates {
            dates.extend(summary.analysis_dates.iter().cloned());
        }
    }

    fn add_date(&mut self, provider: Option<ExtensionType>, date: String) {
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

    fn finish(self) -> AnalysisData {
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
}

fn merge_compact_rows(
    target: &mut FastHashMap<String, AggregatedAnalysisRow>,
    source: &FastHashMap<String, AggregatedAnalysisRow>,
) {
    for (model, row) in source {
        let entry = target
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
        entry.edit_lines += row.edit_lines;
        entry.read_lines += row.read_lines;
        entry.write_lines += row.write_lines;
        entry.bash_count += row.bash_count;
        entry.edit_count += row.edit_count;
        entry.read_count += row.read_count;
        entry.todo_write_count += row.todo_write_count;
        entry.write_count += row.write_count;
    }
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
}
