//! Per-model USD cost resolution against a [`ModelPricingMap`], branching on each provider's [`CostSource`].

use crate::pricing::{ModelPricing, ModelPricingMap, calculate_cost};
#[cfg(test)]
use crate::utils::TierSlice;
use crate::utils::TokenCounts;

/// Authoritative pricing strategy for resolving a model's USD cost.
#[derive(Debug, Clone, Copy)]
pub enum CostSource {
    /// Prices tokens via the full [`ModelPricingMap::get`] lookup chain (exact, normalized, substring, fuzzy).
    Litellm,
    /// Prices tokens on an exact LiteLLM key match; otherwise returns the stored OpenCode cost verbatim.
    OpenCodeStored(f64),
    /// Prices tokens on an exact LiteLLM key match; otherwise returns `0.0`.
    CursorEstimate,
    /// Prices tokens on an exact LiteLLM key match; otherwise returns the stored Hermes cost verbatim.
    HermesStored(f64),
    /// Prices via [`ModelPricingMap::get`], falling back to `input_cost_per_token` when the matched entry has no cache-read price.
    GrokGauge,
}

/// Returns `(cost_usd, matched_model)` for `model` and `counts` under `source`.
///
/// Token costs are computed with [`calculate_cost`] plus per-query `web_search_requests` charges. `matched_model` is `Some` only when [`ModelPricingMap::get`] resolved via a non-exact LiteLLM key.
pub fn resolve_model_cost(
    model: &str,
    counts: &TokenCounts,
    pricing_map: &ModelPricingMap,
    source: CostSource,
) -> (f64, Option<String>) {
    let priced = |pricing: &ModelPricing| {
        let token_cost = calculate_cost(counts, pricing);
        token_cost + counts.web_search_requests as f64 * pricing.web_search_cost_per_query
    };

    let exact_or = |stored: f64| match pricing_map.get_exact_ref(model) {
        Some(pricing) => (priced(pricing), None),
        None => (stored, None),
    };

    match source {
        CostSource::CursorEstimate => exact_or(0.0),
        CostSource::OpenCodeStored(stored) | CostSource::HermesStored(stored) => exact_or(stored),
        CostSource::Litellm => {
            let (pricing, matched_model) = pricing_map.lookup_ref(model);
            let cost = pricing.map_or(0.0, priced);
            (cost, matched_model.map(|key| key.to_string()))
        }
        CostSource::GrokGauge => {
            let (pricing, matched_model) = pricing_map.lookup_ref(model);
            let cost = match pricing {
                Some(pricing) if pricing.cache_read_input_token_cost > 0.0 => priced(pricing),
                Some(pricing) => {
                    let mut adjusted = pricing.clone();
                    adjusted.cache_read_input_token_cost = adjusted.input_cost_per_token;
                    priced(&adjusted)
                }
                None => 0.0,
            };
            (cost, matched_model.map(|key| key.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::clear_pricing_cache;
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
        let (cost, matched) = resolve_model_cost(
            "gpt-4",
            &counts(1_000_000),
            &map,
            CostSource::OpenCodeStored(99.0),
        );
        assert!((cost - 10.0).abs() < 1e-6);
        assert!(matched.is_none());
    }

    #[test]
    fn test_opencode_no_exact_match_uses_stored_cost() {
        clear_pricing_cache();
        let map = map_with_gpt4();
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
    fn test_opencode_tier_slice_cannot_reach_the_stored_cost_branch() {
        clear_pricing_cache();
        let map = map_with_gpt4();
        // Tier snapshots normalize names while stored-cost lookups require an exact key; an inert tier slice must not affect the stored fallback.
        let mut counts = counts(1_000_000);
        counts.above_tiers = vec![TierSlice {
            input_tokens: 1_000_000,
            ..Default::default()
        }];
        let (cost, matched) = resolve_model_cost(
            "openai/gpt-4",
            &counts,
            &map,
            CostSource::OpenCodeStored(99.0),
        );
        assert_eq!(cost, 99.0);
        assert!(matched.is_none());
    }

    #[test]
    fn test_cursor_estimate_prices_only_on_exact_match() {
        clear_pricing_cache();
        let map = map_with_gpt4();
        let (cost, matched) = resolve_model_cost(
            "gpt-4",
            &counts(1_000_000),
            &map,
            CostSource::CursorEstimate,
        );
        assert!((cost - 10.0).abs() < 1e-6);
        assert!(matched.is_none());

        let (cost, matched) = resolve_model_cost(
            "composer-2",
            &counts(1_000_000),
            &map,
            CostSource::CursorEstimate,
        );
        assert_eq!(cost, 0.0);
        assert!(matched.is_none());
    }

    #[test]
    fn test_non_opencode_keeps_existing_lookup() {
        clear_pricing_cache();
        let map = map_with_gpt4();
        let (cost, matched) =
            resolve_model_cost("gpt-4", &counts(1_000_000), &map, CostSource::Litellm);
        assert!((cost - 10.0).abs() < 1e-6);
        assert!(matched.is_none());
    }
}
