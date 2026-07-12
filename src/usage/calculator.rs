//! Aggregates per-model token usage across the four provider session trees.
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
use crate::constants::{FastHashMap, capacity};
use crate::models::{
    CodeAnalysis, ExtensionType, PerProviderUsage, Provider, ProviderActiveDays, UsageResult,
};
use crate::pricing::{ModelPricingMap, calculate_cost};
use crate::session::{
    ParseMode, parse_session_file_as, read_cursor_usage, read_hermes_usage, read_opencode_usage,
};
use crate::utils::{
    COPILOT_SESSION_MAX_DEPTH, HelperPaths, TokenCounts, collect_files_with_max_depth,
    is_claude_session_file, is_codex_session_file, is_copilot_session_file, is_gemini_session_file,
    resolve_paths,
};
use anyhow::Result;
use rayon::prelude::*;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;

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
}

/// Provider-authoritative per-model costs, kept **separate per provider**.
///
/// OpenCode records assistant-message costs and Cursor reports its billed cost
/// per usage event, so when a model has no exact LiteLLM price we display this
/// stored cost instead of guessing from a fuzzy match. The two are kept apart
/// (rather than in one model-keyed map) because a legacy OpenCode session can
/// carry a *bare* model name that collides with a Cursor model of the same name
/// — a shared map would then cross-contaminate their costs.
#[derive(Debug, Default, Clone)]
pub struct StoredCosts {
    /// OpenCode's per-model stored cost, keyed by model name.
    pub opencode: FastHashMap<String, f64>,
    /// Cursor's per-model dashboard cost, keyed by model name.
    pub cursor: FastHashMap<String, f64>,
    /// Hermes's per-model stored cost, keyed by model name.
    pub hermes: FastHashMap<String, f64>,
}

/// Extracts token usage data from a typed `CodeAnalysis`.
///
/// Reads directly from the typed `conversation_usage` map instead of walking
/// `Value` via `.get(...)`, so no intermediate `serde_json::Value` tree is
/// built or retained here.
fn extract_conversation_usage_from_analysis(analysis: &CodeAnalysis) -> FastHashMap<String, Value> {
    let mut conversation_usage = FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);

    let mut merge_into = |model: &String, usage: &Value| {
        conversation_usage
            .entry(model.clone())
            .and_modify(|existing_usage| merge_usage_values(existing_usage, usage))
            .or_insert_with(|| usage.clone());
    };

    for record in &analysis.records {
        for (model, usage) in &record.conversation_usage {
            merge_into(model, usage);
        }
        // Claude advisor-message tokens live in a separate map so the
        // `analysis` aggregator ignores them; the `usage` path folds them in
        // here, attributed to the advisor's own model for correct pricing.
        for (model, usage) in &record.advisor_usage {
            merge_into(model, usage);
        }
    }

    conversation_usage
}

/// Aggregates token usage from all AI provider session directories.
///
/// Scans the Claude Code, Codex, Copilot, and Gemini session trees resolved by
/// [`resolve_paths`], filtered by `time_range`, and rolls every session's
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
    let mut result = FastHashMap::with_capacity(capacity::MODEL_COMBINATIONS);
    let mut per_provider = PerProviderUsage::default();
    let mut stored_costs = StoredCosts::default();

    let mut claude_dates: HashSet<String> = HashSet::new();
    let mut codex_dates: HashSet<String> = HashSet::new();
    let mut copilot_dates: HashSet<String> = HashSet::new();
    let mut gemini_dates: HashSet<String> = HashSet::new();
    let mut opencode_dates: HashSet<String> = HashSet::new();
    let mut cursor_dates: HashSet<String> = HashSet::new();
    let mut hermes_dates: HashSet<String> = HashSet::new();

    if providers.claude && paths.claude_session_dir.exists() {
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
            None,
        )?;
    }

    if providers.codex && paths.codex_session_dir.exists() {
        process_usage_directory(
            &paths.codex_session_dir,
            ExtensionType::Codex,
            &mut result,
            &mut per_provider.codex,
            &mut codex_dates,
            is_codex_session_file,
            time_range,
            None,
        )?;
    }

    if providers.copilot && paths.copilot_session_dir.exists() {
        // `events.jsonl` always lives exactly two levels under
        // `session-state/`. Bounding the walk here keeps per-session
        // snapshot subtrees (`rewind-snapshots/backups/*`, `files/*`, …)
        // out of the `WalkDir` iteration entirely, so the scan cost stays
        // linear in the number of sessions rather than total artifacts.
        process_usage_directory(
            &paths.copilot_session_dir,
            ExtensionType::Copilot,
            &mut result,
            &mut per_provider.copilot,
            &mut copilot_dates,
            is_copilot_session_file,
            time_range,
            Some(COPILOT_SESSION_MAX_DEPTH),
        )?;
    }

    if providers.gemini && paths.gemini_session_dir.exists() {
        process_usage_directory(
            &paths.gemini_session_dir,
            ExtensionType::Gemini,
            &mut result,
            &mut per_provider.gemini,
            &mut gemini_dates,
            is_gemini_session_file,
            time_range,
            None,
        )?;
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
            &mut stored_costs.cursor,
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
    all_dates.extend(opencode_dates.iter());
    all_dates.extend(cursor_dates.iter());
    all_dates.extend(hermes_dates.iter());

    let provider_days = ProviderActiveDays {
        claude: claude_dates.len(),
        codex: codex_dates.len(),
        copilot: copilot_dates.len(),
        gemini: gemini_dates.len(),
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

/// Walks one provider directory and merges its usage into both result maps.
///
/// Files matching `filter_fn` (and within `max_depth`, when set) are parsed in
/// parallel with the provider fixed to `provider` — never re-detected from
/// contents — and each session's per-model tokens are merged into both
/// `global_result` (cross-provider view) and `provider_result` (source-scoped
/// view). Every contributing session's modified date is inserted into
/// `unique_dates` for the active-day count. A file that fails to parse logs a
/// warning and is skipped.
///
/// # Errors
///
/// Returns an error only if the candidate-file collector returns one. The
/// current collector skips traversal and metadata errors, and per-file parse
/// failures are logged and skipped rather than propagated.
#[allow(clippy::too_many_arguments)] // per-provider helper; struct-wrapping the args would hurt readability
fn process_usage_directory<P, F>(
    dir: P,
    provider: ExtensionType,
    global_result: &mut UsageResult,
    provider_result: &mut UsageResult,
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
            match parse_session_file_as(&file_info.path, provider, ParseMode::UsageOnly) {
                Ok(analysis) => {
                    let conversation_usage = extract_conversation_usage_from_analysis(&analysis);
                    Some((file_info.modified_date.clone(), conversation_usage))
                }
                Err(e) => {
                    log::warn!("failed to analyze {}: {e}", file_info.path.display());
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
    fold_stored_cost_sessions(
        sessions,
        global_result,
        provider_result,
        stored_costs,
        unique_dates,
    );
    Ok(())
}

/// Reads Cursor's per-model usage (a local estimate from the chat stores) and
/// merges it into both the global and Cursor-scoped maps.
///
/// Mirrors [`process_opencode_usage`]: the estimate carries its own per-model
/// cost, so it uses the same stored-cost path rather than a fuzzy price guess.
fn process_cursor_usage(
    chats_dir: &Path,
    tracking_db: &Path,
    global_result: &mut UsageResult,
    provider_result: &mut UsageResult,
    stored_costs: &mut FastHashMap<String, f64>,
    unique_dates: &mut HashSet<String>,
    time_range: TimeRange,
) -> Result<()> {
    let sessions = read_cursor_usage(chats_dir, tracking_db, time_range)?;
    fold_stored_cost_sessions(
        sessions,
        global_result,
        provider_result,
        stored_costs,
        unique_dates,
    );
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
    fold_stored_cost_sessions(
        sessions,
        global_result,
        provider_result,
        stored_costs,
        unique_dates,
    );
    Ok(())
}

/// Folds `(date, analysis, cost)` rows from a stored-cost provider (OpenCode /
/// Cursor) into the global + provider-scoped maps and the stored-cost table.
fn fold_stored_cost_sessions(
    sessions: Vec<(String, CodeAnalysis, f64)>,
    global_result: &mut UsageResult,
    provider_result: &mut UsageResult,
    stored_costs: &mut FastHashMap<String, f64>,
    unique_dates: &mut HashSet<String>,
) {
    for (date, analysis, session_cost) in sessions {
        unique_dates.insert(date);

        let conversation_usage = extract_conversation_usage_from_analysis(&analysis);
        for (model, usage_value) in conversation_usage {
            *stored_costs.entry(model.clone()).or_insert(0.0) += session_cost;

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

/// How a model's USD cost is resolved.
///
/// Different providers carry different authoritative cost sources, so the cost
/// resolver branches on which one applies.
#[derive(Debug, Clone, Copy)]
pub enum CostSource {
    /// File-based providers: the full LiteLLM lookup (exact → normalized →
    /// substring → fuzzy).
    Litellm,
    /// OpenCode: an **exact** LiteLLM match prices from tokens, otherwise the
    /// stored assistant-message cost is used verbatim. No fuzzy guessing, so a
    /// novel model like `deepseek-v4-pro` reports OpenCode's own cost instead of
    /// being priced against a loosely-similar name.
    OpenCodeStored(f64),
    /// Cursor: the local estimate's own per-model cost, used **verbatim** and
    /// never re-priced from tokens (it is already priced when the estimate is
    /// built), so a merged row can't be re-scored against another provider's
    /// same-named model.
    CursorStored(f64),
    /// Hermes: same basis as [`OpenCodeStored`] — an **exact** LiteLLM match
    /// prices from tokens, otherwise Hermes's own stored cost is used. Hermes
    /// often bills novel models LiteLLM can't price, so its own number is the
    /// safest fallback; the map is kept separate so a colliding bare model name
    /// can't cross-contaminate another provider's cost.
    HermesStored(f64),
}

/// Resolves the USD cost (and optional matched-model annotation) for one model.
///
/// Returns `(cost_usd, matched_model)` where `matched_model` is `Some` only
/// when a non-exact LiteLLM key was used (for display annotation).
pub fn resolve_model_cost(
    model: &str,
    counts: &TokenCounts,
    pricing_map: &ModelPricingMap,
    source: CostSource,
) -> (f64, Option<String>) {
    let priced = |pricing: &crate::pricing::ModelPricing| {
        let token_cost = calculate_cost(
            counts.input_tokens,
            counts.output_tokens,
            counts.reasoning_tokens,
            counts.cache_read,
            counts.cache_creation_5m,
            counts.cache_creation_1h,
            pricing,
        );
        // Web search is billed per query (Claude `server_tool_use`),
        // separately from tokens. `web_search_requests` is 0 for every
        // non-Claude model, so this term is a no-op for them.
        token_cost + counts.web_search_requests as f64 * pricing.web_search_cost_per_query
    };

    match source {
        // Cursor's dashboard cost is authoritative; never re-price from tokens.
        CostSource::CursorStored(stored) => (stored, None),
        // OpenCode / Hermes: only trust an exact price match; otherwise use the
        // provider's own stored cost.
        CostSource::OpenCodeStored(stored) | CostSource::HermesStored(stored) => {
            match pricing_map.get_exact(model) {
                Some(pricing) => (priced(&pricing), None),
                None => (stored, None),
            }
        }
        CostSource::Litellm => {
            let result = pricing_map.get(model);
            (priced(&result.pricing), result.matched_model)
        }
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
}

/// Accumulates the token fields of `new` into `existing` in place.
///
/// Detects the on-disk usage shape from a marker key and merges accordingly:
/// the Claude / Gemini / Copilot shape (keyed by `input_tokens`, including the
/// nested `cache_creation` breakdown) or the Codex shape (keyed by
/// `total_token_usage`). Values that are not both JSON objects, or that match
/// neither shape, are left untouched.
fn merge_usage_values(existing: &mut Value, new: &Value) {
    use crate::utils::{accumulate_i64_fields, accumulate_nested_object, extract_token_counts};

    let (Some(existing_ro), Some(new_ro)) = (existing.as_object(), new.as_object()) else {
        return;
    };
    let existing_flat = existing_ro.contains_key("input_tokens");
    let existing_codex = existing_ro.contains_key("total_token_usage");
    let new_flat = new_ro.contains_key("input_tokens");
    let new_codex = new_ro.contains_key("total_token_usage");

    // Mixed shapes — e.g. a Codex `total_token_usage` row and a Cursor / Copilot
    // flat `input_tokens` row that share a model name like `gpt-5`. The
    // shape-specific branches below only accumulate when both sides carry the
    // *same* shape, so a mismatch would silently drop the other side's tokens.
    // Normalize both to disjoint counts and rewrite `existing` as a flat value
    // that keeps every bucket (and round-trips through `extract_token_counts`).
    if (existing_flat && new_codex) || (existing_codex && new_flat) {
        let merged = add_token_counts(&extract_token_counts(existing), &extract_token_counts(new));
        *existing = token_counts_to_flat_value(&merged);
        return;
    }

    if let (Some(existing_obj), Some(new_obj)) = (existing.as_object_mut(), new.as_object()) {
        // Handle Claude/Gemini/Copilot format (has input_tokens)
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
/// so the result round-trips: re-extracting it yields the same counts. `total`
/// is intentionally omitted (the extractor recomputes it as the bucket sum).
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
    use crate::pricing::{ModelPricing, clear_pricing_cache};
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

    #[test]
    fn test_opencode_exact_match_computes_from_tokens() {
        clear_pricing_cache();
        let map = map_with_gpt4();
        // Exact LiteLLM price exists -> compute from tokens, ignore stored cost.
        let (cost, matched) = resolve_model_cost(
            "gpt-4",
            &counts(1_000_000),
            &map,
            CostSource::OpenCodeStored(99.0),
        );
        assert!((cost - 10.0).abs() < 1e-6); // 1e6 * 1e-5
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
            CostSource::OpenCodeStored(99.0),
        );
        assert!((cost - 99.0).abs() < 1e-9);
        assert!(matched.is_none());
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
            CostSource::CursorStored(3.5),
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
}
