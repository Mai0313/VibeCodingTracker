//! Neutral token-bucket merge / normalization helpers.
//!
//! These operate purely on the JSON usage shapes produced by the parsers, with
//! no `usage`- or `analysis`-feature knowledge, so both features (and the
//! shared scan cache) share one implementation instead of one reaching into the
//! other.

use crate::utils::{
    TokenCounts, accumulate_i64_fields, accumulate_nested_object, extract_token_counts,
};
use serde_json::{Value, json};

/// Accumulates the token fields of `new` into `existing` in place.
///
/// Detects the on-disk usage shape from a marker key and merges accordingly:
/// the flat provider shape (keyed by `input_tokens`, including the
/// nested `cache_creation` breakdown) or the Codex shape (keyed by
/// `total_token_usage`). Values that are not both JSON objects, or that match
/// neither shape, are left untouched.
pub(crate) fn merge_usage_values(existing: &mut Value, new: &Value) {
    let (Some(existing_ro), Some(new_ro)) = (existing.as_object(), new.as_object()) else {
        return;
    };
    let existing_flat = existing_ro.contains_key("input_tokens");
    let existing_codex = existing_ro.contains_key("total_token_usage");
    let new_flat = new_ro.contains_key("input_tokens");
    let new_codex = new_ro.contains_key("total_token_usage");

    // Mixed shapes — e.g. a Codex `total_token_usage` row and a Cursor / Copilot
    // flat `input_tokens` row that share a model name like `gpt-5`. The
    // shape-specific branches below only accumulate when both sides carry the
    // *same* shape, so a mismatch would silently drop the other side's tokens.
    // Normalize both to disjoint counts and rewrite `existing` as a flat value
    // that keeps every bucket (and round-trips through `extract_token_counts`).
    if (existing_flat && new_codex) || (existing_codex && new_flat) {
        let merged = add_token_counts(&extract_token_counts(existing), &extract_token_counts(new));
        *existing = token_counts_to_flat_value(&merged);
        return;
    }

    if let (Some(existing_obj), Some(new_obj)) = (existing.as_object_mut(), new.as_object()) {
        // Handle the flat provider format (has input_tokens)
        if existing_obj.contains_key("input_tokens") {
            accumulate_i64_fields(
                existing_obj,
                new_obj,
                &[
                    "input_tokens",
                    "cache_creation_input_tokens",
                    "cache_read_input_tokens",
                    "output_tokens",
                    // Gemini `thoughts_tokens` and Copilot's normalised
                    // `reasoning_output_tokens` both carry the same
                    // reasoning-budget semantics and must accumulate so
                    // cross-provider aggregation in `usage` doesn't drop
                    // the thinking-time tokens the model was actually
                    // billed for.
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
        // Handle Codex format (has total_token_usage)
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
    TokenCounts {
        input_tokens: a.input_tokens + b.input_tokens,
        output_tokens: a.output_tokens + b.output_tokens,
        reasoning_tokens: a.reasoning_tokens + b.reasoning_tokens,
        cache_read: a.cache_read + b.cache_read,
        cache_creation: a.cache_creation + b.cache_creation,
        cache_creation_5m: a.cache_creation_5m + b.cache_creation_5m,
        cache_creation_1h: a.cache_creation_1h + b.cache_creation_1h,
        web_search_requests: a.web_search_requests + b.web_search_requests,
        total: a.total + b.total,
        above_input: a.above_input + b.above_input,
        above_output: a.above_output + b.above_output,
        above_reasoning: a.above_reasoning + b.above_reasoning,
        above_cache_read: a.above_cache_read + b.above_cache_read,
        above_cache_creation_5m: a.above_cache_creation_5m + b.above_cache_creation_5m,
        above_cache_creation_1h: a.above_cache_creation_1h + b.above_cache_creation_1h,
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
    let counts = extract_token_counts(usage);
    let mut value = token_counts_to_flat_value(&counts);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("total_tokens".into(), json!(counts.total));
    }
    value
}

/// Serializes normalized counts back into the flat usage shape.
///
/// The key set is exactly what [`extract_token_counts`] reads for a flat value,
/// so the result round-trips: re-extracting it yields the same counts. `total`
/// is intentionally omitted (the extractor recomputes it as the bucket sum).
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
    if c.above_input != 0
        || c.above_output != 0
        || c.above_reasoning != 0
        || c.above_cache_read != 0
        || c.above_cache_creation_5m != 0
        || c.above_cache_creation_1h != 0
    {
        obj.insert(
            "above_tier".into(),
            json!({
                "input_tokens": c.above_input,
                "output_tokens": c.above_output,
                "reasoning_tokens": c.above_reasoning,
                "cache_read_tokens": c.above_cache_read,
                "cache_creation_5m_tokens": c.above_cache_creation_5m,
                "cache_creation_1h_tokens": c.above_cache_creation_1h,
            }),
        );
    }
    Value::Object(obj)
}
