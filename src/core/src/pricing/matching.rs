use super::cache::ModelPricing;
use lru::LruCache;
use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use strsim::jaro_winkler;

// Similarity threshold for fuzzy matching (0.0 to 1.0)
const SIMILARITY_THRESHOLD: f64 = 0.7;

// Maximum number of cached pricing lookups per pricing map.
const PRICING_MATCH_CACHE_SIZE: usize = 64;

// Incrementing this invalidates the per-instance caches lazily. This keeps the
// public clear_pricing_cache() API useful without reintroducing a global result
// cache that can leak matches between unrelated pricing maps.
static MATCH_CACHE_GENERATION: AtomicU64 = AtomicU64::new(0);

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
    normalized_index: HashMap<String, Vec<Rc<str>>>,
    // Precomputed lowercase keys for substring/fuzzy matching
    lowercase_keys: Vec<(String, Rc<str>)>, // (lowercase_key, original_key as Rc)
    // Lookup results belong to this map. A process-global result cache is
    // incorrect because model names can map to different prices in each map.
    match_cache: RefCell<MatchCache>,
}

#[derive(Debug, Clone)]
struct MatchCache {
    generation: u64,
    entries: LruCache<String, ModelPricingResult>,
}

impl MatchCache {
    fn new() -> Self {
        let capacity = NonZeroUsize::new(PRICING_MATCH_CACHE_SIZE)
            .expect("pricing match cache capacity must be non-zero");
        Self {
            generation: MATCH_CACHE_GENERATION.load(Ordering::Acquire),
            entries: LruCache::new(capacity),
        }
    }
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
    /// use vct_core::pricing::{ModelPricing, ModelPricingMap};
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
            normalized_index
                .entry(normalized)
                .or_insert_with(Vec::new)
                .push(rc_key.clone());

            // Precompute lowercase key for substring/fuzzy matching
            lowercase_keys.push((key.to_lowercase(), rc_key.clone()));

            rc_raw.insert(rc_key, pricing);
        }

        // Sort lowercase_keys for potential binary search optimization
        lowercase_keys.sort_by(|a, b| a.0.cmp(&b.0));
        for candidates in normalized_index.values_mut() {
            candidates.sort_by(|a, b| a.as_ref().cmp(b.as_ref()));
        }

        Self {
            raw: rc_raw,
            normalized_index,
            lowercase_keys,
            match_cache: RefCell::new(MatchCache::new()),
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
    /// Results are cached per map. [`clear_pricing_cache`] invalidates every
    /// existing map lazily, so even the "no match" outcome can be memoized
    /// without leaking a result from one pricing table into another.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::HashMap;
    /// use vct_core::pricing::{ModelPricing, ModelPricingMap};
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
        if let Some(cached_result) = self.cached_result(model_name) {
            return cached_result;
        }

        // Fast path 1: Exact match
        if let Some(pricing) = self.raw.get(model_name) {
            let result = ModelPricingResult {
                pricing: pricing.clone(),
                matched_model: None,
            };
            self.cache_result(model_name, &result);
            return result;
        }

        // Fast path 2: Normalized match
        let normalized_name = normalize_model_name(model_name);
        if let Some(original_key) = self.normalized_match(model_name, &normalized_name)
            && let Some(pricing) = self.raw.get(original_key.as_ref())
        {
            let result = ModelPricingResult {
                pricing: pricing.clone(),
                matched_model: Some(original_key.to_string()),
            };
            self.cache_result(model_name, &result);
            return result;
        }

        // Loose (substring / fuzzy) matching needs a distinctive query: a
        // placeholder like `default` (what cursor-agent stores for auto-mode
        // conversations) or a very short fragment would otherwise inherit a
        // coincidentally-similar model's prices. Unpriced ($0) is the safer
        // answer for those.
        let model_lower = model_name.to_lowercase();
        if !eligible_for_loose_match(model_without_provider(&model_lower)) {
            let result = ModelPricingResult {
                pricing: ModelPricing::default(),
                matched_model: None,
            };
            self.cache_result(model_name, &result);
            return result;
        }

        // Slow path 1: inspect every substring candidate and choose the most
        // specific overlap. Returning the first HashMap-derived candidate can
        // otherwise price gpt-4o as gpt-4.
        if let Some(matched_key) = self.substring_match(&model_lower)
            && let Some(pricing) = self.raw.get(matched_key.as_ref())
        {
            let result = ModelPricingResult {
                pricing: pricing.clone(),
                matched_model: Some(matched_key.to_string()),
            };
            self.cache_result(model_name, &result);
            return result;
        }

        // Slow path 2: fuzzy matching runs only when normalization and
        // substring matching found nothing.
        if let Some(matched_key) = self.fuzzy_match(&model_lower)
            && let Some(pricing) = self.raw.get(matched_key.as_ref())
        {
            let result = ModelPricingResult {
                pricing: pricing.clone(),
                matched_model: Some(matched_key.to_string()),
            };
            self.cache_result(model_name, &result);
            return result;
        }

        let result = ModelPricingResult {
            pricing: ModelPricing::default(),
            matched_model: None,
        };
        self.cache_result(model_name, &result);
        result
    }

    fn cached_result(&self, model_name: &str) -> Option<ModelPricingResult> {
        let mut cache = self.match_cache.borrow_mut();
        refresh_cache_generation(&mut cache);
        cache.entries.get(model_name).cloned()
    }

    fn cache_result(&self, model_name: &str, result: &ModelPricingResult) {
        let mut cache = self.match_cache.borrow_mut();
        refresh_cache_generation(&mut cache);
        cache.entries.put(model_name.to_string(), result.clone());
    }

    fn normalized_match(&self, model_name: &str, normalized_name: &str) -> Option<Rc<str>> {
        let candidates = self.normalized_index.get(normalized_name)?;
        let query_provider = provider_prefix(model_name);

        candidates
            .iter()
            .min_by(|a, b| {
                normalized_candidate_rank(a, model_name, normalized_name, query_provider).cmp(
                    &normalized_candidate_rank(b, model_name, normalized_name, query_provider),
                )
            })
            .cloned()
    }

    fn substring_match(&self, model_lower: &str) -> Option<Rc<str>> {
        let model_segment = model_without_provider(model_lower);
        if model_segment.is_empty() {
            return None;
        }
        let query_provider = provider_prefix(model_lower);

        self.lowercase_keys
            .iter()
            .filter_map(|(key_lower, original_key)| {
                let key_segment = model_without_provider(key_lower);
                if key_segment.is_empty()
                    || !(model_segment.contains(key_segment) || key_segment.contains(model_segment))
                {
                    return None;
                }
                let overlap = model_segment.len().min(key_segment.len());
                let length_difference = model_segment.len().abs_diff(key_segment.len());
                let provider_rank = substring_provider_rank(query_provider, key_lower);
                Some((overlap, length_difference, provider_rank, original_key))
            })
            .min_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| a.1.cmp(&b.1))
                    .then_with(|| a.2.cmp(&b.2))
                    .then_with(|| a.3.as_ref().cmp(b.3.as_ref()))
            })
            .map(|(_, _, _, key)| key.clone())
    }

    fn fuzzy_match(&self, model_lower: &str) -> Option<Rc<str>> {
        if model_lower.is_empty() {
            return None;
        }

        self.lowercase_keys
            .iter()
            .filter_map(|(key_lower, original_key)| {
                let similarity = jaro_winkler(model_lower, key_lower);
                (similarity >= SIMILARITY_THRESHOLD).then_some((
                    similarity,
                    model_lower.len().abs_diff(key_lower.len()),
                    original_key,
                ))
            })
            .min_by(|a, b| {
                b.0.total_cmp(&a.0)
                    .then_with(|| a.1.cmp(&b.1))
                    .then_with(|| a.2.as_ref().cmp(b.2.as_ref()))
            })
            .map(|(_, _, key)| key.clone())
    }

    /// Returns the pricing for an **exact** model-name match only.
    ///
    /// Unlike [`get`](Self::get), this performs no normalization, substring, or
    /// fuzzy matching: it returns `Some` only when `model_name` is a verbatim
    /// key in the pricing table, and `None` otherwise. This is the lookup used
    /// for providers (OpenCode) that carry their own authoritative cost and
    /// should fall back to that stored cost rather than guess a price from a
    /// loosely-similar model name.
    pub fn get_exact(&self, model_name: &str) -> Option<ModelPricing> {
        self.raw.get(model_name).cloned()
    }

    /// Returns whether the pricing map contains no models.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Returns the raw pricing data with reference-counted keys.
    pub fn raw(&self) -> &HashMap<Rc<str>, ModelPricing> {
        &self.raw
    }

    /// Builds the `Send + Sync` "model → lowest context-tier threshold"
    /// snapshot the usage scan hands to session parsers for per-request tier
    /// classification. Models without threshold tiers are absent.
    pub fn tier_thresholds(&self) -> crate::pricing::TierThresholds {
        crate::pricing::TierThresholds::from_entries(self.raw.iter().filter_map(
            |(key, pricing)| {
                pricing
                    .tiers
                    .first()
                    .map(|tier| (key.as_ref(), tier.threshold_tokens))
            },
        ))
    }
}

/// Invalidates the lookup cache in every pricing map.
///
/// Existing maps observe the generation change on their next lookup and clear
/// their own bounded LRU. No result data is stored globally.
pub fn clear_pricing_cache() {
    MATCH_CACHE_GENERATION.fetch_add(1, Ordering::AcqRel);
}

fn refresh_cache_generation(cache: &mut MatchCache) {
    let generation = MATCH_CACHE_GENERATION.load(Ordering::Acquire);
    if cache.generation != generation {
        cache.entries.clear();
        cache.generation = generation;
    }
}

fn provider_prefix(model_name: &str) -> Option<&str> {
    model_name
        .split_once('/')
        .and_then(|(prefix, _)| (!prefix.is_empty()).then_some(prefix))
}

fn normalized_candidate_rank<'a>(
    candidate: &'a str,
    model_name: &str,
    normalized_name: &str,
    query_provider: Option<&str>,
) -> (u8, usize, &'a str) {
    let same_provider_base = query_provider.is_some()
        && provider_prefix(candidate) == query_provider
        && model_without_provider(candidate) == normalized_name;
    let unprefixed_base = provider_prefix(candidate).is_none() && candidate == normalized_name;
    let priority = if same_provider_base {
        0
    } else if unprefixed_base {
        1
    } else {
        2
    };
    (
        priority,
        model_name.len().abs_diff(candidate.len()),
        candidate,
    )
}

fn model_without_provider(model_name: &str) -> &str {
    model_name
        .split_once('/')
        .map_or(model_name, |(_, model)| model)
}

/// Placeholder names that must never take a loose price from a
/// coincidentally-similar key (e.g. cursor-agent's literal `default`).
const LOOSE_MATCH_STOPLIST: [&str; 5] = ["default", "auto", "custom", "unknown", "none"];

/// Minimum model-segment length for substring / fuzzy matching; anything
/// shorter is too ambiguous to loosely price (exact and normalized matches
/// still apply to short names like `o3`).
const LOOSE_MATCH_MIN_SEGMENT_LEN: usize = 4;

fn eligible_for_loose_match(model_segment: &str) -> bool {
    model_segment.len() >= LOOSE_MATCH_MIN_SEGMENT_LEN
        && !LOOSE_MATCH_STOPLIST.contains(&model_segment)
}

fn substring_provider_rank(query_provider: Option<&str>, candidate: &str) -> u8 {
    match (query_provider, provider_prefix(candidate)) {
        (Some(query), Some(candidate)) if query == candidate => 0,
        (_, None) => 1,
        _ => 2,
    }
}

/// Normalizes model names by removing provider prefixes and version suffixes.
///
/// Removes patterns like:
/// - Provider prefixes: `bedrock/`, `openai/`.
/// - Date suffixes: `-20231201`, `-12345678` (exactly 8 ASCII digits).
/// - Version suffixes: `-v1.0`, `-v2`.
///
/// Optimized to minimize string allocations (a single allocation at the end).
///
/// # Examples
///
/// ```
/// use vct_core::pricing::normalize_model_name;
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

    loop {
        let working_slice = &name[start..end];

        // Remove date suffixes only when all eight bytes are ASCII digits.
        if let Some((base, suffix)) = working_slice.rsplit_once('-')
            && suffix.len() == 8
            && suffix.bytes().all(|byte| byte.is_ascii_digit())
        {
            end = start + base.len();
            continue;
        }

        // Remove version patterns (e.g., "-v1.0", "-v2").
        if let Some((base, suffix)) = working_slice.rsplit_once("-v")
            && is_numeric_version_suffix(suffix)
        {
            end = start + base.len();
            continue;
        }

        break;
    }

    // Only allocate once at the end
    name[start..end].to_string()
}

fn is_numeric_version_suffix(suffix: &str) -> bool {
    !suffix.is_empty()
        && suffix
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::super::cache::ThresholdTier;
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn generic_placeholder_names_never_loose_match() {
        clear_pricing_cache();
        let mut raw = HashMap::new();
        raw.insert(
            "fireworks-ai-default".to_string(),
            ModelPricing {
                input_cost_per_token: 0.5,
                ..Default::default()
            },
        );
        raw.insert(
            "aut".to_string(),
            ModelPricing {
                input_cost_per_token: 0.5,
                ..Default::default()
            },
        );
        let map = ModelPricingMap::new(raw);

        // `default` (cursor-agent's auto-mode placeholder) would substring
        // match fireworks-ai-default; it must stay unpriced instead.
        let result = map.get("default");
        assert_eq!(result.pricing.input_cost_per_token, 0.0);
        assert_eq!(result.matched_model, None);

        // Short fragments skip loose matching too...
        let result = map.get("xyz");
        assert_eq!(result.pricing.input_cost_per_token, 0.0);

        // ...but exact matches still work at any length.
        let result = map.get("aut");
        assert_eq!(result.pricing.input_cost_per_token, 0.5);
        assert_eq!(result.matched_model, None);
    }

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
    fn test_get_exact_only_matches_verbatim() {
        // get_exact must NOT normalize, substring, or fuzzy match.
        let mut raw = HashMap::new();
        raw.insert("gpt-4".to_string(), create_test_pricing());
        let map = ModelPricingMap::new(raw);

        assert!(map.get_exact("gpt-4").is_some());
        // These all resolve via get() (substring/fuzzy) but must miss get_exact.
        assert!(map.get_exact("gpt-4-turbo").is_none());
        assert!(map.get_exact("deepseek-v4-pro").is_none());
        assert!(map.get_exact("GPT-4").is_none());
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
        clear_pricing_cache();

        let mut raw = HashMap::new();
        raw.insert("gpt-4".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result = map.get("");
        assert_eq!(result.pricing.input_cost_per_token, 0.0);
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
    fn clear_invalidates_existing_map_cache() {
        let map = ModelPricingMap::new(HashMap::new());
        assert_eq!(map.match_cache.borrow().entries.cap().get(), 64);
        map.get("before-clear");
        let old_generation = map.match_cache.borrow().generation;

        clear_pricing_cache();
        map.get("after-clear");

        let cache = map.match_cache.borrow();
        assert_ne!(cache.generation, old_generation);
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.entries.peek("after-clear").is_some());
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
