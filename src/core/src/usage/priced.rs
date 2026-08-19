//! Priced `usage` rows: the library-owned `usage --json` payload.
//!
//! [`price_usage_data`] joins each model's tokens with its resolved USD cost
//! and returns a `Serialize`-able row set, so a non-CLI consumer produces the
//! same shape the CLI emits.

use crate::models::PerProviderUsage;
use crate::pricing::{CostSource, ModelPricingMap, resolve_model_cost};
use crate::usage::{StoredCosts, UsageData};
use crate::utils::{extract_token_counts, normalize_usage_value};
use serde::Serialize;
use serde_json::Value;

/// One priced model row of the `usage --json` output.
///
/// Fields are declared in alphabetical order (`cost_usd`, `matched_model`,
/// `model`, `usage`) on purpose: the CLI used to build each row as a
/// `serde_json::Value`, whose `serde_json::Map` (this crate does not enable
/// `preserve_order`) serializes keys alphabetically, so keeping that order makes
/// the derived output byte-for-byte identical. Do not reorder them.
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

/// Builds the priced `usage --json` payload, joining each model's token counts
/// with its resolved USD cost.
///
/// OpenCode and Hermes models without an exact LiteLLM price report their own
/// stored cost for their own portion of a merged row rather than applying it to
/// other providers with the same model name. Rows follow the insertion order of
/// `usage_data.models` (deliberately unsorted, matching the historical output).
pub fn price_usage_data(
    usage_data: &UsageData,
    pricing_map: &ModelPricingMap,
) -> Vec<PricedUsageRow> {
    let mut rows = Vec::with_capacity(usage_data.models.len());

    for (model, usage) in usage_data.models.iter() {
        let (cost, matched_model) = resolve_merged_model_cost(
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

/// Resolves cost for one merged per-model row from its provider-scoped usage
/// pieces.
///
/// The merged row is priced by summing each provider's own portion under that
/// provider's cost basis (LiteLLM for the file providers, an exact match else
/// the stored cost for OpenCode / Hermes / Cursor, LiteLLM with an input-rate
/// fallback for Grok's cache-read gauge), so OpenCode's stored cost applies only
/// to OpenCode's own tokens even when another provider shares the row. Returns
/// `None` when no provider bucket holds `model`.
pub(crate) fn resolve_merged_model_cost(
    model: &str,
    per_provider: &PerProviderUsage,
    pricing_map: &ModelPricingMap,
    stored_costs: &StoredCosts,
) -> Option<(f64, Option<String>)> {
    let mut total_cost = 0.0;
    let mut matched_model = None;
    let mut found = false;

    for usage in [
        &per_provider.claude,
        &per_provider.codex,
        &per_provider.copilot,
        &per_provider.gemini,
        &per_provider.deepseek,
    ] {
        if let Some(raw_usage) = usage.get(model) {
            found = true;
            let (cost, matched) =
                price_usage_value(model, raw_usage, pricing_map, CostSource::Litellm);
            total_cost += cost;
            if matched_model.is_none() {
                matched_model = matched;
            }
        }
    }

    // Cursor passes `OpenCodeStored(0.0)`: a local token estimate takes an exact
    // LiteLLM price when there is one and otherwise stays unpriced.
    let stored =
        |m: &crate::constants::FastHashMap<String, f64>| m.get(model).copied().unwrap_or(0.0);
    for (usage, source) in [
        (&per_provider.grok, CostSource::GrokGauge),
        (
            &per_provider.opencode,
            CostSource::OpenCodeStored(stored(&stored_costs.opencode)),
        ),
        (&per_provider.cursor, CostSource::OpenCodeStored(0.0)),
        (
            &per_provider.hermes,
            CostSource::HermesStored(stored(&stored_costs.hermes)),
        ),
    ] {
        if let Some(raw_usage) = usage.get(model) {
            found = true;
            let (cost, matched) = price_usage_value(model, raw_usage, pricing_map, source);
            total_cost += cost;
            if matched_model.is_none() {
                matched_model = matched;
            }
        }
    }

    found.then_some((total_cost, matched_model))
}

/// Prices one raw usage value under `source`.
pub(crate) fn price_usage_value(
    model: &str,
    usage: &Value,
    pricing_map: &ModelPricingMap,
    source: CostSource,
) -> (f64, Option<String>) {
    let counts = extract_token_counts(usage);
    resolve_model_cost(model, &counts, pricing_map, source)
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
}
