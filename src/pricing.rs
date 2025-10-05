use crate::utils::get_current_date;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use strsim::jaro_winkler;

const LITELLM_PRICING_URL: &str =
    "https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json";

// Similarity threshold for fuzzy matching (0.0 to 1.0)
const SIMILARITY_THRESHOLD: f64 = 0.7;

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
}

/// Result of model pricing lookup with optional matched model name
#[derive(Debug, Clone)]
pub struct ModelPricingResult {
    pub pricing: ModelPricing,
    pub matched_model: Option<String>,
}

impl Default for ModelPricing {
    fn default() -> Self {
        Self {
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
        }
    }
}

/// Get cache directory path
fn get_cache_dir() -> Result<PathBuf> {
    let home_dir =
        home::home_dir().ok_or_else(|| anyhow::anyhow!("Unable to resolve user home directory"))?;
    let cache_dir = home_dir.join(".vibe-coding-tracker");

    // Create directory if it doesn't exist
    if !cache_dir.exists() {
        fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;
    }

    Ok(cache_dir)
}

/// Get cache file path for today
fn get_today_cache_path() -> Result<PathBuf> {
    let cache_dir = get_cache_dir()?;
    let date_str = get_current_date();
    Ok(cache_dir.join(format!("model_pricing_{}.json", date_str)))
}

/// Find existing cache file for today
fn find_today_cache() -> Option<PathBuf> {
    let Ok(cache_dir) = get_cache_dir() else {
        return None;
    };

    let today = get_current_date();
    let today_cache = cache_dir.join(format!("model_pricing_{}.json", today));

    if today_cache.exists() {
        Some(today_cache)
    } else {
        None
    }
}

/// Clean up old cache files (keep only today's)
fn cleanup_old_cache() {
    let Ok(cache_dir) = get_cache_dir() else {
        return;
    };

    let Ok(entries) = fs::read_dir(&cache_dir) else {
        return;
    };

    let today = get_current_date();

    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
            // Match pattern: model_pricing_YYYY-MM-DD.json
            if filename.starts_with("model_pricing_")
                && filename.ends_with(".json")
                && !filename.contains(&today)
            {
                // Delete old cache file
                let _ = fs::remove_file(&path);
                log::debug!("Removed old cache file: {:?}", path);
            }
        }
    }
}

/// Load pricing from cache
fn load_from_cache() -> Result<HashMap<String, ModelPricing>> {
    let cache_path =
        find_today_cache().ok_or_else(|| anyhow::anyhow!("No cache file found for today"))?;

    let content = fs::read_to_string(&cache_path).context("Failed to read cached pricing file")?;
    let pricing: HashMap<String, ModelPricing> =
        serde_json::from_str(&content).context("Failed to parse cached pricing JSON")?;
    Ok(pricing)
}

/// Save pricing to cache
fn save_to_cache(pricing: &HashMap<String, ModelPricing>) -> Result<()> {
    let cache_path = get_today_cache_path()?;

    // Save pricing data with today's date in filename
    let pricing_json =
        serde_json::to_string_pretty(pricing).context("Failed to serialize pricing data")?;
    fs::write(&cache_path, pricing_json).context("Failed to write pricing cache file")?;

    // Clean up old cache files
    cleanup_old_cache();

    Ok(())
}

/// Fetch model pricing from LiteLLM repository (with caching)
pub fn fetch_model_pricing() -> Result<HashMap<String, ModelPricing>> {
    // Check if today's cache exists
    if find_today_cache().is_some() {
        // Load from cache
        match load_from_cache() {
            Ok(pricing) => {
                log::debug!("Loaded model pricing from today's cache");
                return Ok(pricing);
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

    // Save to cache with today's date
    if let Err(e) = save_to_cache(&pricing) {
        log::warn!("Failed to save pricing to cache: {}", e);
    } else {
        log::debug!("Saved model pricing to cache with today's date");
    }

    Ok(pricing)
}

/// Calculate cost based on token usage and model pricing
pub fn calculate_cost(
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    pricing: &ModelPricing,
) -> f64 {
    let input_cost = input_tokens as f64 * pricing.input_cost_per_token;
    let output_cost = output_tokens as f64 * pricing.output_cost_per_token;
    let cache_read_cost = cache_read_tokens as f64 * pricing.cache_read_input_token_cost;
    let cache_creation_cost =
        cache_creation_tokens as f64 * pricing.cache_creation_input_token_cost;

    input_cost + output_cost + cache_read_cost + cache_creation_cost
}

/// Get pricing for a specific model, with fallback handling
pub fn get_model_pricing(
    model_name: &str,
    pricing_map: &HashMap<String, ModelPricing>,
) -> ModelPricingResult {
    // Try exact match first
    if let Some(pricing) = pricing_map.get(model_name) {
        return ModelPricingResult {
            pricing: *pricing,
            matched_model: None,
        };
    }

    // Try to find a match by removing version suffixes or provider prefixes
    let normalized_name = normalize_model_name(model_name);
    if let Some(pricing) = pricing_map.get(&normalized_name) {
        return ModelPricingResult {
            pricing: *pricing,
            matched_model: Some(normalized_name),
        };
    }

    // Pre-compute lowercase model name for comparisons
    let model_lower = model_name.to_lowercase();

    // Try to find a partial match (substring) - use lowercase for case-insensitive matching
    // Build a lowercase cache for the pricing map keys to avoid repeated conversions
    let key_cache: Vec<(&String, String)> =
        pricing_map.keys().map(|k| (k, k.to_lowercase())).collect();

    for (key, key_lower) in &key_cache {
        if model_lower.contains(key_lower) || key_lower.contains(&model_lower) {
            if let Some(pricing) = pricing_map.get(*key) {
                return ModelPricingResult {
                    pricing: *pricing,
                    matched_model: Some((*key).clone()),
                };
            }
        }
    }

    // Try fuzzy matching based on string similarity
    let mut best_match: Option<(String, f64)> = None;

    for (key, key_lower) in &key_cache {
        let similarity = jaro_winkler(&model_lower, key_lower);

        if similarity >= SIMILARITY_THRESHOLD {
            if let Some((_, best_similarity)) = &best_match {
                if similarity > *best_similarity {
                    best_match = Some(((*key).clone(), similarity));
                }
            } else {
                best_match = Some(((*key).clone(), similarity));
            }
        }
    }

    if let Some((matched_key, _)) = best_match {
        if let Some(pricing) = pricing_map.get(&matched_key) {
            return ModelPricingResult {
                pricing: *pricing,
                matched_model: Some(matched_key),
            };
        }
    }

    // Return default (zero costs) if no match found
    ModelPricingResult {
        pricing: ModelPricing::default(),
        matched_model: None,
    }
}

/// Normalize model name by removing common version suffixes and prefixes
fn normalize_model_name(name: &str) -> String {
    let mut normalized = name.to_string();

    // Remove common date patterns (e.g., "-20231201", "-20240320")
    if let Some(idx) = normalized.rfind("-20") {
        if normalized[idx + 1..].len() == 8 {
            // "-20YYMMDD" pattern (8 digits: 20YYMMDD)
            normalized = normalized[..idx].to_string();
        }
    }

    // Remove version patterns (e.g., "-v1.0", "-v2")
    if let Some(idx) = normalized.rfind("-v") {
        normalized = normalized[..idx].to_string();
    }

    // Remove provider prefixes (e.g., "bedrock/", "openai/")
    if let Some(idx) = normalized.find('/') {
        normalized = normalized[idx + 1..].to_string();
    }

    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_model_name() {
        assert_eq!(
            normalize_model_name("claude-3-sonnet-20240229"),
            "claude-3-sonnet"
        );
        assert_eq!(normalize_model_name("gpt-4-v1.0"), "gpt-4");
        assert_eq!(
            normalize_model_name("bedrock/claude-3-opus"),
            "claude-3-opus"
        );
    }

    #[test]
    fn test_calculate_cost() {
        let pricing = ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            cache_read_input_token_cost: 0.0000001,
            cache_creation_input_token_cost: 0.0000005,
        };

        let cost = calculate_cost(1000, 500, 200, 100, &pricing);
        assert_eq!(cost, 0.001_000 + 0.001_000 + 0.000_020 + 0.000_050);
    }
}
