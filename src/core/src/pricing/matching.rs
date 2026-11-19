//! Multi-stage model name matching, normalization, and tier-boundary derivation.

use super::cache::ModelPricing;
use crate::utils::MAX_TIER_LEVELS;
use lru::LruCache;
use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use strsim::jaro_winkler;

// Jaro-Winkler score at or above which a fuzzy candidate is accepted.
const SIMILARITY_THRESHOLD: f64 = 0.7;

// Per-map LRU capacity for pricing lookups.
const PRICING_MATCH_CACHE_SIZE: usize = 64;

// Bumped by `clear_pricing_cache()` to lazily invalidate every `ModelPricingMap`'s local LRU without sharing results across maps.
static MATCH_CACHE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Result of a [`ModelPricingMap::get`] lookup.
#[derive(Debug, Clone)]
pub struct ModelPricingResult {
    /// Pricing for the matched model, or `ModelPricing::default()` (all zero) on no match.
    pub pricing: ModelPricing,
    /// The actual model key that matched, or `None` for an exact match or no match.
    pub matched_model: Option<String>,
}

/// Model pricing table with precomputed indices for exact, normalized, substring, and fuzzy lookups.
///
/// Not `Send` or `Sync`; use [`ModelPricingMap::tier_thresholds`] to share per-request tier boundaries across threads.
#[derive(Debug, Clone)]
pub struct ModelPricingMap {
    // Keys are shared across indices via `Rc<str>`.
    raw: HashMap<Rc<str>, ModelPricing>,
    normalized_index: HashMap<String, Vec<Rc<str>>>,
    // `(lowercase_key, original_key)` pairs; order is unobservable because substring and fuzzy scans tie-break on the original key.
    lowercase_keys: Vec<(String, Rc<str>)>,
    // Per-instance LRU so distinct pricing tables cannot leak results into each other.
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
        let capacity = raw.len();
        let mut normalized_index = HashMap::with_capacity(capacity);
        let mut lowercase_keys = Vec::with_capacity(capacity);
        let mut rc_raw = HashMap::with_capacity(capacity);

        for (key, pricing) in raw {
            let rc_key: Rc<str> = key.as_str().into();

            let normalized = normalize_model_name(&key);
            normalized_index
                .entry(normalized)
                .or_insert_with(Vec::new)
                .push(rc_key.clone());

            lowercase_keys.push((key.to_lowercase(), rc_key.clone()));

            rc_raw.insert(rc_key, pricing);
        }

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

    /// Returns pricing for `model_name` using a multi-stage matching strategy.
    ///
    /// Matching proceeds in order:
    /// 1. Exact key match.
    /// 2. Normalized match via [`normalize_model_name`] (provider prefix, 8-digit date, and `-vN` version suffixes stripped).
    /// 3. Bidirectional substring match on the provider-stripped model segment.
    /// 4. Fuzzy match (Jaro-Winkler similarity >= 0.7).
    /// 5. Zero-cost [`ModelPricing::default()`] when no stage matches.
    ///
    /// Stages 3 and 4 are skipped for placeholder names (`default`, `auto`, `custom`, `unknown`, `none`) and model segments shorter than four bytes.
    ///
    /// Results (including misses) are cached in a per-map LRU invalidated lazily by [`clear_pricing_cache`].
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

        if let Some(pricing) = self.raw.get(model_name) {
            let result = ModelPricingResult {
                pricing: pricing.clone(),
                matched_model: None,
            };
            self.cache_result(model_name, &result);
            return result;
        }

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

        let model_lower = model_name.to_lowercase();
        if !eligible_for_loose_match(model_without_provider(&model_lower)) {
            let result = ModelPricingResult {
                pricing: ModelPricing::default(),
                matched_model: None,
            };
            self.cache_result(model_name, &result);
            return result;
        }

        // Score every substring candidate by overlap specificity so `gpt-4o` does not match `gpt-4` first.
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
                normalized_candidate_rank(a, normalized_name, query_provider).cmp(
                    &normalized_candidate_rank(b, normalized_name, query_provider),
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
                let p_rank = provider_rank(query_provider, key_lower);
                Some((overlap, length_difference, p_rank, original_key))
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

    /// Returns pricing only when `model_name` is an exact key in the pricing table.
    ///
    /// Unlike [`Self::get`], performs no normalization, substring, or fuzzy matching.
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

    /// Returns a [`TierThresholds`](crate::pricing::TierThresholds) snapshot of ascending context boundaries for all tiered and range-priced models in this map.
    pub fn tier_thresholds(&self) -> crate::pricing::TierThresholds {
        crate::pricing::TierThresholds::from_entries(
            self.raw.iter().filter_map(|(key, pricing)| {
                Some((key.as_ref(), classification_boundaries(pricing)?))
            }),
        )
    }
}

/// Returns ascending context-size boundaries for `pricing` in `calculate_cost` dispatch order (`ranges` before `tiers`), or `None` for single-level models.
///
/// Range rows emit `min_tokens - 1` (skipping row 0) because [`TierClassifier`](crate::pricing::TierClassifier) compares strictly greater and `min_tokens` is inclusive; using `min_tokens` instead of the prior row's `max_tokens` avoids promoting requests in gaps between ranges.
fn classification_boundaries(pricing: &ModelPricing) -> Option<Vec<i64>> {
    match &pricing.ranges {
        Some(ranges) => {
            let (_, above_first) = ranges.split_first()?;
            ascending_boundaries(above_first.iter().map(|row| row.min_tokens - 1))
        }
        None => ascending_boundaries(pricing.tiers.iter().map(|tier| tier.threshold_tokens)),
    }
}

/// Takes the leading non-negative, strictly ascending prefix of `bounds`, capped at [`MAX_TIER_LEVELS`].
///
/// Stops at the first non-ascending bound rather than skipping it so remaining levels stay aligned with their price rows.
fn ascending_boundaries(bounds: impl Iterator<Item = i64>) -> Option<Vec<i64>> {
    let mut boundaries: Vec<i64> = Vec::new();
    for bound in bounds {
        if boundaries.len() == MAX_TIER_LEVELS || bound <= boundaries.last().copied().unwrap_or(-1)
        {
            break;
        }
        boundaries.push(bound);
    }
    (!boundaries.is_empty()).then_some(boundaries)
}

/// Lazily invalidates the lookup cache in every [`ModelPricingMap`].
///
/// Existing maps clear their local LRU on the next lookup; no lookup results are stored globally.
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
    normalized_name: &str,
    query_provider: Option<&str>,
) -> (u8, u8, usize, &'a str) {
    let cand_segment = model_without_provider(candidate);
    let p_rank = provider_rank(query_provider, candidate);
    let tier_group = if p_rank == 3 {
        2
    } else if cand_segment == normalized_name {
        0
    } else {
        1
    };
    (
        tier_group,
        p_rank,
        normalized_name.len().abs_diff(cand_segment.len()),
        candidate,
    )
}

fn has_any_ascii_prefix(s: &str, prefixes: &[&str]) -> bool {
    let bytes = s.as_bytes();
    prefixes.iter().any(|prefix| {
        bytes
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
    })
}

/// Whether `prefix` is the model vendor's own LiteLLM namespace rather than a third-party host or reseller.
fn is_official_provider(prefix: Option<&str>, model_segment: &str) -> bool {
    match prefix {
        Some(
            "anthropic" | "cohere" | "deepseek" | "elevenlabs" | "gemini" | "meta" | "minimax"
            | "moonshot" | "openai" | "volcengine" | "xai" | "xiaomi_mimo" | "zai",
        ) => true,
        Some("vertex_ai") => has_any_ascii_prefix(
            model_segment,
            &[
                "gemini",
                "gemma",
                "chirp",
                "deep-research",
                "imagen",
                "learnlm",
                "lyria",
                "palm",
                "text-bison",
                "chat-bison",
                "code-bison",
                "veo",
            ],
        ),
        Some("mistral") => has_any_ascii_prefix(
            model_segment,
            &[
                "mistral",
                "ministral",
                "codestral",
                "devstral",
                "labs-devstral",
                "magistral",
                "open-mistral",
                "open-mixtral",
                "pixtral",
                "voxtral",
            ],
        ),
        Some("dashscope") => has_any_ascii_prefix(model_segment, &["qwen", "qwq", "qvq", "wan"]),
        Some("perplexity") => has_any_ascii_prefix(model_segment, &["sonar", "pplx"]),
        Some("tencent") => has_any_ascii_prefix(model_segment, &["hy", "hunyuan"]),
        _ => false,
    }
}

fn model_without_provider(model_name: &str) -> &str {
    model_name
        .split_once('/')
        .map_or(model_name, |(_, model)| model)
}

/// Placeholder names excluded from substring and fuzzy matching.
const LOOSE_MATCH_STOPLIST: [&str; 5] = ["default", "auto", "custom", "unknown", "none"];

/// Minimum model-segment byte length eligible for substring or fuzzy matching.
const LOOSE_MATCH_MIN_SEGMENT_LEN: usize = 4;

fn eligible_for_loose_match(model_segment: &str) -> bool {
    model_segment.len() >= LOOSE_MATCH_MIN_SEGMENT_LEN
        && !LOOSE_MATCH_STOPLIST.contains(&model_segment)
}

fn provider_rank(query_provider: Option<&str>, candidate: &str) -> u8 {
    let cand_provider = provider_prefix(candidate);
    if query_provider.is_some() && query_provider == cand_provider {
        0
    } else if cand_provider.is_none() {
        1
    } else if is_official_provider(cand_provider, model_without_provider(candidate)) {
        2
    } else {
        3
    }
}

/// Returns `name` with any leading provider prefix (up to the first `/`) and trailing date (`-YYYYMMDD`) or numeric version (`-vN`, `-vN.M`) suffixes removed.
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

    if let Some(idx) = name.find('/') {
        start = idx + 1;
    }

    loop {
        let working_slice = &name[start..end];

        if let Some((base, suffix)) = working_slice.rsplit_once('-')
            && suffix.len() == 8
            && suffix.bytes().all(|byte| byte.is_ascii_digit())
        {
            end = start + base.len();
            continue;
        }

        if let Some((base, suffix)) = working_slice.rsplit_once("-v")
            && is_numeric_version_suffix(suffix)
        {
            end = start + base.len();
            continue;
        }

        break;
    }

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
    use super::super::cache::{ThresholdTier, TierRange};
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

        // `default` would substring-match `fireworks-ai-default`; must stay unpriced.
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
        clear_pricing_cache();

        let mut raw = HashMap::new();
        raw.insert("gpt-4".to_string(), create_test_pricing());
        raw.insert("claude-3-opus".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result = map.get("gpt-4");
        assert!(result.pricing.input_cost_per_token > 0.0);

        let result2 = map.get("claude-3-opus");
        assert!(result2.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_normalized_match() {
        clear_pricing_cache();

        let mut raw = HashMap::new();
        raw.insert("claude-3-opus-20240229".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result = map.get("claude-3-opus");
        assert!(result.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_substring_match() {
        clear_pricing_cache();

        let mut raw = HashMap::new();
        raw.insert("gpt-4-0613".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        // 4-digit `-0613` is not stripped as an 8-digit date, exercising the substring scan.
        let result = map.get("gpt-4");
        assert!(result.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_case_insensitive_match() {
        // Case folding runs only in the loose stages, so this resolves via substring match.
        let mut raw = HashMap::new();
        raw.insert("GPT-4".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result = map.get("gpt-4");
        assert!(result.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_fuzzy_match() {
        let mut raw = HashMap::new();
        raw.insert("claude-3-sonnet".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        // No substring overlap, so this resolves via Jaro-Winkler (~0.98 >= 0.7).
        let result = map.get("claude-3-sonet");
        assert!(result.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_get_exact_only_matches_verbatim() {
        let mut raw = HashMap::new();
        raw.insert("gpt-4".to_string(), create_test_pricing());
        let map = ModelPricingMap::new(raw);

        assert!(map.get_exact("gpt-4").is_some());
        // Keys that `get()` would match loosely must return `None` here.
        assert!(map.get_exact("gpt-4-turbo").is_none());
        assert!(map.get_exact("deepseek-v4-pro").is_none());
        assert!(map.get_exact("GPT-4").is_none());
    }

    #[test]
    fn test_no_match_returns_default() {
        let raw = HashMap::new();
        let map = ModelPricingMap::new(raw);

        let result = map.get("unknown-model");
        assert_eq!(result.pricing.input_cost_per_token, 0.0);
        assert_eq!(result.pricing.output_cost_per_token, 0.0);
        assert!(result.matched_model.is_none());
    }

    #[test]
    fn test_multiple_models() {
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
        let mut raw = HashMap::new();
        raw.insert("test-model".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);
        let debug_str = format!("{:?}", map);

        assert!(!debug_str.is_empty());
    }

    #[test]
    fn test_pricing_map_clone() {
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

        let result = map.get("gpt-4");
        assert_eq!(result.pricing.input_cost_per_token, 0.000001);
    }

    #[test]
    fn test_version_stripping() {
        let mut raw = HashMap::new();
        raw.insert("claude-3-opus".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result = map.get("claude-3-opus-20240229");
        assert!(result.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_result_clone() {
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
        let mut raw = HashMap::new();
        let long_name = "a".repeat(1000);
        raw.insert(long_name.clone(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result = map.get(&long_name);
        assert!(result.pricing.input_cost_per_token > 0.0);
    }

    #[test]
    fn test_unicode_model_names() {
        let mut raw = HashMap::new();
        raw.insert("模型-1".to_string(), create_test_pricing());
        raw.insert("モデル-2".to_string(), create_test_pricing());

        let map = ModelPricingMap::new(raw);

        let result1 = map.get("模型-1");
        assert!(result1.pricing.input_cost_per_token > 0.0);

        let result2 = map.get("モデル-2");
        assert!(result2.pricing.input_cost_per_token > 0.0);
    }

    fn range_row(min_tokens: i64, max_tokens: i64) -> TierRange {
        TierRange {
            min_tokens,
            max_tokens,
            input_cost_per_token: 0.000001,
            ..Default::default()
        }
    }

    #[test]
    fn tier_thresholds_exports_every_range_bound_below_the_top_row() {
        let mut raw = HashMap::new();
        raw.insert(
            "dashscope/qwen3-coder-plus".to_string(),
            ModelPricing {
                ranges: Some(vec![
                    range_row(0, 32_000),
                    range_row(32_000, 128_000),
                    range_row(128_000, 256_000),
                    range_row(256_000, 1_000_000),
                ]),
                ..Default::default()
            },
        );
        let thresholds = ModelPricingMap::new(raw).tier_thresholds();

        // `min_tokens` is inclusive while `TierClassifier` compares strictly greater, so each higher row emits `min_tokens - 1`.
        assert_eq!(
            thresholds.boundaries_for("qwen3-coder-plus"),
            Some(&[31_999, 127_999, 255_999][..])
        );
    }

    #[test]
    fn tier_thresholds_never_promote_a_request_into_a_gap_between_rows() {
        let mut raw = HashMap::new();
        raw.insert(
            "vendor/qwen-gapped".to_string(),
            ModelPricing {
                // Nothing prices 128k..256k.
                ranges: Some(vec![
                    range_row(0, 32_000),
                    range_row(32_000, 128_000),
                    range_row(256_000, 1_000_000),
                ]),
                ..Default::default()
            },
        );
        let thresholds = ModelPricingMap::new(raw).tier_thresholds();

        // Using the prior row's `max_tokens` would wrongly promote a 200k request to the top row.
        assert_eq!(
            thresholds.boundaries_for("qwen-gapped"),
            Some(&[31_999, 255_999][..])
        );
    }

    #[test]
    fn tier_thresholds_skips_single_row_range_models() {
        let mut raw = HashMap::new();
        raw.insert(
            "dashscope/qwen-single".to_string(),
            ModelPricing {
                ranges: Some(vec![range_row(0, 32_000)]),
                ..Default::default()
            },
        );
        // Nothing to promote to, so classifying would only cost work.
        assert!(
            ModelPricingMap::new(raw)
                .tier_thresholds()
                .boundaries_for("qwen-single")
                .is_none()
        );
    }

    #[test]
    fn tier_thresholds_stop_at_a_bound_that_does_not_ascend() {
        let mut raw = HashMap::new();
        raw.insert(
            "vendor/qwen-malformed".to_string(),
            ModelPricing {
                ranges: Some(vec![
                    range_row(0, 32_000),
                    range_row(32_000, 128_000),
                    range_row(32_000, 128_000),
                    range_row(128_000, 256_000),
                ]),
                ..Default::default()
            },
        );
        // Row 2 repeats row 1's lower bound; stop at level 1 rather than dropping row 2 and misaligning row 3.
        assert_eq!(
            ModelPricingMap::new(raw)
                .tier_thresholds()
                .boundaries_for("qwen-malformed"),
            Some(&[31_999][..])
        );
    }

    #[test]
    fn tier_thresholds_still_exports_threshold_models() {
        let mut raw = HashMap::new();
        raw.insert(
            "claude-sonnet-4-5".to_string(),
            ModelPricing {
                tiers: vec![ThresholdTier {
                    threshold_tokens: 200_000,
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        assert_eq!(
            ModelPricingMap::new(raw)
                .tier_thresholds()
                .boundaries_for("claude-sonnet-4-5"),
            Some(&[200_000][..])
        );
    }
}
