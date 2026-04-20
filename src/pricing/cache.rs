use crate::utils::{
    find_pricing_cache_for_date, get_current_date, get_pricing_cache_path, list_pricing_cache_files,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

/// A threshold-based pricing tier.
///
/// When a request's total input context (input + cache_read + cache_creation)
/// exceeds `threshold_tokens`, these per-token prices replace the base prices
/// for ALL token types on the model. Matches the Anthropic / Google "above Nk
/// tokens" model where the entire request switches to a higher rate once the
/// prompt crosses a size threshold.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ThresholdTier {
    pub threshold_tokens: i64,
    #[serde(default)]
    pub input_cost_per_token: f64,
    #[serde(default)]
    pub output_cost_per_token: f64,
    #[serde(default)]
    pub cache_read_input_token_cost: f64,
    #[serde(default)]
    pub cache_creation_input_token_cost: f64,
}

/// A single range for range-based tiered pricing (Qwen / doubao style).
///
/// Matches when `input_tokens` falls in `[min_tokens, max_tokens)`. Unlike
/// `ThresholdTier`, each range is a fully independent price table — base
/// prices are not used as fallback.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TierRange {
    pub min_tokens: i64,
    pub max_tokens: i64,
    #[serde(default)]
    pub input_cost_per_token: f64,
    #[serde(default)]
    pub output_cost_per_token: f64,
    #[serde(default)]
    pub cache_read_input_token_cost: f64,
    #[serde(default)]
    pub output_cost_per_reasoning_token: f64,
}

/// Pricing data for a single AI model in USD per token.
///
/// Supports three strategies, checked in this order by `calculate_cost`:
/// 1. **Range-based** (`ranges` is `Some`): `input_tokens` selects a `TierRange`
///    and its prices are applied standalone. Used by Qwen / doubao families.
/// 2. **Threshold-based** (`tiers` is non-empty): the highest `ThresholdTier`
///    whose `threshold_tokens` is exceeded by total input context wins; all
///    token types switch to that tier's prices. Used by Claude Sonnet 4.x,
///    Gemini 2.5 Pro, Gemini 1.5 (128k), GPT-5.x (272k), etc.
/// 3. **Flat** (neither set): base prices apply to every request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelPricing {
    #[serde(default)]
    pub input_cost_per_token: f64,
    #[serde(default)]
    pub output_cost_per_token: f64,
    #[serde(default)]
    pub cache_read_input_token_cost: f64,
    #[serde(default)]
    pub cache_creation_input_token_cost: f64,

    /// Threshold-based tiers, sorted ascending by `threshold_tokens`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<ThresholdTier>,

    /// Range-based pricing (mutually exclusive with `tiers` in practice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranges: Option<Vec<TierRange>>,
}

/// Extracts the numeric token count from a LiteLLM threshold suffix.
///
/// E.g. `"200k_tokens" → Some(200_000)`, `"1hr" → None`.
fn parse_threshold_suffix(suffix: &str) -> Option<i64> {
    let without_tokens = suffix.strip_suffix("_tokens")?;
    let num_part = without_tokens.strip_suffix('k')?;
    num_part.parse::<i64>().ok().map(|n| n * 1000)
}

fn parse_tier_range(value: &serde_json::Value) -> Option<TierRange> {
    let obj = value.as_object()?;
    let range = obj.get("range")?.as_array()?;
    if range.len() != 2 {
        return None;
    }
    let min = range[0].as_f64()? as i64;
    let max = range[1].as_f64()? as i64;
    let f = |k: &str| obj.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
    Some(TierRange {
        min_tokens: min,
        max_tokens: max,
        input_cost_per_token: f("input_cost_per_token"),
        output_cost_per_token: f("output_cost_per_token"),
        cache_read_input_token_cost: f("cache_read_input_token_cost"),
        output_cost_per_reasoning_token: f("output_cost_per_reasoning_token"),
    })
}

/// Converts one LiteLLM model entry into our normalized `ModelPricing`.
///
/// Extracts base prices, consolidates all `*_above_Nk_tokens` fields into
/// `ThresholdTier` rows keyed by the numeric threshold, and parses
/// `tiered_pricing` arrays into `TierRange` rows. Unsupported fields
/// (batch / priority / audio / computer_use / above_1hr cache duration) are
/// ignored — they are tracked as known gaps for future work.
pub fn parse_litellm_entry(value: &serde_json::Value) -> ModelPricing {
    let obj = match value.as_object() {
        Some(o) => o,
        None => return ModelPricing::default(),
    };

    let mut pricing = ModelPricing::default();
    let mut tier_input: HashMap<i64, f64> = HashMap::new();
    let mut tier_output: HashMap<i64, f64> = HashMap::new();
    let mut tier_cache_read: HashMap<i64, f64> = HashMap::new();
    let mut tier_cache_creation: HashMap<i64, f64> = HashMap::new();

    for (key, raw_val) in obj {
        if key == "tiered_pricing" {
            if let Some(arr) = raw_val.as_array() {
                let ranges: Vec<TierRange> = arr.iter().filter_map(parse_tier_range).collect();
                if !ranges.is_empty() {
                    pricing.ranges = Some(ranges);
                }
            }
            continue;
        }

        let num_value = match raw_val.as_f64() {
            Some(n) => n,
            None => continue,
        };

        match key.as_str() {
            "input_cost_per_token" => pricing.input_cost_per_token = num_value,
            "output_cost_per_token" => pricing.output_cost_per_token = num_value,
            "cache_read_input_token_cost" => pricing.cache_read_input_token_cost = num_value,
            "cache_creation_input_token_cost" => pricing.cache_creation_input_token_cost = num_value,
            _ => {
                if let Some(suffix) = key.strip_prefix("input_cost_per_token_above_") {
                    if let Some(th) = parse_threshold_suffix(suffix) {
                        tier_input.insert(th, num_value);
                    }
                } else if let Some(suffix) = key.strip_prefix("output_cost_per_token_above_") {
                    if let Some(th) = parse_threshold_suffix(suffix) {
                        tier_output.insert(th, num_value);
                    }
                } else if let Some(suffix) = key.strip_prefix("cache_read_input_token_cost_above_") {
                    if let Some(th) = parse_threshold_suffix(suffix) {
                        tier_cache_read.insert(th, num_value);
                    }
                } else if let Some(suffix) =
                    key.strip_prefix("cache_creation_input_token_cost_above_")
                {
                    // Skip `above_1hr` — it is cache TTL, not context size tier.
                    if !suffix.starts_with("1hr") {
                        if let Some(th) = parse_threshold_suffix(suffix) {
                            tier_cache_creation.insert(th, num_value);
                        }
                    }
                }
            }
        }
    }

    let mut thresholds: Vec<i64> = tier_input
        .keys()
        .chain(tier_output.keys())
        .chain(tier_cache_read.keys())
        .chain(tier_cache_creation.keys())
        .copied()
        .collect();
    thresholds.sort();
    thresholds.dedup();

    pricing.tiers = thresholds
        .into_iter()
        .map(|th| ThresholdTier {
            threshold_tokens: th,
            input_cost_per_token: *tier_input
                .get(&th)
                .unwrap_or(&pricing.input_cost_per_token),
            output_cost_per_token: *tier_output
                .get(&th)
                .unwrap_or(&pricing.output_cost_per_token),
            cache_read_input_token_cost: *tier_cache_read
                .get(&th)
                .unwrap_or(&pricing.cache_read_input_token_cost),
            cache_creation_input_token_cost: *tier_cache_creation
                .get(&th)
                .unwrap_or(&pricing.cache_creation_input_token_cost),
        })
        .collect();

    pricing
}

/// Parses the full LiteLLM `model_prices_and_context_window.json` payload.
pub fn parse_litellm_pricing_map(raw: serde_json::Value) -> HashMap<String, ModelPricing> {
    let obj = match raw.as_object() {
        Some(o) => o,
        None => return HashMap::new(),
    };
    obj.iter()
        .filter(|(_, v)| v.is_object())
        .map(|(k, v)| (k.clone(), parse_litellm_entry(v)))
        .collect()
}

/// Removes outdated pricing cache files, keeping only today's cache
pub fn cleanup_old_cache() {
    let Ok(cache_files) = list_pricing_cache_files() else {
        return;
    };

    let today = get_current_date();

    for (filename, path) in cache_files {
        if !filename.contains(&today) {
            let _ = fs::remove_file(&path);
            log::debug!("Removed old cache file: {:?}", path);
        }
    }
}

/// Loads pricing data from today's cache file
pub fn load_from_cache() -> Result<HashMap<String, ModelPricing>> {
    let today = get_current_date();
    let cache_path = find_pricing_cache_for_date(&today)
        .ok_or_else(|| anyhow::anyhow!("No cache file found for today"))?;

    let content = fs::read_to_string(&cache_path).context("Failed to read cached pricing file")?;
    let pricing: HashMap<String, ModelPricing> =
        serde_json::from_str(&content).context("Failed to parse cached pricing JSON")?;
    Ok(pricing)
}

/// Saves pricing data to today's cache file and cleans up old caches
pub fn save_to_cache(pricing: &HashMap<String, ModelPricing>) -> Result<()> {
    let today = get_current_date();
    let cache_path = get_pricing_cache_path(&today)?;

    let pricing_json =
        serde_json::to_string_pretty(pricing).context("Failed to serialize pricing data")?;
    fs::write(&cache_path, pricing_json).context("Failed to write pricing cache file")?;

    cleanup_old_cache();
    Ok(())
}

#[cfg(test)]
mod parser_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_flat_model_no_tiers() {
        // A typical Anthropic Opus entry — no above_Xk fields, no tiered_pricing.
        let raw = json!({
            "input_cost_per_token": 5e-6,
            "output_cost_per_token": 2.5e-5,
            "cache_read_input_token_cost": 5e-7,
            "cache_creation_input_token_cost": 6.25e-6,
            "cache_creation_input_token_cost_above_1hr": 1e-5,
            "max_input_tokens": 200000
        });
        let p = parse_litellm_entry(&raw);
        assert_eq!(p.input_cost_per_token, 5e-6);
        assert_eq!(p.output_cost_per_token, 2.5e-5);
        assert_eq!(p.cache_read_input_token_cost, 5e-7);
        assert_eq!(p.cache_creation_input_token_cost, 6.25e-6);
        // above_1hr is cache TTL, not a context-size tier — must NOT become a tier.
        assert!(p.tiers.is_empty());
        assert!(p.ranges.is_none());
    }

    #[test]
    fn parses_sonnet_like_with_200k_tier() {
        let raw = json!({
            "input_cost_per_token": 3e-6,
            "output_cost_per_token": 1.5e-5,
            "cache_read_input_token_cost": 3e-7,
            "cache_creation_input_token_cost": 3.75e-6,
            "input_cost_per_token_above_200k_tokens": 6e-6,
            "output_cost_per_token_above_200k_tokens": 2.25e-5,
            "cache_read_input_token_cost_above_200k_tokens": 6e-7,
            "cache_creation_input_token_cost_above_200k_tokens": 7.5e-6
        });
        let p = parse_litellm_entry(&raw);
        assert_eq!(p.tiers.len(), 1);
        let t = &p.tiers[0];
        assert_eq!(t.threshold_tokens, 200_000);
        assert_eq!(t.input_cost_per_token, 6e-6);
        assert_eq!(t.output_cost_per_token, 2.25e-5);
        assert_eq!(t.cache_read_input_token_cost, 6e-7);
        assert_eq!(t.cache_creation_input_token_cost, 7.5e-6);
    }

    #[test]
    fn parses_multiple_thresholds_sorted() {
        // Synthetic GPT-5.x-like entry with 272k tier.
        let raw = json!({
            "input_cost_per_token": 1e-6,
            "output_cost_per_token": 2e-6,
            "input_cost_per_token_above_272k_tokens": 4e-6,
            "output_cost_per_token_above_272k_tokens": 8e-6,
            "input_cost_per_token_above_128k_tokens": 2e-6,
            "output_cost_per_token_above_128k_tokens": 4e-6
        });
        let p = parse_litellm_entry(&raw);
        assert_eq!(p.tiers.len(), 2);
        // Must be sorted ascending by threshold.
        assert_eq!(p.tiers[0].threshold_tokens, 128_000);
        assert_eq!(p.tiers[1].threshold_tokens, 272_000);
        assert_eq!(p.tiers[0].input_cost_per_token, 2e-6);
        assert_eq!(p.tiers[1].input_cost_per_token, 4e-6);
    }

    #[test]
    fn missing_tier_fields_fall_back_to_base() {
        // Only input has a 200k override; output/cache should inherit base.
        let raw = json!({
            "input_cost_per_token": 1e-6,
            "output_cost_per_token": 2e-6,
            "cache_read_input_token_cost": 1e-7,
            "input_cost_per_token_above_200k_tokens": 2e-6
        });
        let p = parse_litellm_entry(&raw);
        assert_eq!(p.tiers.len(), 1);
        let t = &p.tiers[0];
        assert_eq!(t.input_cost_per_token, 2e-6);
        assert_eq!(t.output_cost_per_token, 2e-6); // from base
        assert_eq!(t.cache_read_input_token_cost, 1e-7); // from base
    }

    #[test]
    fn parses_tiered_pricing_ranges() {
        // Mimics dashscope/qwen3-coder-plus structure.
        let raw = json!({
            "tiered_pricing": [
                {
                    "range": [0, 32000],
                    "input_cost_per_token": 1e-6,
                    "output_cost_per_token": 5e-6,
                    "cache_read_input_token_cost": 1e-7
                },
                {
                    "range": [32000, 128000],
                    "input_cost_per_token": 1.8e-6,
                    "output_cost_per_token": 9e-6
                },
                {
                    "range": [256000, 1000000],
                    "input_cost_per_token": 6e-6,
                    "output_cost_per_token": 6e-5
                }
            ]
        });
        let p = parse_litellm_entry(&raw);
        let ranges = p.ranges.expect("ranges should be parsed");
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0].min_tokens, 0);
        assert_eq!(ranges[0].max_tokens, 32_000);
        assert_eq!(ranges[0].input_cost_per_token, 1e-6);
        assert_eq!(ranges[1].input_cost_per_token, 1.8e-6);
        assert_eq!(ranges[2].max_tokens, 1_000_000);
    }

    #[test]
    fn skips_non_token_tiered_pricing() {
        // exa_ai / firecrawl use max_results_range — not token-based. Skip.
        let raw = json!({
            "tiered_pricing": [
                {"max_results_range": [0, 25], "input_cost_per_query": 0.005}
            ]
        });
        let p = parse_litellm_entry(&raw);
        assert!(p.ranges.is_none());
    }

    #[test]
    fn ignores_unknown_fields() {
        let raw = json!({
            "input_cost_per_token": 1e-6,
            "output_cost_per_token": 2e-6,
            "input_cost_per_token_priority": 5e-6,
            "input_cost_per_token_batches": 5e-7,
            "output_cost_per_reasoning_token": 3e-6,
            "supports_vision": true,
            "litellm_provider": "anthropic"
        });
        let p = parse_litellm_entry(&raw);
        assert_eq!(p.input_cost_per_token, 1e-6);
        assert_eq!(p.output_cost_per_token, 2e-6);
        assert!(p.tiers.is_empty());
        assert!(p.ranges.is_none());
    }
}

/// Filters out models whose every pricing field is zero (unpriced / free).
///
/// A model is kept if any base price is non-zero OR it has non-empty range
/// pricing. Tiers alone are not sufficient to keep a model: tiers without base
/// prices (or non-zero tier prices) are effectively unpriced.
pub fn normalize_pricing(
    mut pricing: HashMap<String, ModelPricing>,
) -> HashMap<String, ModelPricing> {
    pricing.retain(|_name, p| {
        let has_base = p.input_cost_per_token != 0.0
            || p.output_cost_per_token != 0.0
            || p.cache_read_input_token_cost != 0.0
            || p.cache_creation_input_token_cost != 0.0;
        let has_ranges = p
            .ranges
            .as_ref()
            .map(|r| !r.is_empty())
            .unwrap_or(false);
        let has_nonzero_tier = p.tiers.iter().any(|t| {
            t.input_cost_per_token != 0.0
                || t.output_cost_per_token != 0.0
                || t.cache_read_input_token_cost != 0.0
                || t.cache_creation_input_token_cost != 0.0
        });
        has_base || has_ranges || has_nonzero_tier
    });
    pricing
}
