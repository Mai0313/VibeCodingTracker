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

/// Fetches AI model pricing data from LiteLLM repository with automatic caching
///
/// Returns an optimized pricing map with precomputed indices for fast lookups.
/// Pricing is cached locally for 24 hours (one file per date) to minimize API calls.
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
    let pricing = cache::parse_litellm_pricing_map(raw);

    // Filter out models with entirely zero pricing (free / unpriced entries).
    let normalized_pricing = cache::normalize_pricing(pricing);

    // Save to cache with today's date
    if let Err(e) = cache::save_to_cache(&normalized_pricing) {
        log::warn!("Failed to save pricing to cache: {}", e);
    } else {
        log::debug!("Saved model pricing to cache with today's date");
    }

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
