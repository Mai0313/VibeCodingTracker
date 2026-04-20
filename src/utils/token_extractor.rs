use serde_json::Value;

/// Normalized token counts extracted from provider-specific usage data.
///
/// `cache_creation` is the total across TTL variants. For Claude Code, the
/// `cache_creation_5m` / `cache_creation_1h` split reflects Anthropic's two
/// cache TTL tiers (5 minutes default, 1 hour extended — the latter is ~60%
/// more expensive per write). Providers that don't split TTL (Codex, Gemini,
/// older Claude Code records) get all cache_creation tokens into the 5m bucket.
///
/// Invariant: `cache_creation == cache_creation_5m + cache_creation_1h` when
/// the split data is available.
#[derive(Debug, Default)]
pub struct TokenCounts {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub cache_creation_5m: i64,
    pub cache_creation_1h: i64,
    pub total: i64,
}

/// Extracts token counts from usage data in any provider format
///
/// Supports three formats:
/// - Claude/Gemini: Direct fields like `input_tokens`, `output_tokens`
/// - Codex: Nested `total_token_usage` object with different field names
///
/// Returns normalized TokenCounts structure.
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

        // Claude Code records cache_creation split by TTL:
        //   "cache_creation": { ephemeral_5m_input_tokens, ephemeral_1h_input_tokens }
        // When present, use it verbatim for accurate 1hr TTL pricing.
        if let Some(cc_split) = usage_obj.get("cache_creation").and_then(|v| v.as_object()) {
            counts.cache_creation_5m = cc_split
                .get("ephemeral_5m_input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            counts.cache_creation_1h = cc_split
                .get("ephemeral_1h_input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            // If the split has tokens but the scalar total was missing, derive it.
            if counts.cache_creation == 0 {
                counts.cache_creation = counts.cache_creation_5m + counts.cache_creation_1h;
            }
        } else {
            // No TTL split → treat every cache_creation token as default (5 min) TTL.
            counts.cache_creation_5m = counts.cache_creation;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_format_without_ttl_split_defaults_to_5m() {
        // Old-style Claude record or provider that predates the ephemeral split.
        let usage = json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 10_000
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.cache_creation, 10_000);
        assert_eq!(c.cache_creation_5m, 10_000);
        assert_eq!(c.cache_creation_1h, 0);
    }

    #[test]
    fn claude_code_ttl_split_is_preserved() {
        let usage = json!({
            "input_tokens": 6,
            "output_tokens": 866,
            "cache_read_input_tokens": 16_651,
            "cache_creation_input_tokens": 10_338,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 0,
                "ephemeral_1h_input_tokens": 10_338
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.cache_creation, 10_338);
        assert_eq!(c.cache_creation_5m, 0);
        assert_eq!(c.cache_creation_1h, 10_338);
    }

    #[test]
    fn ttl_split_mixed_5m_and_1h() {
        let usage = json!({
            "cache_creation_input_tokens": 1_500,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 500,
                "ephemeral_1h_input_tokens": 1_000
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.cache_creation, 1_500);
        assert_eq!(c.cache_creation_5m, 500);
        assert_eq!(c.cache_creation_1h, 1_000);
    }

    #[test]
    fn ttl_split_derives_total_when_scalar_missing() {
        // Edge case: only the split object is present, not the scalar total.
        let usage = json!({
            "cache_creation": {
                "ephemeral_5m_input_tokens": 200,
                "ephemeral_1h_input_tokens": 300
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.cache_creation, 500);
        assert_eq!(c.cache_creation_5m, 200);
        assert_eq!(c.cache_creation_1h, 300);
    }

    #[test]
    fn codex_format_no_cache_creation() {
        // Codex JSONL uses total_token_usage; no cache_creation concept at all.
        let usage = json!({
            "total_token_usage": {
                "input_tokens": 1000,
                "output_tokens": 500,
                "cached_input_tokens": 200,
                "total_tokens": 1700
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.cache_creation, 0);
        assert_eq!(c.cache_creation_5m, 0);
        assert_eq!(c.cache_creation_1h, 0);
        assert_eq!(c.cache_read, 200);
    }
}
