//! Per-model USD cost resolution.
//!
//! Turns a model's [`TokenCounts`](crate::utils::TokenCounts) into a dollar
//! cost against the [`ModelPricingMap`], branching on the authoritative cost
//! source for the provider that produced the tokens. This is pure pricing
//! policy (no `usage`- or `analysis`-feature knowledge), so it lives in
//! `pricing` and both the `usage` roll-up and the display summaries consume it.

use crate::pricing::{ModelPricing, ModelPricingMap, calculate_cost};
use crate::utils::TokenCounts;

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
    /// Caller-supplied Cursor cost used verbatim. Retained for source
    /// compatibility; VCT's local Cursor reader now returns zero stored cost
    /// and its display path accepts only exact LiteLLM matches.
    CursorStored(f64),
    /// Hermes: same basis as [`OpenCodeStored`] — an **exact** LiteLLM match
    /// prices from tokens, otherwise Hermes's own stored cost is used. Hermes
    /// often bills novel models LiteLLM can't price, so its own number is the
    /// safest fallback; the map is kept separate so a colliding bare model name
    /// can't cross-contaminate another provider's cost.
    HermesStored(f64),
    /// Grok: the full LiteLLM lookup, but the context-gauge estimate lives
    /// entirely in the cache-read bucket, so a matched model whose LiteLLM
    /// entry publishes no cache-read price (null for several `xai/grok-*`
    /// variants) falls back to the input rate instead of silently costing $0.
    GrokGauge,
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
    let priced = |pricing: &ModelPricing| {
        let token_cost = calculate_cost(counts, pricing);
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
        CostSource::GrokGauge => {
            let result = pricing_map.get(model);
            let mut pricing = result.pricing;
            if pricing.cache_read_input_token_cost <= 0.0 {
                pricing.cache_read_input_token_cost = pricing.input_cost_per_token;
            }
            (priced(&pricing), result.matched_model)
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
}
