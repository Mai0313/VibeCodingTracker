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
///
/// `cache_creation_input_token_cost_above_1hr` is the price for cache writes
/// with Anthropic's extended (1 hour) TTL. A value of `0.0` means the model
/// doesn't offer 1hr cached writes at this tier — callers should fall back to
/// the 5-minute (`cache_creation_input_token_cost`) price.
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
    #[serde(default)]
    pub cache_creation_input_token_cost_above_1hr: f64,
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
///
/// `deny_unknown_fields` forces `load_from_cache` to fail on pre-Phase-1 cache
/// files (which carried `*_above_200k_tokens` fields) so a stale same-day cache
/// is rejected and refetched — otherwise serde would silently drop the removed
/// fields and under-price tiered models for the rest of the day.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPricing {
    #[serde(default)]
    pub input_cost_per_token: f64,
    #[serde(default)]
    pub output_cost_per_token: f64,
    #[serde(default)]
    pub cache_read_input_token_cost: f64,
    #[serde(default)]
    pub cache_creation_input_token_cost: f64,

    /// Price per token for cache writes using Anthropic's extended (1 hour) TTL.
    /// `0.0` means the model doesn't support 1hr cached writes — callers fall
    /// back to `cache_creation_input_token_cost` (5-minute TTL price).
    #[serde(default)]
    pub cache_creation_input_token_cost_above_1hr: f64,

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
/// `tiered_pricing` arrays into `TierRange` rows. `cache_creation_input_token_cost_above_1hr`
/// is captured as a separate 1-hour TTL price (base and per-tier). Unsupported
/// fields (batch / priority / audio / computer_use) are ignored.
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
    // 1-hour TTL variants: a threshold of 0 means the base (non-tiered) 1hr price.
    let mut tier_cache_creation_1hr: HashMap<i64, f64> = HashMap::new();

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
            "cache_creation_input_token_cost" => {
                pricing.cache_creation_input_token_cost = num_value
            }
            "cache_creation_input_token_cost_above_1hr" => {
                // Base (non-tiered) 1hr TTL price.
                pricing.cache_creation_input_token_cost_above_1hr = num_value;
            }
            _ => {
                if let Some(suffix) = key.strip_prefix("input_cost_per_token_above_") {
                    if let Some(th) = parse_threshold_suffix(suffix) {
                        tier_input.insert(th, num_value);
                    }
                } else if let Some(suffix) = key.strip_prefix("output_cost_per_token_above_") {
                    if let Some(th) = parse_threshold_suffix(suffix) {
                        tier_output.insert(th, num_value);
                    }
                } else if let Some(suffix) = key.strip_prefix("cache_read_input_token_cost_above_")
                {
                    if let Some(th) = parse_threshold_suffix(suffix) {
                        tier_cache_read.insert(th, num_value);
                    }
                } else if let Some(suffix) =
                    key.strip_prefix("cache_creation_input_token_cost_above_")
                {
                    // Two possible shapes:
                    //   "200k_tokens"           → context-size tier at 200K
                    //   "1hr_above_200k_tokens" → 1hr TTL variant of the 200K tier
                    if let Some(inner) = suffix.strip_prefix("1hr_above_") {
                        if let Some(th) = parse_threshold_suffix(inner) {
                            tier_cache_creation_1hr.insert(th, num_value);
                        }
                    } else if !suffix.starts_with("1hr") {
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
        .chain(tier_cache_creation_1hr.keys())
        .copied()
        .collect();
    thresholds.sort();
    thresholds.dedup();

    pricing.tiers = thresholds
        .into_iter()
        .map(|th| ThresholdTier {
            threshold_tokens: th,
            input_cost_per_token: *tier_input.get(&th).unwrap_or(&pricing.input_cost_per_token),
            output_cost_per_token: *tier_output
                .get(&th)
                .unwrap_or(&pricing.output_cost_per_token),
            cache_read_input_token_cost: *tier_cache_read
                .get(&th)
                .unwrap_or(&pricing.cache_read_input_token_cost),
            cache_creation_input_token_cost: *tier_cache_creation
                .get(&th)
                .unwrap_or(&pricing.cache_creation_input_token_cost),
            // Intentionally do NOT inherit base 1hr into the tier: if LiteLLM
            // doesn't publish a tier-specific 1hr price, leaving this at 0 lets
            // `calculate_cost` fall back to the tier's own 5m rate. Inheriting
            // base 1hr could produce a tier 1hr price BELOW the tier 5m price
            // (nonsensical) whenever the 200K tier substantially marks up the
            // 5m rate but the base 1hr stays at its unmarked level.
            cache_creation_input_token_cost_above_1hr: tier_cache_creation_1hr
                .get(&th)
                .copied()
                .unwrap_or(0.0),
        })
        .collect();

    // Range-based models: sort by min_tokens ascending so selection can assume
    // ordering (LiteLLM data is already sorted, but being explicit makes the
    // `calculate_cost` dispatch logic simpler to reason about).
    if let Some(ranges) = pricing.ranges.as_mut() {
        ranges.sort_by_key(|r| r.min_tokens);
    }

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

/// Filters out models whose every pricing field is zero (unpriced / free).
///
/// A model is kept if **any** of the following yields a non-zero price:
/// - The base-level per-token costs.
/// - Any tier entry (`ThresholdTier`) with at least one non-zero field.
/// - Any range entry (`TierRange`) with at least one non-zero field.
///
/// Models are dropped only when every strategy they publish is entirely zero;
/// this preserves synthetic models that ship tier or range data without base
/// prices, while still excluding free / placeholder entries from LiteLLM.
pub fn normalize_pricing(
    mut pricing: HashMap<String, ModelPricing>,
) -> HashMap<String, ModelPricing> {
    pricing.retain(|_name, p| {
        let has_base = p.input_cost_per_token != 0.0
            || p.output_cost_per_token != 0.0
            || p.cache_read_input_token_cost != 0.0
            || p.cache_creation_input_token_cost != 0.0;
        let has_nonzero_tier = p.tiers.iter().any(|t| {
            t.input_cost_per_token != 0.0
                || t.output_cost_per_token != 0.0
                || t.cache_read_input_token_cost != 0.0
                || t.cache_creation_input_token_cost != 0.0
                || t.cache_creation_input_token_cost_above_1hr != 0.0
        });
        let has_nonzero_range = p
            .ranges
            .as_ref()
            .map(|rs| {
                rs.iter().any(|r| {
                    r.input_cost_per_token != 0.0
                        || r.output_cost_per_token != 0.0
                        || r.cache_read_input_token_cost != 0.0
                        || r.output_cost_per_reasoning_token != 0.0
                })
            })
            .unwrap_or(false);
        has_base || has_nonzero_tier || has_nonzero_range
    });
    pricing
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
    fn parses_combined_1hr_plus_200k_tier() {
        // Claude 3.5 Sonnet-like: has both `_above_1hr` (base) and
        // `_above_1hr_above_200k_tokens` (tiered 1hr). Verify the tiered 1hr
        // price lands on the right tier entry, not inherited from base.
        let raw = json!({
            "input_cost_per_token": 3e-6,
            "output_cost_per_token": 1.5e-5,
            "cache_creation_input_token_cost": 3.75e-6,
            "cache_creation_input_token_cost_above_1hr": 7.5e-6,
            "input_cost_per_token_above_200k_tokens": 6e-6,
            "output_cost_per_token_above_200k_tokens": 2.25e-5,
            "cache_creation_input_token_cost_above_200k_tokens": 7.5e-6,
            "cache_creation_input_token_cost_above_1hr_above_200k_tokens": 1.5e-5
        });
        let p = parse_litellm_entry(&raw);
        assert_eq!(p.cache_creation_input_token_cost_above_1hr, 7.5e-6);
        assert_eq!(p.tiers.len(), 1);
        let t = &p.tiers[0];
        assert_eq!(t.threshold_tokens, 200_000);
        assert_eq!(t.cache_creation_input_token_cost, 7.5e-6);
        // The tiered 1hr price must be $15/M (from `_above_1hr_above_200k_tokens`),
        // NOT the base 1hr $7.5/M.
        assert_eq!(t.cache_creation_input_token_cost_above_1hr, 1.5e-5);
    }

    #[test]
    fn tier_1hr_left_zero_when_missing_so_calculate_cost_can_fall_back() {
        // If LiteLLM publishes a 200K tier but omits the 1hr-tiered field, the
        // parser must NOT inherit base 1hr (that could yield tier_1hr < tier_5m).
        let raw = json!({
            "input_cost_per_token": 3e-6,
            "cache_creation_input_token_cost": 3.75e-6,
            "cache_creation_input_token_cost_above_1hr": 6e-6,
            "cache_creation_input_token_cost_above_200k_tokens": 7.5e-6
        });
        let p = parse_litellm_entry(&raw);
        assert_eq!(p.cache_creation_input_token_cost_above_1hr, 6e-6);
        let t = &p.tiers[0];
        assert_eq!(t.cache_creation_input_token_cost, 7.5e-6);
        assert_eq!(t.cache_creation_input_token_cost_above_1hr, 0.0);
    }

    #[test]
    fn old_cache_format_is_rejected_by_deny_unknown_fields() {
        // A pre-Phase-1 cache entry had `input_cost_per_token_above_200k_tokens`
        // as a flat field. With `deny_unknown_fields` on ModelPricing, parsing
        // MUST fail so `load_from_cache` returns Err → caller refetches.
        let old_format = r#"{
            "input_cost_per_token": 3e-6,
            "output_cost_per_token": 1.5e-5,
            "input_cost_per_token_above_200k_tokens": 6e-6
        }"#;
        let result: Result<ModelPricing, _> = serde_json::from_str(old_format);
        assert!(
            result.is_err(),
            "stale cache entry with removed fields must be rejected"
        );
    }

    #[test]
    fn ranges_are_sorted_by_min_tokens_after_parse() {
        // Feed intentionally-unsorted ranges; parser should sort ascending.
        let raw = json!({
            "tiered_pricing": [
                {"range": [128_000, 256_000], "input_cost_per_token": 3e-6},
                {"range": [0, 32_000],        "input_cost_per_token": 1e-6},
                {"range": [32_000, 128_000],  "input_cost_per_token": 2e-6}
            ]
        });
        let p = parse_litellm_entry(&raw);
        let ranges = p.ranges.expect("ranges");
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0].min_tokens, 0);
        assert_eq!(ranges[1].min_tokens, 32_000);
        assert_eq!(ranges[2].min_tokens, 128_000);
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
