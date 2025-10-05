use serde_json::Value;
use std::collections::HashMap;

/// Process Claude usage data and merge into conversation_usage map
pub fn process_claude_usage(
    conversation_usage: &mut HashMap<String, Value>,
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

    // Add numeric fields
    for field in &[
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
    ] {
        if let Some(value) = usage_obj.get(*field).and_then(|v| v.as_i64()) {
            let current = existing_obj
                .get(*field)
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            existing_obj.insert(field.to_string(), (current + value).into());
        }
    }

    // Handle service_tier
    if let Some(service_tier) = usage_obj.get("service_tier").and_then(|v| v.as_str()) {
        existing_obj.insert("service_tier".to_string(), service_tier.into());
    }

    // Handle cache_creation nested object
    if let Some(cache_creation) = usage_obj.get("cache_creation").and_then(|v| v.as_object()) {
        let existing_cache = existing_obj
            .entry("cache_creation".to_string())
            .or_insert_with(|| serde_json::json!({}));

        if let Some(existing_cache_obj) = existing_cache.as_object_mut() {
            for (key, value) in cache_creation {
                if let Some(v) = value.as_i64() {
                    let current = existing_cache_obj
                        .get(key)
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    existing_cache_obj.insert(key.clone(), (current + v).into());
                }
            }
        }
    }
}

/// Process Codex usage data and merge into conversation_usage map
pub fn process_codex_usage(
    conversation_usage: &mut HashMap<String, Value>,
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

    // Process total_token_usage
    if let Some(total_usage) = info_obj
        .get("total_token_usage")
        .and_then(|v| v.as_object())
    {
        let existing_total = existing_obj
            .entry("total_token_usage".to_string())
            .or_insert_with(|| serde_json::json!({}));

        if let Some(existing_total_obj) = existing_total.as_object_mut() {
            for (key, value) in total_usage {
                if let Some(v) = value.as_i64() {
                    let current = existing_total_obj
                        .get(key)
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    existing_total_obj.insert(key.clone(), (current + v).into());
                }
            }
        }
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

/// Process Gemini usage data and merge into conversation_usage map
pub fn process_gemini_usage(
    conversation_usage: &mut HashMap<String, Value>,
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

    // Add input tokens
    let current_input = existing_obj
        .get("input_tokens")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    existing_obj.insert(
        "input_tokens".to_string(),
        (current_input + tokens.input).into(),
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
