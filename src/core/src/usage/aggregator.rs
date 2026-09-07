//! Aggregates per-model token usage across every provider source.
//!
//! File-backed providers are walked with the provider fixed by its *source
//! path* (never re-detected from file contents) and parsed in
//! [`ParseMode::UsageOnly`] to skip the heavy file-operation payloads;
//! OpenCode, Cursor and Hermes are read straight out of their SQLite
//! databases. Every source's per-model usage is merged into a [`UsageData`],
//! which tracks the provider twice on purpose — once merged across providers
//! (the per-model table) and once kept per source (the per-provider footer) —
//! see [`UsageData`] for why.

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
    COPILOT_SESSION_MAX_DEPTH, DSH_SESSION_MAX_DEPTH, GROK_SESSION_MAX_DEPTH, HelperPaths,
    is_claude_session_file, is_codex_session_file, is_copilot_session_file, is_dsh_session_file,
    is_gemini_session_file, is_grok_session_file, merge_usage_values, resolve_paths,
};
use anyhow::Result;
use rayon::prelude::*;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

/// Aggregated token usage plus the per-provider active-day counts.
///
/// All fields are public for the display layer to read. Token totals are
/// tracked two ways at once because the two views need different attribution:
/// [`models`](UsageData::models) merges a shared model (e.g. `claude-sonnet-4-6`
/// emitted by both Claude Code and Copilot CLI) into one row, while
/// [`per_provider`](UsageData::per_provider) keeps the same tokens scoped to the
/// source so the footer can attribute them correctly. The same tokens land in
/// both maps, so a consumer reads one view or the other and never sums them.
///
/// # Examples
///
/// ```no_run
/// use vct_core::{aggregate_usage_from_home, TimeRange};
///
/// let data = aggregate_usage_from_home(TimeRange::All)?;
/// // Total distinct days that contributed any usage, across all providers.
/// println!("active days: {}", data.provider_days.total);
/// # Ok::<(), anyhow::Error>(())
/// ```
#[derive(Debug, Clone, Serialize)]
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
}

// Usage and analysis both report the one unified scan-diagnostics type; it is
// re-exported here so callers can reach it as `usage::ScanDiagnostics`.
pub use crate::scan::{ScanDiagnostics, ScanFailure};

/// Usage data paired with source-collection diagnostics.
pub struct UsageCollection {
    /// Successfully collected usage.
    pub data: UsageData,
    /// Candidate, success, and failure counts from the scan.
    pub diagnostics: ScanDiagnostics,
}

/// Provider-authoritative per-model costs, kept **separate per provider**.
///
/// OpenCode and Hermes are the providers that record their own costs; Cursor
/// carries none, so its local estimate is priced by an exact LiteLLM match
/// alone. Separate maps prevent a colliding bare model name from
/// cross-contaminating providers.
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
        // Claude advisor-message tokens live in a separate map so the
        // `analysis` aggregator ignores them; the `usage` path folds them in
        // here, attributed to the advisor's own model for correct pricing.
        for (model, usage) in record.advisor_usage {
            merge_into(model, usage);
        }
    }

    conversation_usage
}

/// Aggregates token usage from all AI provider session directories.
///
/// Scans every provider source resolved by [`resolve_paths`], filtered by
/// `time_range`, and rolls every session's per-model usage into a
/// [`UsageData`]. Missing provider sources are skipped silently, and a source
/// file or database that fails to parse logs a warning to the diagnostic log
/// and is excluded rather than aborting the whole scan.
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
/// use vct_core::{aggregate_usage_from_home, TimeRange};
///
/// let data = aggregate_usage_from_home(TimeRange::All)?;
/// for model in data.models.keys() {
///     println!("{model}");
/// }
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn aggregate_usage_from_home(time_range: TimeRange) -> Result<UsageData> {
    aggregate_usage_from_home_with_providers(time_range, ProvidersConfig::default())
}

/// [`aggregate_usage_from_home`] with explicit per-provider toggles (from
/// `~/.vct/config.toml`). A disabled provider is skipped entirely.
pub fn aggregate_usage_from_home_with_providers(
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<UsageData> {
    aggregate_usage_from_paths_with_providers(&resolve_paths()?, time_range, providers)
}

/// Aggregates token usage from provider session directories rooted at an
/// explicit [`HelperPaths`].
///
/// The env-free, injectable counterpart of [`aggregate_usage_from_home`]:
/// every provider path comes from `paths` rather than the resolved home
/// directory, so tests can point them at a temp tree and exercise the real
/// aggregation without mutating process-global `HOME`. See
/// [`aggregate_usage_from_home`] for the aggregation semantics.
///
/// # Errors
///
/// Returns an error only under the same conditions as
/// [`aggregate_usage_from_home`].
pub fn aggregate_usage_from_paths(paths: &HelperPaths, time_range: TimeRange) -> Result<UsageData> {
    aggregate_usage_from_paths_with_providers(paths, time_range, ProvidersConfig::default())
}

/// [`aggregate_usage_from_paths`] with explicit provider toggles (the injectable core
/// used by the CLI once `config.toml` is loaded).
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
    let mut opencode_dates: HashSet<String> = HashSet::new();
    let mut cursor_dates: HashSet<String> = HashSet::new();
    let mut hermes_dates: HashSet<String> = HashSet::new();

    if providers.claude && paths.claude_session_dir.exists() {
        // Walks the projects tree recursively, so top-level `<session>.jsonl` logs
        // and `<session>/subagents/agent-*.jsonl` logs are both collected here.
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
        // `events.jsonl` always lives exactly two levels under `session-state/`,
        // so bounding the walk keeps per-session snapshot subtrees
        // (`rewind-snapshots/backups/*`, `files/*`, …) out of the iteration and
        // the scan cost linear in sessions rather than total artifacts.
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

    // OpenCode lives in a single SQLite database rather than a session
    // directory, so it is read directly instead of walked.
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

    // Cursor's usage is a local estimate from its chat stores (read directly like
    // OpenCode, not a walked session directory), so it is only attempted when the
    // chat stores are present — matching the analysis path.
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

    // Hermes, like OpenCode, is a single SQLite database read directly.
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

/// Optional knobs for a usage scan.
///
/// `tiers` is the per-request context-tier snapshot derived from the current
/// pricing map (see [`TierThresholds`]); `None` (the default) classifies
/// nothing and every request bills at base rates.
#[derive(Debug, Default, Clone)]
pub struct UsageScanOptions {
    /// "Model → lowest tier threshold" snapshot for per-request classification.
    pub tiers: Option<Arc<TierThresholds>>,
}

/// Diagnostics-aware usage scan rooted at the current user's provider paths.
///
/// Reads and writes the on-disk session ledger under `~/.vct/sessions/`, so
/// sessions whose source is gone still count; see
/// [`aggregate_usage_from_home_with_diagnostics_opts`].
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

/// [`aggregate_usage_from_home_with_diagnostics`] with scan options.
///
/// Opens the session ledger under the user's `~/.vct`, scans through it, and
/// writes it back; a ledger that cannot be written is logged and the scan's
/// result returned anyway.
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

/// Diagnostics-aware usage scan rooted at explicit provider paths, through an
/// in-memory ledger that touches nothing on disk.
pub fn aggregate_usage_from_paths_with_diagnostics(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
) -> Result<UsageCollection> {
    let mut ledger = SessionLedger::new();
    aggregate_usage_from_paths_with_cache(paths, time_range, providers, &mut ledger)
}

/// Incremental usage scan through a session ledger.
///
/// Reusing `ledger` across calls re-reads only sources whose stamp changed,
/// and keeps folding sessions whose source has gone. Retained schema failures
/// keep their diagnostics, while metadata, open, and read errors are not
/// recorded and are retried next time.
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

/// [`aggregate_usage_from_paths_with_cache`] with scan options.
///
/// The ledger stamps each session's usage half with the tier snapshot it was
/// classified against, so one ledger serves both features: an entry an
/// analysis scan wrote is re-read by a usage scan that carries a snapshot, and
/// nothing else is disturbed.
pub fn aggregate_usage_from_paths_with_cache_opts(
    paths: &HelperPaths,
    time_range: TimeRange,
    providers: ProvidersConfig,
    ledger: &mut SessionLedger,
    options: &UsageScanOptions,
) -> Result<UsageCollection> {
    let tiers = options.tiers.as_deref();
    ledger.begin_scan();
    let mut accumulator = UsageAccumulator::default();
    let mut diagnostics = ScanDiagnostics::default();

    crate::scan::scan_all_cached_files(
        paths,
        providers,
        time_range,
        ledger,
        ScanFeature::Usage,
        &mut accumulator,
        &mut diagnostics,
        tiers,
    )?;

    // Every database read covers all time: the range is applied to the folded
    // days, so a `--daily` scan cannot mistake older sessions for deleted ones.
    if providers.opencode {
        scan_database_half(
            ExtensionType::OpenCode,
            &paths.opencode_db,
            ScanFeature::Usage,
            stamp::tiers_fingerprint(tiers),
            time_range,
            ledger,
            &mut accumulator,
            &mut diagnostics,
            || {
                read_opencode_usage_contributions(&paths.opencode_db, TimeRange::All, tiers)
                    .map(|read| group_usage_rows(read, "usage records"))
            },
        );
    }
    // Cursor and Hermes classify nothing, so no snapshot goes on their stamps
    // and a pricing change never re-reads them.
    if providers.cursor {
        scan_cursor_stores(
            paths,
            ScanFeature::Usage,
            None,
            time_range,
            ledger,
            &mut accumulator,
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
            &mut accumulator,
            &mut diagnostics,
            || {
                read_hermes_usage_contributions(&paths.hermes_db, TimeRange::All)
                    .map(|read| group_usage_rows(read, "usage records"))
            },
        );
    }

    diagnostics.finalize();
    Ok(UsageCollection {
        data: accumulator.finish(),
        diagnostics,
    })
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
    opencode_dates: HashSet<String>,
    cursor_dates: HashSet<String>,
    hermes_dates: HashSet<String>,
}

impl crate::scan::CompactSink for UsageAccumulator {
    fn fold(&mut self, provider: ExtensionType, date: &str, day: &DayEntry) {
        let provider_result = match provider {
            ExtensionType::ClaudeCode => &mut self.per_provider.claude,
            ExtensionType::Codex => &mut self.per_provider.codex,
            ExtensionType::Copilot => &mut self.per_provider.copilot,
            ExtensionType::Gemini => &mut self.per_provider.gemini,
            ExtensionType::Grok => &mut self.per_provider.grok,
            ExtensionType::DeepSeek => &mut self.per_provider.deepseek,
            ExtensionType::OpenCode => &mut self.per_provider.opencode,
            ExtensionType::Cursor => &mut self.per_provider.cursor,
            ExtensionType::Hermes => &mut self.per_provider.hermes,
        };
        // Clone the model key only on a miss (an insert genuinely needs an owned
        // key); a merge into an existing row needs no allocation at all.
        for (model, raw) in &day.usage {
            let usage = DayEntry::usage_value(raw);
            match provider_result.get_mut(model) {
                Some(existing) => merge_usage_values(existing, &usage),
                None => {
                    provider_result.insert(model.clone(), usage.clone());
                }
            }
            match self.models.get_mut(model) {
                Some(existing) => merge_usage_values(existing, &usage),
                None => {
                    // Last use of `usage`, so move it in rather than clone.
                    self.models.insert(model.clone(), usage);
                }
            }
        }

        let stored = match provider {
            ExtensionType::OpenCode => Some(&mut self.stored_costs.opencode),
            ExtensionType::Hermes => Some(&mut self.stored_costs.hermes),
            _ => None,
        };
        if let Some(stored) = stored {
            for (model, cost) in &day.stored_cost {
                *stored.entry(model.clone()).or_insert(0.0) += cost;
            }
        }

        if day.active.usage {
            let dates = match provider {
                ExtensionType::ClaudeCode => &mut self.claude_dates,
                ExtensionType::Codex => &mut self.codex_dates,
                ExtensionType::Copilot => &mut self.copilot_dates,
                ExtensionType::Gemini => &mut self.gemini_dates,
                ExtensionType::Grok => &mut self.grok_dates,
                ExtensionType::DeepSeek => &mut self.deepseek_dates,
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
        // Only the union's cardinality is needed, so union references rather
        // than cloning every date string across the nine per-provider sets.
        let mut all_dates: HashSet<&String> = HashSet::new();
        all_dates.extend(self.claude_dates.iter());
        all_dates.extend(self.codex_dates.iter());
        all_dates.extend(self.copilot_dates.iter());
        all_dates.extend(self.gemini_dates.iter());
        all_dates.extend(self.grok_dates.iter());
        all_dates.extend(self.deepseek_dates.iter());
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
                opencode: self.opencode_dates.len(),
                cursor: self.cursor_dates.len(),
                hermes: self.hermes_dates.len(),
                total: total_days,
            },
            stored_costs: self.stored_costs,
        }
    }
}

/// Walks one provider's session roots and merges their usage into both result maps.
///
/// Files matching `filter_fn` (and within `max_depth`, when set) are parsed in
/// parallel with the provider fixed to `provider` — never re-detected from
/// contents — and each session's per-model tokens are merged into both
/// `global_result` (cross-provider view) and `provider_result` (source-scoped
/// view). A session that contributed any tokens has its modified date inserted
/// into `unique_dates` for the active-day count. Traversal and metadata errors
/// are skipped by the collector, and a file that fails to parse logs a warning
/// and is skipped, so this is best-effort by construction and cannot fail.
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
    // This entry point has no diagnostics channel, so discovery failures are
    // dropped here; the `*_with_diagnostics` scanners are the ones that report them.
    let files = collect_provider_files_diagnostics(dirs, filter_fn, time_range, max_depth).files;

    // Fixing the provider by source directory is what keeps a Claude session
    // whose first line is a metadata sentinel (`permission-mode`,
    // `file-history-snapshot`) from being re-detected as another provider and
    // silently dropped. The global file cache is bypassed on purpose: the
    // `usage` path never needs the heavy `write_file_details` /
    // `edit_file_details` payloads, so caching the full analysis would waste
    // the memory win from `UsageOnly`.
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

    // Merged sequentially; it is cheap next to the parallel parse above.
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

/// Reads OpenCode's SQLite database and merges its per-model usage into both
/// the global and OpenCode-scoped maps.
///
/// Mirrors the tail of [`process_usage_directory`] but sources sessions from
/// the database (via [`read_opencode_usage`]) instead of a directory walk. Each
/// row's date comes from the assistant message timestamp (falling back to
/// `session.time_updated` on legacy schemas) and is recorded in `unique_dates`
/// for the active-day count.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or queried.
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

/// Reads Cursor's per-model usage (a local estimate from the chat stores) and
/// merges it into both the global and Cursor-scoped maps.
///
/// Mirrors [`process_opencode_usage`], but Cursor records no cost of its own,
/// so there is no stored-cost map to fold into and pricing accepts only an
/// exact LiteLLM match rather than a fuzzy price guess.
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

/// Reads Hermes's per-model usage from its SQLite database and merges it into
/// both the global and Hermes-scoped maps.
///
/// Mirrors [`process_opencode_usage`]: Hermes stores its own per-model cost, so
/// it uses the same stored-cost path rather than a fuzzy price guess.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or queried.
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

/// Folds `(date, analysis, cost)` rows from a database provider (OpenCode /
/// Cursor / Hermes) into the global + provider-scoped maps.
///
/// `stored_costs` is `None` for Cursor, whose local estimate reports no cost of
/// its own; the other two fold their per-model cost into the table.
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
        // A Gemini row for the same model name. `tool_tokens` is the one bucket
        // with no price and no field of its own in the flat key set, so it is
        // the one the mixed-shape rewrite used to drop.
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
