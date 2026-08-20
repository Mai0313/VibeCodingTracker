//! Neutral token-bucket merge / normalization helpers.
//!
//! These operate purely on the JSON usage shapes produced by the parsers, with
//! no `usage`- or `analysis`-feature knowledge, so both features (and the
//! shared scan cache) share one implementation instead of one reaching into the
//! other.

use crate::utils::{
    TierSlice, TokenCounts, accumulate_i64_fields, accumulate_nested_object, extract_token_counts,
};
use serde_json::{Value, json};

/// Accumulates the token fields of `new` into `existing` in place.
///
/// Dispatches on `existing`'s marker key: the flat provider shape
/// (`input_tokens`, plus the nested `cache_creation`, `server_tool_use` and
/// `above_tier` objects) or the Codex shape (`total_token_usage`, plus
/// `above_tier`). When the two sides carry *different* shapes, both are
/// normalized through [`extract_token_counts`] and `existing` is replaced
/// wholesale by the flat sum. Values that are not both JSON objects, or that
/// match neither shape, are left untouched.
pub(crate) fn merge_usage_values(existing: &mut Value, new: &Value) {
    let (Some(existing_ro), Some(new_ro)) = (existing.as_object(), new.as_object()) else {
        return;
    };
    let existing_flat = existing_ro.contains_key("input_tokens");
    let existing_codex = existing_ro.contains_key("total_token_usage");
    let new_flat = new_ro.contains_key("input_tokens");
    let new_codex = new_ro.contains_key("total_token_usage");

    // Mixed shapes — e.g. a Codex `total_token_usage` row and a Cursor /
    // Copilot flat `input_tokens` row that share a model name like `gpt-5`.
    // The branches below only accumulate when both sides carry the *same*
    // shape, so a mismatch would silently drop the other side's tokens.
    if (existing_flat && new_codex) || (existing_codex && new_flat) {
        let merged = add_token_counts(&extract_token_counts(existing), &extract_token_counts(new));
        *existing = token_counts_to_flat_value(&merged);
        return;
    }

    if let (Some(existing_obj), Some(new_obj)) = (existing.as_object_mut(), new.as_object()) {
        if existing_obj.contains_key("input_tokens") {
            accumulate_i64_fields(
                existing_obj,
                new_obj,
                &[
                    "input_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                    "output_tokens",
                    // Gemini's `thoughts_tokens` and the other flat providers'
                    // `reasoning_output_tokens` carry the same reasoning-budget
                    // semantics, so both accumulate; dropping either loses
                    // thinking-time tokens the model was billed for.
                    "thoughts_tokens",
                    "reasoning_output_tokens",
                    "tool_tokens",
                    "total_tokens",
                ],
            );

            if let Some(new_cache) = new_obj.get("cache_creation").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "cache_creation", new_cache);
            }

            // Claude server-side tool counts (web_search_requests /
            // web_fetch_requests) merge across files just like cache_creation.
            if let Some(new_stu) = new_obj.get("server_tool_use").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "server_tool_use", new_stu);
            }

            // Per-request tier slices accumulate the same way.
            if let Some(new_above) = new_obj.get("above_tier").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "above_tier", new_above);
            }
        }
        // Codex keeps its buckets nested, so the whole inner object accumulates
        // rather than a named field list.
        else if existing_obj.contains_key("total_token_usage") {
            if let Some(new_total) = new_obj.get("total_token_usage").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "total_token_usage", new_total);
            }
            if let Some(new_above) = new_obj.get("above_tier").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "above_tier", new_above);
            }
        }
    }
}

/// Sums two normalized [`TokenCounts`] field by field.
fn add_token_counts(a: &TokenCounts, b: &TokenCounts) -> TokenCounts {
    // Tier slices line up by index because the index *is* the price level; the
    // longer side's extra levels carry over untouched.
    let mut above_tiers = a.above_tiers.clone();
    above_tiers.resize(
        above_tiers.len().max(b.above_tiers.len()),
        TierSlice::default(),
    );
    for (slice, other) in above_tiers.iter_mut().zip(&b.above_tiers) {
        slice.merge(other);
    }

    TokenCounts {
        input_tokens: a.input_tokens + b.input_tokens,
        output_tokens: a.output_tokens + b.output_tokens,
        reasoning_tokens: a.reasoning_tokens + b.reasoning_tokens,
        cache_read: a.cache_read + b.cache_read,
        cache_creation: a.cache_creation + b.cache_creation,
        cache_creation_5m: a.cache_creation_5m + b.cache_creation_5m,
        cache_creation_1h: a.cache_creation_1h + b.cache_creation_1h,
        web_search_requests: a.web_search_requests + b.web_search_requests,
        tool_tokens: a.tool_tokens + b.tool_tokens,
        total: a.total + b.total,
        above_tiers,
    }
}

/// Normalizes any provider-shaped usage value into the flat key set.
///
/// `usage --json` rows pass through here so every model row carries the same
/// flat fields (`input_tokens` / `output_tokens` / `reasoning_output_tokens` /
/// `cache_read_input_tokens` / `cache_creation_input_tokens` / `total_tokens`)
/// regardless of provider. Without this, Codex-only models would serialize
/// their internal nested `total_token_usage` shape and consumers reading the
/// flat keys would see `null` for all of that model's tokens.
pub fn normalize_usage_value(usage: &Value) -> Value {
    token_counts_to_flat_value(&extract_token_counts(usage))
}

/// Serializes normalized counts back into the flat usage shape.
///
/// The key set is exactly what [`extract_token_counts`] reads for a flat value,
/// so every quantity round-trips — `total` included, which is why it is written
/// even where it equals the bucket sum. Deriving it back from the buckets
/// instead would hold only while no provider publishes a `total_tokens` larger
/// than its own buckets sum to, and one that did would lose the difference
/// here, the way `tool_tokens` used to.
fn token_counts_to_flat_value(c: &TokenCounts) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("input_tokens".into(), json!(c.input_tokens));
    obj.insert("output_tokens".into(), json!(c.output_tokens));
    obj.insert("reasoning_output_tokens".into(), json!(c.reasoning_tokens));
    obj.insert("cache_read_input_tokens".into(), json!(c.cache_read));
    obj.insert(
        "cache_creation_input_tokens".into(),
        json!(c.cache_creation),
    );
    if c.cache_creation_5m != 0 || c.cache_creation_1h != 0 {
        obj.insert(
            "cache_creation".into(),
            json!({
                "ephemeral_5m_input_tokens": c.cache_creation_5m,
                "ephemeral_1h_input_tokens": c.cache_creation_1h,
            }),
        );
    }
    if c.web_search_requests != 0 {
        obj.insert(
            "server_tool_use".into(),
            json!({ "web_search_requests": c.web_search_requests }),
        );
    }
    // Only Gemini publishes this, so writing it unconditionally would put a
    // `"tool_tokens": 0` on every other provider's row. Omitting it when it
    // carries tokens is what loses them: the extractor recomputes the total
    // from the keys it finds, and nothing else records them.
    if c.tool_tokens != 0 {
        obj.insert("tool_tokens".into(), json!(c.tool_tokens));
    }
    obj.insert("total_tokens".into(), json!(c.total));

    let mut above = serde_json::Map::new();
    for (index, slice) in c.above_tiers.iter().enumerate() {
        write_tier_slice(&mut above, index + 1, slice);
    }
    if !above.is_empty() {
        obj.insert("above_tier".into(), Value::Object(above));
    }
    Value::Object(obj)
}

/// Writes one price level's non-zero buckets as `level_<n>_<bucket>` keys.
///
/// Keeping the whole `above_tier` object a flat integer map is what lets it
/// merge through `accumulate_nested_object` like `cache_creation` and
/// `server_tool_use` do, whatever levels the two sides carry.
pub(crate) fn write_tier_slice(
    target: &mut serde_json::Map<String, Value>,
    level: usize,
    slice: &TierSlice,
) {
    let mut push = |bucket: &str, value: i64| {
        if value != 0 {
            target.insert(format!("level_{level}_{bucket}"), value.into());
        }
    };
    push("input_tokens", slice.input_tokens);
    push("output_tokens", slice.output_tokens);
    push("reasoning_tokens", slice.reasoning_tokens);
    push("cache_read_tokens", slice.cache_read);
    push("cache_creation_5m_tokens", slice.cache_creation_5m);
    push("cache_creation_1h_tokens", slice.cache_creation_1h);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_gemini_row_keeps_tool_tokens_in_its_buckets() {
        let normalized = normalize_usage_value(&json!({
            "input_tokens": 100,
            "output_tokens": 20,
            "cache_read_input_tokens": 50,
            "thoughts_tokens": 30,
            "tool_tokens": 7,
            "total_tokens": 150
        }));

        assert_eq!(normalized["tool_tokens"], json!(7));
        // Without the key the row would report a total its own buckets fall
        // short of by exactly the tool tokens.
        let buckets: i64 = [
            "input_tokens",
            "output_tokens",
            "reasoning_output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
            "tool_tokens",
        ]
        .iter()
        .map(|key| normalized[*key].as_i64().unwrap())
        .sum();
        assert_eq!(buckets, normalized["total_tokens"].as_i64().unwrap());
    }

    #[test]
    fn mixed_shape_merge_keeps_a_total_its_buckets_fall_short_of() {
        // The mixed branch replaces the row wholesale with the flat sum, so
        // anything the flat shape has no key for is gone. No provider publishes
        // a `total_tokens` larger than its own buckets sum to today, but
        // nothing stops one, and deriving the total back from the buckets is
        // what silently drops the difference — the way `tool_tokens` was lost.
        let mut existing = json!({
            "input_tokens": 100,
            "output_tokens": 20,
            "total_tokens": 500
        });
        merge_usage_values(
            &mut existing,
            &json!({
                "total_token_usage": {
                    "input_tokens": 50,
                    "output_tokens": 10,
                    "total_tokens": 60
                }
            }),
        );

        assert_eq!(existing["input_tokens"], json!(150));
        assert_eq!(existing["total_tokens"], json!(560));
        assert_eq!(extract_token_counts(&existing).total, 560);
    }

    #[test]
    fn mixed_shape_merge_keeps_each_tier_slice_on_its_own_level() {
        // Two rows for one model classified into different price levels. The
        // levels are positional, so folding one into the other's index would
        // bill a level-2 request at level 1's cheaper prices.
        let mut existing = json!({
            "input_tokens": 300_000,
            "above_tier": { "level_2_input_tokens": 300_000 }
        });
        merge_usage_values(
            &mut existing,
            &json!({
                "total_token_usage": { "input_tokens": 40_000, "total_tokens": 40_000 },
                "above_tier": { "level_1_input_tokens": 40_000 }
            }),
        );

        let counts = extract_token_counts(&existing);
        assert_eq!(counts.above_tiers[0].input_tokens, 40_000);
        assert_eq!(counts.above_tiers[1].input_tokens, 300_000);
    }

    #[test]
    fn normalized_row_without_tool_tokens_omits_the_key() {
        // Every provider but Gemini: the flat key set is unchanged.
        let normalized = normalize_usage_value(&json!({
            "input_tokens": 100,
            "output_tokens": 20,
            "cache_read_input_tokens": 50
        }));

        assert!(normalized.get("tool_tokens").is_none());
    }
}
