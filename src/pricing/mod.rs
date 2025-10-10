mod cache;
mod calculation;
mod matching;

use crate::utils::get_current_date;
use anyhow::{Context, Result};
use std::collections::HashMap;

const LITELLM_PRICING_URL: &str =
    "https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json";

// Re-export public types and functions
pub use cache::ModelPricing;
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
    let response = reqwest::blocking::get(LITELLM_PRICING_URL)
        .context("Failed to fetch model pricing from LiteLLM")?;

    let pricing: HashMap<String, ModelPricing> = response
        .json()
        .context("Failed to parse model pricing JSON")?;

    // Normalize pricing: fill above_200k prices with base prices if they are 0
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

    #[test]
    fn test_normalize_pricing() {
        let mut pricing_map = HashMap::new();
        pricing_map.insert(
            "test-model".to_string(),
            ModelPricing {
                input_cost_per_token: 0.000001,
                output_cost_per_token: 0.000002,
                cache_read_input_token_cost: 0.0000001,
                cache_creation_input_token_cost: 0.0000005,
                // above_200k prices are 0.0
                ..Default::default()
            },
        );

        let normalized = cache::normalize_pricing(pricing_map);
        let test_pricing = normalized.get("test-model").unwrap();

        // Verify above_200k prices were filled with base prices
        assert_eq!(
            test_pricing.input_cost_per_token_above_200k_tokens,
            0.000001
        );
        assert_eq!(
            test_pricing.output_cost_per_token_above_200k_tokens,
            0.000002
        );
        assert_eq!(
            test_pricing.cache_read_input_token_cost_above_200k_tokens,
            0.0000001
        );
        assert_eq!(
            test_pricing.cache_creation_input_token_cost_above_200k_tokens,
            0.0000005
        );
    }
}
