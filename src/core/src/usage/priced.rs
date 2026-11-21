//! Priced `usage` rows for the `usage --json` and `usage --json --by-day` payloads.
//!
//! [`price_usage_data`] joins each model's normalized token counts with its resolved USD cost, [`merge_priced_usage_rows`] collapses provider-prefixed rows by base model name, and [`price_daily_usage_data`] produces the per-day JSON payload.

use crate::models::{PerProviderUsage, Provider};
use crate::pricing::{CostSource, ModelPricingMap, resolve_model_cost};
use crate::usage::summary::{ModelProviderUsage, base_model_key};
use crate::usage::{DailyUsageData, StoredCosts, UsageData};
use crate::utils::token_merge::{add_token_counts, token_counts_to_flat_value};
use crate::utils::{TokenCounts, extract_token_counts, normalize_usage_value};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;

/// One priced model row of the `usage --json` output.
///
/// Fields are declared in alphabetical order (`cost_usd`, `matched_model`, `model`, `usage`) to preserve canonical JSON key order; do not reorder them.
#[derive(Debug, Clone, Serialize)]
pub struct PricedUsageRow {
    /// Resolved cost in USD.
    pub cost_usd: f64,
    /// The LiteLLM key actually used, when it differed from `model`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_model: Option<String>,
    /// Model name (merged across providers).
    pub model: String,
    /// Token counts normalized to the flat key set (see [`normalize_usage_value`]).
    pub usage: Value,
}

/// One priced model row of the `usage --json --by-day` output.
///
/// Fields are declared in alphabetical order (`cost_usd`, `matched_model`, `model`, `priced`, `usage`).
#[derive(Debug, Clone, Serialize)]
pub struct DailyPricedUsageRow {
    /// Resolved cost in USD.
    pub cost_usd: f64,
    /// The LiteLLM key actually used, when it differed from `model`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_model: Option<String>,
    /// Model name (or base model name when `--merge-providers` is enabled).
    pub model: String,
    /// Whether a pricing entry matched or a positive provider-stored cost existed.
    pub priced: bool,
    /// Token counts normalized to the flat key set (see [`normalize_usage_value`]).
    pub usage: Value,
}

/// Priced per-model usage for one local calendar date (`YYYY-MM-DD`).
#[derive(Debug, Clone, Serialize)]
pub struct DailyUsageDay {
    /// Local calendar date (`YYYY-MM-DD`).
    pub date: String,
    /// Priced per-model rows for this day, sorted by ascending cost then model name.
    pub models: Vec<DailyPricedUsageRow>,
}

/// Top-level payload for `usage --json --by-day`.
#[derive(Debug, Clone, Serialize)]
pub struct DailyUsageOutput {
    /// Per-day usage entries, sorted ascending by local date (`YYYY-MM-DD`).
    pub days: Vec<DailyUsageDay>,
    /// Sorted names of synced remote snapshots included in this scan.
    pub remotes: Vec<String>,
}

/// Returns the priced `usage --json` rows, joining each model's normalized tokens with its resolved USD cost.
///
/// Each provider's share of a merged model row is priced under that provider's cost basis (stored fallback costs for OpenCode and Hermes apply only to their own tokens). Rows are unsorted and follow the iteration order of `usage_data.models`.
pub fn price_usage_data(
    usage_data: &UsageData,
    pricing_map: &ModelPricingMap,
) -> Vec<PricedUsageRow> {
    let mut rows = Vec::with_capacity(usage_data.models.len());

    for (model, usage) in usage_data.models.iter() {
        let (cost, matched_model, _) = resolve_merged_model_cost(
            model,
            &usage_data.per_provider,
            pricing_map,
            &usage_data.stored_costs,
        )
        .unwrap_or_else(|| price_usage_value(model, usage, pricing_map, CostSource::Litellm));

        rows.push(PricedUsageRow {
            model: model.clone(),
            usage: normalize_usage_value(usage),
            cost_usd: cost,
            matched_model,
        });
    }

    rows
}

/// Collapses [`PricedUsageRow`]s that share a base model name (after stripping any leading `<prefix>/` segment), summing normalized token counts and pre-resolved costs verbatim.
///
/// Follows the same rules as [`merge_rows_by_base_model`](crate::usage::summary::merge_rows_by_base_model): costs are summed without re-pricing, a lone stripped row keeps `matched_model` only when it differs from the base name, multi-variant groups clear `matched_model`, and output is sorted by ascending cost then model name.
pub fn merge_priced_usage_rows(rows: Vec<PricedUsageRow>) -> Vec<PricedUsageRow> {
    let mut groups: HashMap<String, Vec<PricedUsageRow>> = HashMap::new();
    for row in rows {
        let key = base_model_key(&row.model).to_string();
        groups.entry(key).or_default().push(row);
    }

    let mut merged = Vec::with_capacity(groups.len());
    for (key, members) in groups {
        let matched_model = if members.len() == 1 {
            members[0]
                .matched_model
                .as_ref()
                .filter(|matched| matched.as_str() != key)
                .cloned()
        } else {
            None
        };
        let mut counts = TokenCounts::default();
        let mut cost_usd = 0.0;
        for m in members {
            counts = add_token_counts(&counts, &extract_token_counts(&m.usage));
            cost_usd += m.cost_usd;
        }
        merged.push(PricedUsageRow {
            cost_usd,
            matched_model,
            model: key,
            usage: token_counts_to_flat_value(&counts),
        });
    }

    merged.sort_by(|a, b| {
        a.cost_usd
            .partial_cmp(&b.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model.cmp(&b.model))
    });
    merged
}

/// Collapses [`DailyPricedUsageRow`]s that share a base model name, summing pre-resolved costs verbatim and OR-ing [`DailyPricedUsageRow::priced`].
fn merge_daily_priced_rows(rows: Vec<DailyPricedUsageRow>) -> Vec<DailyPricedUsageRow> {
    let mut groups: HashMap<String, Vec<DailyPricedUsageRow>> = HashMap::new();
    for row in rows {
        let key = base_model_key(&row.model).to_string();
        groups.entry(key).or_default().push(row);
    }

    let mut merged = Vec::with_capacity(groups.len());
    for (key, members) in groups {
        let matched_model = if members.len() == 1 {
            members[0]
                .matched_model
                .as_ref()
                .filter(|matched| matched.as_str() != key)
                .cloned()
        } else {
            None
        };
        let mut counts = TokenCounts::default();
        let mut cost_usd = 0.0;
        let mut priced = false;
        for m in members {
            counts = add_token_counts(&counts, &extract_token_counts(&m.usage));
            cost_usd += m.cost_usd;
            priced |= m.priced;
        }
        merged.push(DailyPricedUsageRow {
            cost_usd,
            matched_model,
            model: key,
            priced,
            usage: token_counts_to_flat_value(&counts),
        });
    }

    sort_daily_rows(&mut merged);
    merged
}

fn sort_daily_rows(rows: &mut [DailyPricedUsageRow]) {
    rows.sort_by(|a, b| {
        a.cost_usd
            .partial_cmp(&b.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model.cmp(&b.model))
    });
}

/// Prices per-day usage data into the `usage --json --by-day` payload.
///
/// Each day's model rows are priced under each provider's own [`CostSource`]. When `merge_providers` is `true`, rows within each day are collapsed by base model name without re-pricing.
pub fn price_daily_usage_data(
    daily: &DailyUsageData,
    pricing_map: &ModelPricingMap,
    merge_providers: bool,
) -> DailyUsageOutput {
    let mut days = Vec::with_capacity(daily.days.len());

    for (date, bucket) in &daily.days {
        let mut rows = Vec::with_capacity(bucket.models.len());
        for (model, usage) in &bucket.models {
            let (cost_usd, matched_model, priced) = resolve_merged_model_cost(
                model,
                &bucket.per_provider,
                pricing_map,
                &bucket.stored_costs,
            )
            .unwrap_or_else(|| price_usage_value(model, usage, pricing_map, CostSource::Litellm));

            rows.push(DailyPricedUsageRow {
                cost_usd,
                matched_model,
                model: model.clone(),
                priced,
                usage: normalize_usage_value(usage),
            });
        }

        let models = if merge_providers {
            merge_daily_priced_rows(rows)
        } else {
            sort_daily_rows(&mut rows);
            rows
        };

        days.push(DailyUsageDay {
            date: date.clone(),
            models,
        });
    }

    DailyUsageOutput {
        days,
        remotes: daily.remotes.clone(),
    }
}

/// Resolved cost, matched pricing key, priced indicator, and per-provider breakdown for one merged model row.
pub(crate) struct MergedModelCost {
    pub cost: f64,
    pub matched_model: Option<String>,
    pub priced: bool,
    pub providers: Vec<ModelProviderUsage>,
}

fn source_is_priced(
    model: &str,
    pricing_map: &ModelPricingMap,
    source: CostSource,
    cost: f64,
    matched: &Option<String>,
) -> bool {
    match source {
        CostSource::Litellm | CostSource::GrokGauge => pricing_map.lookup_ref(model).0.is_some(),
        CostSource::CursorEstimate => {
            pricing_map.get_exact_ref(model).is_some() || matched.is_some() || cost > 0.0
        }
        CostSource::OpenCodeStored(stored) | CostSource::HermesStored(stored) => {
            pricing_map.get_exact_ref(model).is_some()
                || matched.is_some()
                || stored > 0.0
                || cost > 0.0
        }
    }
}

/// Resolves `model`'s total cost, matched key, and per-provider breakdown by pricing each provider's share under its own [`CostSource`], returning `None` when no provider holds `model`.
pub(crate) fn resolve_merged_model_breakdown(
    model: &str,
    per_provider: &PerProviderUsage,
    pricing_map: &ModelPricingMap,
    stored_costs: &StoredCosts,
) -> Option<MergedModelCost> {
    let PerProviderUsage {
        claude,
        codex,
        copilot,
        gemini,
        opencode,
        cursor,
        hermes,
        grok,
        deepseek,
        antigravity,
    } = per_provider;

    let stored =
        |m: &crate::constants::FastHashMap<String, f64>| m.get(model).copied().unwrap_or(0.0);

    let mut total_cost = 0.0;
    let mut matched_model = None;
    let mut priced = false;
    let mut found = false;
    let mut providers = Vec::new();

    for (provider, usage, source) in [
        (Provider::ClaudeCode, claude, CostSource::Litellm),
        (Provider::Codex, codex, CostSource::Litellm),
        (Provider::Copilot, copilot, CostSource::Litellm),
        (Provider::Gemini, gemini, CostSource::Litellm),
        (Provider::DeepSeek, deepseek, CostSource::Litellm),
        (Provider::Antigravity, antigravity, CostSource::Litellm),
        (Provider::Grok, grok, CostSource::GrokGauge),
        (
            Provider::OpenCode,
            opencode,
            CostSource::OpenCodeStored(stored(&stored_costs.opencode)),
        ),
        (Provider::Cursor, cursor, CostSource::CursorEstimate),
        (
            Provider::Hermes,
            hermes,
            CostSource::HermesStored(stored(&stored_costs.hermes)),
        ),
    ] {
        if let Some(raw_usage) = usage.get(model) {
            found = true;
            let counts = extract_token_counts(raw_usage);
            let (cost, matched) = resolve_model_cost(model, &counts, pricing_map, source);
            priced |= source_is_priced(model, pricing_map, source, cost, &matched);
            total_cost += cost;
            if matched_model.is_none() {
                matched_model = matched;
            }
            if counts.total != 0 || cost != 0.0 {
                providers.push(ModelProviderUsage {
                    provider,
                    total_tokens: counts.total,
                    cost,
                });
            }
        }
    }

    providers.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.total_tokens.cmp(&a.total_tokens))
            .then_with(|| a.provider.display_name().cmp(b.provider.display_name()))
    });

    found.then_some(MergedModelCost {
        cost: total_cost,
        matched_model,
        priced,
        providers,
    })
}

/// Returns the resolved cost, optional matched pricing key, and priced flag for `model` across provider buckets, or `None` when absent.
pub(crate) fn resolve_merged_model_cost(
    model: &str,
    per_provider: &PerProviderUsage,
    pricing_map: &ModelPricingMap,
    stored_costs: &StoredCosts,
) -> Option<(f64, Option<String>, bool)> {
    resolve_merged_model_breakdown(model, per_provider, pricing_map, stored_costs)
        .map(|resolved| (resolved.cost, resolved.matched_model, resolved.priced))
}

/// Prices one raw usage value under `source`, returning `(cost, matched_model, priced)`.
pub(crate) fn price_usage_value(
    model: &str,
    usage: &Value,
    pricing_map: &ModelPricingMap,
    source: CostSource,
) -> (f64, Option<String>, bool) {
    let counts = extract_token_counts(usage);
    let (cost, matched) = resolve_model_cost(model, &counts, pricing_map, source);
    let priced = source_is_priced(model, pricing_map, source, cost, &matched);
    (cost, matched, priced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{PerProviderUsage, ProviderActiveDays, UsageResult};
    use crate::pricing::{ModelPricing, clear_pricing_cache};
    use crate::usage::StoredCosts;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn priced_rows_include_grok_source_cost() {
        clear_pricing_cache();
        let mut raw_pricing = HashMap::new();
        raw_pricing.insert(
            "shared-model".to_string(),
            ModelPricing {
                input_cost_per_token: 0.01,
                ..Default::default()
            },
        );
        let pricing_map = ModelPricingMap::new(raw_pricing);
        let mut models = UsageResult::default();
        models.insert("shared-model".to_string(), json!({"input_tokens": 200}));
        let mut per_provider = PerProviderUsage::default();
        per_provider
            .claude
            .insert("shared-model".to_string(), json!({"input_tokens": 100}));
        per_provider
            .grok
            .insert("shared-model".to_string(), json!({"input_tokens": 100}));
        let usage_data = UsageData {
            models,
            per_provider,
            provider_days: ProviderActiveDays::default(),
            stored_costs: StoredCosts::default(),
        };

        let rows = price_usage_data(&usage_data, &pricing_map);

        assert!((rows[0].cost_usd - 2.0).abs() < 1e-9);
    }

    #[test]
    fn priced_rows_price_opencode_fallback_only_for_opencode_tokens() {
        clear_pricing_cache();

        let mut raw_pricing = HashMap::new();
        raw_pricing.insert(
            "shared".to_string(),
            ModelPricing {
                input_cost_per_token: 0.01,
                ..Default::default()
            },
        );
        let pricing_map = ModelPricingMap::new(raw_pricing);

        let mut models = UsageResult::default();
        models.insert("shared-pro".to_string(), json!({"input_tokens": 200}));

        let mut per_provider = PerProviderUsage::default();
        per_provider
            .claude
            .insert("shared-pro".to_string(), json!({"input_tokens": 100}));
        per_provider
            .opencode
            .insert("shared-pro".to_string(), json!({"input_tokens": 100}));

        let mut stored_costs = StoredCosts::default();
        stored_costs.opencode.insert("shared-pro".to_string(), 7.0);

        let usage_data = UsageData {
            models,
            per_provider,
            provider_days: ProviderActiveDays::default(),
            stored_costs,
        };

        let rows = price_usage_data(&usage_data, &pricing_map);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cost_usd, 8.0);
        assert_eq!(rows[0].matched_model.as_deref(), Some("shared"));
    }

    #[test]
    fn daily_priced_flag_distinguishes_zero_cost_match_from_unpriced_model() {
        clear_pricing_cache();
        let mut raw_pricing = HashMap::new();
        // Matched free model: $0.00 rates but explicitly present in pricing map.
        raw_pricing.insert("free-model".to_string(), ModelPricing::default());
        let pricing_map = ModelPricingMap::new(raw_pricing);

        let mut day = crate::usage::DayUsageData::default();
        day.models
            .insert("free-model".to_string(), json!({"input_tokens": 100}));
        day.per_provider
            .claude
            .insert("free-model".to_string(), json!({"input_tokens": 100}));
        day.models
            .insert("unknown-xyzzy".to_string(), json!({"input_tokens": 100}));
        day.per_provider
            .claude
            .insert("unknown-xyzzy".to_string(), json!({"input_tokens": 100}));
        day.models
            .insert("oc-stored".to_string(), json!({"input_tokens": 50}));
        day.per_provider
            .opencode
            .insert("oc-stored".to_string(), json!({"input_tokens": 50}));
        day.stored_costs
            .opencode
            .insert("oc-stored".to_string(), 1.25);
        day.models
            .insert("oc-unpriced".to_string(), json!({"input_tokens": 50}));
        day.per_provider
            .opencode
            .insert("oc-unpriced".to_string(), json!({"input_tokens": 50}));
        day.stored_costs
            .opencode
            .insert("oc-unpriced".to_string(), 0.0);

        let mut days = std::collections::BTreeMap::new();
        days.insert("2026-11-21".to_string(), day);
        let daily = DailyUsageData {
            days,
            remotes: vec!["box-a".to_string()],
        };

        let out = price_daily_usage_data(&daily, &pricing_map, false);
        assert_eq!(out.remotes, vec!["box-a"]);
        assert_eq!(out.days.len(), 1);
        assert_eq!(out.days[0].date, "2026-11-21");

        let by_name = |name: &str| out.days[0].models.iter().find(|r| r.model == name).unwrap();
        assert!(by_name("free-model").priced);
        assert_eq!(by_name("free-model").cost_usd, 0.0);
        assert!(!by_name("unknown-xyzzy").priced);
        assert_eq!(by_name("unknown-xyzzy").cost_usd, 0.0);
        assert!(by_name("oc-stored").priced);
        assert!((by_name("oc-stored").cost_usd - 1.25).abs() < 1e-9);
        assert!(!by_name("oc-unpriced").priced);
        assert_eq!(by_name("oc-unpriced").cost_usd, 0.0);
    }

    #[test]
    fn merge_priced_usage_rows_sums_costs_tokens_and_applies_matched_model_rules() {
        let rows = vec![
            PricedUsageRow {
                cost_usd: 1.5,
                matched_model: Some("gpt-5.5".to_string()),
                model: "openai/gpt-5.5".to_string(),
                usage: normalize_usage_value(&json!({
                    "input_tokens": 100,
                    "output_tokens": 20,
                    "cache_read_input_tokens": 50,
                    "server_tool_use": { "web_search_requests": 1 }
                })),
            },
            PricedUsageRow {
                cost_usd: 2.5,
                matched_model: None,
                model: "azure/gpt-5.5".to_string(),
                usage: normalize_usage_value(&json!({
                    "input_tokens": 200,
                    "output_tokens": 40,
                    "server_tool_use": { "web_search_requests": 2 }
                })),
            },
            PricedUsageRow {
                cost_usd: 0.75,
                matched_model: Some("deepseek-v4".to_string()),
                model: "deepseek/deepseek-v4-pro".to_string(),
                usage: normalize_usage_value(&json!({"input_tokens": 10})),
            },
            PricedUsageRow {
                cost_usd: 0.5,
                matched_model: Some("claude-sonnet".to_string()),
                model: "anthropic/claude-sonnet".to_string(),
                usage: normalize_usage_value(&json!({"input_tokens": 5})),
            },
        ];

        let merged = merge_priced_usage_rows(rows);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].model, "claude-sonnet");
        // Stripped single row drops redundant `matched_model` equal to base key.
        assert_eq!(merged[0].matched_model, None);
        assert_eq!(merged[1].model, "deepseek-v4-pro");
        // Stripped single row keeps distinct `matched_model`.
        assert_eq!(merged[1].matched_model.as_deref(), Some("deepseek-v4"));
        assert_eq!(merged[2].model, "gpt-5.5");
        assert_eq!(merged[2].matched_model, None);
        assert!((merged[2].cost_usd - 4.0).abs() < 1e-9);
        assert_eq!(merged[2].usage["input_tokens"], json!(300));
        assert_eq!(merged[2].usage["output_tokens"], json!(60));
        assert_eq!(merged[2].usage["cache_read_input_tokens"], json!(50));
        assert_eq!(merged[2].usage["total_tokens"], json!(410));
        assert_eq!(
            merged[2].usage["server_tool_use"]["web_search_requests"],
            json!(3)
        );
    }
}
