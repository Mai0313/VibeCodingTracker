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
///
/// `reasoning_tokens` carries the model's "thinking" budget emitted as part
/// of the assistant turn but billed separately from user-visible output.
/// Populated by Gemini (`thoughts_tokens`), Codex
/// (`reasoning_output_tokens`), and Copilot (`reasoning_output_tokens`
/// after `copilot_analyzer` normalises it). Claude has no equivalent and
/// leaves this at 0.
#[derive(Debug, Default)]
pub struct TokenCounts {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub cache_creation_5m: i64,
    pub cache_creation_1h: i64,
    pub total: i64,
}

/// Extracts token counts from usage data in any provider format
///
/// Supports three formats:
/// - Claude/Gemini/Copilot: Direct fields like `input_tokens`, `output_tokens`
/// - Codex: Nested `total_token_usage` object with different field names
///
/// Reasoning tokens (Gemini `thoughts_tokens`, Codex
/// `reasoning_output_tokens`, Copilot `reasoning_output_tokens`) are no
/// longer folded into `output_tokens`. Keeping them separate is what lets
/// `calculate_cost` bill them at `output_cost_per_reasoning_token` for
/// providers that publish a distinct reasoning rate (e.g. Gemini 2.5
/// Flash, Perplexity Sonar Deep Research, dashscope/qwen-turbo).
///
/// Returns normalized TokenCounts structure.
pub fn extract_token_counts(usage: &Value) -> TokenCounts {
    let mut counts = TokenCounts::default();

    if let Some(usage_obj) = usage.as_object() {
        // Claude/Gemini/Copilot usage format (flat fields)
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

        // Gemini writes reasoning budget as `thoughts_tokens`; Copilot's
        // shutdown usage is normalised to `reasoning_output_tokens` by
        // `copilot_analyzer::analyze_copilot_events`. Either key feeds the
        // same bucket — we never see both on the same record.
        if let Some(thoughts) = usage_obj.get("thoughts_tokens").and_then(|v| v.as_i64()) {
            counts.reasoning_tokens = thoughts;
        }
        if let Some(reasoning) = usage_obj
            .get("reasoning_output_tokens")
            .and_then(|v| v.as_i64())
        {
            counts.reasoning_tokens = reasoning;
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
            // Codex follows OpenAI's convention: `input_tokens` is the
            // full prompt size and `cached_input_tokens` is the subset
            // that hit the prompt cache. LiteLLM, in contrast, prices
            // non-cached input (`input_cost_per_token`) and cached reads
            // (`cache_read_input_token_cost`) *separately*. Forwarding
            // Codex's raw `input_tokens` to `calculate_cost` alongside
            // the same `cached_input_tokens` would charge every cached
            // token twice — once at the full input rate, then again at
            // the cache read rate — inflating Codex cost reports by
            // ~130% on heavy-cache sessions.
            //
            // Subtract cached from raw input so each token is billed
            // exactly once, at the right rate.
            let raw_input = total_usage
                .get("input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let cached = total_usage
                .get("cached_input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            counts.input_tokens = (raw_input - cached).max(0);
            counts.cache_read = cached;

            if let Some(output) = total_usage.get("output_tokens").and_then(|v| v.as_i64()) {
                // Codex already accumulates across turns, so replace instead
                // of adding — the value is the running total for the session.
                counts.output_tokens = output;
            }
            if let Some(reasoning) = total_usage
                .get("reasoning_output_tokens")
                .and_then(|v| v.as_i64())
            {
                counts.reasoning_tokens = reasoning;
            }
            if let Some(total) = total_usage.get("total_tokens").and_then(|v| v.as_i64()) {
                // Codex `total_tokens` = input (incl. cached) + output and
                // excludes `reasoning_output_tokens`. We already split
                // input / cached above, so the numeric total the user sees
                // still matches: (non_cached_input + cached) + output +
                // reasoning == Codex's `total_tokens` + reasoning.
                counts.total = total + counts.reasoning_tokens;
                return counts;
            }
        }

        // Calculate total if not provided
        counts.total = counts.input_tokens
            + counts.output_tokens
            + counts.reasoning_tokens
            + counts.cache_read
            + counts.cache_creation;
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
                "total_tokens": 1500
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.cache_creation, 0);
        assert_eq!(c.cache_creation_5m, 0);
        assert_eq!(c.cache_creation_1h, 0);
        assert_eq!(c.cache_read, 200);
        // Codex `input_tokens` includes `cached_input_tokens`; the extractor
        // must subtract cached so the two buckets don't overlap.
        assert_eq!(c.input_tokens, 800);
    }

    #[test]
    fn codex_input_tokens_excludes_cached_bucket() {
        // Taken from a real Codex session: input=576_145 includes
        // cached=408_832. The extractor must split the two so
        // `calculate_cost` doesn't bill every cached token twice.
        let usage = json!({
            "total_token_usage": {
                "input_tokens": 576_145,
                "cached_input_tokens": 408_832,
                "output_tokens": 13_156,
                "reasoning_output_tokens": 8_591,
                "total_tokens": 589_301
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.input_tokens, 576_145 - 408_832);
        assert_eq!(c.cache_read, 408_832);
        assert_eq!(c.output_tokens, 13_156);
        assert_eq!(c.reasoning_tokens, 8_591);
        // Published total + reasoning. Equivalent to
        // (input_tokens + cache_read) + output_tokens + reasoning_tokens.
        assert_eq!(c.total, 589_301 + 8_591);
    }

    #[test]
    fn codex_cached_exceeding_input_clamps_to_zero() {
        // Defensive: never emit a negative `input_tokens` even if the
        // provider ever misreports cached > input. Better to undercount
        // than to panic via integer overflow downstream.
        let usage = json!({
            "total_token_usage": {
                "input_tokens": 100,
                "cached_input_tokens": 500,
                "output_tokens": 50,
                "total_tokens": 150
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.input_tokens, 0);
        assert_eq!(c.cache_read, 500);
    }

    #[test]
    fn gemini_thoughts_tokens_populate_reasoning_bucket() {
        // Drives Gemini 2.5 flash pricing — thoughts_tokens must reach the
        // reasoning bucket instead of being silently dropped or bundled
        // into output.
        let usage = json!({
            "input_tokens": 13_906,
            "output_tokens": 185,
            "cache_read_input_tokens": 0,
            "thoughts_tokens": 306,
            "tool_tokens": 0,
            "total_tokens": 14_397
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.input_tokens, 13_906);
        assert_eq!(c.output_tokens, 185);
        assert_eq!(c.reasoning_tokens, 306);
        // Our recomputed total includes reasoning (tool_tokens are not
        // accounted for — we never bill against them).
        assert_eq!(c.total, 13_906 + 185 + 306);
    }

    #[test]
    fn codex_reasoning_is_separated_from_output() {
        // Mimics a real Codex `event_msg` record mid-session.
        let usage = json!({
            "total_token_usage": {
                "input_tokens": 5_645,
                "cached_input_tokens": 5_504,
                "output_tokens": 810,
                "reasoning_output_tokens": 640,
                "total_tokens": 6_455
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.input_tokens, 5_645 - 5_504);
        assert_eq!(c.cache_read, 5_504);
        assert_eq!(
            c.output_tokens, 810,
            "reasoning must no longer fold into output_tokens"
        );
        assert_eq!(c.reasoning_tokens, 640);
        // Codex `total_tokens` excludes reasoning, so `total` ≥ published total.
        assert_eq!(c.total, 6_455 + 640);
    }

    #[test]
    fn copilot_reasoning_output_tokens_populate_reasoning_bucket() {
        // After `copilot_analyzer` normalisation, Copilot sessions use the
        // same flat `reasoning_output_tokens` key as Codex's nested one.
        let usage = json!({
            "input_tokens": 2_000,
            "output_tokens": 300,
            "cache_read_input_tokens": 100,
            "cache_creation_input_tokens": 0,
            "reasoning_output_tokens": 150
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.output_tokens, 300);
        assert_eq!(c.reasoning_tokens, 150);
        assert_eq!(c.total, 2_000 + 300 + 150 + 100);
    }
}
