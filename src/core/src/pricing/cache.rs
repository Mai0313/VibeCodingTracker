//! LiteLLM pricing data types, JSON parsing, and daily on-disk cache I/O.

use crate::utils::{
    find_pricing_cache_for_date_in, get_pricing_cache_path_in, list_pricing_cache_files_in,
};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Per-token prices applied when a request's input context exceeds `threshold_tokens`.
///
/// Once a request's total input context (`input + cache_read + cache_creation`) clears `threshold_tokens`, these rates replace base prices for all token types on that request. [`calculate_cost`](super::calculate_cost) bills the `n`th classified slice at `tiers[n - 1]`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ThresholdTier {
    /// Total input context (in tokens) above which this tier's prices take over.
    pub threshold_tokens: i64,
    /// Input price in USD per token at this tier.
    #[serde(default)]
    pub input_cost_per_token: f64,
    /// Output price in USD per token at this tier.
    #[serde(default)]
    pub output_cost_per_token: f64,
    /// Cache-read price in USD per token at this tier.
    #[serde(default)]
    pub cache_read_input_token_cost: f64,
    /// Cache-write (5-minute TTL) price in USD per token at this tier.
    #[serde(default)]
    pub cache_creation_input_token_cost: f64,
    /// Cache-write (1-hour TTL) price in USD per token; `0.0` falls back to the 5-minute rate.
    #[serde(default)]
    pub cache_creation_input_token_cost_above_1hr: f64,
}

/// One volume range for range-based tiered pricing.
///
/// Ranges are sorted ascending by `min_tokens`. [`calculate_cost`](super::calculate_cost) bills slice `n` at `ranges[n]` (clamping above the top row) and unclassified tokens at `ranges[0]`.
///
/// Unlike [`ThresholdTier`], a range replaces input, output, reasoning, and cache-read prices outright while cache-write prices stay on [`ModelPricing`]'s base rates.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TierRange {
    /// Inclusive lower bound (in tokens) for this pricing range.
    pub min_tokens: i64,
    /// Exclusive upper bound published by LiteLLM (informational only; classification uses `min_tokens` and requests past the top row still bill there).
    pub max_tokens: i64,
    /// Input price in USD per token within this range.
    #[serde(default)]
    pub input_cost_per_token: f64,
    /// Output price in USD per token within this range.
    #[serde(default)]
    pub output_cost_per_token: f64,
    /// Cache-read price in USD per token within this range.
    #[serde(default)]
    pub cache_read_input_token_cost: f64,
    /// Reasoning-token price in USD per token; `0.0` falls back to `output_cost_per_token`.
    #[serde(default)]
    pub output_cost_per_reasoning_token: f64,
}

/// Per-token and per-query USD pricing for one AI model.
///
/// [`calculate_cost`](super::calculate_cost) dispatches in this order:
/// 1. Range-based when `ranges` is `Some`: slices bill against `ranges[n]` with unclassified tokens at `ranges[0]`.
/// 2. Threshold-based when `tiers` is non-empty: slices bill against `tiers[n - 1]` with unclassified tokens at base rates.
/// 3. Flat otherwise: base rates apply to all tokens.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelPricing {
    /// Base input price in USD per token.
    #[serde(default)]
    pub input_cost_per_token: f64,
    /// Base output price in USD per token.
    #[serde(default)]
    pub output_cost_per_token: f64,
    /// Base cache-read price in USD per token.
    #[serde(default)]
    pub cache_read_input_token_cost: f64,
    /// Base cache-write (5-minute TTL) price in USD per token.
    #[serde(default)]
    pub cache_creation_input_token_cost: f64,

    /// Base 1-hour TTL cache-write price in USD per token; `0.0` falls back to `cache_creation_input_token_cost`.
    #[serde(default)]
    pub cache_creation_input_token_cost_above_1hr: f64,

    /// Base reasoning-token price in USD per token; `0.0` falls back to `output_cost_per_token`.
    #[serde(default)]
    pub output_cost_per_reasoning_token: f64,

    /// Price in USD per server-side web-search query (`server_tool_use.web_search_requests`); `0.0` if unpublished.
    #[serde(default)]
    pub web_search_cost_per_query: f64,

    /// Threshold-based tiers, sorted ascending by `threshold_tokens`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<ThresholdTier>,

    /// Range-based pricing (mutually exclusive with `tiers` in practice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranges: Option<Vec<TierRange>>,
}

/// Extracts the token threshold from a LiteLLM `"<N>k_tokens"` suffix (e.g. `"200k_tokens"` -> `Some(200_000)`).
fn parse_threshold_suffix(suffix: &str) -> Option<i64> {
    let without_tokens = suffix.strip_suffix("_tokens")?;
    let num_part = without_tokens.strip_suffix('k')?;
    num_part.parse::<i64>().ok().map(|n| n * 1000)
}

/// Parses one `tiered_pricing` object into a [`TierRange`], returning `None` for non-token entries.
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

/// Parses one LiteLLM model JSON object into a [`ModelPricing`].
///
/// Consolidates `*_above_Nk_tokens` fields into ascending `tiers`, `tiered_pricing` entries into ascending `ranges`, `cache_creation_input_token_cost_above_1hr` (base and per-tier), and `search_context_cost_per_query` into `web_search_cost_per_query`. Non-object inputs yield [`ModelPricing::default()`].
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
    // Tier-scoped 1-hour TTL rates; the base 1-hour rate lives on `pricing`.
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

        if key == "search_context_cost_per_query" {
            // Nested object; pick medium with low/high fallbacks before the `as_f64()` filter below.
            if let Some(sc) = raw_val.as_object() {
                let pick = |k: &str| sc.get(k).and_then(|v| v.as_f64());
                pricing.web_search_cost_per_query = pick("search_context_size_medium")
                    .or_else(|| pick("search_context_size_low"))
                    .or_else(|| pick("search_context_size_high"))
                    .unwrap_or(0.0);
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
                pricing.cache_creation_input_token_cost_above_1hr = num_value;
            }
            "output_cost_per_reasoning_token" => {
                pricing.output_cost_per_reasoning_token = num_value;
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
                    } else if !suffix.starts_with("1hr")
                        && let Some(th) = parse_threshold_suffix(suffix)
                    {
                        tier_cache_creation.insert(th, num_value);
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
            // Leave unpublished tier 1-hour rates at 0.0 so `calculate_cost` falls back to this tier's 5-minute rate rather than a cheaper base 1-hour rate.
            cache_creation_input_token_cost_above_1hr: tier_cache_creation_1hr
                .get(&th)
                .copied()
                .unwrap_or(0.0),
        })
        .collect();

    // Sort ascending by `min_tokens` so boundary derivation and `calculate_cost` do not rely on upstream ordering.
    if let Some(ranges) = pricing.ranges.as_mut() {
        ranges.sort_by_key(|r| r.min_tokens);
    }

    pricing
}

/// Parses a LiteLLM pricing JSON object into a map of [`ModelPricing`] entries, ignoring non-object payloads and entries.
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

/// Returns a JSON object containing only keys with `"cost"` in their name plus `"tiered_pricing"`, preserving values verbatim.
///
/// Returns `None` if `value` is not an object or contains no matching keys. `"tiered_pricing"` is retained explicitly because it carries range-based pricing without `"cost"` in its key name.
pub fn filter_cost_fields(value: &Value) -> Option<Value> {
    let obj = value.as_object()?;
    let mut filtered = Map::with_capacity(obj.len());
    for (k, v) in obj {
        if k.contains("cost") || k == "tiered_pricing" {
            filtered.insert(k.clone(), v.clone());
        }
    }
    if filtered.is_empty() {
        None
    } else {
        Some(Value::Object(filtered))
    }
}

/// Builds the on-disk cache JSON object by filtering each model entry through [`filter_cost_fields`] and dropping empty or non-object entries.
pub fn build_filtered_cost_json(raw: &Value) -> Value {
    let obj = match raw.as_object() {
        Some(o) => o,
        None => return Value::Object(Map::new()),
    };
    let mut filtered_map = Map::with_capacity(obj.len());
    for (model, entry) in obj {
        if let Some(filtered) = filter_cost_fields(entry) {
            filtered_map.insert(model.clone(), filtered);
        }
    }
    Value::Object(filtered_map)
}

/// Returns today's UTC date formatted as `YYYY-MM-DD`.
pub(crate) fn pricing_cache_date() -> String {
    Utc::now().date_naive().format("%Y-%m-%d").to_string()
}

/// Removes pricing cache files in `dir` that do not match today's UTC date.
///
/// Best-effort: unreadable directories are ignored and failed deletes are logged as warnings.
pub fn cleanup_old_cache_in(dir: &Path) {
    let today = pricing_cache_date();

    for (filename, path) in list_pricing_cache_files_in(dir) {
        if !filename.contains(&today) {
            match fs::remove_file(&path) {
                Ok(()) => log::debug!("Removed old cache file: {}", path.display()),
                Err(e) => log::warn!("Failed to remove old cache file {}: {e}", path.display()),
            }
        }
    }
}

/// Loads and parses today's UTC pricing cache file from `dir`.
///
/// Cache files store the raw LiteLLM cost-field projection produced by [`build_filtered_cost_json`] and are parsed into [`ModelPricing`] on load.
///
/// # Errors
///
/// Returns an error if today's cache file is missing, unreadable, invalid JSON, not a JSON object, in the legacy serialized-[`ModelPricing`] format, contains a negative or non-finite rate, or yields no priced models.
pub fn load_from_cache_in(dir: &Path) -> Result<HashMap<String, ModelPricing>> {
    let today = pricing_cache_date();
    let cache_path = find_pricing_cache_for_date_in(dir, &today)
        .ok_or_else(|| anyhow::anyhow!("No cache file found for today"))?;

    let content = fs::read_to_string(&cache_path).context("Failed to read cached pricing file")?;
    let raw: Value =
        serde_json::from_str(&content).context("Failed to parse cached pricing JSON")?;

    if !raw.is_object() {
        anyhow::bail!("cached pricing JSON must be an object");
    }

    if looks_like_legacy_pricing_cache(&raw) {
        log::warn!(
            "Detected pre-Phase-2 pricing cache format at {:?}; refetching to avoid silent tier/range data loss",
            cache_path
        );
        anyhow::bail!("legacy pricing cache format detected, forcing refetch");
    }

    let parsed = parse_litellm_pricing_map(raw);
    if !pricing_rates_are_valid(&parsed) {
        anyhow::bail!("cached pricing JSON contains a negative or non-finite price");
    }
    let pricing = normalize_pricing(parsed);
    if pricing.is_empty() {
        anyhow::bail!("cached pricing JSON has no priced models");
    }
    Ok(pricing)
}

/// Returns whether `raw` contains top-level `tiers` or `ranges` keys from the legacy serialized-[`ModelPricing`] cache format.
fn looks_like_legacy_pricing_cache(raw: &Value) -> bool {
    let Some(obj) = raw.as_object() else {
        return false;
    };
    obj.values()
        .filter_map(|v| v.as_object())
        .any(|entry| entry.contains_key("tiers") || entry.contains_key("ranges"))
}

/// Writes `filtered_raw` to today's UTC cache file under `dir` and removes older cache files.
///
/// `filtered_raw` must be a raw cost-key projection from [`build_filtered_cost_json`]; [`load_from_cache_in`] rejects serialized [`ModelPricing`] maps. Old cache cleanup runs only after a successful write and logs failures without returning an error.
///
/// # Errors
///
/// Returns an error if `filtered_raw` cannot be serialized or atomically written.
pub fn save_to_cache_in(dir: &Path, filtered_raw: &Value) -> Result<()> {
    let today = pricing_cache_date();
    let cache_path = get_pricing_cache_path_in(dir, &today);

    crate::utils::write_json_atomic_pretty(&cache_path, filtered_raw)
        .context("Failed to write pricing cache file")?;

    cleanup_old_cache_in(dir);
    Ok(())
}

/// Retains only models whose rates are all finite and non-negative and that have at least one positive base, tier, or range price.
pub fn normalize_pricing(
    mut pricing: HashMap<String, ModelPricing>,
) -> HashMap<String, ModelPricing> {
    pricing.retain(|_name, p| {
        let has_base = p.input_cost_per_token > 0.0
            || p.output_cost_per_token > 0.0
            || p.cache_read_input_token_cost > 0.0
            || p.cache_creation_input_token_cost > 0.0
            || p.cache_creation_input_token_cost_above_1hr > 0.0
            || p.output_cost_per_reasoning_token > 0.0
            || p.web_search_cost_per_query > 0.0;
        let has_positive_tier = p.tiers.iter().any(|t| {
            t.input_cost_per_token > 0.0
                || t.output_cost_per_token > 0.0
                || t.cache_read_input_token_cost > 0.0
                || t.cache_creation_input_token_cost > 0.0
                || t.cache_creation_input_token_cost_above_1hr > 0.0
        });
        let has_positive_range = p
            .ranges
            .as_ref()
            .map(|rs| {
                rs.iter().any(|r| {
                    r.input_cost_per_token > 0.0
                        || r.output_cost_per_token > 0.0
                        || r.cache_read_input_token_cost > 0.0
                        || r.output_cost_per_reasoning_token > 0.0
                })
            })
            .unwrap_or(false);
        model_pricing_rates_are_valid(p) && (has_base || has_positive_tier || has_positive_range)
    });
    pricing
}

pub(crate) fn pricing_rates_are_valid(pricing: &HashMap<String, ModelPricing>) -> bool {
    pricing.values().all(model_pricing_rates_are_valid)
}

fn model_pricing_rates_are_valid(pricing: &ModelPricing) -> bool {
    let valid = |rate: f64| rate.is_finite() && rate >= 0.0;
    valid(pricing.input_cost_per_token)
        && valid(pricing.output_cost_per_token)
        && valid(pricing.cache_read_input_token_cost)
        && valid(pricing.cache_creation_input_token_cost)
        && valid(pricing.cache_creation_input_token_cost_above_1hr)
        && valid(pricing.output_cost_per_reasoning_token)
        && valid(pricing.web_search_cost_per_query)
        && pricing.tiers.iter().all(|tier| {
            valid(tier.input_cost_per_token)
                && valid(tier.output_cost_per_token)
                && valid(tier.cache_read_input_token_cost)
                && valid(tier.cache_creation_input_token_cost)
                && valid(tier.cache_creation_input_token_cost_above_1hr)
        })
        && pricing.ranges.as_ref().is_none_or(|ranges| {
            ranges.iter().all(|range| {
                valid(range.input_cost_per_token)
                    && valid(range.output_cost_per_token)
                    && valid(range.cache_read_input_token_cost)
                    && valid(range.output_cost_per_reasoning_token)
            })
        })
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
        // `above_1hr` is a cache TTL, not a context-size tier, so it must not create a tier.
        assert!(p.tiers.is_empty());
        assert!(p.ranges.is_none());
    }

    #[test]
    fn parses_web_search_cost_per_query() {
        // Nested object must be parsed before the `as_f64()` skip.
        let raw = json!({
            "input_cost_per_token": 5e-6,
            "output_cost_per_token": 2.5e-5,
            "search_context_cost_per_query": {
                "search_context_size_low": 0.01,
                "search_context_size_medium": 0.01,
                "search_context_size_high": 0.01
            }
        });
        let p = parse_litellm_entry(&raw);
        assert_eq!(p.web_search_cost_per_query, 0.01);
        assert_eq!(p.input_cost_per_token, 5e-6);
    }

    #[test]
    fn web_search_cost_absent_defaults_to_zero() {
        let raw = json!({ "input_cost_per_token": 5e-6 });
        assert_eq!(parse_litellm_entry(&raw).web_search_cost_per_query, 0.0);
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
        assert_eq!(p.tiers[0].threshold_tokens, 128_000);
        assert_eq!(p.tiers[1].threshold_tokens, 272_000);
        assert_eq!(p.tiers[0].input_cost_per_token, 2e-6);
        assert_eq!(p.tiers[1].input_cost_per_token, 4e-6);
    }

    #[test]
    fn missing_tier_fields_fall_back_to_base() {
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
        assert_eq!(t.output_cost_per_token, 2e-6);
        assert_eq!(t.cache_read_input_token_cost, 1e-7);
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
        // Must come from `_above_1hr_above_200k_tokens`, not the base 1-hour rate.
        assert_eq!(t.cache_creation_input_token_cost_above_1hr, 1.5e-5);
    }

    #[test]
    fn tier_1hr_left_zero_when_missing_so_calculate_cost_can_fall_back() {
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
    fn cache_reload_reconstructs_tiers_from_raw_keys() {
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
        assert_eq!(p.tiers.len(), 1, "tier must be rebuilt on cache reload");
        assert_eq!(p.tiers[0].threshold_tokens, 200_000);
    }

    #[test]
    fn parses_output_cost_per_reasoning_token() {
        let raw = json!({
            "input_cost_per_token": 3e-7,
            "output_cost_per_token": 2.5e-6,
            "output_cost_per_reasoning_token": 2.5e-6
        });
        let p = parse_litellm_entry(&raw);
        assert_eq!(p.output_cost_per_reasoning_token, 2.5e-6);
    }

    #[test]
    fn filter_cost_fields_keeps_only_cost_keys() {
        let raw = json!({
            "input_cost_per_token": 3e-6,
            "output_cost_per_token": 1.5e-5,
            "cache_creation_input_token_cost_above_1hr": 6e-6,
            "max_input_tokens": 200_000,
            "supports_vision": true,
            "litellm_provider": "anthropic",
            "search_context_cost_per_query": {"search_context_size_high": 0.01}
        });
        let filtered = filter_cost_fields(&raw).expect("has cost keys");
        let obj = filtered.as_object().unwrap();
        assert!(obj.contains_key("input_cost_per_token"));
        assert!(obj.contains_key("output_cost_per_token"));
        assert!(obj.contains_key("cache_creation_input_token_cost_above_1hr"));
        assert!(
            obj.contains_key("search_context_cost_per_query"),
            "nested cost objects must survive the filter"
        );
        assert!(!obj.contains_key("max_input_tokens"));
        assert!(!obj.contains_key("supports_vision"));
        assert!(!obj.contains_key("litellm_provider"));
    }

    #[test]
    fn filter_cost_fields_returns_none_for_non_cost_entries() {
        // Entries without cost keys (e.g. retired or embedding-only models) drop out rather than serializing as `{}`.
        let raw = json!({
            "max_input_tokens": 8192,
            "litellm_provider": "azure"
        });
        assert!(filter_cost_fields(&raw).is_none());
    }

    #[test]
    fn build_filtered_cost_json_skips_entries_without_cost_keys() {
        let raw = json!({
            "model-a": {
                "input_cost_per_token": 1e-6,
                "max_input_tokens": 8192
            },
            "model-b": {
                "max_input_tokens": 16384
            }
        });
        let filtered = build_filtered_cost_json(&raw);
        let obj = filtered.as_object().unwrap();
        assert!(obj.contains_key("model-a"));
        assert!(!obj.contains_key("model-b"));
        let a = obj["model-a"].as_object().unwrap();
        assert!(a.contains_key("input_cost_per_token"));
        assert!(!a.contains_key("max_input_tokens"));
    }

    #[test]
    fn ranges_are_sorted_by_min_tokens_after_parse() {
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

    #[test]
    fn filter_cost_fields_preserves_tiered_pricing() {
        // `tiered_pricing` lacks `"cost"` in its key name and requires an explicit whitelist.
        let raw = json!({
            "input_cost_per_token": 0.0,
            "tiered_pricing": [
                {
                    "range": [0, 32000],
                    "input_cost_per_token": 1e-6,
                    "output_cost_per_token": 5e-6
                },
                {
                    "range": [32000, 128000],
                    "input_cost_per_token": 1.8e-6,
                    "output_cost_per_token": 9e-6
                }
            ],
            "max_input_tokens": 1_000_000,
            "litellm_provider": "dashscope"
        });
        let filtered = filter_cost_fields(&raw).expect("has cost keys");
        let obj = filtered.as_object().unwrap();
        assert!(
            obj.contains_key("tiered_pricing"),
            "tiered_pricing must survive the filter — it carries range-based pricing data"
        );
        let ranges = obj["tiered_pricing"].as_array().expect("array preserved");
        assert_eq!(ranges.len(), 2);
        assert!(!obj.contains_key("max_input_tokens"));
        assert!(!obj.contains_key("litellm_provider"));
    }

    #[test]
    fn cache_roundtrip_preserves_range_priced_models() {
        let upstream = json!({
            "qwen3-coder-plus": {
                "tiered_pricing": [
                    {
                        "range": [0, 32000],
                        "input_cost_per_token": 1e-6,
                        "output_cost_per_token": 5e-6
                    },
                    {
                        "range": [32000, 128000],
                        "input_cost_per_token": 1.8e-6,
                        "output_cost_per_token": 9e-6
                    }
                ],
                "max_input_tokens": 1_000_000,
                "litellm_provider": "dashscope"
            }
        });

        let filtered = build_filtered_cost_json(&upstream);
        let reloaded = parse_litellm_pricing_map(filtered);

        let p = reloaded
            .get("qwen3-coder-plus")
            .expect("model must survive roundtrip");
        let ranges = p.ranges.as_ref().expect("ranges must be rebuilt on reload");
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].min_tokens, 0);
        assert_eq!(ranges[0].max_tokens, 32_000);
        assert_eq!(ranges[0].input_cost_per_token, 1e-6);
        assert_eq!(ranges[1].min_tokens, 32_000);
        assert_eq!(ranges[1].input_cost_per_token, 1.8e-6);
    }

    #[test]
    fn looks_like_legacy_pricing_cache_flags_tiers_array() {
        let legacy = json!({
            "claude-sonnet-4-6": {
                "input_cost_per_token": 3e-6,
                "output_cost_per_token": 1.5e-5,
                "tiers": [
                    {
                        "threshold_tokens": 200_000,
                        "input_cost_per_token": 6e-6,
                        "output_cost_per_token": 2.25e-5
                    }
                ]
            }
        });
        assert!(looks_like_legacy_pricing_cache(&legacy));
    }

    #[test]
    fn looks_like_legacy_pricing_cache_flags_ranges_field() {
        let legacy = json!({
            "qwen-plus": {
                "ranges": [
                    {
                        "min_tokens": 0,
                        "max_tokens": 32_000,
                        "input_cost_per_token": 1e-6
                    }
                ]
            }
        });
        assert!(looks_like_legacy_pricing_cache(&legacy));
    }

    #[test]
    fn looks_like_legacy_pricing_cache_accepts_new_format() {
        let new_format = json!({
            "claude-sonnet-4-6": {
                "input_cost_per_token": 3e-6,
                "output_cost_per_token": 1.5e-5,
                "input_cost_per_token_above_200k_tokens": 6e-6,
                "output_cost_per_token_above_200k_tokens": 2.25e-5
            },
            "qwen3-coder-plus": {
                "tiered_pricing": [
                    {
                        "range": [0, 32000],
                        "input_cost_per_token": 1e-6
                    }
                ]
            }
        });
        assert!(!looks_like_legacy_pricing_cache(&new_format));
    }
}

#[cfg(test)]
mod serialization_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_model_pricing_default() {
        let pricing = ModelPricing::default();

        assert_eq!(pricing.input_cost_per_token, 0.0);
        assert_eq!(pricing.output_cost_per_token, 0.0);
        assert_eq!(pricing.cache_read_input_token_cost, 0.0);
        assert_eq!(pricing.cache_creation_input_token_cost, 0.0);
        assert!(pricing.tiers.is_empty());
        assert!(pricing.ranges.is_none());
    }

    #[test]
    fn test_model_pricing_serialization() {
        let pricing = ModelPricing {
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
        };

        let json = serde_json::to_string(&pricing).unwrap();
        let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

        assert_eq!(
            deserialized.input_cost_per_token,
            pricing.input_cost_per_token
        );
        assert_eq!(deserialized.tiers.len(), 1);
        assert_eq!(deserialized.tiers[0].threshold_tokens, 200_000);
        assert_eq!(deserialized.tiers[0].input_cost_per_token, 0.000002);
    }

    #[test]
    fn test_model_pricing_clone() {
        // `ModelPricing` holds a `Vec`, so it is `Clone` but not `Copy`.
        let pricing1 = ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            ..Default::default()
        };

        let pricing2 = pricing1.clone();

        assert_eq!(pricing1.input_cost_per_token, pricing2.input_cost_per_token);
        assert_eq!(
            pricing1.output_cost_per_token,
            pricing2.output_cost_per_token
        );
    }

    #[test]
    fn test_model_pricing_debug() {
        let pricing = ModelPricing::default();
        let debug_str = format!("{:?}", pricing);

        assert!(debug_str.contains("ModelPricing"));
    }

    #[test]
    fn test_model_pricing_with_partial_data() {
        let json = r#"{"input_cost_per_token": 0.000001}"#;
        let pricing: ModelPricing = serde_json::from_str(json).unwrap();

        assert_eq!(pricing.input_cost_per_token, 0.000001);
        assert_eq!(pricing.output_cost_per_token, 0.0);
    }

    #[test]
    fn test_model_pricing_empty_json() {
        let json = "{}";
        let pricing: ModelPricing = serde_json::from_str(json).unwrap();

        assert_eq!(pricing.input_cost_per_token, 0.0);
        assert_eq!(pricing.output_cost_per_token, 0.0);
    }

    #[test]
    fn test_model_pricing_hashmap_serialization() {
        let mut pricing_map = HashMap::new();
        pricing_map.insert(
            "gpt-4".to_string(),
            ModelPricing {
                input_cost_per_token: 0.000030,
                output_cost_per_token: 0.000060,
                ..Default::default()
            },
        );
        pricing_map.insert(
            "claude-3".to_string(),
            ModelPricing {
                input_cost_per_token: 0.000015,
                output_cost_per_token: 0.000075,
                ..Default::default()
            },
        );

        let json = serde_json::to_string(&pricing_map).unwrap();
        let deserialized: HashMap<String, ModelPricing> = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.len(), 2);
        assert!(deserialized.contains_key("gpt-4"));
        assert!(deserialized.contains_key("claude-3"));
    }

    #[test]
    fn test_model_pricing_all_fields() {
        let pricing = ModelPricing {
            input_cost_per_token: 1.0,
            output_cost_per_token: 2.0,
            cache_read_input_token_cost: 3.0,
            cache_creation_input_token_cost: 4.0,
            output_cost_per_reasoning_token: 11.0,
            web_search_cost_per_query: 0.01,
            tiers: vec![ThresholdTier {
                threshold_tokens: 200_000,
                input_cost_per_token: 5.0,
                output_cost_per_token: 6.0,
                cache_read_input_token_cost: 7.0,
                cache_creation_input_token_cost: 8.0,
                cache_creation_input_token_cost_above_1hr: 12.0,
            }],
            cache_creation_input_token_cost_above_1hr: 10.0,
            ranges: Some(vec![TierRange {
                min_tokens: 0,
                max_tokens: 32_000,
                input_cost_per_token: 0.1,
                output_cost_per_token: 0.2,
                cache_read_input_token_cost: 0.01,
                output_cost_per_reasoning_token: 0.5,
            }]),
        };

        let json = serde_json::to_string(&pricing).unwrap();
        let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.input_cost_per_token, 1.0);
        assert_eq!(deserialized.output_cost_per_token, 2.0);
        assert_eq!(deserialized.cache_read_input_token_cost, 3.0);
        assert_eq!(deserialized.cache_creation_input_token_cost, 4.0);
        assert_eq!(deserialized.output_cost_per_reasoning_token, 11.0);
        assert_eq!(deserialized.web_search_cost_per_query, 0.01);
        assert_eq!(deserialized.tiers.len(), 1);
        assert_eq!(deserialized.tiers[0].threshold_tokens, 200_000);
        assert_eq!(deserialized.tiers[0].input_cost_per_token, 5.0);
        assert_eq!(deserialized.tiers[0].output_cost_per_token, 6.0);
        assert_eq!(deserialized.tiers[0].cache_read_input_token_cost, 7.0);
        assert_eq!(deserialized.tiers[0].cache_creation_input_token_cost, 8.0);
        let ranges = deserialized.ranges.unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].max_tokens, 32_000);
        assert_eq!(ranges[0].output_cost_per_reasoning_token, 0.5);
    }

    #[test]
    fn test_model_pricing_zero_values() {
        let pricing = ModelPricing::default();
        let json = serde_json::to_string(&pricing).unwrap();
        let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.input_cost_per_token, 0.0);
        assert_eq!(deserialized.output_cost_per_token, 0.0);
    }

    #[test]
    fn test_model_pricing_negative_values() {
        // Serde preserves negative rates; rejection happens in `model_pricing_rates_are_valid`.
        let pricing = ModelPricing {
            input_cost_per_token: -0.000001,
            output_cost_per_token: -0.000002,
            ..Default::default()
        };

        let json = serde_json::to_string(&pricing).unwrap();
        let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.input_cost_per_token, -0.000001);
        assert_eq!(deserialized.output_cost_per_token, -0.000002);
    }

    #[test]
    fn test_model_pricing_very_small_values() {
        let pricing = ModelPricing {
            input_cost_per_token: 1e-10,
            output_cost_per_token: 1e-15,
            ..Default::default()
        };

        let json = serde_json::to_string(&pricing).unwrap();
        let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

        assert!((deserialized.input_cost_per_token - 1e-10).abs() < 1e-20);
        assert!((deserialized.output_cost_per_token - 1e-15).abs() < 1e-25);
    }

    #[test]
    fn test_model_pricing_large_values() {
        let pricing = ModelPricing {
            input_cost_per_token: 1000000.0,
            output_cost_per_token: 9999999.99,
            ..Default::default()
        };

        let json = serde_json::to_string(&pricing).unwrap();
        let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.input_cost_per_token, 1000000.0);
        assert_eq!(deserialized.output_cost_per_token, 9999999.99);
    }
}
