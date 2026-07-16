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

/// Normalized above-threshold slice of one request's tokens.
///
/// Accumulated into the per-model `above_tier` object that
/// `extract_token_counts` reads back into `TokenCounts::above_*`, so
/// `calculate_cost` can bill this slice at the model's context-tier rate.
/// The buckets use the extractor's disjoint semantics (input excludes cache
/// reads, output excludes reasoning).
#[derive(Debug, Clone, Copy, Default)]
struct AboveTierSlice {
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_creation_5m: i64,
    cache_creation_1h: i64,
}

impl AboveTierSlice {
    fn accumulate_into(self, existing_obj: &mut serde_json::Map<String, Value>) {
        let mut slice = serde_json::Map::with_capacity(6);
        let mut push = |key: &str, value: i64| {
            if value != 0 {
                slice.insert(key.to_string(), value.into());
            }
        };
        push("input_tokens", self.input);
        push("output_tokens", self.output);
        push("reasoning_tokens", self.reasoning);
        push("cache_read_tokens", self.cache_read);
        push("cache_creation_5m_tokens", self.cache_creation_5m);
        push("cache_creation_1h_tokens", self.cache_creation_1h);
        if !slice.is_empty() {
            accumulate_nested_object(existing_obj, "above_tier", &slice);
        }
    }
}

/// The full prompt context of one Claude request: non-cached input plus
/// cache reads plus cache writes — what the provider compares against the
/// "above Nk tokens" threshold.
pub fn claude_request_context(usage_obj: &serde_json::Map<String, Value>) -> i64 {
    let field = |key: &str| usage_obj.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
    let mut cache_creation = field("cache_creation_input_tokens");
    if cache_creation == 0
        && let Some(split) = usage_obj.get("cache_creation").and_then(|v| v.as_object())
    {
        cache_creation = split.values().filter_map(|v| v.as_i64()).sum();
    }
    field("input_tokens") + field("cache_read_input_tokens") + cache_creation
}

/// Merges one Claude usage record into `conversation_usage`, keyed by `model`.
///
/// Token fields accumulate across calls (the per-model entry is created on
/// first sight). `service_tier` is overwritten with the latest value rather
/// than accumulated, and the `cache_creation` TTL split is merged via
/// [`accumulate_nested_object`]. Records for synthetic models (whose name
/// contains `<synthetic>`) and non-object `usage` payloads are ignored.
///
/// `above_tier` marks this record (one request) as classified above the
/// model's context-tier threshold: its buckets are additionally accumulated
/// into the `above_tier` slice that prices at the tier rate.
pub fn process_claude_usage(
    conversation_usage: &mut FastHashMap<String, Value>,
    model: &str,
    usage: &Value,
    above_tier: bool,
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

    // Handle server_tool_use nested object (web_search_requests /
    // web_fetch_requests). Accumulated so per-query web-search billing sees
    // the session total.
    if let Some(server_tool_use) = usage_obj.get("server_tool_use").and_then(|v| v.as_object()) {
        accumulate_nested_object(existing_obj, "server_tool_use", server_tool_use);
    }

    if above_tier {
        let field = |key: &str| usage_obj.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
        let scalar_cc = field("cache_creation_input_tokens");
        let (cc_5m, cc_1h) = match usage_obj.get("cache_creation").and_then(|v| v.as_object()) {
            Some(split) => {
                let ttl = |key: &str| split.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
                (
                    ttl("ephemeral_5m_input_tokens"),
                    ttl("ephemeral_1h_input_tokens"),
                )
            }
            None => (scalar_cc, 0),
        };
        AboveTierSlice {
            input: field("input_tokens"),
            output: field("output_tokens"),
            reasoning: 0,
            cache_read: field("cache_read_input_tokens"),
            cache_creation_5m: cc_5m,
            cache_creation_1h: cc_1h,
        }
        .accumulate_into(existing_obj);
    }
}

/// The five integer fields of a Codex `total_token_usage` object.
pub const CODEX_TOKEN_FIELDS: [&str; 5] = [
    "input_tokens",
    "cached_input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "total_tokens",
];

/// Snapshot of Codex's cumulative `total_token_usage` counters.
///
/// Codex publishes a whole-session running total on every `token_count`
/// event. Attribution therefore works on the *delta* between consecutive
/// snapshots, so a mid-session model switch bills each model only for the
/// tokens produced while it was current instead of double-counting the
/// pre-switch prefix under both models.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexTokenTotals {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

impl CodexTokenTotals {
    /// Reads the cumulative counters out of a `total_token_usage` object,
    /// treating missing or non-integer fields as `0`.
    pub fn from_total_object(total: &serde_json::Map<String, Value>) -> Self {
        let field = |key: &str| total.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
        Self {
            input_tokens: field("input_tokens"),
            cached_input_tokens: field("cached_input_tokens"),
            output_tokens: field("output_tokens"),
            reasoning_output_tokens: field("reasoning_output_tokens"),
            total_tokens: field("total_tokens"),
        }
    }

    fn field(&self, key: &str) -> i64 {
        match key {
            "input_tokens" => self.input_tokens,
            "cached_input_tokens" => self.cached_input_tokens,
            "output_tokens" => self.output_tokens,
            "reasoning_output_tokens" => self.reasoning_output_tokens,
            "total_tokens" => self.total_tokens,
            _ => 0,
        }
    }

    /// Builds the per-event delta map against `prev`, keeping only the keys
    /// actually present in `total` so absent fields stay absent downstream.
    ///
    /// A drop in `total_tokens` means the counter restarted (never observed
    /// in real logs, but cheap to guard): the event is treated as the start
    /// of a new segment rather than clamping every field to zero, which
    /// would silently drop the new segment's early tokens.
    pub fn delta_fields(
        total: &serde_json::Map<String, Value>,
        prev: Option<&Self>,
    ) -> serde_json::Map<String, Value> {
        let current = Self::from_total_object(total);
        let base = match prev {
            Some(prev) if current.total_tokens >= prev.total_tokens => *prev,
            _ => Self::default(),
        };
        let mut delta = serde_json::Map::new();
        for key in CODEX_TOKEN_FIELDS {
            if total.contains_key(key) {
                delta.insert(
                    key.to_string(),
                    (current.field(key) - base.field(key)).max(0).into(),
                );
            }
        }
        delta
    }
}

/// Merges one Codex usage delta into `conversation_usage`, keyed by `model`.
///
/// `delta` carries the per-event increments derived from consecutive
/// cumulative snapshots (see [`CodexTokenTotals`]) and is *accumulated* into
/// the model's `total_token_usage`, so each model ends up with exactly the
/// tokens produced while it was the active model. `last_token_usage` and
/// `model_context_window` are replaced with the latest values. Synthetic
/// models and non-object `info` payloads are ignored.
///
/// `above_tier` marks this turn as classified above the model's context-tier
/// threshold: the delta is additionally accumulated (in the extractor's
/// disjoint form) into the `above_tier` slice that prices at the tier rate.
pub fn process_codex_usage(
    conversation_usage: &mut FastHashMap<String, Value>,
    model: &str,
    delta: &serde_json::Map<String, Value>,
    info: &Value,
    above_tier: bool,
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

    accumulate_nested_object(existing_obj, "total_token_usage", delta);

    if above_tier {
        let field = |key: &str| delta.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
        let cached = field("cached_input_tokens");
        let reasoning = field("reasoning_output_tokens");
        AboveTierSlice {
            input: (field("input_tokens") - cached).max(0),
            output: (field("output_tokens") - reasoning).max(0),
            reasoning,
            cache_read: cached,
            cache_creation_5m: 0,
            cache_creation_1h: 0,
        }
        .accumulate_into(existing_obj);
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
///
/// `above_tier` marks this message (one request) as classified above the
/// model's context-tier threshold: its buckets are additionally accumulated
/// into the `above_tier` slice that prices at the tier rate.
pub fn process_gemini_usage(
    conversation_usage: &mut FastHashMap<String, Value>,
    model: &str,
    tokens: &crate::models::GeminiTokens,
    above_tier: bool,
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

    if above_tier {
        AboveTierSlice {
            input: input_non_cached,
            output: tokens.output,
            reasoning: tokens.thoughts,
            cache_read: tokens.cached,
            cache_creation_5m: 0,
            cache_creation_1h: 0,
        }
        .accumulate_into(existing_obj);
    }
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

        process_claude_usage(&mut conversation_usage, model, &usage, false);

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
        process_claude_usage(&mut conversation_usage, model, &usage1, false);

        let usage2 = json!({
            "input_tokens": 75,
            "output_tokens": 25
        });
        process_claude_usage(&mut conversation_usage, model, &usage2, false);

        let result = conversation_usage.get(model).unwrap();
        assert_eq!(result["input_tokens"].as_i64().unwrap(), 175);
        assert_eq!(result["output_tokens"].as_i64().unwrap(), 75);
    }

    #[test]
    fn test_process_claude_usage_accumulates_server_tool_use() {
        let mut conversation_usage = FastHashMap::default();
        let model = "claude-opus-4-8";

        process_claude_usage(
            &mut conversation_usage,
            model,
            &json!({
                "input_tokens": 10,
                "server_tool_use": { "web_search_requests": 2, "web_fetch_requests": 1 }
            }),
            false,
        );
        process_claude_usage(
            &mut conversation_usage,
            model,
            &json!({
                "input_tokens": 5,
                "server_tool_use": { "web_search_requests": 3, "web_fetch_requests": 0 }
            }),
            false,
        );

        let stu = conversation_usage.get(model).unwrap()["server_tool_use"]
            .as_object()
            .unwrap();
        assert_eq!(stu["web_search_requests"].as_i64().unwrap(), 5);
        assert_eq!(stu["web_fetch_requests"].as_i64().unwrap(), 1);
    }

    #[test]
    fn test_process_claude_usage_skip_synthetic() {
        let mut conversation_usage = FastHashMap::default();
        let model = "claude-3-sonnet<synthetic>";
        let usage = json!({
            "input_tokens": 100,
            "output_tokens": 50
        });

        process_claude_usage(&mut conversation_usage, model, &usage, false);

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
        let total = info["total_token_usage"].as_object().unwrap();
        let delta = CodexTokenTotals::delta_fields(total, None);

        process_codex_usage(&mut conversation_usage, model, &delta, &info, false);

        let result = conversation_usage.get(model).unwrap();
        let total_usage = result["total_token_usage"].as_object().unwrap();
        assert_eq!(total_usage["input_tokens"].as_i64().unwrap(), 100);
        assert_eq!(total_usage["output_tokens"].as_i64().unwrap(), 50);
        assert_eq!(total_usage["cached_input_tokens"].as_i64().unwrap(), 200);
        assert!(!total_usage.contains_key("total_tokens"));
        assert_eq!(result["model_context_window"].as_i64().unwrap(), 128000);
    }

    #[test]
    fn codex_delta_attributes_only_the_increment_to_the_current_model() {
        // Mirrors a real mid-session model switch: the cumulative counter at
        // the switch point must not be billed again under the second model.
        let mut conversation_usage = FastHashMap::default();

        let first = json!({
            "total_token_usage": {
                "input_tokens": 20_000,
                "cached_input_tokens": 4_000,
                "output_tokens": 3_289,
                "reasoning_output_tokens": 1_000,
                "total_tokens": 23_289
            }
        });
        let first_total = first["total_token_usage"].as_object().unwrap();
        let delta = CodexTokenTotals::delta_fields(first_total, None);
        process_codex_usage(
            &mut conversation_usage,
            "gpt-5.6-luna",
            &delta,
            &first,
            false,
        );
        let prev = CodexTokenTotals::from_total_object(first_total);

        let second = json!({
            "total_token_usage": {
                "input_tokens": 90_000,
                "cached_input_tokens": 60_000,
                "output_tokens": 14_576,
                "reasoning_output_tokens": 5_000,
                "total_tokens": 104_576
            }
        });
        let second_total = second["total_token_usage"].as_object().unwrap();
        let delta = CodexTokenTotals::delta_fields(second_total, Some(&prev));
        process_codex_usage(
            &mut conversation_usage,
            "gpt-5.6-sol",
            &delta,
            &second,
            false,
        );

        let luna = conversation_usage["gpt-5.6-luna"]["total_token_usage"]
            .as_object()
            .unwrap();
        let sol = conversation_usage["gpt-5.6-sol"]["total_token_usage"]
            .as_object()
            .unwrap();
        assert_eq!(luna["total_tokens"].as_i64().unwrap(), 23_289);
        assert_eq!(sol["total_tokens"].as_i64().unwrap(), 104_576 - 23_289);
        assert_eq!(sol["input_tokens"].as_i64().unwrap(), 70_000);
        assert_eq!(sol["cached_input_tokens"].as_i64().unwrap(), 56_000);
    }

    #[test]
    fn codex_delta_treats_a_counter_reset_as_a_new_segment() {
        // total_tokens dropping below the previous snapshot means the session
        // counter restarted; the event's own values are the new segment.
        let total = json!({
            "input_tokens": 500,
            "output_tokens": 100,
            "total_tokens": 600
        });
        let prev = CodexTokenTotals {
            input_tokens: 9_000,
            cached_input_tokens: 0,
            output_tokens: 1_000,
            reasoning_output_tokens: 0,
            total_tokens: 10_000,
        };
        let delta = CodexTokenTotals::delta_fields(total.as_object().unwrap(), Some(&prev));
        assert_eq!(delta["input_tokens"].as_i64().unwrap(), 500);
        assert_eq!(delta["output_tokens"].as_i64().unwrap(), 100);
        assert_eq!(delta["total_tokens"].as_i64().unwrap(), 600);
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

        process_gemini_usage(&mut conversation_usage, model, &tokens, false);

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

        process_gemini_usage(&mut conversation_usage, model, &tokens, false);

        let result = conversation_usage.get(model).unwrap();
        assert_eq!(result["input_tokens"].as_i64().unwrap(), 13_906);
        assert_eq!(result["cache_read_input_tokens"].as_i64().unwrap(), 0);
    }
}
