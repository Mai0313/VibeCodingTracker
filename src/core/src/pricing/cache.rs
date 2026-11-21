//! LiteLLM pricing data types, JSON parsing, and daily on-disk cache I/O.

use crate::constants::FastHashMap;
use crate::utils::{
    find_pricing_cache_for_date_in, get_pricing_cache_path_in, list_pricing_cache_files_in,
};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::borrow::Cow;
#[cfg(test)]
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::Arc;

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

#[derive(Clone, Copy)]
enum TierField {
    Input,
    Output,
    CacheRead,
    CacheCreation,
    CacheCreation1hr,
}

struct RawThresholdTier {
    threshold_tokens: i64,
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_creation: Option<f64>,
    cache_creation_1hr: Option<f64>,
}

fn parse_tier_key(key: &str) -> Option<(i64, TierField)> {
    if let Some(suffix) = key.strip_prefix("input_cost_per_token_above_") {
        parse_threshold_suffix(suffix).map(|th| (th, TierField::Input))
    } else if let Some(suffix) = key.strip_prefix("output_cost_per_token_above_") {
        parse_threshold_suffix(suffix).map(|th| (th, TierField::Output))
    } else if let Some(suffix) = key.strip_prefix("cache_read_input_token_cost_above_") {
        parse_threshold_suffix(suffix).map(|th| (th, TierField::CacheRead))
    } else if let Some(suffix) = key.strip_prefix("cache_creation_input_token_cost_above_") {
        // Two possible shapes:
        //   "200k_tokens"           → context-size tier at 200K
        //   "1hr_above_200k_tokens" → 1hr TTL variant of the 200K tier
        if let Some(inner) = suffix.strip_prefix("1hr_above_") {
            parse_threshold_suffix(inner).map(|th| (th, TierField::CacheCreation1hr))
        } else if !suffix.starts_with("1hr") {
            parse_threshold_suffix(suffix).map(|th| (th, TierField::CacheCreation))
        } else {
            None
        }
    } else {
        None
    }
}

fn set_tier_field(
    tiers: &mut Vec<RawThresholdTier>,
    threshold_tokens: i64,
    field: TierField,
    value: f64,
) {
    let entry = match tiers
        .iter_mut()
        .find(|tier| tier.threshold_tokens == threshold_tokens)
    {
        Some(existing) => existing,
        None => {
            tiers.push(RawThresholdTier {
                threshold_tokens,
                input: None,
                output: None,
                cache_read: None,
                cache_creation: None,
                cache_creation_1hr: None,
            });
            tiers.last_mut().expect("just pushed tier")
        }
    };
    match field {
        TierField::Input => entry.input = Some(value),
        TierField::Output => entry.output = Some(value),
        TierField::CacheRead => entry.cache_read = Some(value),
        TierField::CacheCreation => entry.cache_creation = Some(value),
        TierField::CacheCreation1hr => entry.cache_creation_1hr = Some(value),
    }
}

fn finalize_tiers(pricing: &mut ModelPricing, mut raw_tiers: Vec<RawThresholdTier>) {
    if raw_tiers.is_empty() {
        return;
    }
    raw_tiers.sort_by_key(|tier| tier.threshold_tokens);
    pricing.tiers = raw_tiers
        .into_iter()
        .map(|tier| ThresholdTier {
            threshold_tokens: tier.threshold_tokens,
            input_cost_per_token: tier.input.unwrap_or(pricing.input_cost_per_token),
            output_cost_per_token: tier.output.unwrap_or(pricing.output_cost_per_token),
            cache_read_input_token_cost: tier
                .cache_read
                .unwrap_or(pricing.cache_read_input_token_cost),
            cache_creation_input_token_cost: tier
                .cache_creation
                .unwrap_or(pricing.cache_creation_input_token_cost),
            // Leave unpublished tier 1-hour rates at 0.0 so `calculate_cost` falls back to this tier's 5-minute rate rather than a cheaper base 1-hour rate.
            cache_creation_input_token_cost_above_1hr: tier.cache_creation_1hr.unwrap_or(0.0),
        })
        .collect();
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
    let mut raw_tiers: Vec<RawThresholdTier> = Vec::new();

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
                if let Some((th, field)) = parse_tier_key(key) {
                    set_tier_field(&mut raw_tiers, th, field, num_value);
                }
            }
        }
    }

    finalize_tiers(&mut pricing, raw_tiers);

    // Sort ascending by `min_tokens` so boundary derivation and `calculate_cost` do not rely on upstream ordering.
    if let Some(ranges) = pricing.ranges.as_mut() {
        ranges.sort_by_key(|r| r.min_tokens);
    }

    pricing
}

/// Parses a LiteLLM pricing JSON object into a map of [`ModelPricing`] entries, ignoring non-object payloads and entries.
#[cfg(test)]
pub fn parse_litellm_pricing_map(raw: &serde_json::Value) -> HashMap<String, ModelPricing> {
    let obj = match raw.as_object() {
        Some(o) => o,
        None => return HashMap::new(),
    };
    obj.iter()
        .filter(|(_, v)| v.is_object())
        .map(|(k, v)| (k.clone(), parse_litellm_entry(v)))
        .collect()
}

/// Parses, validates, and filters a LiteLLM pricing JSON object in a single pass.
///
/// Rejects the payload if it is not a JSON object, if any entry has a negative or non-finite rate, or if no model has a positive price.
pub(crate) fn parse_and_validate_litellm_map(
    raw: &serde_json::Value,
) -> Result<FastHashMap<Arc<str>, ModelPricing>> {
    let Some(obj) = raw.as_object() else {
        anyhow::bail!("Invalid model pricing JSON: top-level value must be an object");
    };
    let mut pricing = FastHashMap::with_capacity(obj.len());
    for (name, value) in obj {
        if !value.is_object() {
            continue;
        }
        let entry = parse_litellm_entry(value);
        anyhow::ensure!(
            model_pricing_rates_are_valid(&entry),
            "Invalid model pricing JSON: negative or non-finite price"
        );
        if model_pricing_has_positive_rate(&entry) {
            pricing.insert(Arc::from(name.as_str()), entry);
        }
    }
    anyhow::ensure!(
        !pricing.is_empty(),
        "Invalid model pricing JSON: no priced models"
    );
    Ok(pricing)
}

macro_rules! ignore_json_values {
    ($ret:expr; $( $kind:ident ),+ $(,)?) => {
        $( ignore_json_values!(@arm $ret, $kind); )+
    };
    (@arm $ret:expr, bool) => {
        fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
            Ok($ret)
        }
    };
    (@arm $ret:expr, num) => {
        fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
            Ok($ret)
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
            Ok($ret)
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
            Ok($ret)
        }
    };
    (@arm $ret:expr, str) => {
        fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<Self::Value, E> {
            Ok($ret)
        }
        fn visit_borrowed_str<E: de::Error>(
            self,
            _: &'de str,
        ) -> std::result::Result<Self::Value, E> {
            Ok($ret)
        }
    };
    (@arm $ret:expr, unit) => {
        fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok($ret)
        }
        fn visit_none<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok($ret)
        }
    };
    (@arm $ret:expr, seq) => {
        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok($ret)
        }
    };
    (@arm $ret:expr, map) => {
        fn visit_map<A: MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok($ret)
        }
    };
}

/// Zero-copy JSON string key helper that borrows unescaped slices and allocates only on escape sequences.
struct JsonStr<'de>(Cow<'de, str>);

impl<'de> Deserialize<'de> for JsonStr<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct StrVisitor;

        impl<'de> Visitor<'de> for StrVisitor {
            type Value = JsonStr<'de>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string")
            }

            fn visit_borrowed_str<E: de::Error>(
                self,
                v: &'de str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(JsonStr(Cow::Borrowed(v)))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                Ok(JsonStr(Cow::Owned(v.to_owned())))
            }

            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value, E> {
                Ok(JsonStr(Cow::Owned(v)))
            }
        }

        deserializer.deserialize_str(StrVisitor)
    }
}

/// Streaming JSON number extractor matching [`Value::as_f64`], silently ignoring non-numeric JSON values.
struct OptF64(Option<f64>);

impl<'de> Deserialize<'de> for OptF64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct OptF64Visitor;

        impl<'de> Visitor<'de> for OptF64Visitor {
            type Value = OptF64;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                Ok(OptF64(Some(v)))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(OptF64(Some(v as f64)))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(OptF64(Some(v as f64)))
            }

            ignore_json_values!(OptF64(None); bool, str, unit, seq, map);
        }

        deserializer.deserialize_any(OptF64Visitor)
    }
}

/// Streaming parser for `search_context_cost_per_query` objects (`medium` -> `low` -> `high`).
struct SearchContextCost(Option<f64>);

impl<'de> Deserialize<'de> for SearchContextCost {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct SearchVisitor;

        impl<'de> Visitor<'de> for SearchVisitor {
            type Value = SearchContextCost;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let (mut low, mut medium, mut high) = (None, None, None);
                while let Some(JsonStr(key)) = map.next_key::<JsonStr<'de>>()? {
                    match key.as_ref() {
                        "search_context_size_medium" => medium = map.next_value::<OptF64>()?.0,
                        "search_context_size_low" => low = map.next_value::<OptF64>()?.0,
                        "search_context_size_high" => high = map.next_value::<OptF64>()?.0,
                        _ => {
                            let _ = map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(SearchContextCost(Some(
                    medium.or(low).or(high).unwrap_or(0.0),
                )))
            }

            ignore_json_values!(SearchContextCost(None); bool, num, str, unit, seq);
        }

        deserializer.deserialize_any(SearchVisitor)
    }
}

/// Streaming parser for a two-element numeric `[min, max]` token range.
struct TierRangePair(Option<(i64, i64)>);

impl<'de> Deserialize<'de> for TierRangePair {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct PairVisitor;

        impl<'de> Visitor<'de> for PairVisitor {
            type Value = TierRangePair;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let Some(first) = seq.next_element::<OptF64>()? else {
                    return Ok(TierRangePair(None));
                };
                let Some(second) = seq.next_element::<OptF64>()? else {
                    return Ok(TierRangePair(None));
                };
                let mut extra = false;
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    extra = true;
                }
                if extra {
                    return Ok(TierRangePair(None));
                }
                Ok(TierRangePair(match (first.0, second.0) {
                    (Some(min), Some(max)) => Some((min as i64, max as i64)),
                    _ => None,
                }))
            }

            ignore_json_values!(TierRangePair(None); bool, num, str, unit, map);
        }

        deserializer.deserialize_any(PairVisitor)
    }
}

/// Streaming parser for one object inside `tiered_pricing`.
struct TierRangeEntry(Option<TierRange>);

impl<'de> Deserialize<'de> for TierRangeEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct EntryVisitor;

        impl<'de> Visitor<'de> for EntryVisitor {
            type Value = TierRangeEntry;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut range = None;
                let (mut input, mut output, mut cache_read, mut reasoning) = (0.0, 0.0, 0.0, 0.0);
                while let Some(JsonStr(key)) = map.next_key::<JsonStr<'de>>()? {
                    match key.as_ref() {
                        "range" => range = map.next_value::<TierRangePair>()?.0,
                        "input_cost_per_token" => {
                            input = map.next_value::<OptF64>()?.0.unwrap_or(0.0);
                        }
                        "output_cost_per_token" => {
                            output = map.next_value::<OptF64>()?.0.unwrap_or(0.0);
                        }
                        "cache_read_input_token_cost" => {
                            cache_read = map.next_value::<OptF64>()?.0.unwrap_or(0.0);
                        }
                        "output_cost_per_reasoning_token" => {
                            reasoning = map.next_value::<OptF64>()?.0.unwrap_or(0.0);
                        }
                        _ => {
                            let _ = map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(TierRangeEntry(range.map(|(min_tokens, max_tokens)| {
                    TierRange {
                        min_tokens,
                        max_tokens,
                        input_cost_per_token: input,
                        output_cost_per_token: output,
                        cache_read_input_token_cost: cache_read,
                        output_cost_per_reasoning_token: reasoning,
                    }
                })))
            }

            ignore_json_values!(TierRangeEntry(None); bool, num, str, unit, seq);
        }

        deserializer.deserialize_any(EntryVisitor)
    }
}

/// Streaming parser for `tiered_pricing` arrays.
struct TieredPricingRanges(Option<Vec<TierRange>>);

impl<'de> Deserialize<'de> for TieredPricingRanges {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct RangesVisitor;

        impl<'de> Visitor<'de> for RangesVisitor {
            type Value = TieredPricingRanges;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut ranges = Vec::new();
                while let Some(TierRangeEntry(entry)) = seq.next_element::<TierRangeEntry>()? {
                    if let Some(range) = entry {
                        ranges.push(range);
                    }
                }
                Ok(TieredPricingRanges((!ranges.is_empty()).then_some(ranges)))
            }

            ignore_json_values!(TieredPricingRanges(None); bool, num, str, unit, map);
        }

        deserializer.deserialize_any(RangesVisitor)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheLoadAbort {
    Legacy,
    InvalidRate,
}

struct PricingEntrySeed<'a> {
    abort: &'a mut Option<CacheLoadAbort>,
}

impl<'de> DeserializeSeed<'de> for PricingEntrySeed<'_> {
    type Value = Option<ModelPricing>;

    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for PricingEntrySeed<'_> {
    type Value = Option<ModelPricing>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a model pricing JSON entry")
    }

    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut pricing = ModelPricing::default();
        let mut raw_tiers: Vec<RawThresholdTier> = Vec::new();

        while let Some(JsonStr(key)) = map.next_key::<JsonStr<'de>>()? {
            match key.as_ref() {
                // Reject legacy serialized-`ModelPricing` cache files whole-file to force a refetch.
                "tiers" | "ranges" => {
                    *self.abort = Some(CacheLoadAbort::Legacy);
                    return Err(de::Error::custom("legacy pricing cache format"));
                }
                "tiered_pricing" => {
                    if let Some(ranges) = map.next_value::<TieredPricingRanges>()?.0 {
                        pricing.ranges = Some(ranges);
                    }
                }
                "search_context_cost_per_query" => {
                    if let Some(cost) = map.next_value::<SearchContextCost>()?.0 {
                        pricing.web_search_cost_per_query = cost;
                    }
                }
                "input_cost_per_token" => {
                    if let Some(v) = map.next_value::<OptF64>()?.0 {
                        pricing.input_cost_per_token = v;
                    }
                }
                "output_cost_per_token" => {
                    if let Some(v) = map.next_value::<OptF64>()?.0 {
                        pricing.output_cost_per_token = v;
                    }
                }
                "cache_read_input_token_cost" => {
                    if let Some(v) = map.next_value::<OptF64>()?.0 {
                        pricing.cache_read_input_token_cost = v;
                    }
                }
                "cache_creation_input_token_cost" => {
                    if let Some(v) = map.next_value::<OptF64>()?.0 {
                        pricing.cache_creation_input_token_cost = v;
                    }
                }
                "cache_creation_input_token_cost_above_1hr" => {
                    if let Some(v) = map.next_value::<OptF64>()?.0 {
                        pricing.cache_creation_input_token_cost_above_1hr = v;
                    }
                }
                "output_cost_per_reasoning_token" => {
                    if let Some(v) = map.next_value::<OptF64>()?.0 {
                        pricing.output_cost_per_reasoning_token = v;
                    }
                }
                other => {
                    if let Some((th, field)) = parse_tier_key(other) {
                        if let Some(v) = map.next_value::<OptF64>()?.0 {
                            set_tier_field(&mut raw_tiers, th, field, v);
                        }
                    } else {
                        let _ = map.next_value::<IgnoredAny>()?;
                    }
                }
            }
        }

        finalize_tiers(&mut pricing, raw_tiers);
        if let Some(ranges) = pricing.ranges.as_mut() {
            ranges.sort_by_key(|r| r.min_tokens);
        }

        Ok(Some(pricing))
    }

    ignore_json_values!(None; bool, num, str, unit, seq);
}

struct PricingCacheMapSeed<'a> {
    initial_capacity: usize,
    abort: &'a mut Option<CacheLoadAbort>,
}

impl<'de> DeserializeSeed<'de> for PricingCacheMapSeed<'_> {
    type Value = FastHashMap<Arc<str>, ModelPricing>;

    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for PricingCacheMapSeed<'_> {
    type Value = FastHashMap<Arc<str>, ModelPricing>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON object mapping model names to pricing objects")
    }

    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut entries = FastHashMap::with_capacity(self.initial_capacity);
        while let Some(JsonStr(key)) = map.next_key::<JsonStr<'de>>()? {
            let Some(pricing) = map.next_value_seed(PricingEntrySeed { abort: self.abort })? else {
                continue;
            };
            if !model_pricing_rates_are_valid(&pricing) {
                *self.abort = Some(CacheLoadAbort::InvalidRate);
                return Err(de::Error::custom("negative or non-finite pricing rate"));
            }
            if model_pricing_has_positive_rate(&pricing) {
                entries.insert(Arc::from(key.as_ref()), pricing);
            } else {
                entries.remove(key.as_ref());
            }
        }
        Ok(entries)
    }
}

/// Stream-parses raw LiteLLM pricing cache bytes directly into validated, priced [`ModelPricing`] entries without materializing a `serde_json::Value` DOM.
fn parse_litellm_cache_bytes(
    bytes: &[u8],
    cache_path: Option<&Path>,
) -> Result<FastHashMap<Arc<str>, ModelPricing>> {
    // Filtered LiteLLM entries average ~200–225 bytes each; pre-sizing avoids repeated rehashes.
    let initial_capacity = (bytes.len() / 220).min(16_384);
    let mut abort = None;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let parsed = PricingCacheMapSeed {
        initial_capacity,
        abort: &mut abort,
    }
    .deserialize(&mut deserializer);

    match abort {
        Some(CacheLoadAbort::Legacy) => {
            if let Some(path) = cache_path {
                log::warn!(
                    "Detected pre-Phase-2 pricing cache format at {:?}; refetching to avoid silent tier/range data loss",
                    path
                );
            }
            anyhow::bail!("legacy pricing cache format detected, forcing refetch");
        }
        Some(CacheLoadAbort::InvalidRate) => {
            anyhow::bail!("cached pricing JSON contains a negative or non-finite price");
        }
        None => {}
    }

    let pricing = parsed.context("Failed to parse cached pricing JSON")?;
    deserializer
        .end()
        .context("Failed to parse cached pricing JSON")?;
    if pricing.is_empty() {
        anyhow::bail!("cached pricing JSON has no priced models");
    }
    Ok(pricing)
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
pub fn load_from_cache_in(dir: &Path) -> Result<FastHashMap<Arc<str>, ModelPricing>> {
    let today = pricing_cache_date();
    let cache_path = find_pricing_cache_for_date_in(dir, &today)
        .ok_or_else(|| anyhow::anyhow!("No cache file found for today"))?;

    let bytes = fs::read(&cache_path).context("Failed to read cached pricing file")?;
    parse_litellm_cache_bytes(&bytes, Some(&cache_path))
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

    crate::utils::write_json_atomic(&cache_path, filtered_raw)
        .context("Failed to write pricing cache file")?;

    cleanup_old_cache_in(dir);
    Ok(())
}

/// Retains only models whose rates are all finite and non-negative and that have at least one positive base, tier, or range price.
#[cfg(test)]
pub fn normalize_pricing(
    mut pricing: HashMap<String, ModelPricing>,
) -> HashMap<String, ModelPricing> {
    pricing
        .retain(|_name, p| model_pricing_rates_are_valid(p) && model_pricing_has_positive_rate(p));
    pricing
}

fn model_pricing_has_positive_rate(p: &ModelPricing) -> bool {
    let has_base = p.input_cost_per_token > 0.0
        || p.output_cost_per_token > 0.0
        || p.cache_read_input_token_cost > 0.0
        || p.cache_creation_input_token_cost > 0.0
        || p.cache_creation_input_token_cost_above_1hr > 0.0
        || p.output_cost_per_reasoning_token > 0.0
        || p.web_search_cost_per_query > 0.0;
    if has_base {
        return true;
    }
    let has_positive_tier = p.tiers.iter().any(|t| {
        t.input_cost_per_token > 0.0
            || t.output_cost_per_token > 0.0
            || t.cache_read_input_token_cost > 0.0
            || t.cache_creation_input_token_cost > 0.0
            || t.cache_creation_input_token_cost_above_1hr > 0.0
    });
    if has_positive_tier {
        return true;
    }
    p.ranges.as_ref().is_some_and(|rs| {
        rs.iter().any(|r| {
            r.input_cost_per_token > 0.0
                || r.output_cost_per_token > 0.0
                || r.cache_read_input_token_cost > 0.0
                || r.output_cost_per_reasoning_token > 0.0
        })
    })
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
        let reloaded = parse_litellm_pricing_map(&filtered);

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
    fn streaming_cache_parser_rejects_legacy_tiers_array() {
        let legacy = serde_json::to_vec(&json!({
            "valid-first": {
                "input_cost_per_token": 1e-6
            },
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
        }))
        .unwrap();
        let err = parse_litellm_cache_bytes(&legacy, None)
            .expect_err("legacy tiers cache must be rejected");
        assert!(err.to_string().contains("legacy pricing cache format"));
    }

    #[test]
    fn streaming_cache_parser_rejects_legacy_ranges_field() {
        let legacy = serde_json::to_vec(&json!({
            "qwen-plus": {
                "ranges": [
                    {
                        "min_tokens": 0,
                        "max_tokens": 32_000,
                        "input_cost_per_token": 1e-6
                    }
                ]
            }
        }))
        .unwrap();
        let err = parse_litellm_cache_bytes(&legacy, None)
            .expect_err("legacy ranges cache must be rejected");
        assert!(err.to_string().contains("legacy pricing cache format"));
    }

    #[test]
    fn streaming_cache_parser_accepts_raw_format_and_matches_dom_parser() {
        let payload = json!({
            "non-object-string": "ignored",
            "non-object-array": [1, 2, 3],
            "zero-only": { "input_cost_per_token": 0.0 },
            "non-numeric": { "input_cost_per_token": "unknown" },
            "provider/escaped\"model": {
                "input_cost_per_token": 5e-6,
                "output_cost_per_token": 2.5e-5,
                "cache_read_input_token_cost": 5e-7,
                "cache_creation_input_token_cost": 6.25e-6,
                "cache_creation_input_token_cost_above_1hr": 1e-5,
                "output_cost_per_reasoning_token": 3e-5,
                "search_context_cost_per_query": {
                    "search_context_size_low": 0.005,
                    "search_context_size_medium": 0.01,
                    "search_context_size_high": 0.02
                }
            },
            "claude-sonnet-4-6": {
                "input_cost_per_token": 3e-6,
                "output_cost_per_token": 1.5e-5,
                "cache_creation_input_token_cost": 3.75e-6,
                "cache_creation_input_token_cost_above_1hr": 7.5e-6,
                "input_cost_per_token_above_200k_tokens": 6e-6,
                "output_cost_per_token_above_200k_tokens": 2.25e-5,
                "cache_creation_input_token_cost_above_200k_tokens": 7.5e-6,
                "cache_creation_input_token_cost_above_1hr_above_200k_tokens": 1.5e-5
            },
            "qwen3-coder-plus": {
                "tiered_pricing": [
                    {"max_results_range": [0, 25], "input_cost_per_query": 0.005},
                    {"range": [32000, 128000], "input_cost_per_token": 1.8e-6, "output_cost_per_token": 9e-6},
                    {"range": [0, 32000], "input_cost_per_token": 1e-6, "output_cost_per_token": 5e-6}
                ]
            }
        });
        let bytes = serde_json::to_vec(&payload).unwrap();
        let streamed = parse_litellm_cache_bytes(&bytes, None).expect("new format must load");
        let dom = parse_and_validate_litellm_map(&payload).expect("dom parse");

        assert_eq!(streamed.len(), 3);
        assert_eq!(streamed.len(), dom.len());
        for (k, expected) in &dom {
            let actual = streamed.get(k).expect("streamed map has key");
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "mismatch for model {k}"
            );
        }
    }

    #[test]
    fn streaming_cache_parser_rejects_negative_rate_anywhere_in_file() {
        for bad in [
            json!({
                "valid": { "input_cost_per_token": 1e-6 },
                "neg-base": { "input_cost_per_token": -1e-6 }
            }),
            json!({
                "valid": { "input_cost_per_token": 1e-6 },
                "neg-tier": { "input_cost_per_token_above_200k_tokens": -1e-6 }
            }),
            json!({
                "valid": { "input_cost_per_token": 1e-6 },
                "neg-range": {
                    "tiered_pricing": [{ "range": [0, 32000], "output_cost_per_token": -1e-6 }]
                }
            }),
            json!({
                "valid": { "input_cost_per_token": 1e-6 },
                "neg-search": {
                    "search_context_cost_per_query": { "search_context_size_medium": -0.01 }
                }
            }),
        ] {
            let bytes = serde_json::to_vec(&bad).unwrap();
            let err = parse_litellm_cache_bytes(&bytes, None)
                .expect_err("negative rate must reject entire cache");
            assert!(err.to_string().contains("negative or non-finite"));
        }
    }

    #[test]
    fn save_to_cache_writes_compact_json_and_reloads() {
        let dir = tempfile::TempDir::new().unwrap();
        let filtered = build_filtered_cost_json(&json!({
            "claude-sonnet-4-6": {
                "input_cost_per_token": 3e-6,
                "output_cost_per_token": 1.5e-5,
                "input_cost_per_token_above_200k_tokens": 6e-6,
                "max_input_tokens": 200_000
            }
        }));
        save_to_cache_in(dir.path(), &filtered).expect("save compact cache");

        let path = get_pricing_cache_path_in(dir.path(), &pricing_cache_date());
        let raw_text = fs::read_to_string(&path).expect("read saved cache");
        assert!(
            !raw_text.contains('\n'),
            "pricing cache should be serialized as compact JSON"
        );

        let loaded = load_from_cache_in(dir.path()).expect("reload compact cache");
        let sonnet = loaded.get("claude-sonnet-4-6").expect("model loaded");
        assert_eq!(sonnet.input_cost_per_token, 3e-6);
        assert_eq!(sonnet.tiers.len(), 1);
        assert_eq!(sonnet.tiers[0].threshold_tokens, 200_000);
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
