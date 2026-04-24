use crate::utils::{
    find_pricing_cache_for_date, get_current_date, get_pricing_cache_path, list_pricing_cache_files,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
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
/// This struct is only ever held in memory — `tiers` / `ranges` are derived
/// from the raw `*_above_Nk_tokens` / `tiered_pricing` keys of LiteLLM by
/// `parse_litellm_entry`. Cache files store the raw LiteLLM cost fields
/// verbatim (see `filter_cost_fields`); reloading the cache runs
/// them back through `parse_litellm_entry` so the derived structures are
/// reconstructed freshly on every launch.
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

    /// Price per token for cache writes using Anthropic's extended (1 hour) TTL.
    /// `0.0` means the model doesn't support 1hr cached writes — callers fall
    /// back to `cache_creation_input_token_cost` (5-minute TTL price).
    #[serde(default)]
    pub cache_creation_input_token_cost_above_1hr: f64,

    /// Price for reasoning / thinking tokens emitted as part of the assistant
    /// response but billed separately from regular output tokens. Populated
    /// by Gemini 2.5 flash / flash-lite (`thoughts_tokens`), perplexity
    /// `sonar-deep-research`, and some qwen-turbo entries. `0.0` means the
    /// model doesn't split reasoning from output — callers fall back to
    /// `output_cost_per_token`.
    #[serde(default)]
    pub output_cost_per_reasoning_token: f64,

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

/// Copies every `cost`-related key from a LiteLLM model entry into a new
/// object, preserving values verbatim (including nested objects like
/// `search_context_cost_per_query`).
///
/// We keep *all* keys whose name contains `cost` rather than only the ones
/// the current `calculate_cost` knows how to consume. That way the on-disk
/// cache is a faithful, diff-able subset of the upstream LiteLLM JSON —
/// future calculation strategies (priority / flex / batch tiers, audio /
/// image modalities, reasoning-token splits, …) don't require re-fetching
/// or a schema migration to gain access to the numbers they need.
///
/// `tiered_pricing` is whitelisted explicitly even though the key name
/// doesn't contain `cost`: its array values are the **only** source of
/// range-based pricing (Qwen / doubao / dashscope), so dropping it would
/// silently zero out those models on every cache reload.
///
/// Returns `None` when the entry has no cost-related keys at all; such
/// models carry nothing we can price against and are skipped at the map
/// level.
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

/// Builds the on-disk cache payload: a map from model name to its
/// cost-only subset (see `filter_cost_fields`). Non-object top-level
/// entries (e.g. LiteLLM's `sample_spec`, which is kept — it still has
/// cost keys) and entries with no cost keys are dropped here.
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

/// Loads pricing data from today's cache file.
///
/// The cache stores the raw LiteLLM cost-field subset (see
/// `build_filtered_cost_json`) rather than our derived `ModelPricing`
/// shape, so we re-run `parse_litellm_entry` here to rebuild `tiers`
/// and `ranges` on load.
///
/// Pre-Phase-2 versions serialised the derived `ModelPricing` struct
/// directly, carrying top-level `tiers` / `ranges` arrays instead of
/// the raw `*_above_Nk_tokens` / `tiered_pricing` keys.
/// `parse_litellm_entry` would silently drop those arrays (they aren't
/// cost-keyed scalars) and under-price every tier- or range-priced
/// model until the cache rotated the next day. We detect that shape
/// via `looks_like_legacy_pricing_cache` and return `Err` so
/// `fetch_model_pricing` falls through to a refetch, which overwrites
/// the stale cache with the new schema.
pub fn load_from_cache() -> Result<HashMap<String, ModelPricing>> {
    let today = get_current_date();
    let cache_path = find_pricing_cache_for_date(&today)
        .ok_or_else(|| anyhow::anyhow!("No cache file found for today"))?;

    let content = fs::read_to_string(&cache_path).context("Failed to read cached pricing file")?;
    let raw: Value =
        serde_json::from_str(&content).context("Failed to parse cached pricing JSON")?;

    if looks_like_legacy_pricing_cache(&raw) {
        log::warn!(
            "Detected pre-Phase-2 pricing cache format at {:?}; refetching to avoid silent tier/range data loss",
            cache_path
        );
        anyhow::bail!("legacy pricing cache format detected, forcing refetch");
    }

    Ok(parse_litellm_pricing_map(raw))
}

/// Heuristic: does this cache file look like a pre-Phase-2 serialised
/// `ModelPricing` map?
///
/// The new schema (`build_filtered_cost_json` → `filter_cost_fields`)
/// only emits keys that either contain `cost` or equal `tiered_pricing`,
/// so it never produces top-level `tiers` / `ranges` arrays on an
/// entry. The old schema (`ModelPricing` via derived `Serialize`) did
/// emit them whenever a model carried tier or range data. Any entry
/// with such a key is a definitive signal of the old format.
fn looks_like_legacy_pricing_cache(raw: &Value) -> bool {
    let Some(obj) = raw.as_object() else {
        return false;
    };
    obj.values()
        .filter_map(|v| v.as_object())
        .any(|entry| entry.contains_key("tiers") || entry.contains_key("ranges"))
}

/// Saves a raw LiteLLM cost-field subset to today's cache file and cleans
/// up old caches.
///
/// Callers should pass the output of `build_filtered_cost_json` so the
/// on-disk payload is a cost-only projection of the upstream LiteLLM JSON
/// — that keeps the cache file small, diff-able against upstream, and
/// forward-compatible with calculation strategies that aren't wired up
/// yet.
pub fn save_to_cache(filtered_raw: &Value) -> Result<()> {
    let today = get_current_date();
    let cache_path = get_pricing_cache_path(&today)?;

    let pricing_json =
        serde_json::to_string_pretty(filtered_raw).context("Failed to serialize pricing data")?;
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
            || p.cache_creation_input_token_cost != 0.0
            || p.cache_creation_input_token_cost_above_1hr != 0.0
            || p.output_cost_per_reasoning_token != 0.0;
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
    fn cache_reload_reconstructs_tiers_from_raw_keys() {
        // Cache files now store the raw LiteLLM cost-field subset rather
        // than our derived `ModelPricing` shape. Reloading must rebuild
        // `tiers` by re-running `parse_litellm_entry`, or a Sonnet-style
        // 200K tier would silently vanish and under-price large sessions.
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
        // Gemini 2.5 Flash and friends bill `thoughts_tokens` at a separate
        // per-token rate. Older `ModelPricing` dropped this field entirely;
        // the parser now preserves it as a base-level price.
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
        // Some LiteLLM entries (e.g. retired / embedding-only models) have
        // no cost-related keys at all. They should be dropped from the
        // cache, not serialised as empty objects.
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

    #[test]
    fn filter_cost_fields_preserves_tiered_pricing() {
        // `tiered_pricing` is the only source of range-based pricing
        // (Qwen / doubao / dashscope). Its key name doesn't contain
        // "cost", so without an explicit whitelist the filter would
        // silently drop range data on every cache rotation.
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
        // Full-pipeline regression: a range-priced model goes through
        // `build_filtered_cost_json` (what `save_to_cache` writes) and
        // back through `parse_litellm_pricing_map` (what
        // `load_from_cache` reads). `ranges` must survive end-to-end —
        // earlier iterations of the filter dropped `tiered_pricing` as
        // a non-cost key, zeroing every Qwen / doubao model.
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
        // Pre-Phase-2 cache format: serialised `ModelPricing` with a
        // top-level `tiers` array. The new format never emits this key
        // at the entry level (the filter drops it), so its presence
        // unambiguously signals the old shape.
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
        // New format keeps only cost-named keys plus `tiered_pricing`
        // and never emits top-level `tiers` or `ranges` arrays on an
        // entry, so a fresh cache file must not trip the detector.
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
        // Test ModelPricing default values
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
        // Test ModelPricing can be serialized and deserialized with a threshold tier
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
        // Vec means ModelPricing is no longer Copy — explicit clone is required.
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
        // Test ModelPricing debug formatting
        let pricing = ModelPricing::default();
        let debug_str = format!("{:?}", pricing);

        assert!(debug_str.contains("ModelPricing"));
    }

    #[test]
    fn test_model_pricing_with_partial_data() {
        // Test deserializing with partial data (using #[serde(default)])
        let json = r#"{"input_cost_per_token": 0.000001}"#;
        let pricing: ModelPricing = serde_json::from_str(json).unwrap();

        assert_eq!(pricing.input_cost_per_token, 0.000001);
        assert_eq!(pricing.output_cost_per_token, 0.0); // Should use default
    }

    #[test]
    fn test_model_pricing_empty_json() {
        // Test deserializing empty JSON object
        let json = "{}";
        let pricing: ModelPricing = serde_json::from_str(json).unwrap();

        assert_eq!(pricing.input_cost_per_token, 0.0);
        assert_eq!(pricing.output_cost_per_token, 0.0);
    }

    #[test]
    fn test_model_pricing_hashmap_serialization() {
        // Test HashMap<String, ModelPricing> serialization
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
        // Verify base prices + tiers + ranges all survive round-trip serialization.
        let pricing = ModelPricing {
            input_cost_per_token: 1.0,
            output_cost_per_token: 2.0,
            cache_read_input_token_cost: 3.0,
            cache_creation_input_token_cost: 4.0,
            output_cost_per_reasoning_token: 11.0,
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
        // Test with all zero values
        let pricing = ModelPricing::default();
        let json = serde_json::to_string(&pricing).unwrap();
        let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

        // All should be zero
        assert_eq!(deserialized.input_cost_per_token, 0.0);
        assert_eq!(deserialized.output_cost_per_token, 0.0);
    }

    #[test]
    fn test_model_pricing_negative_values() {
        // Test that negative values are preserved (although not realistic)
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
        // Test with very small values (scientific notation)
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
        // Test with large values
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
