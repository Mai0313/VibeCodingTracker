//! Per-provider usage accumulation: merges the token fields of one session
//! record into a running per-model map, normalising provider-specific shapes
//! along the way.

use crate::constants::FastHashMap;
use serde_json::Value;

/// Adds the named `i64` fields from `source` into `target`, in place.
///
/// For each name in `fields`, the matching integer in `source` is added to
/// the matching integer in `target` (treating a missing target as `0`).
/// Fields absent from `source`, or present but non-integer, are skipped — so
/// `target` only ever gains the keys that actually carried a number.
///
/// # Examples
///
/// ```
/// use serde_json::{json, Map};
/// use vibe_coding_tracker::utils::accumulate_i64_fields;
///
/// let mut target = Map::new();
/// target.insert("input".into(), json!(10));
///
/// let mut source = Map::new();
/// source.insert("input".into(), json!(5));
/// source.insert("output".into(), json!(7));
///
/// accumulate_i64_fields(&mut target, &source, &["input", "output"]);
/// assert_eq!(target["input"], json!(15));
/// assert_eq!(target["output"], json!(7));
/// ```
pub fn accumulate_i64_fields(
    target: &mut serde_json::Map<String, Value>,
    source: &serde_json::Map<String, Value>,
    fields: &[&str],
) {
    for field in fields {
        if let Some(value) = source.get(*field).and_then(|v| v.as_i64()) {
            let current = target.get(*field).and_then(|v| v.as_i64()).unwrap_or(0);
            target.insert(field.to_string(), (current + value).into());
        }
    }
}

/// Adds every `i64` field of `source_nested` into the nested object stored
/// at `target_obj[field_name]`.
///
/// The nested target object is created (as `{}`) if it does not yet exist.
/// Unlike [`accumulate_i64_fields`], the set of keys is taken from
/// `source_nested` rather than a fixed list, so any integer key present in
/// the source is merged. Non-integer source values are ignored.
///
/// # Examples
///
/// ```
/// use serde_json::{json, Map};
/// use vibe_coding_tracker::utils::accumulate_nested_object;
///
/// let mut target = Map::new();
/// target.insert("usage".into(), json!({ "input": 100 }));
///
/// let mut nested = Map::new();
/// nested.insert("input".into(), json!(25));
/// nested.insert("cached".into(), json!(10));
///
/// accumulate_nested_object(&mut target, "usage", &nested);
/// assert_eq!(target["usage"]["input"], json!(125));
/// assert_eq!(target["usage"]["cached"], json!(10));
/// ```
pub fn accumulate_nested_object(
    target_obj: &mut serde_json::Map<String, Value>,
    field_name: &str,
    source_nested: &serde_json::Map<String, Value>,
) {
    let target_nested = target_obj
        .entry(field_name.to_string())
        .or_insert_with(|| serde_json::json!({}));

    if let Some(target_nested_obj) = target_nested.as_object_mut() {
        for (key, value) in source_nested {
            if let Some(v) = value.as_i64() {
                let current = target_nested_obj
                    .get(key)
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                target_nested_obj.insert(key.clone(), (current + v).into());
            }
        }
    }
}

/// Merges one Claude usage record into `conversation_usage`, keyed by `model`.
///
/// Token fields accumulate across calls (the per-model entry is created on
/// first sight). `service_tier` is overwritten with the latest value rather
/// than accumulated, and the `cache_creation` TTL split is merged via
/// [`accumulate_nested_object`]. Records for synthetic models (whose name
/// contains `<synthetic>`) and non-object `usage` payloads are ignored.
pub fn process_claude_usage(
    conversation_usage: &mut FastHashMap<String, Value>,
    model: &str,
    usage: &Value,
) {
    // Skip synthetic models
    if model.contains("<synthetic>") {
        return;
    }

    let usage_obj = match usage.as_object() {
        Some(obj) => obj,
        None => return,
    };

    // Get or create usage entry
    let existing = conversation_usage
        .entry(model.to_string())
        .or_insert_with(|| {
            serde_json::json!({
                "input_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "cache_creation": {},
                "output_tokens": 0,
                "service_tier": ""
            })
        });

    let Some(existing_obj) = existing.as_object_mut() else {
        return;
    };

    // Accumulate numeric token fields
    accumulate_i64_fields(
        existing_obj,
        usage_obj,
        &[
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
            "output_tokens",
        ],
    );

    // Handle service_tier
    if let Some(service_tier) = usage_obj.get("service_tier").and_then(|v| v.as_str()) {
        existing_obj.insert("service_tier".to_string(), service_tier.into());
    }

    // Handle cache_creation nested object
    if let Some(cache_creation) = usage_obj.get("cache_creation").and_then(|v| v.as_object()) {
        accumulate_nested_object(existing_obj, "cache_creation", cache_creation);
    }
}

/// Merges one Codex usage record into `conversation_usage`, keyed by `model`.
///
/// `total_token_usage`, `last_token_usage`, and `model_context_window` are
/// *replaced* with the incoming values rather than accumulated, because each
/// Codex record already carries the running session total — adding them would
/// double-count. Synthetic models and non-object `info` payloads are ignored.
pub fn process_codex_usage(
    conversation_usage: &mut FastHashMap<String, Value>,
    model: &str,
    info: &Value,
) {
    // Skip synthetic models
    if model.contains("<synthetic>") {
        return;
    }

    let info_obj = match info.as_object() {
        Some(obj) => obj,
        None => return,
    };

    let existing = conversation_usage
        .entry(model.to_string())
        .or_insert_with(|| {
            serde_json::json!({
                "total_token_usage": {},
                "last_token_usage": {},
                "model_context_window": null
            })
        });

    let Some(existing_obj) = existing.as_object_mut() else {
        return;
    };

    // Process total_token_usage (replace, not accumulate - Codex already accumulates)
    if let Some(total_usage) = info_obj.get("total_token_usage") {
        existing_obj.insert("total_token_usage".to_string(), total_usage.clone());
    }

    // Process last_token_usage
    if let Some(last_usage) = info_obj.get("last_token_usage") {
        existing_obj.insert("last_token_usage".to_string(), last_usage.clone());
    }

    // Handle model_context_window
    if let Some(context_window) = info_obj.get("model_context_window") {
        existing_obj.insert("model_context_window".to_string(), context_window.clone());
    }
}

/// Merges one Gemini usage record into `conversation_usage`, keyed by `model`.
///
/// All token buckets accumulate across calls. Gemini reports `tokens.input`
/// as the *full* prompt count (cached subset included), so this function
/// stores `input - cached` under `input_tokens` and the cached portion under
/// `cache_read_input_tokens`, mirroring the Claude convention where input and
/// cache reads are disjoint. The subtraction is clamped at `0` to stay
/// defensive against a misreport where `cached > input`.
pub fn process_gemini_usage(
    conversation_usage: &mut FastHashMap<String, Value>,
    model: &str,
    tokens: &crate::models::GeminiTokens,
) {
    let existing = conversation_usage
        .entry(model.to_string())
        .or_insert_with(|| {
            serde_json::json!({
                "input_tokens": 0,
                "cache_read_input_tokens": 0,
                "output_tokens": 0,
                "thoughts_tokens": 0,
                "tool_tokens": 0,
                "total_tokens": 0,
            })
        });

    let Some(existing_obj) = existing.as_object_mut() else {
        return;
    };

    // Gemini's `tokens.input` is the full promptTokenCount (mirrors
    // Google's API), which already includes the cached subset reported
    // as `tokens.cached`. LiteLLM prices the two independently — if we
    // accumulated `tokens.input` verbatim, every cached token would be
    // billed at both `input_cost_per_token` and
    // `cache_read_input_token_cost`, inflating Gemini cost reports.
    //
    // Subtract cached from input before accumulating so downstream
    // bookkeeping matches the Claude convention (input ⊥ cache_read).
    // We verify this against the Gemini CLI event stream: every
    // observed record satisfies `total == input + output + thoughts`
    // with `cached` *not* added — i.e. cached is already folded into
    // `input`, not stored alongside it.
    let input_non_cached = (tokens.input - tokens.cached).max(0);

    let current_input = existing_obj
        .get("input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "input_tokens".to_string(),
        (current_input + input_non_cached).into(),
    );

    // Add cached tokens as cache_read_input_tokens
    let current_cached = existing_obj
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "cache_read_input_tokens".to_string(),
        (current_cached + tokens.cached).into(),
    );

    // Add output tokens
    let current_output = existing_obj
        .get("output_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "output_tokens".to_string(),
        (current_output + tokens.output).into(),
    );

    // Add thoughts tokens (Gemini-specific)
    let current_thoughts = existing_obj
        .get("thoughts_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "thoughts_tokens".to_string(),
        (current_thoughts + tokens.thoughts).into(),
    );

    // Add tool tokens (Gemini-specific)
    let current_tool = existing_obj
        .get("tool_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "tool_tokens".to_string(),
        (current_tool + tokens.tool).into(),
    );

    // Add total tokens
    let current_total = existing_obj
        .get("total_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "total_tokens".to_string(),
        (current_total + tokens.total).into(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_accumulate_i64_fields() {
        let mut target = serde_json::Map::new();
        target.insert("count".to_string(), json!(10));
        target.insert("total".to_string(), json!(100));

        let mut source = serde_json::Map::new();
        source.insert("count".to_string(), json!(5));
        source.insert("total".to_string(), json!(50));
        source.insert("new_field".to_string(), json!(25));

        accumulate_i64_fields(&mut target, &source, &["count", "total", "new_field"]);

        assert_eq!(target.get("count").unwrap().as_i64().unwrap(), 15);
        assert_eq!(target.get("total").unwrap().as_i64().unwrap(), 150);
        assert_eq!(target.get("new_field").unwrap().as_i64().unwrap(), 25);
    }

    #[test]
    fn test_accumulate_i64_fields_missing_source() {
        let mut target = serde_json::Map::new();
        target.insert("count".to_string(), json!(10));

        let source = serde_json::Map::new();

        accumulate_i64_fields(&mut target, &source, &["count", "missing"]);

        assert_eq!(target.get("count").unwrap().as_i64().unwrap(), 10);
        assert!(!target.contains_key("missing"));
    }

    #[test]
    fn test_accumulate_i64_fields_non_numeric() {
        let mut target = serde_json::Map::new();
        target.insert("count".to_string(), json!(10));

        let mut source = serde_json::Map::new();
        source.insert("count".to_string(), json!("not a number"));

        accumulate_i64_fields(&mut target, &source, &["count"]);

        assert_eq!(target.get("count").unwrap().as_i64().unwrap(), 10);
    }

    #[test]
    fn test_accumulate_nested_object() {
        let mut target = serde_json::Map::new();
        target.insert(
            "usage".to_string(),
            json!({
                "input": 100,
                "output": 50
            }),
        );

        let mut source_nested = serde_json::Map::new();
        source_nested.insert("input".to_string(), json!(25));
        source_nested.insert("output".to_string(), json!(15));
        source_nested.insert("cached".to_string(), json!(10));

        accumulate_nested_object(&mut target, "usage", &source_nested);

        let usage = target.get("usage").unwrap().as_object().unwrap();
        assert_eq!(usage.get("input").unwrap().as_i64().unwrap(), 125);
        assert_eq!(usage.get("output").unwrap().as_i64().unwrap(), 65);
        assert_eq!(usage.get("cached").unwrap().as_i64().unwrap(), 10);
    }

    #[test]
    fn test_accumulate_nested_object_new_field() {
        let mut target = serde_json::Map::new();

        let mut source_nested = serde_json::Map::new();
        source_nested.insert("input".to_string(), json!(100));
        source_nested.insert("output".to_string(), json!(50));

        accumulate_nested_object(&mut target, "usage", &source_nested);

        let usage = target.get("usage").unwrap().as_object().unwrap();
        assert_eq!(usage.get("input").unwrap().as_i64().unwrap(), 100);
        assert_eq!(usage.get("output").unwrap().as_i64().unwrap(), 50);
    }

    #[test]
    fn test_process_claude_usage_basic() {
        let mut conversation_usage = FastHashMap::default();
        let model = "claude-3-sonnet";
        let usage = json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 25
        });

        process_claude_usage(&mut conversation_usage, model, &usage);

        let result = conversation_usage.get(model).unwrap();
        assert_eq!(result["input_tokens"].as_i64().unwrap(), 100);
        assert_eq!(result["output_tokens"].as_i64().unwrap(), 50);
        assert_eq!(result["cache_read_input_tokens"].as_i64().unwrap(), 200);
        assert_eq!(result["cache_creation_input_tokens"].as_i64().unwrap(), 25);
    }

    #[test]
    fn test_process_claude_usage_accumulation() {
        let mut conversation_usage = FastHashMap::default();
        let model = "claude-3-sonnet";

        let usage1 = json!({
            "input_tokens": 100,
            "output_tokens": 50
        });
        process_claude_usage(&mut conversation_usage, model, &usage1);

        let usage2 = json!({
            "input_tokens": 75,
            "output_tokens": 25
        });
        process_claude_usage(&mut conversation_usage, model, &usage2);

        let result = conversation_usage.get(model).unwrap();
        assert_eq!(result["input_tokens"].as_i64().unwrap(), 175);
        assert_eq!(result["output_tokens"].as_i64().unwrap(), 75);
    }

    #[test]
    fn test_process_claude_usage_skip_synthetic() {
        let mut conversation_usage = FastHashMap::default();
        let model = "claude-3-sonnet<synthetic>";
        let usage = json!({
            "input_tokens": 100,
            "output_tokens": 50
        });

        process_claude_usage(&mut conversation_usage, model, &usage);

        assert!(conversation_usage.is_empty());
    }

    #[test]
    fn test_process_codex_usage_basic() {
        let mut conversation_usage = FastHashMap::default();
        let model = "gpt-4";
        let info = json!({
            "total_token_usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cached_input_tokens": 200
            },
            "model_context_window": 128000
        });

        process_codex_usage(&mut conversation_usage, model, &info);

        let result = conversation_usage.get(model).unwrap();
        let total_usage = result["total_token_usage"].as_object().unwrap();
        assert_eq!(total_usage["input_tokens"].as_i64().unwrap(), 100);
        assert_eq!(total_usage["output_tokens"].as_i64().unwrap(), 50);
        assert_eq!(total_usage["cached_input_tokens"].as_i64().unwrap(), 200);
        assert_eq!(result["model_context_window"].as_i64().unwrap(), 128000);
    }

    #[test]
    fn test_process_gemini_usage_basic() {
        let mut conversation_usage = FastHashMap::default();
        let model = "gemini-2.0-flash";
        // Use a realistic shape: `input` (300) includes `cached` (200),
        // so non-cached input is 100.
        let tokens = crate::models::GeminiTokens {
            input: 300,
            output: 50,
            cached: 200,
            thoughts: 10,
            tool: 5,
            total: 360,
        };

        process_gemini_usage(&mut conversation_usage, model, &tokens);

        let result = conversation_usage.get(model).unwrap();
        assert_eq!(
            result["input_tokens"].as_i64().unwrap(),
            100,
            "input must be stored as non-cached (input - cached) to match Claude semantics"
        );
        assert_eq!(result["output_tokens"].as_i64().unwrap(), 50);
        assert_eq!(result["cache_read_input_tokens"].as_i64().unwrap(), 200);
        assert_eq!(result["thoughts_tokens"].as_i64().unwrap(), 10);
        assert_eq!(result["tool_tokens"].as_i64().unwrap(), 5);
        assert_eq!(result["total_tokens"].as_i64().unwrap(), 360);
    }

    #[test]
    fn test_process_gemini_usage_no_cache() {
        // Sanity check: a record with `cached: 0` must not alter input.
        let mut conversation_usage = FastHashMap::default();
        let model = "gemini-2.0-flash";
        let tokens = crate::models::GeminiTokens {
            input: 13_906,
            output: 185,
            cached: 0,
            thoughts: 306,
            tool: 0,
            total: 14_397,
        };

        process_gemini_usage(&mut conversation_usage, model, &tokens);

        let result = conversation_usage.get(model).unwrap();
        assert_eq!(result["input_tokens"].as_i64().unwrap(), 13_906);
        assert_eq!(result["cache_read_input_tokens"].as_i64().unwrap(), 0);
    }
}
