use super::cache::ModelPricing;
use std::collections::HashMap;
use strsim::jaro_winkler;

// Similarity threshold for fuzzy matching (0.0 to 1.0)
const SIMILARITY_THRESHOLD: f64 = 0.7;

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
        // Pre-allocate with exact capacity
        let capacity = raw.len();
        let mut normalized_index = HashMap::with_capacity(capacity);
        let mut lowercase_keys = Vec::with_capacity(capacity);

        for key in raw.keys() {
            // Precompute normalized key
            let normalized = normalize_model_name(key);
            if normalized != *key {
                normalized_index.insert(normalized, key.clone());
            }

            // Precompute lowercase key for substring/fuzzy matching
            lowercase_keys.push((key.to_lowercase(), key.clone()));
        }

        // Sort lowercase_keys for potential binary search optimization
        lowercase_keys.sort_by(|a, b| a.0.cmp(&b.0));

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

        // Slow path: Substring and fuzzy matching (optimized)
        let model_lower = model_name.to_lowercase();
        let mut best_match: Option<(String, f64, bool)> = None; // (key, score, is_substring)

        for (key_lower, original_key) in &self.lowercase_keys {
            // Substring matching (higher priority, score = 1.0)
            if (model_lower.contains(key_lower) || key_lower.contains(&model_lower))
                && (best_match.is_none() || !best_match.as_ref().unwrap().2)
            {
                best_match = Some((original_key.clone(), 1.0, true));
                // Early exit if exact substring match found
                if model_lower == *key_lower {
                    break;
                }
            }

            // Fuzzy matching (only if no substring match yet)
            if best_match.is_none() || best_match.as_ref().unwrap().1 < 1.0 {
                let similarity = jaro_winkler(&model_lower, key_lower);
                if similarity >= SIMILARITY_THRESHOLD {
                    if let Some((_, best_score, is_sub)) = &best_match {
                        if !is_sub && similarity > *best_score {
                            best_match = Some((original_key.clone(), similarity, false));
                        }
                    } else {
                        best_match = Some((original_key.clone(), similarity, false));
                    }
                }
            }
        }

        // Return best match if found
        if let Some((matched_key, _, _)) = best_match {
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

/// Normalize model name by removing common version suffixes and prefixes
pub fn normalize_model_name(name: &str) -> String {
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
}
