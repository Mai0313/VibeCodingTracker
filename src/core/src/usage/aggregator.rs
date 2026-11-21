//! Aggregates per-model token usage across every provider source.
//!
//! File-backed providers fix their provider from the source path rather than content detection and parse in [`ParseMode::UsageOnly`]; OpenCode, Cursor, and Hermes are read from SQLite. Results land in [`UsageData`], storing both cross-provider model totals and source-scoped per-provider maps.

use crate::config::ProvidersConfig;
use crate::constants::{FastHashMap, capacity};
use crate::ledger::{DayEntry, SessionLedger, stamp};
use crate::models::TimeRange;
use crate::models::{
    CodeAnalysis, ExtensionType, PerProviderUsage, ProviderActiveDays, UsageResult,
};
use crate::pricing::TierThresholds;
use crate::scan::{ScanFeature, group_usage_rows, scan_cursor_stores, scan_database_half};
use crate::session::hermes::read_hermes_usage_contributions;
use crate::session::opencode::read_opencode_usage_contributions;
use crate::session::{
    ParseMode, parse_session_file_typed_as, read_cursor_usage, read_hermes_usage,
    read_opencode_usage,
};
use crate::utils::directory::collect_provider_files_diagnostics;
use crate::utils::{
    ANTIGRAVITY_SESSION_MAX_DEPTH, COPILOT_SESSION_MAX_DEPTH, DSH_SESSION_MAX_DEPTH,
    GROK_SESSION_MAX_DEPTH, HelperPaths, is_antigravity_session_file, is_claude_session_file,
    is_codex_session_file, is_copilot_session_file, is_dsh_session_file, is_gemini_session_file,
    is_grok_session_file, merge_usage_values, resolve_paths,
};
use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Aggregated token usage, per-provider active-day counts, and stored provider costs.
///
/// [`models`](UsageData::models) merges tokens across providers by model name, while [`per_provider`](UsageData::per_provider) partitions the same tokens by originating provider. Consumers read one view or the other and never sum both.
///
/// # Examples
///
/// ```no_run
/// use vct_core::{aggregate_usage_from_home, TimeRange};
///
/// let data = aggregate_usage_from_home(TimeRange::All)?;
/// println!("active days: {}", data.provider_days.total);
/// # Ok::<(), anyhow::Error>(())
/// ```
#[derive(Debug, Clone, Serialize)]
pub struct UsageData {
    /// Tokens aggregated across all providers, keyed by model name.
    pub models: UsageResult,
    /// Tokens partitioned by originating provider, then keyed by model name.
    pub per_provider: PerProviderUsage,
    /// Count of distinct calendar dates that contributed usage, per provider and overall.
    pub provider_days: ProviderActiveDays,
    /// Provider-authoritative per-model cost (USD), summed from the source.
    pub stored_costs: StoredCosts,
}

// Re-exported from `scan` so callers can reach the shared diagnostics type as `usage::ScanDiagnostics`.
pub use crate::scan::{ScanDiagnostics, ScanFailure};

/// Usage data paired with source-collection diagnostics.
pub struct UsageCollection {
    /// Successfully collected usage.
    pub data: UsageData,
    /// Candidate, success, and failure counts from the scan.
    pub diagnostics: ScanDiagnostics,
}

/// Token usage accumulated for one local calendar date (`YYYY-MM-DD`).
#[derive(Debug, Default, Clone, Serialize)]
pub struct DayUsageData {
    /// Tokens aggregated across all providers for this day, keyed by model name.
    pub models: UsageResult,
    /// Tokens partitioned by originating provider for this day.
    pub per_provider: PerProviderUsage,
    /// Provider-authoritative per-model costs (USD) for this day.
    pub stored_costs: StoredCosts,
}

/// Per-day aggregated token usage paired with synced remote snapshot names.
#[derive(Debug, Default, Clone, Serialize)]
pub struct DailyUsageData {
    /// Usage partitioned by local calendar date (`YYYY-MM-DD`), sorted ascending.
    pub days: BTreeMap<String, DayUsageData>,
    /// Sorted names of synced remote snapshots included in the scan.
    pub remotes: Vec<String>,
}

/// Per-day usage data paired with source-collection diagnostics.
pub struct DailyUsageCollection {
    /// Successfully collected per-day usage.
    pub data: DailyUsageData,
    /// Candidate, success, and failure counts from the scan.
    pub diagnostics: ScanDiagnostics,
}

/// Provider-authoritative per-model costs in USD, partitioned by provider.
///
/// Only OpenCode and Hermes record their own costs; separating maps isolates colliding bare model names across providers.
#[derive(Debug, Default, Clone, Serialize)]
pub struct StoredCosts {
    /// OpenCode's per-model stored cost, keyed by model name.
    pub opencode: FastHashMap<String, f64>,
    /// Hermes's per-model stored cost, keyed by model name.
    pub hermes: FastHashMap<String, f64>,
}

/// Merges a session's per-model token usage out of a typed [`CodeAnalysis`].
fn extract_conversation_usage_from_analysis(analysis: CodeAnalysis) -> FastHashMap<String, Value> {
    let mut conversation_usage = FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);

    let mut merge_into = |model: String, usage: Value| {
        conversation_usage
            .entry(model)
            .and_modify(|existing_usage| merge_usage_values(existing_usage, &usage))
            .or_insert(usage);
    };

    for record in analysis.records {
        for (model, usage) in record.conversation_usage {
            merge_into(model, usage);
        }
        // Advisor and compaction tokens live in `advisor_usage` so `analysis` skips crediting them with file ops; fold them here for usage and pricing.
        for (model, usage) in record.advisor_usage {
            merge_into(model, usage);
        }
    }

    conversation_usage
}

/// Aggregates token usage from all enabled AI provider sources under the current user's home.
///
/// Scans every source resolved by [`resolve_paths`] within `time_range` and rolls per-model usage into [`UsageData`]. Missing provider sources are skipped, and unreadable sources are logged and excluded.
///
/// # Examples
///
/// ```no_run
/// use vct_core::{aggregate_usage_from_home, TimeRange};
///
/// let data = aggregate_usage_from_home(TimeRange::All)?;
/// for model in data.models.keys() {
///     println!("{model}");
/// }
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
///
/// Returns an error if [`resolve_paths`] cannot determine provider directories.
pub fn aggregate_usage_from_home(time_range: TimeRange) -> Result<UsageData> {
    aggregate_usage_from_home_with_providers(time_range, ProvidersConfig::default())
}

/// Aggregates token usage from the current user's home for providers enabled in `providers`.
///
/// # Errors
///
/// Returns an error if [`resolve_paths`] cannot determine provider directories.
pub fn aggregate_usage_from_home_with_providers(
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<UsageData> {
    aggregate_usage_from_paths_with_providers(&resolve_paths()?, time_range, providers)
}

/// Aggregates token usage from explicit provider locations in `paths` without reading environment variables or the session ledger.
pub fn aggregate_usage_from_paths(paths: &HelperPaths, time_range: TimeRange) -> Result<UsageData> {
    aggregate_usage_from_paths_with_providers(paths, time_range, ProvidersConfig::default())
}

/// Aggregates token usage from `paths` for providers enabled in `providers`, without reading the session ledger.
pub fn aggregate_usage_from_paths_with_providers(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<UsageData> {
    let mut result = FastHashMap::with_capacity(capacity::MODEL_COMBINATIONS);
    let mut per_provider = PerProviderUsage::default();
    let mut stored_costs = StoredCosts::default();

    let mut claude_dates: HashSet<String> = HashSet::new();
    let mut codex_dates: HashSet<String> = HashSet::new();
    let mut copilot_dates: HashSet<String> = HashSet::new();
    let mut gemini_dates: HashSet<String> = HashSet::new();
    let mut grok_dates: HashSet<String> = HashSet::new();
    let mut deepseek_dates: HashSet<String> = HashSet::new();
    let mut antigravity_dates: HashSet<String> = HashSet::new();
    let mut opencode_dates: HashSet<String> = HashSet::new();
    let mut cursor_dates: HashSet<String> = HashSet::new();
    let mut hermes_dates: HashSet<String> = HashSet::new();

    if providers.claude && paths.claude_session_dir.exists() {
        // Recursive walk collects both top-level `<session>.jsonl` and `<session>/subagents/agent-*.jsonl`.
        process_usage_directory(
            &[paths.claude_session_dir.as_path()],
            ExtensionType::ClaudeCode,
            &mut result,
            &mut per_provider.claude,
            &mut claude_dates,
            is_claude_session_file,
            time_range,
            None,
        );
    }

    let codex_dirs = paths.codex_session_dirs();
    if providers.codex && codex_dirs.iter().any(|dir| dir.exists()) {
        process_usage_directory(
            &codex_dirs,
            ExtensionType::Codex,
            &mut result,
            &mut per_provider.codex,
            &mut codex_dates,
            is_codex_session_file,
            time_range,
            None,
        );
    }

    if providers.copilot && paths.copilot_session_dir.exists() {
        // Cap depth because `events.jsonl` sits two levels under `session-state/`; deeper subtrees hold snapshots and artifacts.
        process_usage_directory(
            &[paths.copilot_session_dir.as_path()],
            ExtensionType::Copilot,
            &mut result,
            &mut per_provider.copilot,
            &mut copilot_dates,
            is_copilot_session_file,
            time_range,
            Some(COPILOT_SESSION_MAX_DEPTH),
        );
    }

    if providers.gemini && paths.gemini_session_dir.exists() {
        process_usage_directory(
            &[paths.gemini_session_dir.as_path()],
            ExtensionType::Gemini,
            &mut result,
            &mut per_provider.gemini,
            &mut gemini_dates,
            is_gemini_session_file,
            time_range,
            None,
        );
    }

    if providers.grok && paths.grok_session_dir.exists() {
        process_usage_directory(
            &[paths.grok_session_dir.as_path()],
            ExtensionType::Grok,
            &mut result,
            &mut per_provider.grok,
            &mut grok_dates,
            is_grok_session_file,
            time_range,
            Some(GROK_SESSION_MAX_DEPTH),
        );
    }

    if providers.dsh && paths.dsh_session_dir.exists() {
        process_usage_directory(
            &[paths.dsh_session_dir.as_path()],
            ExtensionType::DeepSeek,
            &mut result,
            &mut per_provider.deepseek,
            &mut deepseek_dates,
            is_dsh_session_file,
            time_range,
            Some(DSH_SESSION_MAX_DEPTH),
        );
    }

    if providers.antigravity {
        let antigravity_dirs = paths.antigravity_session_dirs();
        let antigravity_dir_refs: Vec<&Path> =
            antigravity_dirs.iter().map(PathBuf::as_path).collect();
        process_usage_directory(
            &antigravity_dir_refs,
            ExtensionType::Antigravity,
            &mut result,
            &mut per_provider.antigravity,
            &mut antigravity_dates,
            is_antigravity_session_file,
            time_range,
            Some(ANTIGRAVITY_SESSION_MAX_DEPTH),
        );
    }

    if providers.opencode
        && paths.opencode_db.exists()
        && let Err(err) = process_opencode_usage(
            &paths.opencode_db,
            &mut result,
            &mut per_provider.opencode,
            &mut stored_costs.opencode,
            &mut opencode_dates,
            time_range,
        )
    {
        log::warn!(
            "failed to read OpenCode DB {}: {err}",
            paths.opencode_db.display()
        );
    }

    if providers.cursor
        && paths.cursor_chats_dir.exists()
        && let Err(err) = process_cursor_usage(
            &paths.cursor_chats_dir,
            &paths.cursor_tracking_db,
            &mut result,
            &mut per_provider.cursor,
            &mut cursor_dates,
            time_range,
        )
    {
        log::warn!("failed to read Cursor usage: {err}");
    }

    if providers.hermes
        && paths.hermes_db.exists()
        && let Err(err) = process_hermes_usage(
            &paths.hermes_db,
            &mut result,
            &mut per_provider.hermes,
            &mut stored_costs.hermes,
            &mut hermes_dates,
            time_range,
        )
    {
        log::warn!(
            "failed to read Hermes DB {}: {err}",
            paths.hermes_db.display()
        );
    }

    let mut all_dates: HashSet<&String> = HashSet::new();
    all_dates.extend(claude_dates.iter());
    all_dates.extend(codex_dates.iter());
    all_dates.extend(copilot_dates.iter());
    all_dates.extend(gemini_dates.iter());
    all_dates.extend(grok_dates.iter());
    all_dates.extend(deepseek_dates.iter());
    all_dates.extend(antigravity_dates.iter());
    all_dates.extend(opencode_dates.iter());
    all_dates.extend(cursor_dates.iter());
    all_dates.extend(hermes_dates.iter());

    let provider_days = ProviderActiveDays {
        claude: claude_dates.len(),
        codex: codex_dates.len(),
        copilot: copilot_dates.len(),
        gemini: gemini_dates.len(),
        grok: grok_dates.len(),
        deepseek: deepseek_dates.len(),
        antigravity: antigravity_dates.len(),
        opencode: opencode_dates.len(),
        cursor: cursor_dates.len(),
        hermes: hermes_dates.len(),
        total: all_dates.len(),
    };

    Ok(UsageData {
        models: result,
        per_provider,
        provider_days,
        stored_costs,
    })
}

/// Options controlling a usage scan.
///
/// When [`tiers`](Self::tiers) is `None` (the default), all requests bill at base rates.
#[derive(Debug, Default, Clone)]
pub struct UsageScanOptions {
    /// Per-model context-tier thresholds for per-request tier classification.
    pub tiers: Option<Arc<TierThresholds>>,
}

/// Scans usage from the current user's resolved paths with default [`UsageScanOptions`] and returns scan diagnostics.
///
/// Delegates to [`aggregate_usage_from_home_with_diagnostics_opts`].
///
/// # Errors
///
/// Returns an error if [`resolve_paths`] cannot determine provider directories.
pub fn aggregate_usage_from_home_with_diagnostics(
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<UsageCollection> {
    aggregate_usage_from_home_with_diagnostics_opts(
        time_range,
        providers,
        &UsageScanOptions::default(),
    )
}

/// Scans usage from the current user's resolved paths with `options`, updating the on-disk session ledger.
///
/// Retains sessions whose source files have been deleted; ledger save failures are logged without failing the scan.
///
/// # Errors
///
/// Returns an error if [`resolve_paths`] cannot determine provider directories.
pub fn aggregate_usage_from_home_with_diagnostics_opts(
    time_range: TimeRange,
    providers: ProvidersConfig,
    options: &UsageScanOptions,
) -> Result<UsageCollection> {
    let paths = resolve_paths()?;
    let mut ledger = SessionLedger::open(&paths.cache_dir);
    let collection = aggregate_usage_from_paths_with_cache_opts(
        &paths,
        time_range,
        providers,
        &mut ledger,
        options,
    )?;
    if let Err(error) = ledger.save() {
        log::warn!("failed to save the session ledger: {error:#}");
    }
    Ok(collection)
}

/// Scans usage from explicit `paths` with diagnostics using a transient in-memory ledger.
pub fn aggregate_usage_from_paths_with_diagnostics(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<UsageCollection> {
    let mut ledger = SessionLedger::new();
    aggregate_usage_from_paths_with_cache(paths, time_range, providers, &mut ledger)
}

/// Runs an incremental usage scan from `paths` with default [`UsageScanOptions`], updating `ledger` in place.
///
/// Unchanged sources fold from `ledger` and deleted sources remain counted. Cached schema failures preserve their diagnostics, while transient I/O errors are retried on the next scan.
pub fn aggregate_usage_from_paths_with_cache(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
) -> Result<UsageCollection> {
    aggregate_usage_from_paths_with_cache_opts(
        paths,
        time_range,
        providers,
        ledger,
        &UsageScanOptions::default(),
    )
}

/// Scans usage from the current user's resolved paths with `options`, updating the on-disk session ledger and partitioning results by local calendar date (`YYYY-MM-DD`).
///
/// Retains sessions whose source files have been deleted; ledger save failures are logged without failing the scan.
///
/// # Errors
///
/// Returns an error if [`resolve_paths`] cannot determine provider directories.
pub fn aggregate_daily_usage_from_home_with_diagnostics_opts(
    time_range: TimeRange,
    providers: ProvidersConfig,
    options: &UsageScanOptions,
) -> Result<DailyUsageCollection> {
    let paths = resolve_paths()?;
    let mut ledger = SessionLedger::open(&paths.cache_dir);
    let collection = aggregate_daily_usage_from_paths_with_cache_opts(
        &paths,
        time_range,
        providers,
        &mut ledger,
        options,
    )?;
    if let Err(error) = ledger.save() {
        log::warn!("failed to save the session ledger: {error:#}");
    }
    Ok(collection)
}

/// Runs an incremental usage scan from `paths` with `options`, updating `ledger` in place.
///
/// Each session's usage half records the tier-threshold fingerprint used to classify it; entries stamped without a matching fingerprint (including those written by an analysis scan) are re-read.
pub fn aggregate_usage_from_paths_with_cache_opts(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
    options: &UsageScanOptions,
) -> Result<UsageCollection> {
    let mut accumulator = UsageAccumulator::default();
    let diagnostics = scan_usage_into_sink(
        paths,
        time_range,
        providers,
        ledger,
        options,
        &mut accumulator,
    )?;
    Ok(UsageCollection {
        data: accumulator.finish(),
        diagnostics,
    })
}

/// Runs an incremental per-day usage scan from `paths` with `options`, updating `ledger` in place.
///
/// Uses the same ledger, provider filters, time-range cutoff, tier thresholds, and remote-session deduplication as [`aggregate_usage_from_paths_with_cache_opts`], preserving local `YYYY-MM-DD` day boundaries.
pub fn aggregate_daily_usage_from_paths_with_cache_opts(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
    options: &UsageScanOptions,
) -> Result<DailyUsageCollection> {
    let mut accumulator = DailyUsageAccumulator::default();
    let diagnostics = scan_usage_into_sink(
        paths,
        time_range,
        providers,
        ledger,
        options,
        &mut accumulator,
    )?;
    let remotes = ledger.remote_names();
    Ok(DailyUsageCollection {
        data: DailyUsageData {
            days: accumulator.days,
            remotes,
        },
        diagnostics,
    })
}

fn scan_usage_into_sink<S: crate::scan::CompactSink>(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
    options: &UsageScanOptions,
    sink: &mut S,
) -> Result<ScanDiagnostics> {
    let tiers = options.tiers.as_deref();
    ledger.begin_scan();
    let mut diagnostics = ScanDiagnostics::default();

    crate::scan::scan_all_cached_files(
        paths,
        providers,
        time_range,
        ledger,
        ScanFeature::Usage,
        sink,
        &mut diagnostics,
        tiers,
    )?;

    // Read all time from databases and apply `time_range` when folding so narrower ranges do not mark older sessions missing.
    if providers.opencode {
        scan_database_half(
            ExtensionType::OpenCode,
            &paths.opencode_db,
            ScanFeature::Usage,
            stamp::tiers_fingerprint(tiers),
            time_range,
            ledger,
            sink,
            &mut diagnostics,
            || {
                read_opencode_usage_contributions(&paths.opencode_db, TimeRange::All, tiers)
                    .map(|read| group_usage_rows(read, "usage records", true))
            },
        );
    }
    // Cursor and Hermes do not classify context tiers, so leave their tier fingerprint empty to avoid unnecessary re-reads on pricing updates.
    if providers.cursor {
        scan_cursor_stores(
            paths,
            ScanFeature::Usage,
            None,
            time_range,
            ledger,
            sink,
            &mut diagnostics,
        );
    }
    if providers.hermes {
        scan_database_half(
            ExtensionType::Hermes,
            &paths.hermes_db,
            ScanFeature::Usage,
            None,
            time_range,
            ledger,
            sink,
            &mut diagnostics,
            || {
                read_hermes_usage_contributions(&paths.hermes_db, TimeRange::All)
                    .map(|read| group_usage_rows(read, "usage records", true))
            },
        );
    }

    crate::scan::fold_remotes(
        ledger,
        |provider| providers.includes(provider),
        time_range,
        sink,
    );

    diagnostics.finalize();
    Ok(diagnostics)
}

fn fold_day_usage_into(
    models: &mut UsageResult,
    per_provider: &mut PerProviderUsage,
    stored_costs: &mut StoredCosts,
    provider: ExtensionType,
    day: &DayEntry,
) {
    let provider_result = match provider {
        ExtensionType::ClaudeCode => &mut per_provider.claude,
        ExtensionType::Codex => &mut per_provider.codex,
        ExtensionType::Copilot => &mut per_provider.copilot,
        ExtensionType::Gemini => &mut per_provider.gemini,
        ExtensionType::Grok => &mut per_provider.grok,
        ExtensionType::DeepSeek => &mut per_provider.deepseek,
        ExtensionType::Antigravity => &mut per_provider.antigravity,
        ExtensionType::OpenCode => &mut per_provider.opencode,
        ExtensionType::Cursor => &mut per_provider.cursor,
        ExtensionType::Hermes => &mut per_provider.hermes,
    };
    // Use `get_mut` before `insert` to avoid cloning `model` on existing keys.
    for (model, raw) in &day.usage {
        let usage = DayEntry::usage_value(raw);
        match provider_result.get_mut(model) {
            Some(existing) => merge_usage_values(existing, &usage),
            None => {
                provider_result.insert(model.clone(), usage.clone());
            }
        }
        match models.get_mut(model) {
            Some(existing) => merge_usage_values(existing, &usage),
            None => {
                models.insert(model.clone(), usage);
            }
        }
    }

    let stored = match provider {
        ExtensionType::OpenCode => Some(&mut stored_costs.opencode),
        ExtensionType::Hermes => Some(&mut stored_costs.hermes),
        _ => None,
    };
    if let Some(stored) = stored {
        for (model, cost) in &day.stored_cost {
            *stored.entry(model.clone()).or_insert(0.0) += cost;
        }
    }
}

#[derive(Default)]
struct DailyUsageAccumulator {
    days: BTreeMap<String, DayUsageData>,
}

impl crate::scan::CompactSink for DailyUsageAccumulator {
    fn fold(&mut self, provider: ExtensionType, date: &str, day: &DayEntry) {
        if day.usage.is_empty() {
            return;
        }
        let bucket = match self.days.get_mut(date) {
            Some(bucket) => bucket,
            None => self.days.entry(date.to_string()).or_default(),
        };
        fold_day_usage_into(
            &mut bucket.models,
            &mut bucket.per_provider,
            &mut bucket.stored_costs,
            provider,
            day,
        );
    }
}

#[derive(Default)]
struct UsageAccumulator {
    models: UsageResult,
    per_provider: PerProviderUsage,
    stored_costs: StoredCosts,
    claude_dates: HashSet<String>,
    codex_dates: HashSet<String>,
    copilot_dates: HashSet<String>,
    gemini_dates: HashSet<String>,
    grok_dates: HashSet<String>,
    deepseek_dates: HashSet<String>,
    antigravity_dates: HashSet<String>,
    opencode_dates: HashSet<String>,
    cursor_dates: HashSet<String>,
    hermes_dates: HashSet<String>,
}

impl crate::scan::CompactSink for UsageAccumulator {
    fn fold(&mut self, provider: ExtensionType, date: &str, day: &DayEntry) {
        fold_day_usage_into(
            &mut self.models,
            &mut self.per_provider,
            &mut self.stored_costs,
            provider,
            day,
        );

        if day.active.usage {
            let dates = match provider {
                ExtensionType::ClaudeCode => &mut self.claude_dates,
                ExtensionType::Codex => &mut self.codex_dates,
                ExtensionType::Copilot => &mut self.copilot_dates,
                ExtensionType::Gemini => &mut self.gemini_dates,
                ExtensionType::Grok => &mut self.grok_dates,
                ExtensionType::DeepSeek => &mut self.deepseek_dates,
                ExtensionType::Antigravity => &mut self.antigravity_dates,
                ExtensionType::OpenCode => &mut self.opencode_dates,
                ExtensionType::Cursor => &mut self.cursor_dates,
                ExtensionType::Hermes => &mut self.hermes_dates,
            };
            dates.insert(date.to_string());
        }
    }
}

impl UsageAccumulator {
    fn finish(self) -> UsageData {
        let mut all_dates: HashSet<&String> = HashSet::new();
        all_dates.extend(self.claude_dates.iter());
        all_dates.extend(self.codex_dates.iter());
        all_dates.extend(self.copilot_dates.iter());
        all_dates.extend(self.gemini_dates.iter());
        all_dates.extend(self.grok_dates.iter());
        all_dates.extend(self.deepseek_dates.iter());
        all_dates.extend(self.antigravity_dates.iter());
        all_dates.extend(self.opencode_dates.iter());
        all_dates.extend(self.cursor_dates.iter());
        all_dates.extend(self.hermes_dates.iter());
        let total_days = all_dates.len();
        UsageData {
            models: self.models,
            per_provider: self.per_provider,
            provider_days: ProviderActiveDays {
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
                total: total_days,
            },
            stored_costs: self.stored_costs,
        }
    }
}

/// Walks one provider's session roots in [`ParseMode::UsageOnly`] and merges per-model usage into `global_result`, `provider_result`, and `unique_dates`.
#[allow(clippy::too_many_arguments)] // per-provider helper; struct-wrapping the args would hurt readability
fn process_usage_directory<F>(
    dirs: &[&Path],
    provider: ExtensionType,
    global_result: &mut UsageResult,
    provider_result: &mut UsageResult,
    unique_dates: &mut HashSet<String>,
    filter_fn: F,
    time_range: TimeRange,
    max_depth: Option<usize>,
) where
    F: Copy + Fn(&Path) -> bool + Sync + Send,
{
    // Discovery failures are dropped here because this non-diagnostic path has no diagnostics sink.
    let files =
        collect_provider_files_diagnostics(dirs, provider, filter_fn, time_range, max_depth).files;

    // Fix the provider by directory so metadata-only preambles cannot trigger mis-detection, and parse in `UsageOnly` to skip file-op detail payloads.
    let file_results: Vec<(String, FastHashMap<String, Value>)> = files
        .into_par_iter()
        .filter_map(|file_info| {
            match parse_session_file_typed_as(&file_info.path, provider, ParseMode::UsageOnly) {
                Ok(analysis) => {
                    let conversation_usage = extract_conversation_usage_from_analysis(analysis);
                    Some((file_info.modified_date, conversation_usage))
                }
                Err(e) => {
                    log::warn!("failed to analyze {}: {e}", file_info.path.display());
                    None
                }
            }
        })
        .collect();

    for (date, conversation_usage) in file_results {
        if usage_map_has_activity(&conversation_usage, 0.0) {
            unique_dates.insert(date);
        }

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
}

/// Reads OpenCode's SQLite database and merges its usage and stored costs into the accumulators.
fn process_opencode_usage(
    db_path: &Path,
    global_result: &mut UsageResult,
    provider_result: &mut UsageResult,
    stored_costs: &mut FastHashMap<String, f64>,
    unique_dates: &mut HashSet<String>,
    time_range: TimeRange,
) -> Result<()> {
    let sessions = read_opencode_usage(db_path, time_range)?;
    fold_database_sessions(
        sessions,
        global_result,
        provider_result,
        Some(stored_costs),
        unique_dates,
    );
    Ok(())
}

/// Reads Cursor's chat-store usage estimates and merges them into the accumulators.
fn process_cursor_usage(
    chats_dir: &Path,
    tracking_db: &Path,
    global_result: &mut UsageResult,
    provider_result: &mut UsageResult,
    unique_dates: &mut HashSet<String>,
    time_range: TimeRange,
) -> Result<()> {
    let sessions = read_cursor_usage(chats_dir, tracking_db, time_range)?;
    fold_database_sessions(sessions, global_result, provider_result, None, unique_dates);
    Ok(())
}

/// Reads Hermes's SQLite database and merges its usage and stored costs into the accumulators.
fn process_hermes_usage(
    db_path: &Path,
    global_result: &mut UsageResult,
    provider_result: &mut UsageResult,
    stored_costs: &mut FastHashMap<String, f64>,
    unique_dates: &mut HashSet<String>,
    time_range: TimeRange,
) -> Result<()> {
    let sessions = read_hermes_usage(db_path, time_range)?;
    fold_database_sessions(
        sessions,
        global_result,
        provider_result,
        Some(stored_costs),
        unique_dates,
    );
    Ok(())
}

/// Merges `(date, analysis, cost)` rows from a database provider into the global and provider-scoped accumulators.
fn fold_database_sessions(
    sessions: Vec<(String, CodeAnalysis, f64)>,
    global_result: &mut UsageResult,
    provider_result: &mut UsageResult,
    mut stored_costs: Option<&mut FastHashMap<String, f64>>,
    unique_dates: &mut HashSet<String>,
) {
    for (date, analysis, session_cost) in sessions {
        let conversation_usage = extract_conversation_usage_from_analysis(analysis);
        if usage_map_has_activity(&conversation_usage, session_cost) {
            unique_dates.insert(date);
        }
        for (model, usage_value) in conversation_usage {
            if let Some(stored_costs) = &mut stored_costs {
                *stored_costs.entry(model.clone()).or_insert(0.0) += session_cost;
            }

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
}

fn usage_map_has_activity(usage: &FastHashMap<String, Value>, stored_cost: f64) -> bool {
    stored_cost != 0.0
        || usage
            .values()
            .any(|value| crate::utils::extract_token_counts(value).has_activity())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::TokenCounts;
    use serde_json::json;

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
    fn merge_preserves_tool_tokens_across_mixed_shapes() {
        use crate::utils::extract_token_counts;

        let codex = json!({
            "total_token_usage": {
                "input_tokens": 1000,
                "cached_input_tokens": 200,
                "output_tokens": 500,
                "total_tokens": 1500
            }
        });
        // Gemini's `tool_tokens` is unpriced and outside the standard flat key set, so verify mixed-shape merges preserve it.
        let gemini = json!({
            "input_tokens": 100,
            "output_tokens": 20,
            "cache_read_input_tokens": 50,
            "thoughts_tokens": 30,
            "tool_tokens": 7,
            "total_tokens": 200
        });

        let expect = |c: TokenCounts| {
            assert_eq!(c.tool_tokens, 7);
            // 1500 (Codex) + 100 + 20 + 50 + 30 + 7 (Gemini).
            assert_eq!(c.total, 1707);
        };

        let mut existing = codex.clone();
        merge_usage_values(&mut existing, &gemini);
        expect(extract_token_counts(&existing));

        let mut existing = gemini.clone();
        merge_usage_values(&mut existing, &codex);
        expect(extract_token_counts(&existing));
    }
}
