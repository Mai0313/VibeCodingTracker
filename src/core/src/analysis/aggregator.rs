//! Session collection and per-model aggregation for the `analysis` command.

use crate::config::ProvidersConfig;
use crate::constants::{FastHashMap, FastHashSet, capacity};
use crate::ledger::{DayEntry, SessionLedger, merge_analysis_rows};
use crate::models::TimeRange;
use crate::models::{CodeAnalysis, ExtensionType, ProviderActiveDays};
use crate::scan::{ScanFeature, scan_cursor_stores, scan_database_half};
use crate::session::cursor::read_cursor_analysis_with_diagnostics;
use crate::session::diagnostics::DatabaseAnalysisRow;
use crate::session::opencode::read_opencode_analysis_with_diagnostics;
use crate::session::parser::parse_session_file_typed_as_with_diagnostics;
use crate::session::state::ParseMode;
use crate::utils::directory::{FileInfo, collect_provider_files_diagnostics};
use crate::utils::{
    ANTIGRAVITY_SESSION_MAX_DEPTH, COPILOT_SESSION_MAX_DEPTH, DSH_SESSION_MAX_DEPTH,
    GROK_SESSION_MAX_DEPTH, HelperPaths, is_antigravity_session_file, is_claude_session_file,
    is_codex_session_file, is_copilot_session_file, is_dsh_session_file, is_gemini_session_file,
    is_grok_session_file,
};
use anyhow::Result;
use rayon::prelude::*;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::path::{Path, PathBuf};

// Defined in `models` so the scan cache can share it without a cycle; re-exported here to preserve `analysis::AggregatedAnalysisRow`.
pub use crate::models::AggregatedAnalysisRow;

/// Aggregated analysis rows and per-provider active-day counts.
#[derive(Debug, Clone, Serialize)]
pub struct AnalysisData {
    /// Rows aggregated across all providers, sorted by model name.
    ///
    /// Same-named models from different providers share a single row here.
    pub rows: Vec<AggregatedAnalysisRow>,
    /// Same aggregation partitioned by originating provider before model roll-up.
    pub per_provider: PerProviderAnalysisRows,
    /// Distinct active-day counts per provider and overall.
    pub provider_days: ProviderActiveDays,
}

/// Aggregated analysis data paired with scan diagnostics.
pub struct AnalysisCollection {
    /// Successfully parsed metrics, even when some other sources failed.
    pub data: AnalysisData,
    /// Candidate, success, and failure information for the scan.
    pub diagnostics: ScanDiagnostics,
}

/// Aggregated analysis rows partitioned by the provider that produced them.
///
/// Attribution follows the source directory or database rather than the model name.
#[derive(Debug, Default, Clone, Serialize)]
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
    /// Rows from the DeepSeek Harness session directory.
    pub deepseek: Vec<AggregatedAnalysisRow>,
    /// Rows from the Antigravity conversation directories.
    pub antigravity: Vec<AggregatedAnalysisRow>,
    /// Rows from the OpenCode database.
    pub opencode: Vec<AggregatedAnalysisRow>,
    /// Rows from the Cursor chat stores.
    pub cursor: Vec<AggregatedAnalysisRow>,
}

/// One parsed session in the canonical batch-analysis dataset.
///
/// `provider` and `date` carry source provenance for summary projection and are omitted when [`AnalysisDataset`] serializes to JSON.
#[derive(Debug, Clone)]
pub struct AnalysisSession {
    /// Provider selected from the source directory or database.
    pub provider: ExtensionType,
    /// Local `YYYY-MM-DD` date used by the active-day summary.
    pub date: String,
    /// Complete normalized parser result for this session.
    pub analysis: CodeAnalysis,
}

// Re-exported from `scan` so callers can reach the shared diagnostics type as `analysis::ScanDiagnostics`.
pub use crate::scan::{ScanDiagnostics, ScanFailure};

/// Canonical batch-analysis dataset before summary projection.
///
/// Serializes transparently as a JSON array of [`CodeAnalysis`] objects, omitting session provenance and [`diagnostics`](Self::diagnostics).
#[derive(Debug, Clone, Default)]
pub struct AnalysisDataset {
    /// Sessions in deterministic provider and source order.
    pub sessions: Vec<AnalysisSession>,
    /// Candidate, success, and failure information from collection; omitted during JSON serialization.
    pub diagnostics: ScanDiagnostics,
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

    /// Returns the per-model and per-provider summary projection of this dataset.
    pub fn summarize(&self) -> AnalysisData {
        project_analysis_dataset(self)
    }

    /// Returns the summary projection paired with this dataset's collection diagnostics.
    pub fn summarize_with_diagnostics(&self) -> AnalysisCollection {
        AnalysisCollection {
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

/// Aggregates file-operation and tool-call metrics across all analysis-capable providers within `time_range`.
///
/// Scans every enabled provider except Hermes (which is usage-only) through the on-disk session ledger in [`ParseMode::UsageOnly`], returning rows sorted by model name alongside per-provider active-day counts. Missing provider directories yield no rows.
///
/// # Examples
///
/// ```no_run
/// use vct_core::analysis::aggregate_sessions_by_model;
/// use vct_core::TimeRange;
///
/// let data = aggregate_sessions_by_model(TimeRange::All)?;
/// for row in &data.rows {
///     println!("{}: {} edit lines", row.model, row.edit_lines);
/// }
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
///
/// Returns an error if provider paths cannot be resolved. Traversal, metadata, and per-source parse failures are recorded in scan diagnostics and discarded by this entry point; use [`aggregate_sessions_by_model_with_diagnostics`] to inspect them.
pub fn aggregate_sessions_by_model(time_range: TimeRange) -> Result<AnalysisData> {
    aggregate_sessions_by_model_with_providers(time_range, ProvidersConfig::default())
}

/// Aggregates file-operation and tool-call metrics across enabled providers in `providers`.
///
/// Behaves like [`aggregate_sessions_by_model`], skipping providers disabled in `providers`.
///
/// # Errors
///
/// Returns an error if provider paths cannot be resolved.
pub fn aggregate_sessions_by_model_with_providers(
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<AnalysisData> {
    Ok(aggregate_sessions_by_model_with_diagnostics(time_range, providers)?.data)
}

/// Aggregates analysis metrics across enabled providers and returns scan diagnostics.
///
/// Reads and updates the on-disk session ledger under `~/.vct`, retaining sessions whose sources have been deleted; ledger save failures are logged without failing the scan. Use [`aggregate_sessions_by_model_from_paths_with_diagnostics`] for a ledger-free scan over explicit paths.
///
/// # Errors
///
/// Returns an error if provider paths cannot be resolved.
pub fn aggregate_sessions_by_model_with_diagnostics(
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<AnalysisCollection> {
    aggregate_sessions_by_model_with_diagnostics_and_remote_hook(time_range, providers, || {})
}

/// Like [`aggregate_sessions_by_model_with_diagnostics`], invoking `before_remotes` after local scanning finishes and before remote snapshots are loaded and folded.
#[doc(hidden)]
pub fn aggregate_sessions_by_model_with_diagnostics_and_remote_hook(
    time_range: TimeRange,
    providers: ProvidersConfig,
    before_remotes: impl FnOnce(),
) -> Result<AnalysisCollection> {
    let paths = crate::utils::resolve_paths()?;
    let mut ledger = SessionLedger::open(&paths.cache_dir);
    let collection = aggregate_sessions_by_model_from_paths_with_cache_and_remote_hook(
        &paths,
        time_range,
        providers,
        &mut ledger,
        before_remotes,
    )?;
    if let Err(error) = ledger.save() {
        log::warn!("failed to save the session ledger: {error:#}");
    }
    Ok(collection)
}

/// Aggregates file-operation metrics from explicit provider locations in `paths` without touching the session ledger.
pub fn aggregate_sessions_by_model_from_paths(
    paths: &HelperPaths,
    time_range: TimeRange,
) -> Result<AnalysisData> {
    aggregate_sessions_by_model_from_paths_with_providers(
        paths,
        time_range,
        ProvidersConfig::default(),
    )
}

/// Aggregates file-operation metrics from `paths` for providers enabled in `providers`, without touching the session ledger.
pub fn aggregate_sessions_by_model_from_paths_with_providers(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<AnalysisData> {
    Ok(aggregate_sessions_by_model_from_paths_with_diagnostics(paths, time_range, providers)?.data)
}

/// Aggregates analysis metrics from `paths` with scan diagnostics, reading every source afresh without a ledger.
///
/// Parsed sessions fold into the summary as each provider finishes rather than accumulating an [`AnalysisDataset`].
pub fn aggregate_sessions_by_model_from_paths_with_diagnostics(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<AnalysisCollection> {
    let mut projection = AnalysisProjection::new();
    let diagnostics = visit_analysis_sessions_from_paths_with(
        paths,
        time_range,
        providers,
        ParseMode::UsageOnly,
        &mut |session| projection.add_session(&session),
    )?;
    Ok(AnalysisCollection {
        data: projection.finish(),
        diagnostics,
    })
}

/// Collects the canonical batch-analysis dataset from the current user's resolved provider paths.
///
/// Providers appear in canonical order (Claude, Codex, Copilot, Gemini, Grok, DeepSeek, Antigravity, OpenCode, Cursor). `mode` controls only detail-list retention; scalar counters remain populated.
///
/// # Errors
///
/// Returns an error if provider paths cannot be resolved.
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
/// File-backed sessions are sorted by path and database sessions by date and source identity within the fixed provider order, yielding deterministic JSON output.
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

/// Visits parsed sessions across enabled providers in deterministic provider and source order.
fn visit_analysis_sessions_from_paths_with<F>(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    mode: ParseMode,
    visitor: &mut F,
) -> Result<ScanDiagnostics>
where
    F: FnMut(AnalysisSession),
{
    let mut diagnostics = ScanDiagnostics::default();

    if providers.claude {
        visit_file_sessions(
            &[paths.claude_session_dir.as_path()],
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
            &paths.codex_session_dirs(),
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
            &[paths.copilot_session_dir.as_path()],
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
            &[paths.gemini_session_dir.as_path()],
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
            &[paths.grok_session_dir.as_path()],
            ExtensionType::Grok,
            is_grok_session_file,
            time_range,
            Some(GROK_SESSION_MAX_DEPTH),
            mode,
            &mut diagnostics,
            visitor,
        )?;
    }

    if providers.dsh {
        visit_file_sessions(
            &[paths.dsh_session_dir.as_path()],
            ExtensionType::DeepSeek,
            is_dsh_session_file,
            time_range,
            Some(DSH_SESSION_MAX_DEPTH),
            mode,
            &mut diagnostics,
            visitor,
        )?;
    }

    if providers.antigravity {
        let antigravity_dirs = paths.antigravity_session_dirs();
        let antigravity_dir_refs: Vec<&Path> =
            antigravity_dirs.iter().map(PathBuf::as_path).collect();
        visit_file_sessions(
            &antigravity_dir_refs,
            ExtensionType::Antigravity,
            is_antigravity_session_file,
            time_range,
            Some(ANTIGRAVITY_SESSION_MAX_DEPTH),
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

/// Runs an incremental analysis scan rooted at the current user's paths through `ledger`.
///
/// # Errors
///
/// Returns an error if provider paths cannot be resolved.
pub fn aggregate_sessions_by_model_with_cache(
    time_range: TimeRange,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
) -> Result<AnalysisCollection> {
    aggregate_sessions_by_model_from_paths_with_cache(
        &crate::utils::resolve_paths()?,
        time_range,
        providers,
        ledger,
    )
}

/// Runs an incremental analysis scan from `paths`, updating `ledger` in place.
///
/// Unchanged sources fold from their ledger entries and deleted sources remain counted. Sources are parsed and stamped without a tier snapshot; a subsequent usage scan with tier thresholds re-reads them to classify tiers accurately.
pub fn aggregate_sessions_by_model_from_paths_with_cache(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
) -> Result<AnalysisCollection> {
    aggregate_sessions_by_model_from_paths_with_cache_and_remote_hook(
        paths,
        time_range,
        providers,
        ledger,
        || {},
    )
}

fn aggregate_sessions_by_model_from_paths_with_cache_and_remote_hook(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
    before_remotes: impl FnOnce(),
) -> Result<AnalysisCollection> {
    ledger.begin_local_scan();
    let mut projection = AnalysisProjection::new();
    let mut diagnostics = ScanDiagnostics::default();

    crate::scan::scan_all_cached_files(
        paths,
        providers,
        time_range,
        ledger,
        ScanFeature::Analysis,
        &mut projection,
        &mut diagnostics,
        None,
    )?;

    // Read all time from databases and apply `time_range` when folding so narrower ranges do not mark older sessions missing.
    if providers.opencode {
        scan_database_half(
            ExtensionType::OpenCode,
            &paths.opencode_db,
            ScanFeature::Analysis,
            None,
            time_range,
            ledger,
            &mut projection,
            &mut diagnostics,
            || crate::scan::database::read_opencode_analysis_half(&paths.opencode_db),
        );
    }
    if providers.cursor {
        scan_cursor_stores(
            paths,
            ScanFeature::Analysis,
            None,
            time_range,
            ledger,
            &mut projection,
            &mut diagnostics,
        );
    }

    before_remotes();
    ledger.reload_remotes();
    // Hermes records no file operations, so analysis folds it nowhere.
    crate::scan::fold_remotes(
        ledger,
        |provider| provider != ExtensionType::Hermes && providers.includes(provider),
        time_range,
        &mut projection,
    );

    diagnostics.finalize();
    Ok(AnalysisCollection {
        data: projection.finish(),
        diagnostics,
    })
}

/// Projects a canonical [`AnalysisDataset`] into per-model and per-provider [`AnalysisData`].
pub fn project_analysis_dataset(dataset: &AnalysisDataset) -> AnalysisData {
    let mut projection = AnalysisProjection::new();
    for session in &dataset.sessions {
        projection.add_session(session);
    }
    projection.finish()
}

/// Projects a single [`CodeAnalysis`] into [`AnalysisData`], matching the batch summary shape.
pub fn project_code_analysis(analysis: &CodeAnalysis) -> AnalysisData {
    let provider = extension_type_from_name(&analysis.extension_name);
    let mut projection = AnalysisProjection::new();
    projection.add_analysis(provider, analysis);

    let mut dates: FastHashSet<String> = FastHashSet::default();
    for record in &analysis.records {
        if let Some(date) = local_date_from_millis(record.timestamp) {
            dates.insert(date);
        }
    }
    if dates.is_empty() && !analysis.records.is_empty() {
        dates.insert("single".to_string());
    }
    for date in dates {
        projection.add_date(provider, &date);
    }

    projection.finish()
}

/// Drains a model-keyed map into a `Vec` sorted by model name.
fn into_sorted_rows(map: FastHashMap<String, AggregatedAnalysisRow>) -> Vec<AggregatedAnalysisRow> {
    let mut v: Vec<AggregatedAnalysisRow> = map.into_values().collect();
    v.sort_unstable_by(|a, b| a.model.cmp(&b.model));
    v
}

type FileSessionOutcome =
    std::result::Result<(Option<AnalysisSession>, Option<ScanFailure>), ScanFailure>;

/// Visits one file-backed provider in deterministic path order.
#[allow(clippy::too_many_arguments)]
fn visit_file_sessions<F, V>(
    dirs: &[&Path],
    provider: ExtensionType,
    filter_fn: F,
    time_range: TimeRange,
    max_depth: Option<usize>,
    mode: ParseMode,
    diagnostics: &mut ScanDiagnostics,
    visitor: &mut V,
) -> Result<()>
where
    F: Copy + Fn(&Path) -> bool + Sync + Send,
    V: FnMut(AnalysisSession),
{
    let discovery =
        collect_provider_files_diagnostics(dirs, provider, filter_fn, time_range, max_depth);
    diagnostics.candidates += discovery.failures.len();
    for failure in discovery.failures {
        record_failure(diagnostics, provider, &failure.path, failure.error);
    }

    let mut files = discovery.files;
    files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    diagnostics.candidates += files.len();

    // `Vec::into_par_iter` is indexed, preserving the sorted path order through parallel parsing.
    let outcomes: Vec<FileSessionOutcome> = files
        .into_par_iter()
        .map(|file_info| {
            let FileInfo {
                path,
                modified_date,
                ..
            } = file_info;
            match parse_session_file_typed_as_with_diagnostics(&path, provider, mode, None) {
                Ok(parsed) if parsed.diagnostics.is_complete_failure() => {
                    let error = if parsed.diagnostics.recognized_records == 0 {
                        "source contained no recognized provider records".to_string()
                    } else {
                        format!(
                            "none of {} analyzer-relevant provider records used a supported schema",
                            parsed.diagnostics.relevant_records
                        )
                    };
                    Err(ScanFailure {
                        provider,
                        source: path,
                        error,
                    })
                }
                Ok(parsed)
                    if parsed.diagnostics.should_emit_session()
                        && parsed.analysis.records.is_empty() =>
                {
                    Err(ScanFailure {
                        provider,
                        source: path,
                        error: "normalized source produced no analysis records".to_string(),
                    })
                }
                Ok(parsed) => {
                    let partial_failure_count = parsed.diagnostics.partial_failure_count();
                    let partial_failure = (partial_failure_count > 0).then_some(ScanFailure {
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
                Err(err) => Err(ScanFailure {
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
    diagnostics: &mut ScanDiagnostics,
    provider: ExtensionType,
    source: &Path,
    error: String,
) {
    push_failure(
        diagnostics,
        ScanFailure {
            provider,
            source: source.to_path_buf(),
            error,
        },
    );
}

fn push_failure(diagnostics: &mut ScanDiagnostics, failure: ScanFailure) {
    // Partial parses retain recognized data, so log them without the "failed to collect" prefix.
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

/// Mutable accumulator shared by the batch, cached, and single-file projections.
struct AnalysisProjection {
    unattributed: FastHashMap<String, AggregatedAnalysisRow>,
    claude: FastHashMap<String, AggregatedAnalysisRow>,
    codex: FastHashMap<String, AggregatedAnalysisRow>,
    copilot: FastHashMap<String, AggregatedAnalysisRow>,
    gemini: FastHashMap<String, AggregatedAnalysisRow>,
    grok: FastHashMap<String, AggregatedAnalysisRow>,
    deepseek: FastHashMap<String, AggregatedAnalysisRow>,
    antigravity: FastHashMap<String, AggregatedAnalysisRow>,
    opencode: FastHashMap<String, AggregatedAnalysisRow>,
    cursor: FastHashMap<String, AggregatedAnalysisRow>,
    unattributed_dates: FastHashSet<String>,
    claude_dates: FastHashSet<String>,
    codex_dates: FastHashSet<String>,
    copilot_dates: FastHashSet<String>,
    gemini_dates: FastHashSet<String>,
    grok_dates: FastHashSet<String>,
    deepseek_dates: FastHashSet<String>,
    antigravity_dates: FastHashSet<String>,
    opencode_dates: FastHashSet<String>,
    cursor_dates: FastHashSet<String>,
    hermes_dates: FastHashSet<String>,
}

impl crate::scan::CompactSink for AnalysisProjection {
    fn fold(&mut self, provider: ExtensionType, date: &str, day: &DayEntry) {
        let rows = self.provider_rows_mut(Some(provider));
        merge_analysis_rows(rows, &day.analysis);
        if day.active.analysis {
            self.add_date(Some(provider), date);
        }
    }
}

impl AnalysisProjection {
    fn new() -> Self {
        Self {
            unattributed: FastHashMap::default(),
            claude: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            codex: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            copilot: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            gemini: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            grok: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            deepseek: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            antigravity: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            opencode: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            cursor: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            unattributed_dates: FastHashSet::default(),
            claude_dates: FastHashSet::default(),
            codex_dates: FastHashSet::default(),
            copilot_dates: FastHashSet::default(),
            gemini_dates: FastHashSet::default(),
            grok_dates: FastHashSet::default(),
            deepseek_dates: FastHashSet::default(),
            antigravity_dates: FastHashSet::default(),
            opencode_dates: FastHashSet::default(),
            cursor_dates: FastHashSet::default(),
            hermes_dates: FastHashSet::default(),
        }
    }

    fn provider_rows_mut(
        &mut self,
        provider: Option<ExtensionType>,
    ) -> &mut FastHashMap<String, AggregatedAnalysisRow> {
        match provider {
            Some(ExtensionType::ClaudeCode) => &mut self.claude,
            Some(ExtensionType::Codex) => &mut self.codex,
            Some(ExtensionType::Copilot) => &mut self.copilot,
            Some(ExtensionType::Gemini) => &mut self.gemini,
            Some(ExtensionType::Grok) => &mut self.grok,
            Some(ExtensionType::DeepSeek) => &mut self.deepseek,
            Some(ExtensionType::Antigravity) => &mut self.antigravity,
            Some(ExtensionType::OpenCode) => &mut self.opencode,
            Some(ExtensionType::Cursor) => &mut self.cursor,
            Some(ExtensionType::Hermes) | None => &mut self.unattributed,
        }
    }

    fn add_session(&mut self, session: &AnalysisSession) {
        self.add_analysis(Some(session.provider), &session.analysis);
        self.add_date(Some(session.provider), &session.date);
    }

    fn add_analysis(&mut self, provider: Option<ExtensionType>, analysis: &CodeAnalysis) {
        let rows = self.provider_rows_mut(provider);
        aggregate_analysis_result(rows, analysis);
    }

    fn add_date(&mut self, provider: Option<ExtensionType>, date: &str) {
        let dates = match provider {
            Some(ExtensionType::ClaudeCode) => &mut self.claude_dates,
            Some(ExtensionType::Codex) => &mut self.codex_dates,
            Some(ExtensionType::Copilot) => &mut self.copilot_dates,
            Some(ExtensionType::Gemini) => &mut self.gemini_dates,
            Some(ExtensionType::Grok) => &mut self.grok_dates,
            Some(ExtensionType::DeepSeek) => &mut self.deepseek_dates,
            Some(ExtensionType::Antigravity) => &mut self.antigravity_dates,
            Some(ExtensionType::OpenCode) => &mut self.opencode_dates,
            Some(ExtensionType::Cursor) => &mut self.cursor_dates,
            Some(ExtensionType::Hermes) => &mut self.hermes_dates,
            None => &mut self.unattributed_dates,
        };
        if !dates.contains(date) {
            dates.insert(date.to_string());
        }
    }

    fn finish(self) -> AnalysisData {
        let mut all_dates: FastHashSet<&str> = FastHashSet::default();
        for set in [
            &self.claude_dates,
            &self.codex_dates,
            &self.copilot_dates,
            &self.gemini_dates,
            &self.grok_dates,
            &self.deepseek_dates,
            &self.antigravity_dates,
            &self.opencode_dates,
            &self.cursor_dates,
            &self.hermes_dates,
            &self.unattributed_dates,
        ] {
            all_dates.extend(set.iter().map(String::as_str));
        }
        let provider_days = ProviderActiveDays {
            claude: self.claude_dates.len(),
            codex: self.codex_dates.len(),
            copilot: self.copilot_dates.len(),
            gemini: self.gemini_dates.len(),
            grok: self.grok_dates.len(),
            deepseek: self.deepseek_dates.len(),
            antigravity: self.antigravity_dates.len(),
            opencode: self.opencode_dates.len(),
            cursor: self.cursor_dates.len(),
            hermes: self.hermes_dates.len(),
            total: all_dates.len(),
        };

        let mut all: FastHashMap<String, AggregatedAnalysisRow> =
            FastHashMap::with_capacity(capacity::MODEL_COMBINATIONS);
        for map in [
            &self.claude,
            &self.codex,
            &self.copilot,
            &self.gemini,
            &self.grok,
            &self.deepseek,
            &self.antigravity,
            &self.opencode,
            &self.cursor,
            &self.unattributed,
        ] {
            for (model, row) in map {
                match all.get_mut(model) {
                    Some(entry) => {
                        entry.edit_lines += row.edit_lines;
                        entry.read_lines += row.read_lines;
                        entry.write_lines += row.write_lines;
                        entry.bash_count += row.bash_count;
                        entry.edit_count += row.edit_count;
                        entry.read_count += row.read_count;
                        entry.todo_write_count += row.todo_write_count;
                        entry.write_count += row.write_count;
                    }
                    None => {
                        all.insert(model.clone(), row.clone());
                    }
                }
            }
        }

        AnalysisData {
            rows: into_sorted_rows(all),
            per_provider: PerProviderAnalysisRows {
                claude: into_sorted_rows(self.claude),
                codex: into_sorted_rows(self.codex),
                copilot: into_sorted_rows(self.copilot),
                gemini: into_sorted_rows(self.gemini),
                grok: into_sorted_rows(self.grok),
                deepseek: into_sorted_rows(self.deepseek),
                antigravity: into_sorted_rows(self.antigravity),
                opencode: into_sorted_rows(self.opencode),
                cursor: into_sorted_rows(self.cursor),
            },
            provider_days,
        }
    }
}

fn extension_type_from_name(name: &str) -> Option<ExtensionType> {
    match name {
        "Claude-Code" => Some(ExtensionType::ClaudeCode),
        "Codex" => Some(ExtensionType::Codex),
        "Copilot-CLI" => Some(ExtensionType::Copilot),
        "Gemini" => Some(ExtensionType::Gemini),
        "Grok" => Some(ExtensionType::Grok),
        "DeepSeek-Harness" => Some(ExtensionType::DeepSeek),
        "Antigravity" => Some(ExtensionType::Antigravity),
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

/// Folds one session's line and tool-call counts into `aggregated`, preferring `per_model_analysis` when populated and falling back to each non-`<synthetic>` model in `conversation_usage`.
fn aggregate_analysis_result(
    aggregated: &mut FastHashMap<String, AggregatedAnalysisRow>,
    analysis: &CodeAnalysis,
) {
    for record in &analysis.records {
        if !record.per_model_analysis.is_empty() {
            for (model, row) in &record.per_model_analysis {
                if model.contains("<synthetic>") {
                    continue;
                }
                let entry = match aggregated.get_mut(model) {
                    Some(entry) => entry,
                    None => aggregated
                        .entry(model.clone())
                        .or_insert_with(|| AggregatedAnalysisRow::empty(model)),
                };
                entry.add_assign(row);
            }
            continue;
        }
        for model in record.conversation_usage.keys() {
            if model.contains("<synthetic>") {
                continue;
            }

            let entry = match aggregated.get_mut(model) {
                Some(entry) => entry,
                None => aggregated
                    .entry(model.clone())
                    .or_insert_with(|| AggregatedAnalysisRow::empty(model)),
            };

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
            per_model_analysis: FastHashMap::default(),
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
    fn remote_hook_runs_before_remote_snapshots_are_folded() {
        let dir = tempfile::TempDir::new().unwrap();
        let paths = crate::utils::resolve_paths_from_home(dir.path());
        let mut ledger = SessionLedger::open(&paths.cache_dir);
        let export = r#"{"vct_ledger_export":1,"providers":{"claude":{"schema_version":1,"provider":"claude","sessions":{"s1":{"days":{"2026-11-20":{"active":{"analysis":true},"analysis":{"claude-test":{"model":"claude-test","editLines":7,"readLines":0,"writeLines":0,"bashCount":0,"editCount":2,"readCount":0,"todoWriteCount":0,"writeCount":0}}}}}}}}}"#;

        let out = aggregate_sessions_by_model_from_paths_with_cache_and_remote_hook(
            &paths,
            TimeRange::All,
            ProvidersConfig::default(),
            &mut ledger,
            || {
                crate::ledger::store_export(&paths.cache_dir, "devbox", export).unwrap();
            },
        )
        .unwrap();

        let row = out
            .data
            .rows
            .iter()
            .find(|r| r.model == "claude-test")
            .expect("remote snapshot written inside before_remotes hook must be folded");
        assert_eq!(row.edit_lines, 7);
        assert_eq!(row.edit_count, 2);
    }
}
