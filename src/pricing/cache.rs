use crate::utils::{
    find_pricing_cache_for_date, get_current_date, get_pricing_cache_path, list_pricing_cache_files,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ModelPricing {
    #[serde(default)]
    pub input_cost_per_token: f64,
    #[serde(default)]
    pub output_cost_per_token: f64,
    #[serde(default)]
    pub cache_read_input_token_cost: f64,
    #[serde(default)]
    pub cache_creation_input_token_cost: f64,
    // Above 200K pricing (optional, fallback to base price if not available)
    #[serde(default)]
    pub input_cost_per_token_above_200k_tokens: f64,
    #[serde(default)]
    pub output_cost_per_token_above_200k_tokens: f64,
    #[serde(default)]
    pub cache_read_input_token_cost_above_200k_tokens: f64,
    #[serde(default)]
    pub cache_creation_input_token_cost_above_200k_tokens: f64,
}

impl Default for ModelPricing {
    fn default() -> Self {
        Self {
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
            input_cost_per_token_above_200k_tokens: 0.0,
            output_cost_per_token_above_200k_tokens: 0.0,
            cache_read_input_token_cost_above_200k_tokens: 0.0,
            cache_creation_input_token_cost_above_200k_tokens: 0.0,
        }
    }
}

/// Clean up old cache files (keep only today's)
pub fn cleanup_old_cache() {
    let Ok(cache_files) = list_pricing_cache_files() else {
        return;
    };

    let today = get_current_date();

    for (filename, path) in cache_files {
        // Delete if not today's cache
        if !filename.contains(&today) {
            let _ = fs::remove_file(&path);
            log::debug!("Removed old cache file: {:?}", path);
        }
    }
}

/// Load pricing from cache
pub fn load_from_cache() -> Result<HashMap<String, ModelPricing>> {
    let today = get_current_date();
    let cache_path = find_pricing_cache_for_date(&today)
        .ok_or_else(|| anyhow::anyhow!("No cache file found for today"))?;

    let content = fs::read_to_string(&cache_path).context("Failed to read cached pricing file")?;
    let pricing: HashMap<String, ModelPricing> =
        serde_json::from_str(&content).context("Failed to parse cached pricing JSON")?;
    Ok(pricing)
}

/// Save pricing to cache
pub fn save_to_cache(pricing: &HashMap<String, ModelPricing>) -> Result<()> {
    let today = get_current_date();
    let cache_path = get_pricing_cache_path(&today)?;

    // Save pricing data with today's date in filename
    let pricing_json =
        serde_json::to_string_pretty(pricing).context("Failed to serialize pricing data")?;
    fs::write(&cache_path, pricing_json).context("Failed to write pricing cache file")?;

    // Clean up old cache files
    cleanup_old_cache();

    Ok(())
}

/// Normalize pricing data: fill above_200k prices with base prices if they are 0
pub fn normalize_pricing(
    mut pricing: HashMap<String, ModelPricing>,
) -> HashMap<String, ModelPricing> {
    for p in pricing.values_mut() {
        // Macro to reduce repetition: if above_200k price is 0, use base price
        macro_rules! normalize_field {
            ($above_200k:ident, $base:ident) => {
                if p.$above_200k == 0.0 {
                    p.$above_200k = p.$base;
                }
            };
        }

        normalize_field!(input_cost_per_token_above_200k_tokens, input_cost_per_token);
        normalize_field!(
            output_cost_per_token_above_200k_tokens,
            output_cost_per_token
        );
        normalize_field!(
            cache_read_input_token_cost_above_200k_tokens,
            cache_read_input_token_cost
        );
        normalize_field!(
            cache_creation_input_token_cost_above_200k_tokens,
            cache_creation_input_token_cost
        );
    }
    pricing
}
