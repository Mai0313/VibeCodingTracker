//! Priced, aggregated `usage` summaries.
//!
//! Builds [`UsageSummary`] (sorted per-model rows, column totals, and per-provider totals) from aggregated usage maps and pricing data.

use crate::models::{PerProviderUsage, Provider, ProviderActiveDays, UsageResult};
use crate::pricing::CostSource;
use crate::usage::StoredCosts;
use serde_json::Value;

/// Per-provider cost basis, resolved to a [`CostSource`] per model.
#[derive(Clone, Copy)]
enum ProviderPricing<'a> {
    /// File-based providers priced purely from LiteLLM.
    Litellm,
    /// Grok's context-gauge estimate: LiteLLM with an input-rate fallback when cache-read pricing is absent.
    GrokGauge,
    /// OpenCode: exact or official normalized LiteLLM match, else its stored cost.
    OpenCode(&'a crate::constants::FastHashMap<String, f64>),
    /// Cursor local estimate: exact LiteLLM pricing, else zero.
    CursorEstimate,
    /// Hermes: exact or official normalized LiteLLM match, else its stored cost.
    Hermes(&'a crate::constants::FastHashMap<String, f64>),
}

impl ProviderPricing<'_> {
    /// Returns the [`CostSource`] for `model` under this provider's basis.
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

/// One provider's contribution to a [`UsageRow`].
#[derive(Debug, Clone, PartialEq)]
pub struct ModelProviderUsage {
    /// Provider that produced these tokens.
    pub provider: Provider,
    /// Total tokens attributed to this provider for this model.
    pub total_tokens: i64,
    /// Resolved USD cost for this provider's portion of the model.
    pub cost: f64,
}

/// One priced per-model row of a [`UsageSummary`].
///
/// `output_tokens` and `reasoning_tokens` are disjoint buckets that both contribute to `total`; use [`UsageRow::output_with_reasoning`] for their combined count.
#[derive(Default, Clone)]
pub struct UsageRow {
    /// Raw model name as reported by the session (the pricing-lookup key).
    pub model: String,
    /// LiteLLM key used to price this row when it differed from `model`; `None` on an exact match, when unpriced, or when multiple variants merged.
    pub matched_model: Option<String>,
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
    /// Cache-write tokens at the default 5-minute TTL.
    pub cache_creation_5m: i64,
    /// Cache-write tokens at the extended 1-hour TTL.
    pub cache_creation_1h: i64,
    /// Provider-reported tool payload tokens (Gemini `tool_tokens`).
    pub tool_tokens: i64,
    /// Subset of tokens billed at elevated long-context price levels.
    pub above_tier_tokens: i64,
    /// Server-side web-search queries billed separately from tokens.
    pub web_search_requests: i64,
    /// Sum of all token buckets for this model.
    pub total: i64,
    /// Resolved cost in USD for this model's tokens.
    pub cost: f64,
    /// Per-provider breakdown for this model, sorted by descending cost.
    pub providers: Vec<ModelProviderUsage>,
    /// Raw model names folded into this row by [`merge_rows_by_base_model`], populated when multiple variants merged or a prefix was stripped.
    pub variants: Vec<String>,
}

impl UsageRow {
    /// Returns the sum of this row's output and reasoning tokens.
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

    /// Returns the sum of `output_tokens` and `reasoning_tokens`.
    #[inline]
    pub fn output_with_reasoning(&self) -> i64 {
        self.output_tokens + self.reasoning_tokens
    }
}

/// Per-provider totals for usage.
///
/// A nonzero `days_count` indicates that the provider had recorded activity.
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

/// Returns per-provider token and cost totals using source-scoped attribution from `per_provider`.
///
/// Each provider bucket is priced under its own cost basis regardless of shared model names across providers.
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
    totals.antigravity.days_count = provider_days.antigravity;
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
        &mut totals.antigravity,
        &per_provider.antigravity,
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

    // Sum the already-priced per-provider stats so "All Providers" reflects each provider's cost basis and stays equal to Σ providers.
    totals.overall.total_tokens = totals.claude.total_tokens
        + totals.codex.total_tokens
        + totals.copilot.total_tokens
        + totals.gemini.total_tokens
        + totals.grok.total_tokens
        + totals.deepseek.total_tokens
        + totals.antigravity.total_tokens
        + totals.opencode.total_tokens
        + totals.cursor.total_tokens
        + totals.hermes.total_tokens;
    totals.overall.total_cost = totals.claude.total_cost
        + totals.codex.total_cost
        + totals.copilot.total_cost
        + totals.gemini.total_cost
        + totals.grok.total_cost
        + totals.deepseek.total_cost
        + totals.antigravity.total_cost
        + totals.opencode.total_cost
        + totals.cursor.total_cost
        + totals.hermes.total_cost;

    totals
}

/// Prices every model in `usage` under `pricing`'s cost basis and folds the results into `stats`.
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

/// Builds a priced [`UsageSummary`] from merged and per-provider usage maps.
///
/// `usage_data` supplies the per-model rows, while `per_provider` supplies per-provider totals and per-source cost resolution (falling back to plain LiteLLM pricing if a model appears in no provider bucket). Returns [`UsageSummary::default()`] when `usage_data` is empty, discarding `provider_days`.
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
        let (cost, matched_model, providers) = match crate::usage::resolve_merged_model_breakdown(
            model,
            per_provider,
            pricing_map,
            stored_costs,
        ) {
            Some(resolved) => (resolved.cost, resolved.matched_model, resolved.providers),
            None => {
                let (cost, matched) = price_usage(model, usage, pricing_map, CostSource::Litellm);
                (cost, matched, Vec::new())
            }
        };
        let mut row = build_usage_row(model, usage, cost, matched_model);
        row.providers = providers;
        summary.rows.push(row);
    }

    // Sort by ascending cost with a model-name tie-break so hash-map iteration order cannot reorder equal-cost rows.
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

/// Builds one priced [`UsageRow`] from `usage` under `source`.
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

/// Builds a [`UsageRow`] from pre-extracted [`TokenCounts`](crate::utils::TokenCounts) and a resolved cost.
fn build_usage_row_from_counts(
    model: &str,
    counts: &crate::utils::TokenCounts,
    cost: f64,
    matched_model: Option<String>,
) -> UsageRow {
    let above_tier_tokens = counts
        .above_tiers
        .iter()
        .map(|s| {
            s.input_tokens
                + s.output_tokens
                + s.reasoning_tokens
                + s.cache_read
                + s.cache_creation_5m
                + s.cache_creation_1h
        })
        .sum();
    UsageRow {
        model: model.to_string(),
        matched_model,
        input_tokens: counts.input_tokens,
        output_tokens: counts.output_tokens,
        reasoning_tokens: counts.reasoning_tokens,
        cache_read: counts.cache_read,
        cache_creation: counts.cache_creation,
        cache_creation_5m: counts.cache_creation_5m,
        cache_creation_1h: counts.cache_creation_1h,
        tool_tokens: counts.tool_tokens,
        above_tier_tokens,
        web_search_requests: counts.web_search_requests,
        total: counts.total,
        cost,
        providers: Vec::new(),
        variants: Vec::new(),
    }
}

/// Returns the slice after the first `/`, or `model` unchanged when no `/` is present, preserving version and date suffixes.
pub(crate) fn base_model_key(model: &str) -> &str {
    model.split_once('/').map(|(_, rest)| rest).unwrap_or(model)
}

/// Merges rows that share a base model name (after stripping any leading `<prefix>/` segment), summing token buckets and pre-resolved costs.
///
/// Costs are summed verbatim rather than re-priced. Per-provider contributions are folded by provider, and [`UsageRow::variants`] records the contributing raw model names whenever multiple variants merged or a prefix was stripped. A single stripped row retains [`UsageRow::matched_model`] only when it still differs from the base name. Output is sorted by ascending cost, then model name.
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
    for (key, mut members) in groups {
        members.sort_by(|a, b| {
            b.cost
                .partial_cmp(&a.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.model.cmp(&b.model))
        });

        let matched_model = if members.len() == 1 {
            members[0]
                .matched_model
                .as_ref()
                .filter(|matched| matched.as_str() != key)
                .cloned()
        } else {
            None
        };

        let variants = if members.len() > 1 || members.first().is_some_and(|m| m.model != key) {
            members.iter().map(|m| m.model.clone()).collect()
        } else {
            Vec::new()
        };

        let mut provider_acc: HashMap<Provider, (i64, f64)> = HashMap::new();
        let mut acc = UsageRow {
            model: key.to_string(),
            matched_model,
            variants,
            ..UsageRow::default()
        };
        for m in members {
            acc.input_tokens += m.input_tokens;
            acc.output_tokens += m.output_tokens;
            acc.reasoning_tokens += m.reasoning_tokens;
            acc.cache_read += m.cache_read;
            acc.cache_creation += m.cache_creation;
            acc.cache_creation_5m += m.cache_creation_5m;
            acc.cache_creation_1h += m.cache_creation_1h;
            acc.tool_tokens += m.tool_tokens;
            acc.above_tier_tokens += m.above_tier_tokens;
            acc.web_search_requests += m.web_search_requests;
            acc.total += m.total;
            acc.cost += m.cost;
            for p in &m.providers {
                let entry = provider_acc.entry(p.provider).or_insert((0, 0.0));
                entry.0 += p.total_tokens;
                entry.1 += p.cost;
            }
        }
        let mut providers: Vec<ModelProviderUsage> = provider_acc
            .into_iter()
            .map(|(provider, (total_tokens, cost))| ModelProviderUsage {
                provider,
                total_tokens,
                cost,
            })
            .collect();
        providers.sort_by(|a, b| {
            b.cost
                .partial_cmp(&a.cost)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.total_tokens.cmp(&a.total_tokens))
                .then_with(|| a.provider.display_name().cmp(b.provider.display_name()))
        });
        acc.providers = providers;
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
        assert_eq!(summary.rows[0].model, "shared-pro");
        assert_eq!(summary.rows[0].matched_model.as_deref(), Some("shared"));
    }

    #[test]
    fn cursor_row_uses_exact_litellm_price() {
        clear_pricing_cache();

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
        // Empty pricing: OpenCode falls back to its stored cost while Cursor's local estimate stays unpriced.
        let pricing_map = ModelPricingMap::new(std::collections::HashMap::new());

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
            input_tokens: input,
            total,
            cost,
            ..UsageRow::default()
        }
    }

    #[test]
    fn merge_collapses_prefixed_and_bare_names_and_sums() {
        let mut r1 = row("openai/gpt-5.5", 100, 100, 0.20);
        r1.providers = vec![ModelProviderUsage {
            provider: Provider::OpenCode,
            total_tokens: 100,
            cost: 0.20,
        }];
        let mut r2 = row("azure/gpt-5.5", 200, 200, 3.00);
        r2.providers = vec![ModelProviderUsage {
            provider: Provider::OpenCode,
            total_tokens: 200,
            cost: 3.00,
        }];
        let mut r3 = row("gpt-5.5", 300, 300, 5.00);
        r3.providers = vec![ModelProviderUsage {
            provider: Provider::Codex,
            total_tokens: 300,
            cost: 5.00,
        }];
        let rows = vec![r1, r2, r3];

        let merged = merge_rows_by_base_model(&rows);

        assert_eq!(merged.len(), 1);
        let m = &merged[0];
        assert_eq!(m.model, "gpt-5.5");
        assert_eq!(m.matched_model, None);
        assert_eq!(m.input_tokens, 600);
        assert_eq!(m.total, 600);
        assert!((m.cost - 8.20).abs() < 1e-9);
        assert_eq!(
            m.variants,
            vec!["gpt-5.5", "azure/gpt-5.5", "openai/gpt-5.5"]
        );
        assert_eq!(m.providers.len(), 2);
        assert_eq!(m.providers[0].provider, Provider::Codex);
        assert!((m.providers[0].cost - 5.00).abs() < 1e-9);
        assert_eq!(m.providers[1].provider, Provider::OpenCode);
        assert_eq!(m.providers[1].total_tokens, 300);
        assert!((m.providers[1].cost - 3.20).abs() < 1e-9);
    }

    #[test]
    fn merge_keeps_different_versions_apart() {
        let rows = vec![
            row("openai/gpt-5.5", 10, 10, 1.0),
            row("azure/gpt-5.5", 30, 30, 3.0),
            row("openai/gpt-5.4", 20, 20, 2.0),
        ];

        let merged = merge_rows_by_base_model(&rows);

        assert_eq!(merged.len(), 2);
        let five_five = merged.iter().find(|r| r.model == "gpt-5.5").unwrap();
        assert_eq!(five_five.total, 40);
        let five_four = merged.iter().find(|r| r.model == "gpt-5.4").unwrap();
        assert_eq!(five_four.total, 20);
    }

    #[test]
    fn merge_strips_prefix_from_single_row_and_keeps_distinct_pricing_match() {
        let mut only = row("deepseek/deepseek-v4-pro", 5, 5, 1.5);
        only.matched_model = Some("deepseek-v4".to_string());

        let merged = merge_rows_by_base_model(std::slice::from_ref(&only));

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].model, "deepseek-v4-pro");
        assert_eq!(merged[0].matched_model.as_deref(), Some("deepseek-v4"));
        assert_eq!(merged[0].variants, vec!["deepseek/deepseek-v4-pro"]);
        assert_eq!(merged[0].total, 5);

        // Drop `matched_model` when it equals the stripped base name to avoid a redundant `Priced as` label.
        let mut exact_after_strip = row("openai/gpt-5.5", 5, 5, 1.5);
        exact_after_strip.matched_model = Some("gpt-5.5".to_string());
        let merged_exact = merge_rows_by_base_model(std::slice::from_ref(&exact_after_strip));
        assert_eq!(merged_exact[0].matched_model, None);
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
        assert_eq!(merged[0].model, "cheap-model");
        assert_eq!(merged[1].model, "gpt-5.5");
        assert!((merged[1].cost - 18.0).abs() < 1e-9);
    }
}
