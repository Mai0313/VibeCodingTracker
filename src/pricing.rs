use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use strsim::jaro_winkler;

const LITELLM_PRICING_URL: &str =
    "https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json";

// Similarity threshold for fuzzy matching (0.0 to 1.0)
const SIMILARITY_THRESHOLD: f64 = 0.7;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Fetch model pricing from LiteLLM repository
pub fn fetch_model_pricing() -> Result<HashMap<String, ModelPricing>> {
    let response = reqwest::blocking::get(LITELLM_PRICING_URL)
        .context("Failed to fetch model pricing from LiteLLM")?;

    let pricing: HashMap<String, ModelPricing> = response
        .json()
        .context("Failed to parse model pricing JSON")?;

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
            pricing: pricing.clone(),
            matched_model: None,
        };
    }

    // Try to find a match by removing version suffixes or provider prefixes
    let normalized_name = normalize_model_name(model_name);
    if let Some(pricing) = pricing_map.get(&normalized_name) {
        return ModelPricingResult {
            pricing: pricing.clone(),
            matched_model: Some(normalized_name),
        };
    }

    // Try to find a partial match (substring)
    for (key, pricing) in pricing_map {
        if model_name.contains(key) || key.contains(model_name) {
            return ModelPricingResult {
                pricing: pricing.clone(),
                matched_model: Some(key.clone()),
            };
        }
    }

    // Try fuzzy matching based on string similarity
    let mut best_match: Option<(String, f64)> = None;
    let model_lower = model_name.to_lowercase();

    for key in pricing_map.keys() {
        let key_lower = key.to_lowercase();
        let similarity = jaro_winkler(&model_lower, &key_lower);

        if similarity >= SIMILARITY_THRESHOLD {
            if let Some((_, best_similarity)) = &best_match {
                if similarity > *best_similarity {
                    best_match = Some((key.clone(), similarity));
                }
            } else {
                best_match = Some((key.clone(), similarity));
            }
        }
    }

    if let Some((matched_key, _)) = best_match {
        if let Some(pricing) = pricing_map.get(&matched_key) {
            return ModelPricingResult {
                pricing: pricing.clone(),
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
        if normalized[idx + 1..].len() == 9 {
            // "-20YYMMDD" pattern
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
        assert_eq!(normalize_model_name("bedrock/claude-3-opus"), "claude-3-opus");
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
