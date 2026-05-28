//! Model pricing: fetch, cache, match, and cost calculation.
//!
//! This module pulls per-model token prices from LiteLLM, caches them on disk
//! (one file per calendar day), matches a session's model name against that
//! table, and computes the USD cost of a request. The public surface is the
//! re-exports below; the `cache` / `calculation` / `matching` submodules are
//! internal wiring.
//!
//! Lookup proceeds exact -> normalized -> substring -> Jaro-Winkler fuzzy
//! (see [`ModelPricingMap::get`]), and cost is computed by [`calculate_cost`]
//! across flat, threshold-tiered, and range-tiered pricing shapes.

mod cache;
mod calculation;
mod matching;

use crate::utils::get_current_date;
use anyhow::{Context, Result};

const LITELLM_PRICING_URL: &str =
    "https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json";

// Re-export public types and functions
pub use cache::{ModelPricing, ThresholdTier, TierRange};
pub use calculation::calculate_cost;
pub use matching::{
    ModelPricingMap, ModelPricingResult, clear_pricing_cache, normalize_model_name,
};

/// Fetches AI model pricing data from the LiteLLM repository with automatic caching.
///
/// Returns an optimized pricing map with precomputed indices for fast lookups.
/// Pricing is cached locally for 24 hours (one file per date) to minimize
/// network calls. If today's cache exists and is in the current schema it is
/// loaded directly; otherwise the upstream JSON is fetched, filtered to its
/// cost fields, persisted, and parsed. A failure to write the cache is logged
/// but does not abort the fetch.
///
/// # Errors
///
/// Returns an error if the HTTP client cannot be built, the LiteLLM request
/// fails, or the response body is not valid JSON. A corrupt or legacy on-disk
/// cache does not surface here — it is logged and falls through to a refetch.
///
/// # Examples
///
/// ```no_run
/// use vibe_coding_tracker::pricing::fetch_model_pricing;
///
/// let pricing = fetch_model_pricing()?;
/// let opus = pricing.get("claude-opus-4");
/// assert!(opus.pricing.input_cost_per_token >= 0.0);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn fetch_model_pricing() -> Result<ModelPricingMap> {
    let today = get_current_date();

    // Check if today's cache exists
    if crate::utils::find_pricing_cache_for_date(&today).is_some() {
        // Load from cache
        match cache::load_from_cache() {
            Ok(pricing) => {
                log::debug!("Loaded model pricing from today's cache");
                return Ok(ModelPricingMap::new(pricing));
            }
            Err(e) => {
                log::warn!("Failed to load from cache: {}, fetching from remote", e);
            }
        }
    }

    // Fetch from remote
    log::info!("Fetching model pricing from remote...");
    let client = reqwest::blocking::Client::builder()
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(LITELLM_PRICING_URL)
        .send()
        .context("Failed to fetch model pricing from LiteLLM")?;

    let raw: serde_json::Value = response
        .json()
        .context("Failed to parse model pricing JSON")?;

    // Project the upstream JSON down to just the cost-related keys before
    // anything else. Doing the filter first guarantees the on-disk cache
    // and the in-memory `ModelPricing` are derived from the *same* view of
    // the data — so nothing we price against can differ from what the
    // cache preserves for future calculation strategies.
    let filtered_raw = cache::build_filtered_cost_json(&raw);

    // Save the filtered raw JSON to cache. We deliberately persist the raw
    // cost keys (rather than our derived `ModelPricing` shape) so
    // priority / flex / batch / audio / image tiers that `calculate_cost`
    // doesn't consume yet are still available to future versions without
    // a re-fetch.
    if let Err(e) = cache::save_to_cache(&filtered_raw) {
        log::warn!("Failed to save pricing to cache: {}", e);
    } else {
        log::debug!("Saved model pricing to cache with today's date");
    }

    let pricing = cache::parse_litellm_pricing_map(filtered_raw);

    // Filter out models with entirely zero pricing (free / unpriced entries).
    let normalized_pricing = cache::normalize_pricing(pricing);

    Ok(ModelPricingMap::new(normalized_pricing))
}

// Re-export test helper functions
#[cfg(test)]
pub use cache::normalize_pricing;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_normalize_pricing_preserves_valid_model() {
        // normalize_pricing no longer mutates prices — it only drops zero-cost
        // entries. Verify a model with valid base prices survives unchanged.
        let mut pricing_map = HashMap::new();
        pricing_map.insert(
            "test-model".to_string(),
            ModelPricing {
                input_cost_per_token: 0.000001,
                output_cost_per_token: 0.000002,
                cache_read_input_token_cost: 0.0000001,
                cache_creation_input_token_cost: 0.0000005,
                ..Default::default()
            },
        );

        let normalized = cache::normalize_pricing(pricing_map);
        let p = normalized.get("test-model").unwrap();

        assert_eq!(p.input_cost_per_token, 0.000001);
        assert_eq!(p.output_cost_per_token, 0.000002);
        assert_eq!(p.cache_read_input_token_cost, 0.0000001);
        assert_eq!(p.cache_creation_input_token_cost, 0.0000005);
        assert!(p.tiers.is_empty());
        assert!(p.ranges.is_none());
    }

    #[test]
    fn test_normalize_pricing_filters_zero_cost_models() {
        let mut pricing_map = HashMap::new();

        // Add a valid model with non-zero costs
        pricing_map.insert(
            "valid-model".to_string(),
            ModelPricing {
                input_cost_per_token: 0.000001,
                output_cost_per_token: 0.000002,
                ..Default::default()
            },
        );

        // Add a model with all zero costs - should be filtered out
        pricing_map.insert("zero-cost-model".to_string(), ModelPricing::default());

        // Add another model with all zero costs
        pricing_map.insert(
            "another-zero-model".to_string(),
            ModelPricing {
                input_cost_per_token: 0.0,
                output_cost_per_token: 0.0,
                cache_read_input_token_cost: 0.0,
                cache_creation_input_token_cost: 0.0,
                ..Default::default()
            },
        );

        let normalized = cache::normalize_pricing(pricing_map);

        // Only the valid model should remain
        assert_eq!(normalized.len(), 1);
        assert!(normalized.contains_key("valid-model"));
        assert!(!normalized.contains_key("zero-cost-model"));
        assert!(!normalized.contains_key("another-zero-model"));
    }
}
