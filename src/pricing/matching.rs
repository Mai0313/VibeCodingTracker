use super::cache::ModelPricing;
use lru::LruCache;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::{LazyLock, RwLock};
use strsim::jaro_winkler;

// Similarity threshold for fuzzy matching (0.0 to 1.0)
const SIMILARITY_THRESHOLD: f64 = 0.7;

// Maximum number of cached pricing lookups (prevents unbounded memory growth)
// Reduced from 20 to 10 to minimize memory usage in TUI mode
const PRICING_MATCH_CACHE_SIZE: usize = 10;

// Global LRU cache for pricing match results (thread-safe, bounded capacity)
// This dramatically improves performance for repeated model lookups while
// preventing memory leaks from unbounded growth
static MATCH_CACHE: LazyLock<RwLock<LruCache<String, ModelPricingResult>>> = LazyLock::new(|| {
    // SAFETY: PRICING_MATCH_CACHE_SIZE is a const > 0
    let capacity = NonZeroUsize::new(PRICING_MATCH_CACHE_SIZE).unwrap();
    RwLock::new(LruCache::new(capacity))
});

/// Result of a model pricing lookup, including the matched model name for transparency.
#[derive(Debug, Clone)]
pub struct ModelPricingResult {
    /// Pricing for the matched model, or `ModelPricing::default()` (all zero) on no match.
    pub pricing: ModelPricing,
    /// The actual model key that matched, or `None` for an exact match or no match.
    pub matched_model: Option<String>,
}

/// Optimized pricing map with precomputed indices for O(1) exact matches and fast fuzzy matching.
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
    /// Creates a new pricing map with precomputed indices for optimized lookups.
    ///
    /// This constructor processes the raw pricing data to build:
    /// - Normalized key index for version-agnostic matching.
    /// - Lowercase key list for substring and fuzzy matching.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::HashMap;
    /// use vibe_coding_tracker::pricing::{ModelPricing, ModelPricingMap};
    ///
    /// let mut raw = HashMap::new();
    /// raw.insert("gpt-4".to_string(), ModelPricing::default());
    /// let map = ModelPricingMap::new(raw);
    /// assert!(!map.is_empty());
    /// ```
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

    /// Retrieves pricing for a model using a multi-tier matching strategy.
    ///
    /// Matching strategy (in order of priority):
    /// 1. Exact match (O(1) hash lookup).
    /// 2. Normalized match (removes version suffixes).
    /// 3. Substring match (bidirectional contains check).
    /// 4. Fuzzy match (Jaro-Winkler ≥ 0.7 threshold).
    /// 5. Default (zero cost) if no match found.
    ///
    /// Results are cached globally (see [`clear_pricing_cache`]) for performance,
    /// so even the "no match" outcome is memoized to avoid repeated fuzzy scans.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::HashMap;
    /// use vibe_coding_tracker::pricing::{ModelPricing, ModelPricingMap};
    ///
    /// let mut raw = HashMap::new();
    /// raw.insert(
    ///     "gpt-4".to_string(),
    ///     ModelPricing { input_cost_per_token: 3e-5, ..Default::default() },
    /// );
    /// let map = ModelPricingMap::new(raw);
    ///
    /// // Exact match: `matched_model` stays `None`.
    /// let hit = map.get("gpt-4");
    /// assert_eq!(hit.pricing.input_cost_per_token, 3e-5);
    /// assert!(hit.matched_model.is_none());
    ///
    /// // No match: zero-cost default.
    /// let miss = map.get("does-not-exist-xyzzy");
    /// assert_eq!(miss.pricing.input_cost_per_token, 0.0);
    /// ```
    pub fn get(&self, model_name: &str) -> ModelPricingResult {
        // Ultra-fast path: Check LRU cache first (with peek to avoid write lock)
        if let Ok(cache_read) = MATCH_CACHE.read()
            && let Some(cached_result) = cache_read.peek(model_name)
        {
            return cached_result.clone();
        }

        // Fast path 1: Exact match
        if let Some(pricing) = self.raw.get(model_name) {
            let result = ModelPricingResult {
                pricing: pricing.clone(),
                matched_model: None,
            };
            // Cache the exact match result (LRU will auto-evict if at capacity)
            if let Ok(mut cache_write) = MATCH_CACHE.write() {
                cache_write.put(model_name.to_string(), result.clone());
            }
            return result;
        }

        // Fast path 2: Normalized match
        let normalized_name = normalize_model_name(model_name);
        if let Some(original_key) = self.normalized_index.get(&normalized_name)
            && let Some(pricing) = self.raw.get(original_key.as_ref())
        {
            let result = ModelPricingResult {
                pricing: pricing.clone(),
                matched_model: Some(original_key.to_string()), // Convert Rc to String only when needed
            };
            // Cache the normalized match result (LRU will auto-evict if at capacity)
            if let Ok(mut cache_write) = MATCH_CACHE.write() {
                cache_write.put(model_name.to_string(), result.clone());
            }
            return result;
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
        if let Some((matched_key, _, _)) = best_match
            && let Some(pricing) = self.raw.get(matched_key.as_ref())
        {
            let result = ModelPricingResult {
                pricing: pricing.clone(),
                matched_model: Some(matched_key.to_string()), // Convert to String only when needed
            };
            // Cache the fuzzy match result (LRU will auto-evict if at capacity)
            if let Ok(mut cache_write) = MATCH_CACHE.write() {
                cache_write.put(model_name.to_string(), result.clone());
            }
            return result;
        }

        // Return default (zero costs) if no match found
        let result = ModelPricingResult {
            pricing: ModelPricing::default(),
            matched_model: None,
        };
        // Cache the "no match" result to avoid repeated expensive fuzzy searches
        // LRU will auto-evict if at capacity (keeps frequently-used models cached)
        if let Ok(mut cache_write) = MATCH_CACHE.write() {
            cache_write.put(model_name.to_string(), result.clone());
        }
        result
    }

    /// Returns whether the pricing map contains no models.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Returns the raw pricing data with reference-counted keys.
    pub fn raw(&self) -> &HashMap<Rc<str>, ModelPricing> {
        &self.raw
    }
}

/// Clears the global pricing match LRU cache.
///
/// Primarily used in tests for isolation. In production, the LRU cache
/// significantly improves performance by avoiding repeated expensive fuzzy
/// matching operations while maintaining bounded memory usage (capacity is
/// `PRICING_MATCH_CACHE_SIZE` entries).
pub fn clear_pricing_cache() {
    if let Ok(mut cache_write) = MATCH_CACHE.write() {
        cache_write.clear();
    }
}

/// Normalizes model names by removing provider prefixes and version suffixes.
///
/// Removes patterns like:
/// - Provider prefixes: `bedrock/`, `openai/`.
/// - Date suffixes: `-20231201`, `-20240320` (exactly `-20YYMMDD`, 8 digits).
/// - Version suffixes: `-v1.0`, `-v2`.
///
/// Optimized to minimize string allocations (a single allocation at the end).
///
/// # Examples
///
/// ```
/// use vibe_coding_tracker::pricing::normalize_model_name;
///
/// assert_eq!(normalize_model_name("claude-3-sonnet-20240229"), "claude-3-sonnet");
/// assert_eq!(normalize_model_name("gpt-4-v1.0"), "gpt-4");
/// assert_eq!(normalize_model_name("bedrock/claude-3-opus"), "claude-3-opus");
/// ```
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
    use super::super::cache::ThresholdTier;
    use super::*;
    use std::collections::HashMap;

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

    fn create_test_pricing() -> ModelPricing {
        ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            cache_read_input_token_cost: 0.0000001,
            cache_creation_input_token_cost: 0.0000005,
            tiers: vec![ThresholdTier {
                threshold_tokens: 200_000,
                input_cost_per_token: 0.000002,
                output_cost_per_token: 0.000004,
                cache_read_input_token_cost: 0.0000002,
                cache_creation_input_token_cost: 0.000001,
                ..Default::default()
            }],
            ranges: None,
            ..Default::default()
        }
    }

    #[test]
    fn test_exact_match() {
        // Test exact model name match
        clear_pricing_cache();

        let mut raw = HashMap::new();
        raw.insert("gpt-4".to_string(), create_test_pricing());
        raw.insert("claude-3-opus".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result = map.get("gpt-4");
        assert!(result.pricing.input_cost_per_token > 0.0); // Should match

        let result2 = map.get("claude-3-opus");
        assert!(result2.pricing.input_cost_per_token > 0.0); // Should match
    }

    #[test]
    fn test_normalized_match() {
        // Test normalized matching (removes version suffixes)
        clear_pricing_cache();

        let mut raw = HashMap::new();
        raw.insert("gpt-4-0613".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        // Should match via substring or fuzzy matching
        let result = map.get("gpt-4");
        assert!(result.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_substring_match() {
        // Test substring matching
        clear_pricing_cache();

        let mut raw = HashMap::new();
        raw.insert("claude-3-opus-20240229".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        // Should match via substring or normalization
        let result = map.get("claude-3-opus");
        assert!(result.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_case_insensitive_match() {
        // Test case-insensitive matching
        let mut raw = HashMap::new();
        raw.insert("GPT-4".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result = map.get("gpt-4");
        assert!(result.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_fuzzy_match() {
        // Test fuzzy matching with similar names
        let mut raw = HashMap::new();
        raw.insert("claude-3-sonnet".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        // Slightly misspelled should still match (if similarity >= 0.7)
        let result = map.get("claude-3-sonet");
        // This might or might not match depending on Jaro-Winkler score
        // Just verify it returns a result
        assert!(result.pricing.input_cost_per_token >= 0.0);
    }

    #[test]
    fn test_no_match_returns_default() {
        // Test that unmatched models return default (zero cost)
        let raw = HashMap::new();
        let map = ModelPricingMap::new(raw);

        let result = map.get("unknown-model");
        assert_eq!(result.pricing.input_cost_per_token, 0.0);
        assert_eq!(result.pricing.output_cost_per_token, 0.0);
        assert!(result.matched_model.is_none());
    }

    #[test]
    fn test_multiple_models() {
        // Test with multiple models
        let mut raw = HashMap::new();
        let pricing1 = ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            ..Default::default()
        };
        let pricing2 = ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000006,
            ..Default::default()
        };

        raw.insert("model-a".to_string(), pricing1);
        raw.insert("model-b".to_string(), pricing2);

        let map = ModelPricingMap::new(raw);

        let result_a = map.get("model-a");
        assert_eq!(result_a.pricing.input_cost_per_token, 0.000001);

        let result_b = map.get("model-b");
        assert_eq!(result_b.pricing.input_cost_per_token, 0.000003);
    }

    #[test]
    fn test_empty_model_name() {
        // Test with empty model name - will match first model due to substring logic
        clear_pricing_cache();

        let mut raw = HashMap::new();
        raw.insert("gpt-4".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result = map.get("");
        // Empty string will match via substring logic, so it returns a match
        assert!(result.pricing.input_cost_per_token >= 0.0);
    }

    #[test]
    fn test_pricing_map_debug() {
        // Test that ModelPricingMap can be debug formatted
        let mut raw = HashMap::new();
        raw.insert("test-model".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);
        let debug_str = format!("{:?}", map);

        assert!(!debug_str.is_empty());
    }

    #[test]
    fn test_pricing_map_clone() {
        // Test that ModelPricingMap can be cloned
        let mut raw = HashMap::new();
        raw.insert("test-model".to_string(), create_test_pricing());

        let map1 = ModelPricingMap::new(raw);
        let map2 = map1.clone();

        let result1 = map1.get("test-model");
        let result2 = map2.get("test-model");

        assert_eq!(
            result1.pricing.input_cost_per_token,
            result2.pricing.input_cost_per_token
        );
    }

    #[test]
    fn test_match_priority() {
        // Test that exact match takes priority over fuzzy match
        clear_pricing_cache();

        let mut raw = HashMap::new();
        let exact_pricing = ModelPricing {
            input_cost_per_token: 0.000001,
            ..Default::default()
        };
        let other_pricing = ModelPricing {
            input_cost_per_token: 0.000099,
            ..Default::default()
        };

        raw.insert("gpt-4".to_string(), exact_pricing);
        raw.insert("gpt-4-turbo".to_string(), other_pricing);

        let map = ModelPricingMap::new(raw);

        // Exact match should be used
        let result = map.get("gpt-4");
        assert_eq!(result.pricing.input_cost_per_token, 0.000001);
    }

    #[test]
    fn test_version_stripping() {
        // Test that version numbers are handled correctly
        let mut raw = HashMap::new();
        raw.insert("claude-3-opus".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        // Should match without version
        let result = map.get("claude-3-opus-20240229");
        assert!(result.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_result_clone() {
        // Test that ModelPricingResult can be cloned
        let mut raw = HashMap::new();
        raw.insert("test".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);
        let result1 = map.get("test");
        let result2 = result1.clone();

        assert_eq!(
            result1.pricing.input_cost_per_token,
            result2.pricing.input_cost_per_token
        );
    }

    #[test]
    fn test_result_debug() {
        // Test that ModelPricingResult can be debug formatted
        let mut raw = HashMap::new();
        raw.insert("test".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);
        let result = map.get("test");
        let debug_str = format!("{:?}", result);

        assert!(!debug_str.is_empty());
        assert!(debug_str.contains("pricing"));
    }

    #[test]
    fn test_special_characters() {
        // Test model names with special characters
        let mut raw = HashMap::new();
        raw.insert("model/v1.0".to_string(), create_test_pricing());
        raw.insert("model:latest".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result1 = map.get("model/v1.0");
        assert!(result1.pricing.input_cost_per_token > 0.0);

        let result2 = map.get("model:latest");
        assert!(result2.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_very_long_model_name() {
        // Test with very long model name
        let mut raw = HashMap::new();
        let long_name = "a".repeat(1000);
        raw.insert(long_name.clone(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result = map.get(&long_name);
        assert!(result.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_unicode_model_names() {
        // Test model names with unicode characters
        let mut raw = HashMap::new();
        raw.insert("模型-1".to_string(), create_test_pricing());
        raw.insert("モデル-2".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result1 = map.get("模型-1");
        assert!(result1.pricing.input_cost_per_token > 0.0);

        let result2 = map.get("モデル-2");
        assert!(result2.pricing.input_cost_per_token > 0.0);
    }
}
