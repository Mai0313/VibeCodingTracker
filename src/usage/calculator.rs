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
use crate::constants::{FastHashMap, capacity};
use crate::models::{
    CodeAnalysis, ExtensionType, PerProviderUsage, Provider, ProviderActiveDays, UsageResult,
};
use crate::pricing::{ModelPricingMap, calculate_cost};
use crate::session::{ParseMode, parse_session_file_as, read_opencode_usage};
use crate::utils::{
    COPILOT_SESSION_MAX_DEPTH, TokenCounts, collect_files_with_max_depth, is_claude_session_file,
    is_codex_session_file, is_copilot_session_file, is_gemini_session_file, resolve_paths,
};
use anyhow::Result;
use rayon::prelude::*;
use serde_json::Value;
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
    /// OpenCode's own per-model cost (USD), summed from assistant messages.
    ///
    /// OpenCode records authoritative assistant-message costs, so when a model
    /// has no exact LiteLLM price we display this stored cost instead of
    /// guessing from a fuzzy match. Keyed by model name; only OpenCode models
    /// appear.
    pub opencode_costs: FastHashMap<String, f64>,
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
    let paths = resolve_paths()?;
    let mut result = FastHashMap::with_capacity(capacity::MODEL_COMBINATIONS);
    let mut per_provider = PerProviderUsage::default();
    let mut opencode_costs: FastHashMap<String, f64> = FastHashMap::default();

    let mut claude_dates: HashSet<String> = HashSet::new();
    let mut codex_dates: HashSet<String> = HashSet::new();
    let mut copilot_dates: HashSet<String> = HashSet::new();
    let mut gemini_dates: HashSet<String> = HashSet::new();
    let mut opencode_dates: HashSet<String> = HashSet::new();

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
            None,
        )?;
    }

    if paths.codex_session_dir.exists() {
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

    if paths.copilot_session_dir.exists() {
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

    if paths.gemini_session_dir.exists() {
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
    if paths.opencode_db.exists()
        && let Err(err) = process_opencode_usage(
            &paths.opencode_db,
            &mut result,
            &mut per_provider.opencode,
            &mut opencode_costs,
            &mut opencode_dates,
            time_range,
        )
    {
        eprintln!(
            "Warning: Failed to read OpenCode DB {}: {err}",
            paths.opencode_db.display()
        );
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

    Ok(UsageData {
        models: result,
        per_provider,
        provider_days,
        opencode_costs,
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
    opencode_costs: &mut FastHashMap<String, f64>,
    unique_dates: &mut HashSet<String>,
    time_range: TimeRange,
) -> Result<()> {
    let sessions = read_opencode_usage(db_path, time_range)?;

    for (date, analysis, session_cost) in sessions {
        unique_dates.insert(date);

        let conversation_usage = extract_conversation_usage_from_analysis(&analysis);
        for (model, usage_value) in conversation_usage {
            *opencode_costs.entry(model.clone()).or_insert(0.0) += session_cost;

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

/// Resolves the USD cost (and optional matched-model annotation) for one model.
///
/// Behavior depends on `opencode_cost`:
/// - `None` (every provider except OpenCode): the existing LiteLLM lookup,
///   which includes exact, normalized, substring, and fuzzy matching.
/// - `Some(stored)` (OpenCode): only an **exact** LiteLLM match is computed
///   from tokens; otherwise the model's stored OpenCode cost is used verbatim.
///   No fuzzy/normalized guessing happens, so a novel model like
///   `deepseek-v4-pro` reports OpenCode's own cost instead of being priced
///   against a loosely-similar model.
///
/// Returns `(cost_usd, matched_model)` where `matched_model` is `Some` only
/// when a non-exact LiteLLM key was used (for display annotation).
pub fn resolve_model_cost(
    model: &str,
    counts: &TokenCounts,
    pricing_map: &ModelPricingMap,
    opencode_cost: Option<f64>,
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

    if let Some(stored) = opencode_cost {
        // OpenCode: only trust an exact price match; otherwise use its own cost.
        return match pricing_map.get_exact(model) {
            Some(pricing) => (priced(&pricing), None),
            None => (stored, None),
        };
    }

    let result = pricing_map.get(model);
    (priced(&result.pricing), result.matched_model)
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
    use crate::utils::{accumulate_i64_fields, accumulate_nested_object};

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
        let (cost, matched) = resolve_model_cost("gpt-4", &counts(1_000_000), &map, Some(99.0));
        assert!((cost - 10.0).abs() < 1e-6); // 1e6 * 1e-5
        assert!(matched.is_none());
    }

    #[test]
    fn test_opencode_no_exact_match_uses_stored_cost() {
        clear_pricing_cache();
        let map = map_with_gpt4();
        // No exact price; OpenCode must NOT fuzzy match -> use stored cost.
        let (cost, matched) =
            resolve_model_cost("deepseek-v4-pro", &counts(1_000_000), &map, Some(99.0));
        assert!((cost - 99.0).abs() < 1e-9);
        assert!(matched.is_none());
    }

    #[test]
    fn test_non_opencode_keeps_existing_lookup() {
        clear_pricing_cache();
        let map = map_with_gpt4();
        // Non-OpenCode path is unchanged: exact match still computes.
        let (cost, matched) = resolve_model_cost("gpt-4", &counts(1_000_000), &map, None);
        assert!((cost - 10.0).abs() < 1e-6);
        assert!(matched.is_none());
    }
}
