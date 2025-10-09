use super::cache::ModelPricing;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{LazyLock, RwLock};
use strsim::jaro_winkler;

// Similarity threshold for fuzzy matching (0.0 to 1.0)
const SIMILARITY_THRESHOLD: f64 = 0.7;

// Global cache for pricing match results (thread-safe)
// This dramatically improves performance for repeated model lookups
static MATCH_CACHE: LazyLock<RwLock<HashMap<String, ModelPricingResult>>> =
    LazyLock::new(|| RwLock::new(HashMap::with_capacity(50)));

/// Result of model pricing lookup with optional matched model name
#[derive(Debug, Clone)]
pub struct ModelPricingResult {
    pub pricing: ModelPricing,
    pub matched_model: Option<String>,
}

/// Optimized pricing map with precomputed indices for fast lookups
#[derive(Debug, Clone)]
pub struct ModelPricingMap {
    // Original pricing data (use Rc<str> to avoid cloning keys)
    raw: HashMap<Rc<str>, ModelPricing>,
    // Precomputed normalized keys for fast matching
    normalized_index: HashMap<String, Rc<str>>, // normalized_key -> original_key (Rc)
    // Precomputed lowercase keys for substring/fuzzy matching
    lowercase_keys: Vec<(String, Rc<str>)>, // (lowercase_key, original_key as Rc)
}

impl ModelPricingMap {
    /// Create a new ModelPricingMap with precomputed indices
    pub fn new(raw: HashMap<String, ModelPricing>) -> Self {
        // Pre-allocate with exact capacity
        let capacity = raw.len();
        let mut normalized_index = HashMap::with_capacity(capacity);
        let mut lowercase_keys = Vec::with_capacity(capacity);
        let mut rc_raw = HashMap::with_capacity(capacity);

        // Convert keys to Rc<str> to avoid cloning
        for (key, pricing) in raw {
            let rc_key: Rc<str> = key.as_str().into();

            // Precompute normalized key
            let normalized = normalize_model_name(&key);
            if normalized != key {
                normalized_index.insert(normalized, rc_key.clone());
            }

            // Precompute lowercase key for substring/fuzzy matching
            lowercase_keys.push((key.to_lowercase(), rc_key.clone()));

            rc_raw.insert(rc_key, pricing);
        }

        // Sort lowercase_keys for potential binary search optimization
        lowercase_keys.sort_by(|a, b| a.0.cmp(&b.0));

        Self {
            raw: rc_raw,
            normalized_index,
            lowercase_keys,
        }
    }

    /// Get pricing for a specific model with optimized matching
    pub fn get(&self, model_name: &str) -> ModelPricingResult {
        // Ultra-fast path: Check cache first
        if let Ok(cache) = MATCH_CACHE.read() {
            if let Some(cached_result) = cache.get(model_name) {
                return cached_result.clone();
            }
        }

        // Fast path 1: Exact match
        if let Some(pricing) = self.raw.get(model_name) {
            let result = ModelPricingResult {
                pricing: *pricing,
                matched_model: None,
            };
            // Cache the exact match result
            if let Ok(mut cache) = MATCH_CACHE.write() {
                cache.insert(model_name.to_string(), result.clone());
            }
            return result;
        }

        // Fast path 2: Normalized match
        let normalized_name = normalize_model_name(model_name);
        if let Some(original_key) = self.normalized_index.get(&normalized_name) {
            if let Some(pricing) = self.raw.get(original_key.as_ref()) {
                let result = ModelPricingResult {
                    pricing: *pricing,
                    matched_model: Some(original_key.to_string()), // Convert Rc to String only when needed
                };
                // Cache the normalized match result
                if let Ok(mut cache) = MATCH_CACHE.write() {
                    cache.insert(model_name.to_string(), result.clone());
                }
                return result;
            }
        }

        // Slow path: Substring and fuzzy matching (optimized)
        let model_lower = model_name.to_lowercase();
        let mut best_match: Option<(Rc<str>, f64, bool)> = None; // (Rc key, score, is_substring)

        for (key_lower, original_key) in &self.lowercase_keys {
            // Substring matching (higher priority, score = 1.0)
            if (model_lower.contains(key_lower) || key_lower.contains(&model_lower))
                && (best_match.is_none() || !best_match.as_ref().unwrap().2)
            {
                best_match = Some((original_key.clone(), 1.0, true)); // Clone Rc is cheap (just inc ref count)
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
            if let Some(pricing) = self.raw.get(matched_key.as_ref()) {
                let result = ModelPricingResult {
                    pricing: *pricing,
                    matched_model: Some(matched_key.to_string()), // Convert to String only when needed
                };
                // Cache the fuzzy match result
                if let Ok(mut cache) = MATCH_CACHE.write() {
                    cache.insert(model_name.to_string(), result.clone());
                }
                return result;
            }
        }

        // Return default (zero costs) if no match found
        let result = ModelPricingResult {
            pricing: ModelPricing::default(),
            matched_model: None,
        };
        // Cache the "no match" result to avoid repeated expensive fuzzy searches
        if let Ok(mut cache) = MATCH_CACHE.write() {
            cache.insert(model_name.to_string(), result.clone());
        }
        result
    }

    /// Check if the pricing map is empty
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Get the raw pricing map (for backward compatibility)
    /// Note: Returns HashMap<Rc<str>, ModelPricing> instead of HashMap<String, ModelPricing>
    pub fn raw(&self) -> &HashMap<Rc<str>, ModelPricing> {
        &self.raw
    }
}

/// Clear the global pricing match cache
///
/// **Note**: This function is primarily intended for testing to ensure test isolation.
/// In production code, the cache helps improve performance by avoiding repeated
/// expensive fuzzy matching operations.
pub fn clear_pricing_cache() {
    if let Ok(mut cache) = MATCH_CACHE.write() {
        cache.clear();
    }
}

/// Normalize model name by removing common version suffixes and prefixes
/// Optimized to minimize allocations
pub fn normalize_model_name(name: &str) -> String {
    let mut start = 0;
    let mut end = name.len();

    // Remove provider prefixes (e.g., "bedrock/", "openai/") - do this first
    if let Some(idx) = name.find('/') {
        start = idx + 1;
    }

    // Work with the slice after removing prefix
    let working_slice = &name[start..end];

    // Remove common date patterns (e.g., "-20231201", "-20240320")
    if let Some(idx) = working_slice.rfind("-20") {
        let suffix_start = idx + 1; // Points to '2' in "-20..."
        if working_slice.len() - suffix_start == 8 {
            // "-20YYMMDD" pattern (8 chars: 20YYMMDD)
            end = start + idx;
        }
    }

    // Remove version patterns (e.g., "-v1.0", "-v2")
    let working_slice2 = &name[start..end];
    if let Some(idx) = working_slice2.rfind("-v") {
        end = start + idx;
    }

    // Only allocate once at the end
    name[start..end].to_string()
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
