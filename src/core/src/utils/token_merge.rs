//! Provider-agnostic JSON usage merging and flat-schema normalization.

use crate::utils::{
    TierSlice, TokenCounts, accumulate_i64_fields, accumulate_nested_object, extract_token_counts,
};
use serde_json::{Value, json};

/// Accumulates `new`'s token fields into `existing` in place, normalizing to the flat schema when the two sides have different shapes (`input_tokens` vs `total_token_usage`).
pub(crate) fn merge_usage_values(existing: &mut Value, new: &Value) {
    let (Some(existing_ro), Some(new_ro)) = (existing.as_object(), new.as_object()) else {
        return;
    };
    let existing_flat = existing_ro.contains_key("input_tokens");
    let existing_codex = existing_ro.contains_key("total_token_usage");
    let new_flat = new_ro.contains_key("input_tokens");
    let new_codex = new_ro.contains_key("total_token_usage");

    // Mixed shapes (for example, Codex `total_token_usage` + flat `input_tokens` under a shared model name) normalize both sides so neither is dropped.
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
                    // Accumulate both flat reasoning keys (`thoughts_tokens` for Gemini, `reasoning_output_tokens` for other flat providers).
                    "thoughts_tokens",
                    "reasoning_output_tokens",
                    "tool_tokens",
                    "total_tokens",
                ],
            );

            if let Some(new_cache) = new_obj.get("cache_creation").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "cache_creation", new_cache);
            }

            if let Some(new_stu) = new_obj.get("server_tool_use").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "server_tool_use", new_stu);
            }

            if let Some(new_above) = new_obj.get("above_tier").and_then(|v| v.as_object()) {
                accumulate_nested_object(existing_obj, "above_tier", new_above);
            }
        } else if existing_obj.contains_key("total_token_usage") {
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
    // Tier slices are indexed by price level; extra levels on the longer side carry over unchanged.
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

/// Returns `usage` normalized into the flat JSON token schema (`input_tokens`, `output_tokens`, `reasoning_output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, `total_tokens`).
///
/// Optional buckets (`cache_creation`, `server_tool_use`, `tool_tokens`, `above_tier`) are included only when non-zero.
pub fn normalize_usage_value(usage: &Value) -> Value {
    token_counts_to_flat_value(&extract_token_counts(usage))
}

/// Serializes normalized [`TokenCounts`] into the flat JSON schema readable by [`extract_token_counts`], always preserving `total_tokens`.
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
    // Omit `tool_tokens` when zero so non-Gemini rows stay clean, but preserve non-zero values so `extract_token_counts` round-trips them.
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

/// Writes non-zero buckets of `slice` into `target` as `level_<level>_<bucket>` keys.
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
        // Without `tool_tokens`, the bucket sum would fall short of `total_tokens`.
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
        let normalized = normalize_usage_value(&json!({
            "input_tokens": 100,
            "output_tokens": 20,
            "cache_read_input_tokens": 50
        }));

        assert!(normalized.get("tool_tokens").is_none());
    }
}
