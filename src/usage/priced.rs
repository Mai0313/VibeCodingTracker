//! Priced `usage` rows: the library-owned `usage --json` payload.
//!
//! Joining each model's tokens with its resolved USD cost used to live in the
//! binary, so a non-CLI consumer (e.g. a future GUI backend) could not produce
//! the same shape. [`price_usage_data`] returns a `Serialize`-able row set with
//! the same `matched_model`-only-when-present behavior the CLI has always
//! emitted.

use crate::pricing::ModelPricingMap;
use crate::usage::{CostSource, UsageData, resolve_model_cost};
use crate::utils::{extract_token_counts, normalize_usage_value};
use serde::Serialize;
use serde_json::Value;

/// One priced model row of the `usage --json` output.
///
/// The old binary built each row as a `serde_json::Value` object, whose
/// `serde_json::Map` (this crate does not enable `preserve_order`) serializes
/// keys alphabetically. Fields are declared in that same alphabetical order
/// (`cost_usd`, `matched_model`, `model`, `usage`) so the derived output is
/// byte-for-byte identical to what the CLI has always emitted.
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
/// For every model it resolves the cost via [`resolve_model_cost`] and emits a
/// [`PricedUsageRow`]. OpenCode and Hermes models without an exact LiteLLM price
/// report their own stored cost for their own portion of a merged row rather
/// than applying it to other providers with the same model name. Rows follow the
/// insertion order of `usage_data.models` (deliberately unsorted, matching the
/// historical output).
pub fn price_usage_data(
    usage_data: &UsageData,
    pricing_map: &ModelPricingMap,
) -> Vec<PricedUsageRow> {
    let mut rows = Vec::with_capacity(usage_data.models.len());

    for (model, usage) in usage_data.models.iter() {
        let (cost, matched_model) = resolve_merged_model_cost(model, usage_data, pricing_map)
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

/// Resolves cost for one merged row from its provider-scoped usage pieces.
fn resolve_merged_model_cost(
    model: &str,
    usage_data: &UsageData,
    pricing_map: &ModelPricingMap,
) -> Option<(f64, Option<String>)> {
    let mut total_cost = 0.0;
    let mut matched_model = None;
    let mut found = false;

    for usage in [
        &usage_data.per_provider.claude,
        &usage_data.per_provider.codex,
        &usage_data.per_provider.copilot,
        &usage_data.per_provider.gemini,
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

    // OpenCode and Hermes prefer an exact LiteLLM match before their stored
    // costs. Cursor is a local token estimate, so it uses an exact LiteLLM
    // price when available and otherwise remains unpriced.
    let stored =
        |m: &crate::constants::FastHashMap<String, f64>| m.get(model).copied().unwrap_or(0.0);
    for (usage, source) in [
        (&usage_data.per_provider.grok, CostSource::GrokGauge),
        (
            &usage_data.per_provider.opencode,
            CostSource::OpenCodeStored(stored(&usage_data.stored_costs.opencode)),
        ),
        (
            &usage_data.per_provider.cursor,
            CostSource::OpenCodeStored(0.0),
        ),
        (
            &usage_data.per_provider.hermes,
            CostSource::HermesStored(stored(&usage_data.stored_costs.hermes)),
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
fn price_usage_value(
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
