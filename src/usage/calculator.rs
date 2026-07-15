//! Aggregates per-model token usage across the file-backed provider session trees.
//!
//! Each provider directory is walked with the provider fixed by its *source
//! path* (never re-detected from file contents), parsed in
//! [`ParseMode::UsageOnly`] to skip the heavy file-operation payloads, and the
//! small per-model usage maps are merged into a [`UsageData`]. The provider is
//! tracked twice on purpose — once merged across providers (the per-model
//! table) and once kept per source directory (the per-provider footer) — see
//! [`UsageData`] for why.

use crate::cli::TimeRange;
use crate::config::ProvidersConfig;
use crate::constants::FastHashMap;
use crate::models::{ExtensionType, PerProviderUsage, Provider, ProviderActiveDays, UsageResult};
use crate::pricing::{ModelPricingMap, calculate_base_cost, calculate_request_cost};
use crate::session::ParseMode;
use crate::session::cursor::{
    discover_cursor_store_dbs, load_conversation_model_snapshot, read_cursor_usage_store,
};
use crate::session::diagnostics::{DatabaseUsageRead, PricingGranularity, UsageFact};
use crate::session::hermes::read_hermes_usage_contributions;
use crate::session::opencode::read_opencode_usage_contributions;
use crate::session::parser::parse_session_file_as_with_diagnostics;
use crate::session::sqlite::is_cacheable_sqlite_failure;
use crate::summary_cache::{
    CachedSourceSummary, CompactSourceSummary, SourceFingerprint, SummaryCacheKey, SummaryKind,
    SummaryScanCache, provider_scan_rank,
};
use crate::utils::directory::{FileInfo, collect_files_with_max_depth_diagnostics};
use crate::utils::{
    COPILOT_SESSION_MAX_DEPTH, GROK_SESSION_MAX_DEPTH, HelperPaths, TokenCounts,
    is_claude_session_file, is_codex_session_file, is_copilot_session_file, is_gemini_session_file,
    is_grok_session_file, resolve_paths,
};
use anyhow::Result;
use rayon::prelude::*;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Aggregated token usage plus the per-provider active-day counts.
///
/// Built only by [`get_usage_from_directories`]; all fields are public for the
/// display layer to read. Token totals are tracked two ways at once because the
/// two views need different attribution: [`models`](UsageData::models) merges a
/// shared model (e.g. `claude-sonnet-4-6` emitted by both Claude Code and
/// Copilot CLI) into one row, while [`per_provider`](UsageData::per_provider)
/// keeps the same tokens scoped to the source directory so the footer can
/// attribute them correctly. The shared tokens are merged, not summed, so they
/// are never double-counted across the two maps.
///
/// # Examples
///
/// ```no_run
/// use vibe_coding_tracker::{get_usage_from_directories, TimeRange};
///
/// let data = get_usage_from_directories(TimeRange::All)?;
/// // Total distinct days that contributed any usage, across all providers.
/// println!("active days: {}", data.provider_days.total);
/// # Ok::<(), anyhow::Error>(())
/// ```
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
    /// Count of distinct calendar dates that contributed usage, per provider
    /// and overall.
    pub provider_days: ProviderActiveDays,
    /// Provider-authoritative per-model cost (USD), summed from the source.
    pub stored_costs: StoredCosts,
    /// Opaque request-level pricing ledger used internally by every display
    /// mode. It is intentionally absent from JSON output.
    #[doc(hidden)]
    pub pricing_ledger: UsagePricingLedger,
}

/// One independently readable usage source that could not be collected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageCollectionFailure {
    /// Provider selected from the source directory or database.
    pub provider: ExtensionType,
    /// File, database, or Cursor collection root that failed.
    pub source: PathBuf,
    /// Content-safe error summary.
    pub error: String,
}

/// Candidate, success, and failure counts for a usage scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageCollectionDiagnostics {
    /// Independently readable sources discovered by the scan.
    pub candidates: usize,
    /// Sources parsed or read successfully, including valid blank sources.
    pub parsed: usize,
    /// Complete and partial failures in deterministic provider/source order.
    pub failures: Vec<UsageCollectionFailure>,
}

impl UsageCollectionDiagnostics {
    /// Whether at least one source failed or was only partially understood.
    pub fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }

    /// Whether candidates existed but none could be read successfully.
    pub fn all_failed(&self) -> bool {
        self.candidates > 0 && self.parsed == 0
    }

    /// Whether successful results coexist with one or more failures.
    pub fn partially_failed(&self) -> bool {
        self.parsed > 0 && self.has_failures()
    }
}

/// Usage data paired with source-collection diagnostics.
pub struct UsageCollection {
    /// Successfully collected usage.
    pub data: UsageData,
    /// Candidate, success, and failure counts from the scan.
    pub diagnostics: UsageCollectionDiagnostics,
}

/// Provider-authoritative per-model costs, kept **separate per provider**.
///
/// OpenCode and Hermes record their own costs. The Cursor map is retained for
/// source compatibility, but the local Cursor estimate now carries zero stored
/// cost and is priced by a strict LiteLLM match in the display layer. Separate
/// maps prevent a colliding bare model name from cross-contaminating providers.
#[derive(Debug, Default, Clone)]
pub struct StoredCosts {
    /// OpenCode's per-model stored cost, keyed by model name.
    pub opencode: FastHashMap<String, f64>,
    /// Cursor's per-model dashboard cost, keyed by model name.
    pub cursor: FastHashMap<String, f64>,
    /// Hermes's per-model stored cost, keyed by model name.
    pub hermes: FastHashMap<String, f64>,
}

/// Opaque request-level pricing data retained alongside aggregated tokens.
///
/// The fields are private so provider cost basis and confidence do not become
/// part of the public output contract.
#[doc(hidden)]
#[derive(Debug, Default, Clone)]
pub struct UsagePricingLedger {
    inner: ProviderPricingLedger,
}

#[derive(Debug, Default, Clone)]
struct ProviderPricingLedger {
    claude: FastHashMap<String, Vec<LedgerUnit>>,
    codex: FastHashMap<String, Vec<LedgerUnit>>,
    copilot: FastHashMap<String, Vec<LedgerUnit>>,
    gemini: FastHashMap<String, Vec<LedgerUnit>>,
    grok: FastHashMap<String, Vec<LedgerUnit>>,
    opencode: FastHashMap<String, Vec<LedgerUnit>>,
    cursor: FastHashMap<String, Vec<LedgerUnit>>,
    hermes: FastHashMap<String, Vec<LedgerUnit>>,
}

#[derive(Debug, Clone)]
struct LedgerUnit {
    counts: TokenCounts,
    stored_cost: Option<f64>,
    granularity: PricingGranularity,
    provider_pricing_modifiers: Vec<String>,
}

impl UsagePricingLedger {
    fn provider_mut(
        &mut self,
        provider: ExtensionType,
    ) -> &mut FastHashMap<String, Vec<LedgerUnit>> {
        match provider {
            ExtensionType::ClaudeCode => &mut self.inner.claude,
            ExtensionType::Codex => &mut self.inner.codex,
            ExtensionType::Copilot => &mut self.inner.copilot,
            ExtensionType::Gemini => &mut self.inner.gemini,
            ExtensionType::Grok => &mut self.inner.grok,
            ExtensionType::OpenCode => &mut self.inner.opencode,
            ExtensionType::Cursor => &mut self.inner.cursor,
            ExtensionType::Hermes => &mut self.inner.hermes,
        }
    }

    fn provider(&self, provider: ExtensionType) -> &FastHashMap<String, Vec<LedgerUnit>> {
        match provider {
            ExtensionType::ClaudeCode => &self.inner.claude,
            ExtensionType::Codex => &self.inner.codex,
            ExtensionType::Copilot => &self.inner.copilot,
            ExtensionType::Gemini => &self.inner.gemini,
            ExtensionType::Grok => &self.inner.grok,
            ExtensionType::OpenCode => &self.inner.opencode,
            ExtensionType::Cursor => &self.inner.cursor,
            ExtensionType::Hermes => &self.inner.hermes,
        }
    }

    fn resolve(
        &self,
        provider: ExtensionType,
        model: &str,
        pricing_map: &ModelPricingMap,
    ) -> Option<(f64, Option<String>)> {
        let units = self.provider(provider).get(model)?;
        let mut total = 0.0;
        let mut matched_model = None;
        for unit in units {
            if let Some(stored) = unit.stored_cost {
                total += stored;
                continue;
            }
            let Some(result) =
                pricing_map.get_for_cost_with_provider(model, pricing_provider_hint(provider))
            else {
                continue;
            };
            if matched_model.is_none() {
                matched_model.clone_from(&result.matched_model);
            }
            let priced = match unit.granularity {
                PricingGranularity::Request => calculate_request_cost(
                    unit.counts.input_tokens,
                    unit.counts.output_tokens,
                    unit.counts.reasoning_tokens,
                    unit.counts.cache_read,
                    unit.counts.cache_creation_5m,
                    unit.counts.cache_creation_1h,
                    &result.pricing,
                ),
                PricingGranularity::Aggregate => calculate_base_cost(
                    unit.counts.input_tokens,
                    unit.counts.output_tokens,
                    unit.counts.reasoning_tokens,
                    unit.counts.cache_read,
                    unit.counts.cache_creation_5m,
                    unit.counts.cache_creation_1h,
                    &result.pricing,
                ),
            };
            let modifier =
                unit.provider_pricing_modifiers
                    .iter()
                    .try_fold(1.0, |combined, modifier| {
                        result
                            .pricing
                            .provider_specific_multipliers
                            .get(modifier)
                            .copied()
                            .map(|value| combined * value)
                    });
            if let (Some(priced), Some(modifier)) = (priced, modifier) {
                total += priced * modifier;
            } else {
                log::warn!(
                    "could not resolve request-level pricing or provider modifier for {} model {}",
                    provider,
                    model
                );
            }
            total +=
                unit.counts.web_search_requests as f64 * result.pricing.web_search_cost_per_query;
            if unit.counts.cache_creation_1h > 0
                && result.pricing.cache_creation_input_token_cost_above_1hr == 0.0
                && result
                    .pricing
                    .tiers
                    .iter()
                    .all(|tier| tier.cache_creation_input_token_cost_above_1hr == 0.0)
            {
                log::warn!(
                    "model {} has 1-hour cache writes but no published 1-hour rate",
                    model
                );
            }
        }
        Some((total, matched_model))
    }
}

/// Aggregates token usage from all AI provider session directories.
///
/// Scans the file-backed provider session trees resolved by [`resolve_paths`],
/// filtered by `time_range`, and rolls every session's
/// per-model usage into a [`UsageData`]. Missing provider directories are
/// skipped silently, and a source file or OpenCode database that fails to parse
/// logs a warning to stderr and is excluded rather than aborting the whole scan.
///
/// # Errors
///
/// Returns an error if [`resolve_paths`] cannot determine the provider
/// directories (e.g. the home directory is unavailable). Directory traversal
/// and metadata errors are currently skipped by the walker rather than
/// propagated.
///
/// # Examples
///
/// ```no_run
/// use vibe_coding_tracker::{get_usage_from_directories, TimeRange};
///
/// let data = get_usage_from_directories(TimeRange::All)?;
/// for model in data.models.keys() {
///     println!("{model}");
/// }
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn get_usage_from_directories(time_range: TimeRange) -> Result<UsageData> {
    get_usage_from_directories_with(time_range, ProvidersConfig::default())
}

/// [`get_usage_from_directories`] with explicit per-provider toggles (from
/// `~/.vct/config.toml`). A disabled provider is skipped entirely.
pub fn get_usage_from_directories_with(
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<UsageData> {
    get_usage_from_paths_with(&resolve_paths()?, time_range, providers)
}

/// Aggregates token usage from provider session directories rooted at an
/// explicit [`HelperPaths`].
///
/// The env-free, injectable counterpart of [`get_usage_from_directories`]:
/// every provider path comes from `paths` rather than the resolved home
/// directory, so tests can point them at a temp tree and exercise the real
/// aggregation without mutating process-global `HOME`. See
/// [`get_usage_from_directories`] for the aggregation semantics.
///
/// # Errors
///
/// Returns an error only under the same conditions as
/// [`get_usage_from_directories`].
pub fn get_usage_from_paths(paths: &HelperPaths, time_range: TimeRange) -> Result<UsageData> {
    get_usage_from_paths_with(paths, time_range, ProvidersConfig::default())
}

/// [`get_usage_from_paths`] with explicit provider toggles (the injectable core
/// used by the CLI once `config.toml` is loaded).
pub fn get_usage_from_paths_with(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<UsageData> {
    let mut cache = SummaryScanCache::new();
    Ok(get_usage_from_paths_with_cache(paths, time_range, providers, &mut cache)?.data)
}

/// Diagnostics-aware usage scan rooted at the current user's provider paths.
pub fn get_usage_from_directories_with_diagnostics(
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<UsageCollection> {
    let mut cache = SummaryScanCache::new();
    get_usage_from_paths_with_cache(&resolve_paths()?, time_range, providers, &mut cache)
}

/// Diagnostics-aware usage scan rooted at explicit provider paths.
pub fn get_usage_from_paths_with_diagnostics(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<UsageCollection> {
    let mut cache = SummaryScanCache::new();
    get_usage_from_paths_with_cache(paths, time_range, providers, &mut cache)
}

/// Incremental usage scan backed by a process-local compact summary cache.
///
/// Reusing `cache` across calls reparses only sources whose fingerprint
/// changed. Cached schema failures retain their diagnostics, while metadata,
/// open, and read errors are not inserted and are retried next time.
pub fn get_usage_from_paths_with_cache(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    cache: &mut SummaryScanCache,
) -> Result<UsageCollection> {
    get_usage_from_paths_with_cache_inner(paths, time_range, providers, cache)
}

fn get_usage_from_paths_with_cache_inner(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    cache: &mut SummaryScanCache,
) -> Result<UsageCollection> {
    cache.begin_scan();
    let mut accumulator = UsageAccumulator::new(time_range);
    let mut diagnostics = UsageCollectionDiagnostics::default();
    let mut seen = HashSet::new();

    if providers.claude {
        scan_usage_files(
            &paths.claude_session_dir,
            ExtensionType::ClaudeCode,
            is_claude_session_file,
            time_range,
            None,
            cache,
            &mut seen,
            &mut accumulator,
            &mut diagnostics,
        )?;
    }
    if providers.codex {
        scan_usage_files(
            &paths.codex_session_dir,
            ExtensionType::Codex,
            is_codex_session_file,
            time_range,
            None,
            cache,
            &mut seen,
            &mut accumulator,
            &mut diagnostics,
        )?;
    }
    if providers.copilot {
        scan_usage_files(
            &paths.copilot_session_dir,
            ExtensionType::Copilot,
            is_copilot_session_file,
            time_range,
            Some(COPILOT_SESSION_MAX_DEPTH),
            cache,
            &mut seen,
            &mut accumulator,
            &mut diagnostics,
        )?;
    }
    if providers.gemini {
        scan_usage_files(
            &paths.gemini_session_dir,
            ExtensionType::Gemini,
            is_gemini_session_file,
            time_range,
            None,
            cache,
            &mut seen,
            &mut accumulator,
            &mut diagnostics,
        )?;
    }
    if providers.grok {
        scan_usage_files(
            &paths.grok_session_dir,
            ExtensionType::Grok,
            is_grok_session_file,
            time_range,
            Some(GROK_SESSION_MAX_DEPTH),
            cache,
            &mut seen,
            &mut accumulator,
            &mut diagnostics,
        )?;
    }

    if providers.opencode && paths.opencode_db.exists() {
        scan_usage_database(
            ExtensionType::OpenCode,
            &paths.opencode_db,
            SourceFingerprint::sqlite(&paths.opencode_db, &[]),
            time_range,
            cache,
            &mut seen,
            &mut accumulator,
            &mut diagnostics,
            || read_opencode_usage_contributions(&paths.opencode_db, TimeRange::All),
        );
    }
    if providers.cursor && paths.cursor_chats_dir.exists() {
        scan_cursor_usage_database(
            &paths.cursor_chats_dir,
            &paths.cursor_tracking_db,
            time_range,
            cache,
            &mut seen,
            &mut accumulator,
            &mut diagnostics,
        );
    }
    if providers.hermes && paths.hermes_db.exists() {
        scan_usage_database(
            ExtensionType::Hermes,
            &paths.hermes_db,
            SourceFingerprint::sqlite(&paths.hermes_db, &[]),
            time_range,
            cache,
            &mut seen,
            &mut accumulator,
            &mut diagnostics,
            || read_hermes_usage_contributions(&paths.hermes_db, TimeRange::All),
        );
    }

    cache.retain_kinds(&seen, &[SummaryKind::File, SummaryKind::UsageDatabase]);
    diagnostics.failures.sort_by(|left, right| {
        provider_scan_rank(left.provider)
            .cmp(&provider_scan_rank(right.provider))
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.error.cmp(&right.error))
    });
    Ok(UsageCollection {
        data: accumulator.finish(),
        diagnostics,
    })
}

#[allow(clippy::too_many_arguments)]
fn scan_usage_files<F>(
    dir: &Path,
    provider: ExtensionType,
    filter: F,
    time_range: TimeRange,
    max_depth: Option<usize>,
    cache: &mut SummaryScanCache,
    seen: &mut HashSet<SummaryCacheKey>,
    accumulator: &mut UsageAccumulator,
    diagnostics: &mut UsageCollectionDiagnostics,
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
        diagnostics.failures.push(UsageCollectionFailure {
            provider,
            source: failure.path,
            error: failure.error,
        });
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
                    fold_cached_usage(provider, &file.path, cached, accumulator, diagnostics);
                } else {
                    misses.push((file, key, fingerprint));
                }
            }
            Err(error) => diagnostics.failures.push(UsageCollectionFailure {
                provider,
                source: file.path,
                error: error.to_string(),
            }),
        }
    }

    let loaded: Vec<_> = misses
        .into_par_iter()
        .map(|(file, key, fingerprint)| {
            let result = load_file_summary(&file, provider);
            (file.path, key, fingerprint, result)
        })
        .collect();

    for (source, key, fingerprint, result) in loaded {
        cache.record_parse();
        match result {
            Ok(loaded) => {
                fold_loaded_usage(provider, &source, &loaded, accumulator, diagnostics);
                cache.insert(
                    key,
                    fingerprint,
                    loaded.summary,
                    loaded.parsed,
                    loaded.failure,
                );
            }
            Err(error) => diagnostics.failures.push(UsageCollectionFailure {
                provider,
                source,
                error: error.to_string(),
            }),
        }
    }
    Ok(())
}

fn load_file_summary(file: &FileInfo, provider: ExtensionType) -> Result<LoadedSummary> {
    let parsed =
        parse_session_file_as_with_diagnostics(&file.path, provider, ParseMode::UsageOnly)?;
    if parsed.diagnostics.is_complete_failure() {
        let failure = if parsed.diagnostics.recognized_records == 0 {
            "source contained no recognized provider records".to_string()
        } else {
            format!(
                "none of {} analyzer-relevant provider records used a supported schema",
                parsed.diagnostics.relevant_records
            )
        };
        return Ok(LoadedSummary {
            summary: CompactSourceSummary::default(),
            parsed: false,
            failure: Some(failure),
        });
    }

    let emit = parsed.diagnostics.should_emit_session();
    if emit && parsed.analysis.records.is_empty() {
        return Ok(LoadedSummary {
            summary: CompactSourceSummary::default(),
            parsed: false,
            failure: Some("normalized source produced no analysis records".to_string()),
        });
    }
    let partial = parsed.diagnostics.partial_failure_count();
    Ok(LoadedSummary {
        summary: CompactSourceSummary::from_parsed(parsed, emit),
        parsed: true,
        failure: (partial > 0)
            .then(|| format!("skipped {partial} malformed or unsupported analyzer records")),
    })
}

#[allow(clippy::too_many_arguments)]
fn scan_usage_database<F>(
    provider: ExtensionType,
    source: &Path,
    fingerprint: Result<SourceFingerprint>,
    time_range: TimeRange,
    cache: &mut SummaryScanCache,
    seen: &mut HashSet<SummaryCacheKey>,
    accumulator: &mut UsageAccumulator,
    diagnostics: &mut UsageCollectionDiagnostics,
    loader: F,
) where
    F: FnOnce() -> Result<DatabaseUsageRead>,
{
    diagnostics.candidates += 1;
    let key = SummaryCacheKey::new(SummaryKind::UsageDatabase, provider, source, time_range);
    seen.insert(key.clone());
    let fingerprint = match fingerprint {
        Ok(value) => value,
        Err(error) => {
            diagnostics.failures.push(UsageCollectionFailure {
                provider,
                source: source.to_path_buf(),
                error: error.to_string(),
            });
            return;
        }
    };
    if let Some(cached) = cache.get(&key, &fingerprint) {
        fold_cached_usage(provider, source, cached, accumulator, diagnostics);
        return;
    }

    cache.record_parse();
    match loader() {
        Ok(read) => {
            let complete_failure = read.expected_records > 0 && read.parsed_records == 0;
            let failed = read.failed_records();
            let mut summary = CompactSourceSummary::default();
            for contribution in read.rows {
                summary.add_usage_contribution(contribution, provider);
            }
            let loaded = LoadedSummary {
                summary,
                parsed: !complete_failure,
                failure: if complete_failure {
                    Some(format!(
                        "none of {} usage records used a supported schema",
                        read.expected_records
                    ))
                } else if failed > 0 {
                    Some(format!("{failed} usage records used an unsupported schema"))
                } else {
                    None
                },
            };
            fold_loaded_usage(provider, source, &loaded, accumulator, diagnostics);
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
            diagnostics.failures.push(UsageCollectionFailure {
                provider,
                source: source.to_path_buf(),
                error: failure.clone(),
            });
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

fn scan_cursor_usage_database(
    chats_dir: &Path,
    tracking_db: &Path,
    time_range: TimeRange,
    cache: &mut SummaryScanCache,
    seen: &mut HashSet<SummaryCacheKey>,
    accumulator: &mut UsageAccumulator,
    diagnostics: &mut UsageCollectionDiagnostics,
) {
    let provider = ExtensionType::Cursor;
    let discovery = discover_cursor_store_dbs(chats_dir);
    if !discovery.failures.is_empty() {
        cache.preserve_provider_keys(seen, SummaryKind::UsageDatabase, provider);
    }
    for failure in discovery.failures {
        diagnostics.candidates += 1;
        diagnostics.failures.push(UsageCollectionFailure {
            provider,
            source: failure.path,
            error: failure.error,
        });
    }

    let (conv_models, tracking_fingerprint, tracking_ok) =
        match load_conversation_model_snapshot(tracking_db) {
            Ok(snapshot) => (snapshot.models, snapshot.fingerprint, true),
            Err(error) => {
                diagnostics.failures.push(UsageCollectionFailure {
                    provider,
                    source: tracking_db.to_path_buf(),
                    error: error.to_string(),
                });
                (FastHashMap::default(), None, false)
            }
        };

    for store in discovery.stores {
        diagnostics.candidates += 1;
        let key = SummaryCacheKey::new(SummaryKind::UsageDatabase, provider, &store, time_range);
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
                diagnostics.failures.push(UsageCollectionFailure {
                    provider,
                    source: store,
                    error: error.to_string(),
                });
                continue;
            }
        };
        if tracking_ok && let Some(cached) = cache.get(&key, &fingerprint) {
            fold_cached_usage(provider, &store, cached, accumulator, diagnostics);
            continue;
        }

        cache.record_parse();
        match read_cursor_usage_store(&store, &conv_models, TimeRange::All) {
            Ok(read) => {
                let complete_failure = read.expected_records > 0 && read.parsed_records == 0;
                let failed = read.failed_records();
                let mut summary = CompactSourceSummary::default();
                for contribution in read.rows {
                    summary.add_usage_contribution(contribution, provider);
                }
                let loaded = LoadedSummary {
                    summary,
                    parsed: !complete_failure,
                    failure: if complete_failure {
                        Some(format!(
                            "none of {} Cursor usage payloads used a supported schema",
                            read.expected_records
                        ))
                    } else if failed > 0 {
                        Some(format!(
                            "{failed} Cursor usage payloads used an unsupported schema"
                        ))
                    } else {
                        None
                    },
                };
                fold_loaded_usage(provider, &store, &loaded, accumulator, diagnostics);
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
                diagnostics.failures.push(UsageCollectionFailure {
                    provider,
                    source: store.clone(),
                    error: failure.clone(),
                });
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
struct LoadedSummary {
    summary: CompactSourceSummary,
    parsed: bool,
    failure: Option<String>,
}

fn fold_cached_usage(
    provider: ExtensionType,
    source: &Path,
    cached: &CachedSourceSummary,
    accumulator: &mut UsageAccumulator,
    diagnostics: &mut UsageCollectionDiagnostics,
) {
    if cached.parsed {
        diagnostics.parsed += 1;
        accumulator.add(provider, source, &cached.summary);
    }
    if let Some(error) = &cached.failure {
        diagnostics.failures.push(UsageCollectionFailure {
            provider,
            source: source.to_path_buf(),
            error: error.clone(),
        });
    }
}

fn fold_loaded_usage(
    provider: ExtensionType,
    source: &Path,
    loaded: &LoadedSummary,
    accumulator: &mut UsageAccumulator,
    diagnostics: &mut UsageCollectionDiagnostics,
) {
    if loaded.parsed {
        diagnostics.parsed += 1;
        accumulator.add(provider, source, &loaded.summary);
    }
    if let Some(error) = &loaded.failure {
        diagnostics.failures.push(UsageCollectionFailure {
            provider,
            source: source.to_path_buf(),
            error: error.clone(),
        });
    }
}

struct UsageAccumulator {
    cutoff: Option<String>,
    facts: Vec<SourcedUsageFact>,
}

#[derive(Clone)]
struct SourcedUsageFact {
    provider: ExtensionType,
    source: PathBuf,
    fact: UsageFact,
}

#[derive(Default)]
struct ProviderDateSets {
    claude: HashSet<String>,
    codex: HashSet<String>,
    copilot: HashSet<String>,
    gemini: HashSet<String>,
    grok: HashSet<String>,
    opencode: HashSet<String>,
    cursor: HashSet<String>,
    hermes: HashSet<String>,
}

impl ProviderDateSets {
    fn get_mut(&mut self, provider: ExtensionType) -> &mut HashSet<String> {
        match provider {
            ExtensionType::ClaudeCode => &mut self.claude,
            ExtensionType::Codex => &mut self.codex,
            ExtensionType::Copilot => &mut self.copilot,
            ExtensionType::Gemini => &mut self.gemini,
            ExtensionType::Grok => &mut self.grok,
            ExtensionType::OpenCode => &mut self.opencode,
            ExtensionType::Cursor => &mut self.cursor,
            ExtensionType::Hermes => &mut self.hermes,
        }
    }
}

impl UsageAccumulator {
    fn new(time_range: TimeRange) -> Self {
        Self {
            cutoff: time_range
                .cutoff_date()
                .map(|date| date.format("%Y-%m-%d").to_string()),
            facts: Vec::new(),
        }
    }

    fn add(&mut self, provider: ExtensionType, source: &Path, summary: &CompactSourceSummary) {
        self.facts.extend(
            summary
                .usage_facts
                .iter()
                .cloned()
                .map(|fact| SourcedUsageFact {
                    provider,
                    source: source.to_path_buf(),
                    fact,
                }),
        );
    }

    fn finish(self) -> UsageData {
        let mut models = UsageResult::default();
        let mut per_provider = PerProviderUsage::default();
        let mut stored_costs = StoredCosts::default();
        let mut pricing_ledger = UsagePricingLedger::default();
        let mut dates = ProviderDateSets::default();

        for sourced in reduce_usage_facts(self.facts) {
            let date = sourced.fact.timestamp_ms.and_then(local_date_from_millis);
            let included = match (&self.cutoff, &date) {
                (None, _) => true,
                (Some(cutoff), Some(date)) => date >= cutoff,
                (Some(_), None) => false,
            };
            if sourced.fact.timestamp_ms.is_none() {
                log::warn!(
                    "{} usage fact from {} has no event timestamp{}",
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

            let provider_result = provider_usage_mut(&mut per_provider, sourced.provider);
            let provider_ledger = pricing_ledger.provider_mut(sourced.provider);
            let mut active = false;
            for unit in sourced.fact.units {
                let usage = unit.usage.as_ref().clone();
                merge_model_usage(provider_result, unit.model.clone(), usage.clone());
                merge_model_usage(&mut models, unit.model.clone(), usage);
                provider_ledger
                    .entry(unit.model.clone())
                    .or_default()
                    .push(LedgerUnit {
                        counts: unit.counts,
                        stored_cost: unit.stored_cost,
                        granularity: unit.granularity,
                        provider_pricing_modifiers: unit.provider_pricing_modifiers,
                    });
                if let Some(cost) = unit.stored_cost
                    && let Some(stored) =
                        provider_stored_costs_mut(&mut stored_costs, sourced.provider)
                {
                    *stored.entry(unit.model).or_insert(0.0) += cost;
                }
                active |= token_counts_have_activity(&unit.counts)
                    || unit.stored_cost.is_some_and(|cost| cost != 0.0);
            }
            if active && let Some(date) = date {
                dates.get_mut(sourced.provider).insert(date);
            }
        }

        let mut all_dates = HashSet::new();
        all_dates.extend(dates.claude.iter().cloned());
        all_dates.extend(dates.codex.iter().cloned());
        all_dates.extend(dates.copilot.iter().cloned());
        all_dates.extend(dates.gemini.iter().cloned());
        all_dates.extend(dates.grok.iter().cloned());
        all_dates.extend(dates.opencode.iter().cloned());
        all_dates.extend(dates.cursor.iter().cloned());
        all_dates.extend(dates.hermes.iter().cloned());
        UsageData {
            models,
            per_provider,
            provider_days: ProviderActiveDays {
                claude: dates.claude.len(),
                codex: dates.codex.len(),
                copilot: dates.copilot.len(),
                gemini: dates.gemini.len(),
                grok: dates.grok.len(),
                opencode: dates.opencode.len(),
                cursor: dates.cursor.len(),
                hermes: dates.hermes.len(),
                total: all_dates.len(),
            },
            stored_costs,
            pricing_ledger,
        }
    }
}

fn provider_usage_mut(usage: &mut PerProviderUsage, provider: ExtensionType) -> &mut UsageResult {
    match provider {
        ExtensionType::ClaudeCode => &mut usage.claude,
        ExtensionType::Codex => &mut usage.codex,
        ExtensionType::Copilot => &mut usage.copilot,
        ExtensionType::Gemini => &mut usage.gemini,
        ExtensionType::Grok => &mut usage.grok,
        ExtensionType::OpenCode => &mut usage.opencode,
        ExtensionType::Cursor => &mut usage.cursor,
        ExtensionType::Hermes => &mut usage.hermes,
    }
}

fn merge_model_usage(result: &mut UsageResult, model: String, usage: Value) {
    result
        .entry(model)
        .and_modify(|existing| merge_usage_values(existing, &usage))
        .or_insert(usage);
}

fn provider_stored_costs_mut(
    costs: &mut StoredCosts,
    provider: ExtensionType,
) -> Option<&mut FastHashMap<String, f64>> {
    match provider {
        ExtensionType::OpenCode => Some(&mut costs.opencode),
        ExtensionType::Cursor => Some(&mut costs.cursor),
        ExtensionType::Hermes => Some(&mut costs.hermes),
        _ => None,
    }
}

fn local_date_from_millis(timestamp_ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(timestamp_ms).map(|date_time| {
        date_time
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string()
    })
}

fn token_counts_have_activity(counts: &TokenCounts) -> bool {
    counts.total != 0
        || counts.input_tokens != 0
        || counts.output_tokens != 0
        || counts.reasoning_tokens != 0
        || counts.cache_read != 0
        || counts.cache_creation != 0
        || counts.web_search_requests != 0
}

fn reduce_usage_facts(facts: Vec<SourcedUsageFact>) -> Vec<SourcedUsageFact> {
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

/// How a model's USD cost is resolved.
///
/// Different providers carry different authoritative cost sources, so the cost
/// resolver branches on which one applies.
#[derive(Debug, Clone, Copy)]
pub enum CostSource {
    /// File-based providers: strict monetary LiteLLM lookup.
    Litellm,
    /// OpenCode's stored assistant-message cost. `Some(0.0)` is authoritative;
    /// `None` permits a strict LiteLLM fallback.
    OpenCodeStored(Option<f64>),
    /// Caller-supplied Cursor cost used verbatim. Retained for source
    /// compatibility; VCT's local Cursor reader now returns zero stored cost
    /// and its display path accepts only strict LiteLLM matches.
    CursorStored(Option<f64>),
    /// Hermes stored actual, included, or estimated cost. Missing data permits
    /// a strict LiteLLM fallback.
    HermesStored(Option<f64>),
}

/// Resolves the USD cost for aggregate model counts.
///
/// Aggregate counts do not preserve request boundaries, so only base rates are
/// applied. Range-only pricing remains unresolved. Returns `(cost_usd,
/// matched_model)` where `matched_model` is `Some` only when a non-exact
/// LiteLLM key was used.
pub fn resolve_model_cost(
    model: &str,
    counts: &TokenCounts,
    pricing_map: &ModelPricingMap,
    source: CostSource,
) -> (f64, Option<String>) {
    resolve_model_cost_with_provider_hint(model, counts, pricing_map, source, None)
}

fn resolve_model_cost_with_provider_hint(
    model: &str,
    counts: &TokenCounts,
    pricing_map: &ModelPricingMap,
    source: CostSource,
    provider_hint: Option<&str>,
) -> (f64, Option<String>) {
    let priced = |pricing: &crate::pricing::ModelPricing| {
        let token_cost = calculate_base_cost(
            counts.input_tokens,
            counts.output_tokens,
            counts.reasoning_tokens,
            counts.cache_read,
            counts.cache_creation_5m,
            counts.cache_creation_1h,
            pricing,
        )
        .unwrap_or(0.0);
        // Web search is billed per query (Claude `server_tool_use`),
        // separately from tokens. `web_search_requests` is 0 for every
        // non-Claude model, so this term is a no-op for them.
        token_cost + counts.web_search_requests as f64 * pricing.web_search_cost_per_query
    };

    match source {
        CostSource::CursorStored(Some(stored))
        | CostSource::OpenCodeStored(Some(stored))
        | CostSource::HermesStored(Some(stored)) => (stored, None),
        CostSource::CursorStored(None)
        | CostSource::OpenCodeStored(None)
        | CostSource::HermesStored(None)
        | CostSource::Litellm => pricing_map
            .get_for_cost_with_provider(model, provider_hint)
            .map_or((0.0, None), |result| {
                (priced(&result.pricing), result.matched_model)
            }),
    }
}

impl UsageData {
    /// Returns the per-provider usage slice for `provider`, or `None`
    /// when the provider has no dedicated bucket (e.g. `Provider::Unknown`
    /// — the display layer's fallthrough view is fed by the global
    /// `models` map instead).
    pub fn provider_usage(&self, provider: Provider) -> Option<&UsageResult> {
        self.per_provider.get(provider)
    }

    /// Prices one provider/model row from request-level facts when available.
    #[doc(hidden)]
    pub fn price_provider_model(
        &self,
        provider: ExtensionType,
        model: &str,
        pricing_map: &ModelPricingMap,
    ) -> Option<(f64, Option<String>)> {
        let usage = provider_usage_by_extension(&self.per_provider, provider).get(model)?;
        if let Some(cost) = self.pricing_ledger.resolve(provider, model, pricing_map) {
            return Some(cost);
        }
        let stored = match provider {
            ExtensionType::OpenCode => {
                CostSource::OpenCodeStored(self.stored_costs.opencode.get(model).copied())
            }
            ExtensionType::Cursor => CostSource::CursorStored(None),
            ExtensionType::Hermes => {
                CostSource::HermesStored(self.stored_costs.hermes.get(model).copied())
            }
            _ => CostSource::Litellm,
        };
        Some(resolve_model_cost_with_provider_hint(
            model,
            &crate::utils::extract_token_counts(usage),
            pricing_map,
            stored,
            pricing_provider_hint(provider),
        ))
    }

    /// Prices a cross-provider model row by summing each provider's own
    /// request ledger and cost precedence.
    #[doc(hidden)]
    pub fn price_merged_model(
        &self,
        model: &str,
        pricing_map: &ModelPricingMap,
    ) -> Option<(f64, Option<String>)> {
        let mut total = 0.0;
        let mut matched_model = None;
        let mut found = false;
        for provider in [
            ExtensionType::ClaudeCode,
            ExtensionType::Codex,
            ExtensionType::Copilot,
            ExtensionType::Gemini,
            ExtensionType::Grok,
            ExtensionType::OpenCode,
            ExtensionType::Cursor,
            ExtensionType::Hermes,
        ] {
            if let Some((cost, matched)) = self.price_provider_model(provider, model, pricing_map) {
                found = true;
                total += cost;
                if matched_model.is_none() {
                    matched_model = matched;
                }
            }
        }
        found.then_some((total, matched_model))
    }
}

fn provider_usage_by_extension(usage: &PerProviderUsage, provider: ExtensionType) -> &UsageResult {
    match provider {
        ExtensionType::ClaudeCode => &usage.claude,
        ExtensionType::Codex => &usage.codex,
        ExtensionType::Copilot => &usage.copilot,
        ExtensionType::Gemini => &usage.gemini,
        ExtensionType::Grok => &usage.grok,
        ExtensionType::OpenCode => &usage.opencode,
        ExtensionType::Cursor => &usage.cursor,
        ExtensionType::Hermes => &usage.hermes,
    }
}

fn pricing_provider_hint(provider: ExtensionType) -> Option<&'static str> {
    match provider {
        ExtensionType::ClaudeCode => Some("anthropic"),
        ExtensionType::Codex => Some("openai"),
        ExtensionType::Gemini => Some("gemini"),
        ExtensionType::Grok => Some("xai"),
        ExtensionType::Copilot
        | ExtensionType::OpenCode
        | ExtensionType::Cursor
        | ExtensionType::Hermes => None,
    }
}

/// Accumulates the token fields of `new` into `existing` in place.
///
/// Detects the on-disk usage shape from its token keys and merges accordingly:
/// the flat provider shape, including partial records that only publish
/// `total_tokens`, or the Codex shape keyed by `total_token_usage`. Values that
/// are not both JSON objects, or that match neither shape, are left untouched.
pub(crate) fn merge_usage_values(existing: &mut Value, new: &Value) {
    use crate::utils::{accumulate_i64_fields, accumulate_nested_object, extract_token_counts};

    let (Some(existing_ro), Some(new_ro)) = (existing.as_object(), new.as_object()) else {
        return;
    };
    let existing_flat = existing_ro.contains_key("input_tokens");
    let existing_codex = existing_ro.contains_key("total_token_usage");
    let new_codex = new_ro.contains_key("total_token_usage");
    let is_flat_usage = |obj: &serde_json::Map<String, Value>| {
        [
            "input_tokens",
            "output_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
            "thoughts_tokens",
            "reasoning_output_tokens",
            "tool_tokens",
            "total_tokens",
            "cache_creation",
            "server_tool_use",
        ]
        .iter()
        .any(|key| obj.contains_key(*key))
    };
    let existing_flat_usage = is_flat_usage(existing_ro);
    let new_flat_usage = is_flat_usage(new_ro);

    // Mixed shapes — e.g. a Codex `total_token_usage` row and a Cursor / Copilot
    // flat `input_tokens` row that share a model name like `gpt-5`. The
    // shape-specific branches below only accumulate when both sides carry the
    // *same* shape, so a mismatch would silently drop the other side's tokens.
    // Normalize both to disjoint counts and rewrite `existing` as a flat value
    // that keeps every bucket (and round-trips through `extract_token_counts`).
    if (existing_flat_usage || existing_codex)
        && (new_flat_usage || new_codex)
        && ((existing_codex && new_flat_usage)
            || (existing_flat_usage && new_codex)
            || (existing_flat_usage && !existing_flat))
    {
        let merged = add_token_counts(&extract_token_counts(existing), &extract_token_counts(new));
        *existing = token_counts_to_flat_value(&merged);
        return;
    }

    if let (Some(existing_obj), Some(new_obj)) = (existing.as_object_mut(), new.as_object()) {
        // Handle the flat provider format (has input_tokens)
        if existing_obj.contains_key("input_tokens") {
            accumulate_i64_fields(
                existing_obj,
                new_obj,
                &[
                    "input_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                    "output_tokens",
                    // Gemini `thoughts_tokens` and Copilot's normalised
                    // `reasoning_output_tokens` both carry the same
                    // reasoning-budget semantics and must accumulate so
                    // cross-provider aggregation in `usage` doesn't drop
                    // the thinking-time tokens the model was actually
                    // billed for.
                    "thoughts_tokens",
                    "reasoning_output_tokens",
                    "tool_tokens",
                    "total_tokens",
                ],
            );

            if let Some(new_cache) = new_obj.get("cache_creation").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "cache_creation", new_cache);
            }

            // Claude server-side tool counts (web_search_requests /
            // web_fetch_requests) merge across files just like cache_creation.
            if let Some(new_stu) = new_obj.get("server_tool_use").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "server_tool_use", new_stu);
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

/// Sums two normalized [`TokenCounts`] field by field.
fn add_token_counts(a: &TokenCounts, b: &TokenCounts) -> TokenCounts {
    TokenCounts {
        input_tokens: a.input_tokens + b.input_tokens,
        output_tokens: a.output_tokens + b.output_tokens,
        reasoning_tokens: a.reasoning_tokens + b.reasoning_tokens,
        cache_read: a.cache_read + b.cache_read,
        cache_creation: a.cache_creation + b.cache_creation,
        cache_creation_5m: a.cache_creation_5m + b.cache_creation_5m,
        cache_creation_1h: a.cache_creation_1h + b.cache_creation_1h,
        web_search_requests: a.web_search_requests + b.web_search_requests,
        total: a.total + b.total,
    }
}

/// Serializes normalized counts back into the flat usage shape.
///
/// The key set is exactly what [`extract_token_counts`] reads for a flat value,
/// so the result round-trips: re-extracting it yields the same counts. The
/// effective published total is written explicitly because some providers
/// include tool-only tokens that cannot be reconstructed from priced buckets.
fn token_counts_to_flat_value(c: &TokenCounts) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("input_tokens".into(), json!(c.input_tokens));
    obj.insert("output_tokens".into(), json!(c.output_tokens));
    obj.insert("reasoning_output_tokens".into(), json!(c.reasoning_tokens));
    obj.insert("cache_read_input_tokens".into(), json!(c.cache_read));
    obj.insert(
        "cache_creation_input_tokens".into(),
        json!(c.cache_creation),
    );
    obj.insert("total_tokens".into(), json!(c.total));
    if c.cache_creation_5m != 0 || c.cache_creation_1h != 0 {
        obj.insert(
            "cache_creation".into(),
            json!({
                "ephemeral_5m_input_tokens": c.cache_creation_5m,
                "ephemeral_1h_input_tokens": c.cache_creation_1h,
            }),
        );
    }
    if c.web_search_requests != 0 {
        obj.insert(
            "server_tool_use".into(),
            json!({ "web_search_requests": c.web_search_requests }),
        );
    }
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::{ModelPricing, ThresholdTier, TierRange, clear_pricing_cache};
    use std::collections::HashMap;

    fn map_with_gpt4() -> ModelPricingMap {
        let mut raw = HashMap::new();
        raw.insert(
            "gpt-4".to_string(),
            ModelPricing {
                input_cost_per_token: 1e-5,
                ..Default::default()
            },
        );
        ModelPricingMap::new(raw)
    }

    fn counts(input: i64) -> TokenCounts {
        TokenCounts {
            input_tokens: input,
            total: input,
            ..Default::default()
        }
    }

    fn ledger_map(model: &str, pricing: ModelPricing) -> ModelPricingMap {
        ModelPricingMap::new(HashMap::from([(model.to_string(), pricing)]))
    }

    fn request_unit(input_tokens: i64, stored_cost: Option<f64>) -> LedgerUnit {
        LedgerUnit {
            counts: counts(input_tokens),
            stored_cost,
            granularity: PricingGranularity::Request,
            provider_pricing_modifiers: Vec::new(),
        }
    }

    #[test]
    fn request_ledger_does_not_merge_independent_threshold_contexts() {
        let model = "tiered-model";
        let map = ledger_map(
            model,
            ModelPricing {
                input_cost_per_token: 1e-6,
                tiers: vec![ThresholdTier {
                    threshold_tokens: 200_000,
                    input_cost_per_token: 2e-6,
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        let mut ledger = UsagePricingLedger::default();
        ledger.provider_mut(ExtensionType::ClaudeCode).insert(
            model.to_string(),
            vec![request_unit(150_000, None), request_unit(150_000, None)],
        );

        let (cost, _) = ledger
            .resolve(ExtensionType::ClaudeCode, model, &map)
            .expect("ledger entry");
        assert!((cost - 0.3).abs() < 1e-12);
    }

    #[test]
    fn request_ledger_applies_threshold_to_each_request_independently() {
        let model = "tiered-model";
        let map = ledger_map(
            model,
            ModelPricing {
                input_cost_per_token: 1e-6,
                tiers: vec![ThresholdTier {
                    threshold_tokens: 200_000,
                    input_cost_per_token: 2e-6,
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        let mut ledger = UsagePricingLedger::default();
        ledger.provider_mut(ExtensionType::ClaudeCode).insert(
            model.to_string(),
            vec![request_unit(200_001, None), request_unit(1_000, None)],
        );

        let (cost, _) = ledger
            .resolve(ExtensionType::ClaudeCode, model, &map)
            .expect("ledger entry");
        assert!((cost - 0.401_002).abs() < 1e-12);
    }

    #[test]
    fn unresolved_range_unit_does_not_discard_resolved_units() {
        let model = "range-model";
        let map = ledger_map(
            model,
            ModelPricing {
                ranges: Some(vec![
                    TierRange {
                        min_tokens: 0,
                        max_tokens: 10_000,
                        input_cost_per_token: 1e-6,
                        ..Default::default()
                    },
                    TierRange {
                        min_tokens: 20_000,
                        max_tokens: 30_000,
                        input_cost_per_token: 3e-6,
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
        );
        let mut ledger = UsagePricingLedger::default();
        ledger.provider_mut(ExtensionType::Gemini).insert(
            model.to_string(),
            vec![request_unit(5_000, None), request_unit(15_000, None)],
        );

        let (cost, _) = ledger
            .resolve(ExtensionType::Gemini, model, &map)
            .expect("ledger entry");
        assert!((cost - 0.005).abs() < 1e-12);
    }

    #[test]
    fn unresolved_range_still_prices_known_web_search_requests() {
        let model = "range-model";
        let map = ledger_map(
            model,
            ModelPricing {
                web_search_cost_per_query: 0.01,
                ranges: Some(vec![TierRange {
                    min_tokens: 0,
                    max_tokens: 10_000,
                    input_cost_per_token: 1e-6,
                    ..Default::default()
                }]),
                ..Default::default()
            },
        );
        let mut unit = request_unit(15_000, None);
        unit.counts.web_search_requests = 3;
        let mut ledger = UsagePricingLedger::default();
        ledger
            .provider_mut(ExtensionType::ClaudeCode)
            .insert(model.to_string(), vec![unit]);

        let (cost, _) = ledger
            .resolve(ExtensionType::ClaudeCode, model, &map)
            .expect("ledger entry");
        assert!((cost - 0.03).abs() < 1e-12);
    }

    #[test]
    fn provider_modifiers_stack_on_tokens_but_not_web_search() {
        let model = "claude-opus-4-8";
        let map = ledger_map(
            model,
            ModelPricing {
                input_cost_per_token: 1.0,
                output_cost_per_token: 2.0,
                cache_read_input_token_cost: 0.1,
                cache_creation_input_token_cost: 1.25,
                cache_creation_input_token_cost_above_1hr: 2.0,
                web_search_cost_per_query: 0.01,
                provider_specific_multipliers: HashMap::from([
                    ("fast".to_string(), 2.0),
                    ("us".to_string(), 1.1),
                ]),
                ..Default::default()
            },
        );
        let mut unit = request_unit(1, None);
        unit.counts.output_tokens = 1;
        unit.counts.cache_read = 1;
        unit.counts.cache_creation = 2;
        unit.counts.cache_creation_5m = 1;
        unit.counts.cache_creation_1h = 1;
        unit.counts.web_search_requests = 1;
        unit.provider_pricing_modifiers = vec!["fast".to_string(), "us".to_string()];
        let mut ledger = UsagePricingLedger::default();
        ledger
            .provider_mut(ExtensionType::ClaudeCode)
            .insert(model.to_string(), vec![unit]);

        let (cost, _) = ledger
            .resolve(ExtensionType::ClaudeCode, model, &map)
            .expect("ledger entry");
        let token_cost = 1.0 + 2.0 + 0.1 + 1.25 + 2.0;
        assert!((cost - (token_cost * 2.0 * 1.1 + 0.01)).abs() < 1e-12);
    }

    #[test]
    fn missing_provider_modifier_never_falls_back_to_standard_token_price() {
        let model = "claude-opus-future";
        let map = ledger_map(
            model,
            ModelPricing {
                input_cost_per_token: 1.0,
                web_search_cost_per_query: 0.01,
                ..Default::default()
            },
        );
        let mut unit = request_unit(100, None);
        unit.counts.web_search_requests = 2;
        unit.provider_pricing_modifiers = vec!["fast".to_string()];
        let mut ledger = UsagePricingLedger::default();
        ledger
            .provider_mut(ExtensionType::ClaudeCode)
            .insert(model.to_string(), vec![unit]);

        let (cost, _) = ledger
            .resolve(ExtensionType::ClaudeCode, model, &map)
            .expect("ledger entry");
        assert!((cost - 0.02).abs() < 1e-12);
    }

    #[test]
    fn grok_ledger_uses_xai_provider_hint_for_bare_model_id() {
        let model = "grok-4.5";
        let map = ledger_map(
            "xai/grok-4.5",
            ModelPricing {
                input_cost_per_token: 0.01,
                ..Default::default()
            },
        );
        let mut ledger = UsagePricingLedger::default();
        ledger
            .provider_mut(ExtensionType::Grok)
            .insert(model.to_string(), vec![request_unit(100, None)]);

        let (cost, matched) = ledger
            .resolve(ExtensionType::Grok, model, &map)
            .expect("ledger entry");
        assert!((cost - 1.0).abs() < 1e-12);
        assert_eq!(matched.as_deref(), Some("xai/grok-4.5"));
    }

    #[test]
    fn stored_cost_precedence_is_applied_per_ledger_unit() {
        let model = "gpt-4";
        let map = map_with_gpt4();
        let mut ledger = UsagePricingLedger::default();
        ledger.provider_mut(ExtensionType::OpenCode).insert(
            model.to_string(),
            vec![
                request_unit(1_000_000, Some(0.0)),
                request_unit(1_000, None),
            ],
        );

        let (cost, _) = ledger
            .resolve(ExtensionType::OpenCode, model, &map)
            .expect("ledger entry");
        assert!((cost - 0.01).abs() < 1e-12);
    }

    #[test]
    fn test_opencode_stored_cost_precedes_exact_litellm_match() {
        clear_pricing_cache();
        let map = map_with_gpt4();
        // Provider-stored history remains authoritative when a current exact
        // LiteLLM price also exists.
        let (cost, matched) = resolve_model_cost(
            "gpt-4",
            &counts(1_000_000),
            &map,
            CostSource::OpenCodeStored(Some(99.0)),
        );
        assert!((cost - 99.0).abs() < 1e-9);
        assert!(matched.is_none());
    }

    #[test]
    fn test_opencode_no_exact_match_uses_stored_cost() {
        clear_pricing_cache();
        let map = map_with_gpt4();
        // No exact price; OpenCode must NOT fuzzy match -> use stored cost.
        let (cost, matched) = resolve_model_cost(
            "deepseek-v4-pro",
            &counts(1_000_000),
            &map,
            CostSource::OpenCodeStored(Some(99.0)),
        );
        assert!((cost - 99.0).abs() < 1e-9);
        assert!(matched.is_none());
    }

    #[test]
    fn test_explicit_zero_stored_cost_is_authoritative() {
        let map = map_with_gpt4();
        let (cost, matched) = resolve_model_cost(
            "gpt-4",
            &counts(1_000_000),
            &map,
            CostSource::OpenCodeStored(Some(0.0)),
        );
        assert_eq!(cost, 0.0);
        assert!(matched.is_none());
    }

    #[test]
    fn test_missing_stored_cost_uses_strict_litellm_match() {
        let map = map_with_gpt4();
        let (cost, matched) = resolve_model_cost(
            "gpt-4",
            &counts(1_000_000),
            &map,
            CostSource::OpenCodeStored(None),
        );
        assert!((cost - 10.0).abs() < 1e-6);
        assert!(matched.is_none());
    }

    #[test]
    fn test_hermes_explicit_zero_blocks_fallback_but_missing_cost_does_not() {
        let map = map_with_gpt4();
        let (included_cost, included_match) = resolve_model_cost(
            "gpt-4",
            &counts(1_000_000),
            &map,
            CostSource::HermesStored(Some(0.0)),
        );
        assert_eq!(included_cost, 0.0);
        assert!(included_match.is_none());

        let (fallback_cost, fallback_match) = resolve_model_cost(
            "gpt-4",
            &counts(1_000_000),
            &map,
            CostSource::HermesStored(None),
        );
        assert!((fallback_cost - 10.0).abs() < 1e-6);
        assert!(fallback_match.is_none());
    }

    #[test]
    fn test_cursor_stored_cost_ignores_exact_match() {
        clear_pricing_cache();
        let map = map_with_gpt4();
        // Cursor's dashboard cost is authoritative even when an exact LiteLLM
        // price exists -> use the stored cost, never re-price from tokens.
        let (cost, matched) = resolve_model_cost(
            "gpt-4",
            &counts(1_000_000),
            &map,
            CostSource::CursorStored(Some(3.5)),
        );
        assert!((cost - 3.5).abs() < 1e-9);
        assert!(matched.is_none());
    }

    #[test]
    fn test_non_opencode_keeps_existing_lookup() {
        clear_pricing_cache();
        let map = map_with_gpt4();
        // Litellm path is unchanged: exact match still computes.
        let (cost, matched) =
            resolve_model_cost("gpt-4", &counts(1_000_000), &map, CostSource::Litellm);
        assert!((cost - 10.0).abs() < 1e-6);
        assert!(matched.is_none());
    }

    #[test]
    fn merge_preserves_tokens_across_mixed_shapes() {
        use crate::utils::extract_token_counts;

        // A Codex `total_token_usage` value (input 1000 includes 200 cached).
        let codex = json!({
            "total_token_usage": {
                "input_tokens": 1000,
                "cached_input_tokens": 200,
                "output_tokens": 500,
                "total_tokens": 1500
            }
        });
        // A Cursor / flat value for the same model name.
        let flat = json!({
            "input_tokens": 100,
            "output_tokens": 20,
            "cache_read_input_tokens": 50,
            "cache_creation_input_tokens": 10
        });

        // Codex disjoint counts: input 800, cache_read 200, output 500, total 1500.
        // Flat counts: input 100, output 20, cache_read 50, cache_creation 10.
        let expect = |c: TokenCounts| {
            assert_eq!(c.input_tokens, 800 + 100);
            assert_eq!(c.output_tokens, 500 + 20);
            assert_eq!(c.cache_read, 200 + 50);
            assert_eq!(c.cache_creation, 10);
            // Bucket sum: 1500 (Codex) + 180 (flat) = 1680; no tokens dropped.
            assert_eq!(c.total, 1680);
        };

        // Merging is order-independent: neither side's tokens are dropped.
        let mut existing = codex.clone();
        merge_usage_values(&mut existing, &flat);
        expect(extract_token_counts(&existing));

        let mut existing = flat.clone();
        merge_usage_values(&mut existing, &codex);
        expect(extract_token_counts(&existing));
    }

    #[test]
    fn merge_preserves_partial_total_only_usage_in_either_order() {
        use crate::utils::extract_token_counts;

        let total_only = json!({ "total_tokens": 17 });
        let split = json!({
            "input_tokens": 10,
            "output_tokens": 4,
            "reasoning_output_tokens": 2,
            "cache_read_input_tokens": 3,
            "total_tokens": 19
        });
        let expect = |value: &Value| {
            let counts = extract_token_counts(value);
            assert_eq!(counts.input_tokens, 10);
            assert_eq!(counts.output_tokens, 4);
            assert_eq!(counts.reasoning_tokens, 2);
            assert_eq!(counts.cache_read, 3);
            assert_eq!(counts.total, 36);
        };

        let mut forward = total_only.clone();
        merge_usage_values(&mut forward, &split);
        expect(&forward);

        let mut reverse = split;
        merge_usage_values(&mut reverse, &total_only);
        expect(&reverse);
        assert_eq!(
            extract_token_counts(&forward),
            extract_token_counts(&reverse)
        );
    }
}
