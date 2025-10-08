use crate::utils::{get_cache_dir, get_current_date};
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

/// Result of model pricing lookup with optional matched model name
#[derive(Debug, Clone)]
pub struct ModelPricingResult {
    pub pricing: ModelPricing,
    pub matched_model: Option<String>,
}

/// Optimized pricing map with precomputed indices for fast lookups
#[derive(Debug, Clone)]
pub struct ModelPricingMap {
    // Original pricing data
    raw: HashMap<String, ModelPricing>,
    // Precomputed normalized keys for fast matching
    normalized_index: HashMap<String, String>, // normalized_key -> original_key
    // Precomputed lowercase keys for substring/fuzzy matching
    lowercase_keys: Vec<(String, String)>, // (lowercase_key, original_key)
}

impl ModelPricingMap {
    /// Create a new ModelPricingMap with precomputed indices
    pub fn new(raw: HashMap<String, ModelPricing>) -> Self {
        let mut normalized_index = HashMap::new();
        let mut lowercase_keys = Vec::new();

        for key in raw.keys() {
            // Precompute normalized key
            let normalized = normalize_model_name(key);
            if normalized != *key {
                normalized_index.insert(normalized, key.clone());
            }

            // Precompute lowercase key for substring/fuzzy matching
            lowercase_keys.push((key.to_lowercase(), key.clone()));
        }

        Self {
            raw,
            normalized_index,
            lowercase_keys,
        }
    }

    /// Get pricing for a specific model with optimized matching
    pub fn get(&self, model_name: &str) -> ModelPricingResult {
        // Fast path 1: Exact match
        if let Some(pricing) = self.raw.get(model_name) {
            return ModelPricingResult {
                pricing: *pricing,
                matched_model: None,
            };
        }

        // Fast path 2: Normalized match
        let normalized_name = normalize_model_name(model_name);
        if let Some(original_key) = self.normalized_index.get(&normalized_name) {
            if let Some(pricing) = self.raw.get(original_key) {
                return ModelPricingResult {
                    pricing: *pricing,
                    matched_model: Some(original_key.clone()),
                };
            }
        }

        // Slow path: Substring and fuzzy matching (but with precomputed lowercase keys)
        let model_lower = model_name.to_lowercase();
        let mut substring_match: Option<String> = None;
        let mut best_fuzzy_match: Option<(String, f64)> = None;

        for (key_lower, original_key) in &self.lowercase_keys {
            // Substring matching
            if substring_match.is_none()
                && (model_lower.contains(key_lower) || key_lower.contains(&model_lower))
            {
                substring_match = Some(original_key.clone());
            }

            // Fuzzy matching
            let similarity = jaro_winkler(&model_lower, key_lower);
            if similarity >= SIMILARITY_THRESHOLD {
                match &best_fuzzy_match {
                    Some((_, best_similarity)) if similarity > *best_similarity => {
                        best_fuzzy_match = Some((original_key.clone(), similarity));
                    }
                    None => {
                        best_fuzzy_match = Some((original_key.clone(), similarity));
                    }
                    _ => {}
                }
            }
        }

        // Return substring match first, then fuzzy match
        if let Some(matched_key) = substring_match {
            if let Some(pricing) = self.raw.get(&matched_key) {
                return ModelPricingResult {
                    pricing: *pricing,
                    matched_model: Some(matched_key),
                };
            }
        }

        if let Some((matched_key, _)) = best_fuzzy_match {
            if let Some(pricing) = self.raw.get(&matched_key) {
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

    /// Get the raw pricing map (for backward compatibility)
    pub fn raw(&self) -> &HashMap<String, ModelPricing> {
        &self.raw
    }
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

/// Normalize pricing data: fill above_200k prices with base prices if they are 0
fn normalize_pricing(mut pricing: HashMap<String, ModelPricing>) -> HashMap<String, ModelPricing> {
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

/// Fetch model pricing from LiteLLM repository (with caching)
/// Returns an optimized ModelPricingMap with precomputed indices
pub fn fetch_model_pricing() -> Result<ModelPricingMap> {
    // Check if today's cache exists
    if find_today_cache().is_some() {
        // Load from cache
        match load_from_cache() {
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
    let normalized_pricing = normalize_pricing(pricing);

    // Save to cache with today's date
    if let Err(e) = save_to_cache(&normalized_pricing) {
        log::warn!("Failed to save pricing to cache: {}", e);
    } else {
        log::debug!("Saved model pricing to cache with today's date");
    }

    Ok(ModelPricingMap::new(normalized_pricing))
}

/// Calculate cost based on token usage and model pricing
/// Automatically uses above_200k pricing when tokens exceed 200K threshold
pub fn calculate_cost(
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    pricing: &ModelPricing,
) -> f64 {
    const TOKEN_THRESHOLD: i64 = 200_000;

    // Helper function to get the appropriate price based on token count
    // Note: above_200k prices are already normalized to base prices if not provided
    let get_price = |tokens: i64, base_price: f64, above_200k_price: f64| -> f64 {
        if tokens > TOKEN_THRESHOLD {
            above_200k_price
        } else {
            base_price
        }
    };

    // Calculate costs for each token type with appropriate pricing
    let input_price = get_price(
        input_tokens,
        pricing.input_cost_per_token,
        pricing.input_cost_per_token_above_200k_tokens,
    );
    let output_price = get_price(
        output_tokens,
        pricing.output_cost_per_token,
        pricing.output_cost_per_token_above_200k_tokens,
    );
    let cache_read_price = get_price(
        cache_read_tokens,
        pricing.cache_read_input_token_cost,
        pricing.cache_read_input_token_cost_above_200k_tokens,
    );
    let cache_creation_price = get_price(
        cache_creation_tokens,
        pricing.cache_creation_input_token_cost,
        pricing.cache_creation_input_token_cost_above_200k_tokens,
    );

    let input_cost = input_tokens as f64 * input_price;
    let output_cost = output_tokens as f64 * output_price;
    let cache_read_cost = cache_read_tokens as f64 * cache_read_price;
    let cache_creation_cost = cache_creation_tokens as f64 * cache_creation_price;

    input_cost + output_cost + cache_read_cost + cache_creation_cost
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
            input_cost_per_token_above_200k_tokens: 0.000002,
            output_cost_per_token_above_200k_tokens: 0.000004,
            cache_read_input_token_cost_above_200k_tokens: 0.0000002,
            cache_creation_input_token_cost_above_200k_tokens: 0.000001,
        };

        // Test with tokens below 200K threshold - all use base price
        let cost = calculate_cost(1000, 500, 200, 100, &pricing);
        assert_eq!(cost, 0.001_000 + 0.001_000 + 0.000_020 + 0.000_050);

        // Test with ALL tokens above 200K threshold (should use above_200k pricing)
        let cost_above = calculate_cost(250_000, 250_000, 250_000, 250_000, &pricing);
        let expected = 250_000.0 * 0.000002  // input with above_200k price
            + 250_000.0 * 0.000004           // output with above_200k price
            + 250_000.0 * 0.0000002          // cache_read with above_200k price
            + 250_000.0 * 0.000001; // cache_creation with above_200k price
        assert_eq!(cost_above, expected);
    }

    #[test]
    fn test_calculate_cost_mixed_threshold() {
        // Test: Each token type is checked INDEPENDENTLY against 200K
        // 不是總和超過 200K，而是單一類型超過 200K 就用該類型的 above_200k 價格
        let pricing = ModelPricing {
            input_cost_per_token: 0.000003,              // base: $3 per million
            output_cost_per_token: 0.000015,             // base: $15 per million
            cache_read_input_token_cost: 0.0000003,      // base: $0.3 per million
            cache_creation_input_token_cost: 0.00000375, // base: $3.75 per million
            input_cost_per_token_above_200k_tokens: 0.000006, // above: $6 per million (2x)
            output_cost_per_token_above_200k_tokens: 0.0000225, // above: $22.5 per million (1.5x)
            cache_read_input_token_cost_above_200k_tokens: 0.0000006, // above: $0.6 per million (2x)
            cache_creation_input_token_cost_above_200k_tokens: 0.0000075, // above: $7.5 per million (2x)
        };

        // Case 1: Only input_tokens exceeds 200K
        // input: 250K (above 200K) → use above_200k price
        // output: 100K (below 200K) → use base price
        // cache_read: 150K (below 200K) → use base price
        // cache_creation: 50K (below 200K) → use base price
        let cost1 = calculate_cost(250_000, 100_000, 150_000, 50_000, &pricing);
        let expected1 = 250_000.0 * 0.000006      // input: above_200k
            + 100_000.0 * 0.000015                // output: base
            + 150_000.0 * 0.0000003               // cache_read: base
            + 50_000.0 * 0.00000375; // cache_creation: base
        assert_eq!(cost1, expected1);

        // Case 2: Only output_tokens exceeds 200K
        let cost2 = calculate_cost(100_000, 250_000, 150_000, 50_000, &pricing);
        let expected2 = 100_000.0 * 0.000003      // input: base
            + 250_000.0 * 0.0000225               // output: above_200k
            + 150_000.0 * 0.0000003               // cache_read: base
            + 50_000.0 * 0.00000375; // cache_creation: base
        assert_eq!(cost2, expected2);

        // Case 3: input and cache_read exceed 200K, others don't
        let cost3 = calculate_cost(300_000, 100_000, 250_000, 50_000, &pricing);
        let expected3 = 300_000.0 * 0.000006      // input: above_200k
            + 100_000.0 * 0.000015                // output: base
            + 250_000.0 * 0.0000006               // cache_read: above_200k
            + 50_000.0 * 0.00000375; // cache_creation: base
        assert_eq!(cost3, expected3);

        // Case 4: Total > 200K but each type < 200K → all use base price
        // Total: 50K + 80K + 60K + 40K = 230K (超過 200K)
        // 但每個類型都未超過 200K，所以都用基礎價格
        let cost4 = calculate_cost(50_000, 80_000, 60_000, 40_000, &pricing);
        let expected4 = 50_000.0 * 0.000003       // input: base (< 200K)
            + 80_000.0 * 0.000015                 // output: base (< 200K)
            + 60_000.0 * 0.0000003                // cache_read: base (< 200K)
            + 40_000.0 * 0.00000375; // cache_creation: base (< 200K)
        assert_eq!(cost4, expected4);
    }

    #[test]
    fn test_calculate_cost_exactly_200k() {
        // Test boundary condition: exactly 200K tokens
        let pricing = ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            cache_read_input_token_cost: 0.0000001,
            cache_creation_input_token_cost: 0.0000005,
            input_cost_per_token_above_200k_tokens: 0.000002,
            output_cost_per_token_above_200k_tokens: 0.000004,
            cache_read_input_token_cost_above_200k_tokens: 0.0000002,
            cache_creation_input_token_cost_above_200k_tokens: 0.000001,
        };

        // Exactly 200K should use base price (> 200K triggers above_200k)
        let cost_exact = calculate_cost(200_000, 200_000, 200_000, 200_000, &pricing);
        let expected = 200_000.0 * 0.000001      // base price (not > 200K)
            + 200_000.0 * 0.000002               // base price
            + 200_000.0 * 0.0000001              // base price
            + 200_000.0 * 0.0000005; // base price
        assert_eq!(cost_exact, expected);

        // 200K + 1 should use above_200k price
        let cost_above = calculate_cost(200_001, 200_001, 200_001, 200_001, &pricing);
        let expected_above = 200_001.0 * 0.000002  // above_200k price (> 200K)
            + 200_001.0 * 0.000004                 // above_200k price
            + 200_001.0 * 0.0000002                // above_200k price
            + 200_001.0 * 0.000001; // above_200k price
        assert_eq!(cost_above, expected_above);
    }

    #[test]
    fn test_calculate_cost_fallback_to_base() {
        // Test fallback to base price when above_200k price is not available (0.0)
        // Note: In production, normalize_pricing() fills these automatically
        let mut pricing = ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            cache_read_input_token_cost: 0.0000001,
            cache_creation_input_token_cost: 0.0000005,
            ..Default::default()
        };

        // Simulate normalization: fill above_200k with base prices
        pricing.input_cost_per_token_above_200k_tokens = pricing.input_cost_per_token;
        pricing.output_cost_per_token_above_200k_tokens = pricing.output_cost_per_token;
        pricing.cache_read_input_token_cost_above_200k_tokens = pricing.cache_read_input_token_cost;
        pricing.cache_creation_input_token_cost_above_200k_tokens =
            pricing.cache_creation_input_token_cost;

        // With tokens above 200K, should use base pricing (since above_200k was filled with base)
        let cost = calculate_cost(250_000, 250_000, 250_000, 250_000, &pricing);
        let expected = 250_000.0 * 0.000001  // input with base price
            + 250_000.0 * 0.000002           // output with base price
            + 250_000.0 * 0.0000001          // cache_read with base price
            + 250_000.0 * 0.0000005; // cache_creation with base price
        assert_eq!(cost, expected);
    }

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

        let normalized = super::normalize_pricing(pricing_map);
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
