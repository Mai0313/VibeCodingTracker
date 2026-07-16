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
/// after `session::copilot::parse_copilot_events` normalises it). Claude
/// has no equivalent and leaves this at 0.
#[derive(Debug, Default)]
pub struct TokenCounts {
    /// Non-cached prompt tokens (cached reads are excluded; see `cache_read`).
    pub input_tokens: i64,
    /// User-visible completion tokens, excluding reasoning.
    pub output_tokens: i64,
    /// Model "thinking" tokens, billed separately from `output_tokens`.
    pub reasoning_tokens: i64,
    /// Prompt tokens served from the provider's prompt cache.
    pub cache_read: i64,
    /// Total cache-write tokens across all TTL tiers.
    pub cache_creation: i64,
    /// Cache-write tokens at the default 5-minute TTL.
    pub cache_creation_5m: i64,
    /// Cache-write tokens at the extended 1-hour TTL.
    pub cache_creation_1h: i64,
    /// Server-side web-search requests (Claude `server_tool_use`). Billed
    /// per query (not per token) at the model's web-search rate, so it is
    /// tracked here but excluded from `total`.
    pub web_search_requests: i64,
    /// Sum of the billed buckets used for cost and display.
    pub total: i64,
    /// Slice of `input_tokens` from requests whose own prompt context
    /// exceeded the model's context-tier threshold (see the `above_tier`
    /// object written by the usage parsers). Always a subset of the field it
    /// mirrors — never additional tokens — so displays keep using the totals
    /// above while `calculate_cost` bills this slice at the tier rate.
    pub above_input: i64,
    /// Above-threshold slice of `output_tokens`.
    pub above_output: i64,
    /// Above-threshold slice of `reasoning_tokens`.
    pub above_reasoning: i64,
    /// Above-threshold slice of `cache_read`.
    pub above_cache_read: i64,
    /// Above-threshold slice of `cache_creation_5m`.
    pub above_cache_creation_5m: i64,
    /// Above-threshold slice of `cache_creation_1h`.
    pub above_cache_creation_1h: i64,
}

/// Extracts token counts from usage data in any provider format
///
/// Supports two shapes:
/// - Flat providers: direct fields like `input_tokens`, `output_tokens`
/// - Codex: Nested `total_token_usage` object with different field names
///
/// Reasoning tokens (Gemini `thoughts_tokens`, Codex
/// `reasoning_output_tokens`, Copilot `reasoning_output_tokens`) are no
/// longer folded into `output_tokens`. Keeping them separate is what lets
/// `calculate_cost` bill them at `output_cost_per_reasoning_token` for
/// providers that publish a distinct reasoning rate (e.g. Gemini 2.5
/// Flash, Perplexity Sonar Deep Research, dashscope/qwen-turbo).
///
/// Returns a normalized [`TokenCounts`]; a non-object `usage` yields the
/// all-zero default.
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use vibe_coding_tracker::utils::extract_token_counts;
///
/// // Flat provider shape with no TTL split: every
/// // cache_creation token lands in the 5-minute bucket.
/// let counts = extract_token_counts(&json!({
///     "input_tokens": 100,
///     "output_tokens": 50,
///     "cache_read_input_tokens": 200,
///     "cache_creation_input_tokens": 10_000,
/// }));
/// assert_eq!(counts.input_tokens, 100);
/// assert_eq!(counts.cache_creation_5m, 10_000);
/// assert_eq!(counts.cache_creation_1h, 0);
/// ```
pub fn extract_token_counts(usage: &Value) -> TokenCounts {
    let mut counts = TokenCounts::default();

    if let Some(usage_obj) = usage.as_object() {
        // Flat provider usage format
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

        // Claude `server_tool_use.web_search_requests`: server-side web search
        // count, billed per query separately from tokens. Read here in the
        // flat section so it is captured before the Codex `total_token_usage`
        // early-return below (Codex never carries this field, so it stays 0).
        if let Some(server_tool_use) = usage_obj.get("server_tool_use").and_then(|v| v.as_object())
        {
            counts.web_search_requests = server_tool_use
                .get("web_search_requests")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
        }

        // Per-request tier classification (usage scans only): the parsers
        // accumulate the above-threshold slice of every bucket into a nested
        // `above_tier` object. Read it here — before the Codex early return —
        // so both the flat and the nested shapes carry it into pricing.
        if let Some(above) = usage_obj.get("above_tier").and_then(|v| v.as_object()) {
            let field = |key: &str| above.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
            counts.above_input = field("input_tokens");
            counts.above_output = field("output_tokens");
            counts.above_reasoning = field("reasoning_tokens");
            counts.above_cache_read = field("cache_read_tokens");
            counts.above_cache_creation_5m = field("cache_creation_5m_tokens");
            counts.above_cache_creation_1h = field("cache_creation_1h_tokens");
        }

        // Gemini writes reasoning budget as `thoughts_tokens`; the flat
        // providers (Copilot / OpenCode / Hermes) use `reasoning_output_tokens`.
        // A single record only carries one of them, but a cross-provider merge
        // of the same model (e.g. Gemini CLI and Hermes both using `gemini-*`)
        // keeps both keys, so sum them — an overwrite would drop the other
        // provider's thinking-time tokens from the merged Output/Total.
        let thoughts = usage_obj
            .get("thoughts_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let reasoning_output = usage_obj
            .get("reasoning_output_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        counts.reasoning_tokens = thoughts + reasoning_output;

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
            // OpenAI convention: `output_tokens` (completion) already INCLUDES
            // `reasoning_output_tokens`. Split them into disjoint buckets so
            // each token is billed exactly once — visible output at the output
            // rate, reasoning at its dedicated rate (or the output fallback).
            // Without this subtraction reasoning is billed twice: once inside
            // output and again as the separate reasoning bucket. Verified
            // across 21,113 real Codex token_count events: `total_tokens ==
            // input + output` holds for every one and `reasoning > output`
            // never occurs, so reasoning is always a subset of output.
            counts.output_tokens = (counts.output_tokens - counts.reasoning_tokens).max(0);
            if let Some(total) = total_usage.get("total_tokens").and_then(|v| v.as_i64()) {
                // `total_tokens == input (incl. cached) + output`, and output
                // already contains reasoning, so the published total is the
                // correct each-token-once figure. Use it verbatim.
                counts.total = total;
                return counts;
            }
        }

        // Flat providers may publish tool-only tokens that have no separate
        // LiteLLM billing bucket. Keep them in the displayed/activity total
        // without assigning them an input or output price. Prefer a larger
        // provider-published total when present so these sessions do not look
        // inactive merely because every priced bucket is zero.
        let tool_tokens = usage_obj
            .get("tool_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let derived_total = counts.input_tokens
            + counts.output_tokens
            + counts.reasoning_tokens
            + counts.cache_read
            + counts.cache_creation
            + tool_tokens;
        counts.total = usage_obj
            .get("total_tokens")
            .and_then(|value| value.as_i64())
            .map_or(derived_total, |published| published.max(derived_total));
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
    fn merged_thoughts_and_reasoning_output_are_summed() {
        // A single record carries only one reasoning key, but a cross-provider
        // merge (Gemini `thoughts_tokens` + a flat `reasoning_output_tokens` for
        // the same model) keeps both — they must add, not overwrite.
        let gemini_only = json!({ "input_tokens": 10, "thoughts_tokens": 30 });
        assert_eq!(extract_token_counts(&gemini_only).reasoning_tokens, 30);

        let flat_only = json!({ "input_tokens": 10, "reasoning_output_tokens": 7 });
        assert_eq!(extract_token_counts(&flat_only).reasoning_tokens, 7);

        let merged = json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "thoughts_tokens": 30,
            "reasoning_output_tokens": 7,
        });
        assert_eq!(extract_token_counts(&merged).reasoning_tokens, 37);
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
        // `output_tokens` (completion) already includes reasoning; the
        // extractor splits them so each token is billed once.
        assert_eq!(c.output_tokens, 13_156 - 8_591);
        assert_eq!(c.reasoning_tokens, 8_591);
        // `total_tokens == input + output` already, so it is used verbatim.
        assert_eq!(c.total, 589_301);
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
        assert_eq!(c.total, 14_397);
    }

    #[test]
    fn gemini_tool_only_usage_remains_active() {
        let usage = json!({
            "input_tokens": 0,
            "output_tokens": 0,
            "thoughts_tokens": 0,
            "tool_tokens": 5,
            "total_tokens": 5
        });
        let counts = extract_token_counts(&usage);
        assert_eq!(counts.total, 5);
        assert_eq!(counts.input_tokens, 0);
        assert_eq!(counts.output_tokens, 0);
    }

    #[test]
    fn codex_reasoning_is_split_out_of_output() {
        // Mimics a real Codex `event_msg` record mid-session. Per OpenAI's
        // convention `output_tokens` already includes `reasoning_output_tokens`,
        // so the extractor subtracts reasoning out of output to keep the two
        // buckets disjoint (each token billed exactly once).
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
            c.output_tokens,
            810 - 640,
            "reasoning must be subtracted out of output, not double-counted"
        );
        assert_eq!(c.reasoning_tokens, 640);
        // `total_tokens == input + output` (reasoning lives inside output),
        // so the published total is used verbatim.
        assert_eq!(c.total, 6_455);
    }

    #[test]
    fn codex_real_world_reasoning_subset_of_output() {
        // The exact `info.total_token_usage` shape from a real session: output
        // 508 already contains the 255 reasoning tokens; total 73_861 already
        // equals input + output. Regression guard against re-introducing the
        // double-count.
        let usage = json!({
            "total_token_usage": {
                "input_tokens": 73_353,
                "cached_input_tokens": 31_744,
                "output_tokens": 508,
                "reasoning_output_tokens": 255,
                "total_tokens": 73_861
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.input_tokens, 73_353 - 31_744);
        assert_eq!(c.cache_read, 31_744);
        assert_eq!(c.output_tokens, 508 - 255);
        assert_eq!(c.reasoning_tokens, 255);
        assert_eq!(c.total, 73_861);
    }

    #[test]
    fn claude_server_tool_use_web_search_is_extracted() {
        // `server_tool_use.web_search_requests` feeds the per-query web-search
        // billing path; a missing object leaves the count at 0.
        let with_search = json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "server_tool_use": {
                "web_search_requests": 3,
                "web_fetch_requests": 1
            }
        });
        assert_eq!(extract_token_counts(&with_search).web_search_requests, 3);

        let without = json!({ "input_tokens": 100, "output_tokens": 50 });
        assert_eq!(extract_token_counts(&without).web_search_requests, 0);
    }

    #[test]
    fn above_tier_slices_are_extracted_for_both_shapes() {
        // Flat shape (Claude / Gemini): above_tier is a sibling of the token
        // fields and mirrors them as subsets.
        let flat = json!({
            "input_tokens": 300_000,
            "output_tokens": 1_000,
            "cache_read_input_tokens": 50_000,
            "above_tier": {
                "input_tokens": 280_000,
                "output_tokens": 600,
                "cache_read_tokens": 50_000
            }
        });
        let c = extract_token_counts(&flat);
        assert_eq!(c.input_tokens, 300_000);
        assert_eq!(c.above_input, 280_000);
        assert_eq!(c.above_output, 600);
        assert_eq!(c.above_cache_read, 50_000);
        assert_eq!(c.above_cache_creation_5m, 0);

        // Codex shape: above_tier sits beside total_token_usage and must
        // survive the early return that consumes the published total.
        let codex = json!({
            "total_token_usage": {
                "input_tokens": 400_000,
                "cached_input_tokens": 100_000,
                "output_tokens": 2_000,
                "reasoning_output_tokens": 500,
                "total_tokens": 402_000
            },
            "above_tier": {
                "input_tokens": 250_000,
                "cache_read_tokens": 80_000,
                "output_tokens": 900,
                "reasoning_tokens": 300
            }
        });
        let c = extract_token_counts(&codex);
        assert_eq!(c.total, 402_000);
        assert_eq!(c.above_input, 250_000);
        assert_eq!(c.above_cache_read, 80_000);
        assert_eq!(c.above_output, 900);
        assert_eq!(c.above_reasoning, 300);
    }

    #[test]
    fn copilot_reasoning_output_tokens_populate_reasoning_bucket() {
        // After `session::copilot::parse_copilot_events` normalisation,
        // Copilot sessions use the same flat `reasoning_output_tokens` key
        // as Codex's nested one.
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
