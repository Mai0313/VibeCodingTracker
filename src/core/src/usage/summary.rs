//! Priced, aggregated `usage` summary shared by every frontend.
//!
//! Turns the roll-up's per-model and per-provider usage maps into the priced
//! [`UsageSummary`] (sorted per-model rows, column totals, per-provider
//! totals). Terminal-free business logic, so the display layer only renders
//! the result.

use crate::models::{PerProviderUsage, ProviderActiveDays, UsageResult};
use crate::pricing::CostSource;
use crate::usage::StoredCosts;
use serde_json::Value;
use std::borrow::Cow;

/// Per-provider cost basis, resolved to a [`CostSource`] per model.
#[derive(Clone, Copy)]
enum ProviderPricing<'a> {
    /// File-based providers priced purely from LiteLLM.
    Litellm,
    /// Grok's context-gauge estimate: LiteLLM with an input-rate fallback for
    /// entries that publish no cache-read price.
    GrokGauge,
    /// OpenCode: exact LiteLLM match, else its stored cost.
    OpenCode(&'a crate::constants::FastHashMap<String, f64>),
    /// Cursor local estimate: exact LiteLLM pricing, else zero.
    CursorEstimate,
    /// Hermes: exact LiteLLM match, else its stored cost.
    Hermes(&'a crate::constants::FastHashMap<String, f64>),
}

impl ProviderPricing<'_> {
    /// The [`CostSource`] to use for `model` under this provider's basis.
    fn source_for(&self, model: &str) -> CostSource {
        let stored =
            |m: &crate::constants::FastHashMap<String, f64>| m.get(model).copied().unwrap_or(0.0);
        match self {
            Self::Litellm => CostSource::Litellm,
            Self::GrokGauge => CostSource::GrokGauge,
            Self::OpenCode(m) => CostSource::OpenCodeStored(stored(m)),
            Self::CursorEstimate => CostSource::CursorEstimate,
            Self::Hermes(m) => CostSource::HermesStored(stored(m)),
        }
    }
}

/// One priced per-model row of a [`UsageSummary`].
///
/// `output_tokens` and `reasoning_tokens` are disjoint buckets that both feed
/// `total`, so a display layer showing a single "Output" column wants their
/// sum via [`UsageRow::output_with_reasoning`] for the row to add up.
#[derive(Default, Clone)]
pub struct UsageRow {
    /// Raw model name as reported by the session (the pricing-lookup key).
    pub model: String,
    /// Name shown in the table; appends the fuzzy-matched pricing model in
    /// parentheses when the lookup was not exact.
    pub display_model: String,
    /// Prompt (input) tokens.
    pub input_tokens: i64,
    /// User-visible response tokens, excluding reasoning.
    pub output_tokens: i64,
    /// Separately-billed "thinking" tokens.
    pub reasoning_tokens: i64,
    /// Tokens served from the prompt cache.
    pub cache_read: i64,
    /// Tokens written to the prompt cache.
    pub cache_creation: i64,
    /// Sum of all token buckets for this model.
    pub total: i64,
    /// LiteLLM-priced cost in USD for this model's tokens.
    pub cost: f64,
}

impl UsageRow {
    /// Sum of this row's output and reasoning tokens.
    #[inline]
    pub fn output_with_reasoning(&self) -> i64 {
        self.output_tokens + self.reasoning_tokens
    }
}

/// Column-wise totals across every [`UsageRow`] in a summary.
#[derive(Default)]
pub struct UsageTotals {
    /// Summed prompt (input) tokens.
    pub input_tokens: i64,
    /// Summed response tokens, excluding reasoning.
    pub output_tokens: i64,
    /// Summed reasoning ("thinking") tokens.
    pub reasoning_tokens: i64,
    /// Summed cache-read tokens.
    pub cache_read: i64,
    /// Summed cache-creation tokens.
    pub cache_creation: i64,
    /// Summed total tokens.
    pub total: i64,
    /// Summed cost in USD.
    pub cost: f64,
}

impl UsageTotals {
    /// Adds every token bucket and the cost of `row` into these totals.
    pub fn accumulate(&mut self, row: &UsageRow) {
        self.input_tokens += row.input_tokens;
        self.output_tokens += row.output_tokens;
        self.reasoning_tokens += row.reasoning_tokens;
        self.cache_read += row.cache_read;
        self.cache_creation += row.cache_creation;
        self.total += row.total;
        self.cost += row.cost;
    }

    /// Same helper as [`UsageRow::output_with_reasoning`] for totals.
    #[inline]
    pub fn output_with_reasoning(&self) -> i64 {
        self.output_tokens + self.reasoning_tokens
    }
}

/// Per-provider totals for usage.
///
/// `days_count` doubles as the presence predicate: the display layer renders a
/// provider's row only while it is nonzero.
#[derive(Default, Clone)]
pub struct ProviderStats {
    /// Total tokens attributed to this provider.
    pub total_tokens: i64,
    /// Total cost in USD attributed to this provider.
    pub total_cost: f64,
    /// Number of distinct days that contributed to these totals.
    pub days_count: usize,
}

impl ProviderStats {
    /// Adds `row`'s total tokens and cost into these stats.
    fn accumulate_row(&mut self, row: &UsageRow) {
        self.total_tokens += row.total;
        self.total_cost += row.cost;
    }
}

/// Per-provider usage totals, including the "All Providers" bucket.
pub type UsageProviderTotals = crate::models::ProviderTotals<ProviderStats>;

/// Fully priced usage view shared by every output mode.
#[derive(Default)]
pub struct UsageSummary {
    /// Per-model rows, sorted by ascending cost (model name as tie-break).
    pub rows: Vec<UsageRow>,
    /// Column-wise totals across all rows.
    pub totals: UsageTotals,
    /// Per-provider totals for the summary footer.
    pub provider_totals: UsageProviderTotals,
}

/// Calculates per-provider totals using **source-directory** attribution.
///
/// Tokens come from the `per_provider` map, which `usage::aggregator` fills
/// per source directory (or source database), so the provider assignment is
/// exact regardless of what model name the session happens to carry.
pub fn calculate_provider_totals_from_per_provider(
    per_provider: &PerProviderUsage,
    provider_days: &ProviderActiveDays,
    pricing_map: &crate::pricing::ModelPricingMap,
    stored_costs: &StoredCosts,
) -> UsageProviderTotals {
    let mut totals = UsageProviderTotals::default();

    totals.claude.days_count = provider_days.claude;
    totals.codex.days_count = provider_days.codex;
    totals.copilot.days_count = provider_days.copilot;
    totals.gemini.days_count = provider_days.gemini;
    totals.grok.days_count = provider_days.grok;
    totals.deepseek.days_count = provider_days.deepseek;
    totals.opencode.days_count = provider_days.opencode;
    totals.cursor.days_count = provider_days.cursor;
    totals.hermes.days_count = provider_days.hermes;
    totals.overall.days_count = provider_days.total;

    accumulate_provider(
        &mut totals.claude,
        &per_provider.claude,
        pricing_map,
        ProviderPricing::Litellm,
    );
    accumulate_provider(
        &mut totals.codex,
        &per_provider.codex,
        pricing_map,
        ProviderPricing::Litellm,
    );
    accumulate_provider(
        &mut totals.copilot,
        &per_provider.copilot,
        pricing_map,
        ProviderPricing::Litellm,
    );
    accumulate_provider(
        &mut totals.gemini,
        &per_provider.gemini,
        pricing_map,
        ProviderPricing::Litellm,
    );
    accumulate_provider(
        &mut totals.grok,
        &per_provider.grok,
        pricing_map,
        ProviderPricing::GrokGauge,
    );
    accumulate_provider(
        &mut totals.deepseek,
        &per_provider.deepseek,
        pricing_map,
        ProviderPricing::Litellm,
    );
    accumulate_provider(
        &mut totals.opencode,
        &per_provider.opencode,
        pricing_map,
        ProviderPricing::OpenCode(&stored_costs.opencode),
    );
    accumulate_provider(
        &mut totals.cursor,
        &per_provider.cursor,
        pricing_map,
        ProviderPricing::CursorEstimate,
    );
    accumulate_provider(
        &mut totals.hermes,
        &per_provider.hermes,
        pricing_map,
        ProviderPricing::Hermes(&stored_costs.hermes),
    );

    // The "All Providers" row sums the already-priced per-provider stats, so it
    // stays consistent with the rows above. Re-pricing the cross-provider merged
    // `UsageData.models` map instead would total the same tokens but has no
    // provider attribution left to price each portion on its own basis.
    totals.overall.total_tokens = totals.claude.total_tokens
        + totals.codex.total_tokens
        + totals.copilot.total_tokens
        + totals.gemini.total_tokens
        + totals.grok.total_tokens
        + totals.deepseek.total_tokens
        + totals.opencode.total_tokens
        + totals.cursor.total_tokens
        + totals.hermes.total_tokens;
    totals.overall.total_cost = totals.claude.total_cost
        + totals.codex.total_cost
        + totals.copilot.total_cost
        + totals.gemini.total_cost
        + totals.grok.total_cost
        + totals.deepseek.total_cost
        + totals.opencode.total_cost
        + totals.cursor.total_cost
        + totals.hermes.total_cost;

    totals
}

/// Prices every model in `usage` under `pricing`'s cost basis and folds the
/// results into `stats`.
fn accumulate_provider(
    stats: &mut ProviderStats,
    usage: &UsageResult,
    pricing_map: &crate::pricing::ModelPricingMap,
    pricing: ProviderPricing,
) {
    for (model, raw_usage) in usage {
        let row = extract_usage_row(model, raw_usage, pricing_map, pricing.source_for(model));
        stats.accumulate_row(&row);
    }
}

/// Builds the priced summary from a roll-up's usage maps.
///
/// `usage_data` is the cross-provider merged map and supplies the per-model
/// rows; `per_provider` is the source-directory-scoped map and supplies both
/// the per-provider totals and each merged row's cost, so Copilot-billed
/// Claude tokens keep Copilot's cost basis even where they share a row with
/// Claude Code. A model absent from every per-provider map falls back to a
/// plain LiteLLM price. An empty `usage_data` returns the default summary,
/// dropping the day counts `provider_days` carried.
pub fn build_usage_summary(
    usage_data: &UsageResult,
    per_provider: &PerProviderUsage,
    provider_days: &ProviderActiveDays,
    pricing_map: &crate::pricing::ModelPricingMap,
    stored_costs: &StoredCosts,
) -> UsageSummary {
    if usage_data.is_empty() {
        return UsageSummary::default();
    }

    let mut summary = UsageSummary::default();

    summary.rows.reserve(usage_data.len());

    for (model, usage) in usage_data.iter() {
        let (cost, matched_model) =
            crate::usage::resolve_merged_model_cost(model, per_provider, pricing_map, stored_costs)
                .unwrap_or_else(|| price_usage(model, usage, pricing_map, CostSource::Litellm));
        let row = build_usage_row(model, usage, cost, matched_model);
        summary.rows.push(row);
    }

    // Ascending cost puts the expensive models at the bottom of the table. The
    // model-name tie-break is what keeps equal-cost rows in a stable order:
    // `usage_data` is a hash map, so its iteration order is not reproducible.
    summary.rows.sort_by(|a, b| {
        a.cost
            .partial_cmp(&b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model.cmp(&b.model))
    });

    for row in &summary.rows {
        summary.totals.accumulate(row);
    }

    summary.provider_totals = calculate_provider_totals_from_per_provider(
        per_provider,
        provider_days,
        pricing_map,
        stored_costs,
    );
    summary
}

/// Builds one priced [`UsageRow`] from a model's raw usage `Value`, pricing it
/// on the basis `source` names.
///
/// A non-exact LiteLLM match appends the matched model name to `display_model`
/// in parentheses.
fn extract_usage_row(
    model: &str,
    usage: &Value,
    pricing_map: &crate::pricing::ModelPricingMap,
    source: CostSource,
) -> UsageRow {
    use crate::pricing::resolve_model_cost;
    use crate::utils::extract_token_counts;

    let counts = extract_token_counts(usage);
    let (cost, matched_model) = resolve_model_cost(model, &counts, pricing_map, source);
    build_usage_row_from_counts(model, &counts, cost, matched_model)
}

/// Prices one raw usage value under `source`.
fn price_usage(
    model: &str,
    usage: &Value,
    pricing_map: &crate::pricing::ModelPricingMap,
    source: CostSource,
) -> (f64, Option<String>) {
    use crate::pricing::resolve_model_cost;
    use crate::utils::extract_token_counts;

    let counts = extract_token_counts(usage);
    resolve_model_cost(model, &counts, pricing_map, source)
}

/// Builds one display row using an already-resolved cost.
fn build_usage_row(
    model: &str,
    usage: &Value,
    cost: f64,
    matched_model: Option<String>,
) -> UsageRow {
    let counts = crate::utils::extract_token_counts(usage);
    build_usage_row_from_counts(model, &counts, cost, matched_model)
}

/// Builds one display row from already-extracted token counts and a resolved
/// cost, so callers that already have the counts do not re-walk the usage value.
fn build_usage_row_from_counts(
    model: &str,
    counts: &crate::utils::TokenCounts,
    cost: f64,
    matched_model: Option<String>,
) -> UsageRow {
    let display_model = if let Some(matched) = &matched_model {
        Cow::Owned(format!("{} ({})", model, matched))
    } else {
        Cow::Borrowed(model)
    };

    UsageRow {
        model: model.to_string(),
        display_model: display_model.into_owned(),
        input_tokens: counts.input_tokens,
        output_tokens: counts.output_tokens,
        reasoning_tokens: counts.reasoning_tokens,
        cache_read: counts.cache_read,
        cache_creation: counts.cache_creation,
        total: counts.total,
        cost,
    }
}

/// Returns the base model key used to merge rows across provider-routing
/// prefixes: everything after the first `/`, or the whole name when there is no
/// `/`. Unlike [`normalize_model_name`](crate::pricing::normalize_model_name)
/// this does **not** strip version/date suffixes, so `gpt-5.5` and `gpt-5.4`
/// stay distinct.
fn base_model_key(model: &str) -> &str {
    model.split_once('/').map(|(_, rest)| rest).unwrap_or(model)
}

/// Collapses rows by [`base_model_key`], summing every token bucket and the
/// already-resolved cost, and labels each merged row with the bare base name.
///
/// Costs are summed verbatim rather than re-derived: each input row was already
/// priced against its full model name, so a merged `gpt-5.5` adds the
/// differently-priced `openai/gpt-5.5`, `azure/gpt-5.5` and bare `gpt-5.5`
/// pieces. The prefix is dropped even from a model with no duplicate
/// (`opencode/big-pickle` -> `big-pickle`), which also discards any
/// fuzzy-match annotation the row's `display_model` carried. The result is
/// re-sorted by ascending cost, tie-broken by model name, matching
/// [`build_usage_summary`].
pub fn merge_rows_by_base_model(rows: &[UsageRow]) -> Vec<UsageRow> {
    use std::collections::HashMap;

    let mut groups: HashMap<&str, Vec<&UsageRow>> = HashMap::new();
    for row in rows {
        groups
            .entry(base_model_key(&row.model))
            .or_default()
            .push(row);
    }

    let mut merged: Vec<UsageRow> = Vec::with_capacity(groups.len());
    for (key, members) in groups {
        let mut acc = UsageRow {
            model: key.to_string(),
            display_model: key.to_string(),
            ..UsageRow::default()
        };
        for m in members {
            acc.input_tokens += m.input_tokens;
            acc.output_tokens += m.output_tokens;
            acc.reasoning_tokens += m.reasoning_tokens;
            acc.cache_read += m.cache_read;
            acc.cache_creation += m.cache_creation;
            acc.total += m.total;
            acc.cost += m.cost;
        }
        merged.push(acc);
    }

    merged.sort_by(|a, b| {
        a.cost
            .partial_cmp(&b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model.cmp(&b.model))
    });
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::{ModelPricing, ModelPricingMap, clear_pricing_cache};
    use serde_json::json;

    #[test]
    fn merged_rows_include_grok_source_cost() {
        clear_pricing_cache();
        let mut raw_pricing = std::collections::HashMap::new();
        raw_pricing.insert(
            "shared-model".to_string(),
            ModelPricing {
                input_cost_per_token: 0.01,
                ..Default::default()
            },
        );
        let pricing_map = ModelPricingMap::new(raw_pricing);
        let mut usage_data = UsageResult::default();
        usage_data.insert("shared-model".to_string(), json!({"input_tokens": 200}));
        let mut per_provider = PerProviderUsage::default();
        per_provider
            .claude
            .insert("shared-model".to_string(), json!({"input_tokens": 100}));
        per_provider
            .grok
            .insert("shared-model".to_string(), json!({"input_tokens": 100}));

        let summary = build_usage_summary(
            &usage_data,
            &per_provider,
            &ProviderActiveDays::default(),
            &pricing_map,
            &StoredCosts::default(),
        );

        assert!((summary.rows[0].cost - 2.0).abs() < 1e-9);
    }

    #[test]
    fn merged_rows_price_opencode_fallback_only_for_opencode_tokens() {
        clear_pricing_cache();

        let mut raw_pricing = std::collections::HashMap::new();
        raw_pricing.insert(
            "shared".to_string(),
            ModelPricing {
                input_cost_per_token: 0.01,
                ..Default::default()
            },
        );
        let pricing_map = ModelPricingMap::new(raw_pricing);

        let mut usage_data = UsageResult::default();
        usage_data.insert("shared-pro".to_string(), json!({"input_tokens": 200}));

        let mut per_provider = PerProviderUsage::default();
        per_provider
            .claude
            .insert("shared-pro".to_string(), json!({"input_tokens": 100}));
        per_provider
            .opencode
            .insert("shared-pro".to_string(), json!({"input_tokens": 100}));

        let mut stored_costs = StoredCosts::default();
        stored_costs.opencode.insert("shared-pro".to_string(), 7.0);

        let summary = build_usage_summary(
            &usage_data,
            &per_provider,
            &ProviderActiveDays::default(),
            &pricing_map,
            &stored_costs,
        );

        assert_eq!(summary.rows.len(), 1);
        assert!((summary.rows[0].cost - 8.0).abs() < 1e-9);
        assert_eq!(summary.rows[0].display_model, "shared-pro (shared)");
    }

    #[test]
    fn cursor_row_uses_exact_litellm_price() {
        clear_pricing_cache();

        // An exact LiteLLM price exists for the model Cursor reports.
        let mut raw_pricing = std::collections::HashMap::new();
        raw_pricing.insert(
            "gemini-2.5-pro".to_string(),
            ModelPricing {
                input_cost_per_token: 0.01,
                ..Default::default()
            },
        );
        let pricing_map = ModelPricingMap::new(raw_pricing);

        let mut usage_data = UsageResult::default();
        usage_data.insert("gemini-2.5-pro".to_string(), json!({"input_tokens": 1000}));

        let mut per_provider = PerProviderUsage::default();
        per_provider
            .cursor
            .insert("gemini-2.5-pro".to_string(), json!({"input_tokens": 1000}));

        let summary = build_usage_summary(
            &usage_data,
            &per_provider,
            &ProviderActiveDays::default(),
            &pricing_map,
            &StoredCosts::default(),
        );

        assert_eq!(summary.rows.len(), 1);
        assert!((summary.rows[0].cost - 10.0).abs() < 1e-9);
    }

    #[test]
    fn stored_cost_prices_only_its_own_providers_share_of_a_merged_row() {
        clear_pricing_cache();
        // Empty pricing: OpenCode falls back to its stored cost while Cursor's
        // local estimate stays unpriced.
        let pricing_map = ModelPricingMap::new(std::collections::HashMap::new());

        // The same bare model name appears under both OpenCode and Cursor.
        let mut usage_data = UsageResult::default();
        usage_data.insert("collide".to_string(), json!({"input_tokens": 10}));

        let mut per_provider = PerProviderUsage::default();
        per_provider
            .opencode
            .insert("collide".to_string(), json!({"input_tokens": 5}));
        per_provider
            .cursor
            .insert("collide".to_string(), json!({"input_tokens": 5}));

        let mut stored_costs = StoredCosts::default();
        stored_costs.opencode.insert("collide".to_string(), 5.0);

        let summary = build_usage_summary(
            &usage_data,
            &per_provider,
            &ProviderActiveDays::default(),
            &pricing_map,
            &stored_costs,
        );

        assert_eq!(summary.rows.len(), 1);
        assert!((summary.rows[0].cost - 5.0).abs() < 1e-9);
        assert!((summary.provider_totals.opencode.total_cost - 5.0).abs() < 1e-9);
        assert!(summary.provider_totals.cursor.total_cost.abs() < 1e-9);
    }

    fn row(model: &str, input: i64, total: i64, cost: f64) -> UsageRow {
        UsageRow {
            model: model.to_string(),
            display_model: model.to_string(),
            input_tokens: input,
            total,
            cost,
            ..UsageRow::default()
        }
    }

    #[test]
    fn merge_collapses_prefixed_and_bare_names_and_sums() {
        let rows = vec![
            row("openai/gpt-5.5", 100, 100, 0.20),
            row("azure/gpt-5.5", 200, 200, 3.00),
            row("gpt-5.5", 300, 300, 5.00),
        ];

        let merged = merge_rows_by_base_model(&rows);

        assert_eq!(merged.len(), 1);
        let m = &merged[0];
        assert_eq!(m.model, "gpt-5.5");
        assert_eq!(m.display_model, "gpt-5.5");
        assert_eq!(m.input_tokens, 600);
        assert_eq!(m.total, 600);
        assert!((m.cost - 8.20).abs() < 1e-9);
    }

    #[test]
    fn merge_keeps_different_versions_apart() {
        // The key check: the 5.4 tokens never fold into the merged 5.5 row.
        let rows = vec![
            row("openai/gpt-5.5", 10, 10, 1.0),
            row("azure/gpt-5.5", 30, 30, 3.0),
            row("openai/gpt-5.4", 20, 20, 2.0),
        ];

        let merged = merge_rows_by_base_model(&rows);

        assert_eq!(merged.len(), 2);
        let five_five = merged.iter().find(|r| r.model == "gpt-5.5").unwrap();
        assert_eq!(five_five.display_model, "gpt-5.5");
        assert_eq!(five_five.total, 40);
        let five_four = merged.iter().find(|r| r.model == "gpt-5.4").unwrap();
        assert_eq!(five_four.display_model, "gpt-5.4");
        assert_eq!(five_four.total, 20);
    }

    #[test]
    fn merge_strips_prefix_from_single_row() {
        // Even a model with no duplicate drops its provider prefix (and any
        // fuzzy-match hint) so the merged view reads uniformly.
        let mut only = row("deepseek/deepseek-v4-pro", 5, 5, 1.5);
        only.display_model = "deepseek/deepseek-v4-pro (deepseek-v4)".to_string();

        let merged = merge_rows_by_base_model(std::slice::from_ref(&only));

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].model, "deepseek-v4-pro");
        assert_eq!(merged[0].display_model, "deepseek-v4-pro");
        assert_eq!(merged[0].total, 5);
    }

    #[test]
    fn merge_only_strips_first_slash_segment() {
        assert_eq!(base_model_key("a/b/c"), "b/c");
        assert_eq!(base_model_key("gpt-5.5"), "gpt-5.5");
        assert_eq!(base_model_key("openai/gpt-5.5"), "gpt-5.5");
    }

    #[test]
    fn merge_reorders_by_ascending_cost() {
        let rows = vec![
            row("openai/gpt-5.5", 1, 1, 9.0),
            row("azure/gpt-5.5", 1, 1, 9.0),
            row("cheap-model", 1, 1, 0.01),
        ];

        let merged = merge_rows_by_base_model(&rows);

        assert_eq!(merged.len(), 2);
        // cheap-model (0.01) sorts before the merged gpt-5.5 row (18.0).
        assert_eq!(merged[0].model, "cheap-model");
        assert_eq!(merged[1].model, "gpt-5.5");
        assert!((merged[1].cost - 18.0).abs() < 1e-9);
    }
}
