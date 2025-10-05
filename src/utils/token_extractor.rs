use serde_json::Value;

/// Extracted token counts from usage data
#[derive(Debug, Default)]
pub struct TokenCounts {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub total: i64,
}

/// Extract token counts from usage value (supports Claude, Codex, and Gemini formats)
pub fn extract_token_counts(usage: &Value) -> TokenCounts {
    let mut counts = TokenCounts::default();

    if let Some(usage_obj) = usage.as_object() {
        // Claude/Gemini usage format
        if let Some(input) = usage_obj.get("input_tokens").and_then(|v| v.as_i64()) {
            counts.input_tokens = input;
        }
        if let Some(output) = usage_obj.get("output_tokens").and_then(|v| v.as_i64()) {
            counts.output_tokens = output;
        }
        if let Some(cache_read) = usage_obj
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_i64())
        {
            counts.cache_read = cache_read;
        }
        if let Some(cache_creation) = usage_obj
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_i64())
        {
            counts.cache_creation = cache_creation;
        }

        // Codex usage format (has total_token_usage nested object)
        if let Some(total_usage) = usage_obj
            .get("total_token_usage")
            .and_then(|v| v.as_object())
        {
            if let Some(input) = total_usage.get("input_tokens").and_then(|v| v.as_i64()) {
                counts.input_tokens = input;
            }
            if let Some(output) = total_usage.get("output_tokens").and_then(|v| v.as_i64()) {
                counts.output_tokens += output;
            }
            if let Some(reasoning) = total_usage
                .get("reasoning_output_tokens")
                .and_then(|v| v.as_i64())
            {
                counts.output_tokens += reasoning;
            }
            if let Some(cache_read) = total_usage
                .get("cached_input_tokens")
                .and_then(|v| v.as_i64())
            {
                counts.cache_read = cache_read;
            }
            if let Some(total) = total_usage.get("total_tokens").and_then(|v| v.as_i64()) {
                counts.total = total;
                return counts; // If total is available, use it directly
            }
        }

        // Calculate total if not provided
        counts.total =
            counts.input_tokens + counts.output_tokens + counts.cache_read + counts.cache_creation;
    }

    counts
}
