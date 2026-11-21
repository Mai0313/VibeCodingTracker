//! Extraction of normalized token counts and per-tier slices from provider usage JSON.

use serde_json::Value;

/// Maximum context-tier price level supported in [`TokenCounts::above_tiers`].
///
/// [`extract_token_counts`] ignores `above_tier` keys whose level is `0` or exceeds this limit.
pub const MAX_TIER_LEVELS: usize = 16;

/// One context-tier price level's subset of a [`TokenCounts`] record.
///
/// Bucket semantics match [`TokenCounts`] (non-cached input, output excluding reasoning); each slice is a subset of the parent totals rather than additional tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TierSlice {
    /// Non-cached prompt tokens billed at this level.
    pub input_tokens: i64,
    /// Completion tokens (excluding reasoning) billed at this level.
    pub output_tokens: i64,
    /// Reasoning tokens billed at this level.
    pub reasoning_tokens: i64,
    /// Cache reads billed at this level.
    pub cache_read: i64,
    /// Cache writes at the default 5-minute TTL billed at this level.
    pub cache_creation_5m: i64,
    /// Cache writes at the extended 1-hour TTL billed at this level.
    pub cache_creation_1h: i64,
}

impl TierSlice {
    /// Adds every bucket of `other` into this slice.
    pub fn merge(&mut self, other: &Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.cache_read += other.cache_read;
        self.cache_creation_5m += other.cache_creation_5m;
        self.cache_creation_1h += other.cache_creation_1h;
    }

    /// Returns whether all buckets in this slice are zero.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Normalized, disjoint token counts extracted from provider usage JSON.
///
/// `cache_creation == cache_creation_5m + cache_creation_1h` always holds; providers without a TTL split place all cache-write tokens in `cache_creation_5m`. `input_tokens` excludes `cache_read`, and `output_tokens` excludes `reasoning_tokens`.
#[derive(Debug, Default)]
pub struct TokenCounts {
    /// Non-cached prompt tokens (excludes `cache_read`).
    pub input_tokens: i64,
    /// Completion tokens excluding `reasoning_tokens`.
    pub output_tokens: i64,
    /// Model reasoning ("thinking") tokens, disjoint from `output_tokens`.
    pub reasoning_tokens: i64,
    /// Prompt tokens served from the provider's prompt cache.
    pub cache_read: i64,
    /// Total cache-write tokens across all TTL tiers.
    pub cache_creation: i64,
    /// Cache-write tokens at the default 5-minute TTL.
    pub cache_creation_5m: i64,
    /// Cache-write tokens at the extended 1-hour TTL.
    pub cache_creation_1h: i64,
    /// Server-side web-search request count (Claude `server_tool_use`), billed per query and excluded from `total`.
    pub web_search_requests: i64,
    /// Unpriced tool-payload tokens (Gemini `tool_tokens`), included in `total`.
    pub tool_tokens: i64,
    /// Total tokens across all token buckets (including `tool_tokens`, excluding `web_search_requests`).
    pub total: i64,
    /// Per-level token subsets for requests that reached higher context-tier price levels; index `n` corresponds to price level `n + 1`, with zero entries for empty intermediate levels.
    pub above_tiers: Vec<TierSlice>,
}

impl TokenCounts {
    /// Returns a mutable reference to price `level`'s slice (growing `above_tiers`), or `None` for level `0` or levels above [`MAX_TIER_LEVELS`].
    fn tier_slice_mut(&mut self, level: usize) -> Option<&mut TierSlice> {
        if level == 0 || level > MAX_TIER_LEVELS {
            return None;
        }
        if self.above_tiers.len() < level {
            self.above_tiers.resize(level, TierSlice::default());
        }
        self.above_tiers.get_mut(level - 1)
    }

    /// Returns whether any token bucket or `web_search_requests` is non-zero.
    ///
    /// `tool_tokens` and `above_tiers` are covered transitively via `total` and the parent buckets.
    pub fn has_activity(&self) -> bool {
        self.total != 0
            || self.input_tokens != 0
            || self.output_tokens != 0
            || self.reasoning_tokens != 0
            || self.cache_read != 0
            || self.cache_creation != 0
            || self.cache_creation_5m != 0
            || self.cache_creation_1h != 0
            || self.web_search_requests != 0
    }
}

/// Parses `level_<n>_<bucket>` into `(n, bucket)`, returning `None` for other key shapes.
fn split_tier_key(key: &str) -> Option<(usize, &str)> {
    let rest = key.strip_prefix("level_")?;
    let separator = rest.find('_')?;
    let level = rest[..separator].parse().ok()?;
    Some((level, &rest[separator + 1..]))
}

/// Extracts normalized, disjoint [`TokenCounts`] from provider usage JSON.
///
/// Supports both flat provider objects (`input_tokens`, `output_tokens`, ...) and Codex's nested `total_token_usage` object (subtracting cached input from `input_tokens` and reasoning from `output_tokens`). Non-object inputs yield [`TokenCounts::default()`].
///
/// # Examples
///
/// ```
/// use serde_json::json;
/// use vct_core::utils::extract_token_counts;
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

        // Read `server_tool_use` before the Codex early return below.
        if let Some(server_tool_use) = usage_obj.get("server_tool_use").and_then(|v| v.as_object())
        {
            counts.web_search_requests = server_tool_use
                .get("web_search_requests")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
        }

        // Read `above_tier` before the Codex early return so both flat and nested shapes carry tier slices into pricing.
        if let Some(above) = usage_obj.get("above_tier").and_then(|v| v.as_object()) {
            for (key, value) in above {
                let (Some(tokens), Some((level, bucket))) = (value.as_i64(), split_tier_key(key))
                else {
                    continue;
                };
                // Validate the bucket name before resizing `above_tiers` so unknown keys cannot allocate empty levels.
                let bucket: fn(&mut TierSlice) -> &mut i64 = match bucket {
                    "input_tokens" => |slice| &mut slice.input_tokens,
                    "output_tokens" => |slice| &mut slice.output_tokens,
                    "reasoning_tokens" => |slice| &mut slice.reasoning_tokens,
                    "cache_read_tokens" => |slice| &mut slice.cache_read,
                    "cache_creation_5m_tokens" => |slice| &mut slice.cache_creation_5m,
                    "cache_creation_1h_tokens" => |slice| &mut slice.cache_creation_1h,
                    _ => continue,
                };
                let Some(slice) = counts.tier_slice_mut(level) else {
                    continue;
                };
                *bucket(slice) = tokens;
            }
        }

        // Sum both flat reasoning keys so cross-provider merges (Gemini `thoughts_tokens` + `reasoning_output_tokens`) do not overwrite each other.
        let thoughts = usage_obj
            .get("thoughts_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let reasoning_output = usage_obj
            .get("reasoning_output_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        counts.reasoning_tokens = thoughts + reasoning_output;

        // Bill the 1h TTL portion from the split and assign any remainder (including tokens missing from the split) to 5m so 5m + 1h == cache_creation.
        if let Some(cc_split) = usage_obj.get("cache_creation").and_then(|v| v.as_object()) {
            let ephemeral_5m = cc_split
                .get("ephemeral_5m_input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            counts.cache_creation_1h = cc_split
                .get("ephemeral_1h_input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            // Use the larger of the scalar and split sum in case the scalar is absent or stale.
            counts.cache_creation = counts
                .cache_creation
                .max(ephemeral_5m + counts.cache_creation_1h);
            counts.cache_creation_5m = counts.cache_creation - counts.cache_creation_1h;
        } else {
            // No TTL split: treat all `cache_creation` tokens as 5m TTL.
            counts.cache_creation_5m = counts.cache_creation;
        }

        // Codex usage format (has total_token_usage nested object)
        if let Some(total_usage) = usage_obj
            .get("total_token_usage")
            .and_then(|v| v.as_object())
        {
            // OpenAI/Codex `input_tokens` includes `cached_input_tokens` and `cache_write_input_tokens`; subtract both so LiteLLM bills each bucket once.
            let raw_input = total_usage
                .get("input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let cached = total_usage
                .get("cached_input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let cache_write = total_usage
                .get("cache_write_input_tokens")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            counts.input_tokens = (raw_input - cached - cache_write).max(0);
            counts.cache_read = cached;
            counts.cache_creation = cache_write;
            counts.cache_creation_5m = cache_write;
            counts.cache_creation_1h = 0;

            if let Some(output) = total_usage.get("output_tokens").and_then(|v| v.as_i64()) {
                counts.output_tokens = output;
            }
            if let Some(reasoning) = total_usage
                .get("reasoning_output_tokens")
                .and_then(|v| v.as_i64())
            {
                counts.reasoning_tokens = reasoning;
            }
            // OpenAI/Codex `output_tokens` includes `reasoning_output_tokens`; subtract reasoning so each token is billed once.
            counts.output_tokens = (counts.output_tokens - counts.reasoning_tokens).max(0);
            if let Some(total) = total_usage.get("total_tokens").and_then(|v| v.as_i64()) {
                // Published `total_tokens` already equals input (incl. cached) + output (incl. reasoning); use it verbatim.
                counts.total = total;
                return counts;
            }
        }

        // Include unpriced `tool_tokens` in `total`, and take the max against any published `total_tokens` so tool-only sessions stay active.
        counts.tool_tokens = usage_obj
            .get("tool_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or(0);
        let derived_total = counts.input_tokens
            + counts.output_tokens
            + counts.reasoning_tokens
            + counts.cache_read
            + counts.cache_creation
            + counts.tool_tokens;
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
    fn ttl_split_smaller_than_scalar_bills_the_remainder_at_5m() {
        let usage = json!({
            "cache_creation_input_tokens": 397_602_459_i64,
            "cache_creation": {
                "ephemeral_5m_input_tokens": 183_962_961_i64,
                "ephemeral_1h_input_tokens": 213_022_705_i64
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.cache_creation, 397_602_459);
        assert_eq!(c.cache_creation_1h, 213_022_705);
        assert_eq!(c.cache_creation_5m, 397_602_459 - 213_022_705);
        assert_eq!(c.cache_creation_5m + c.cache_creation_1h, c.cache_creation);
    }

    #[test]
    fn ttl_split_derives_total_when_scalar_missing() {
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
        // Codex `input_tokens` includes `cached_input_tokens`.
        assert_eq!(c.input_tokens, 800);
    }

    #[test]
    fn codex_input_tokens_excludes_cached_bucket() {
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
        assert_eq!(c.output_tokens, 13_156 - 8_591);
        assert_eq!(c.reasoning_tokens, 8_591);
        assert_eq!(c.total, 589_301);
    }

    #[test]
    fn codex_cached_exceeding_input_clamps_to_zero() {
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
        assert_eq!(c.total, 6_455);
    }

    #[test]
    fn codex_real_world_reasoning_subset_of_output() {
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
        // Flat shape.
        let flat = json!({
            "input_tokens": 300_000,
            "output_tokens": 1_000,
            "cache_read_input_tokens": 50_000,
            "above_tier": {
                "level_1_input_tokens": 280_000,
                "level_1_output_tokens": 600,
                "level_1_cache_read_tokens": 50_000
            }
        });
        let c = extract_token_counts(&flat);
        assert_eq!(c.input_tokens, 300_000);
        assert_eq!(
            c.above_tiers,
            vec![TierSlice {
                input_tokens: 280_000,
                output_tokens: 600,
                cache_read: 50_000,
                ..Default::default()
            }]
        );

        // Codex shape: `above_tier` must survive the early return for `total_tokens`.
        let codex = json!({
            "total_token_usage": {
                "input_tokens": 400_000,
                "cached_input_tokens": 100_000,
                "output_tokens": 2_000,
                "reasoning_output_tokens": 500,
                "total_tokens": 402_000
            },
            "above_tier": {
                "level_1_input_tokens": 250_000,
                "level_1_cache_read_tokens": 80_000,
                "level_1_output_tokens": 900,
                "level_1_reasoning_tokens": 300
            }
        });
        let c = extract_token_counts(&codex);
        assert_eq!(c.total, 402_000);
        assert_eq!(
            c.above_tiers,
            vec![TierSlice {
                input_tokens: 250_000,
                output_tokens: 900,
                reasoning_tokens: 300,
                cache_read: 80_000,
                ..Default::default()
            }]
        );
    }

    #[test]
    fn every_price_level_gets_its_own_slice() {
        // Missing intermediate level 2 stays a zero entry rather than shifting level 3 down.
        let usage = json!({
            "input_tokens": 500_000,
            "output_tokens": 3_000,
            "above_tier": {
                "level_1_input_tokens": 100_000,
                "level_1_output_tokens": 1_000,
                "level_3_input_tokens": 300_000,
                "level_3_output_tokens": 1_500
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(
            c.above_tiers,
            vec![
                TierSlice {
                    input_tokens: 100_000,
                    output_tokens: 1_000,
                    ..Default::default()
                },
                TierSlice::default(),
                TierSlice {
                    input_tokens: 300_000,
                    output_tokens: 1_500,
                    ..Default::default()
                },
            ]
        );
    }

    #[test]
    fn unreadable_above_tier_keys_are_skipped() {
        let usage = json!({
            "input_tokens": 10,
            "above_tier": {
                "input_tokens": 5,
                "level_x_input_tokens": 5,
                "level_1_unknown_bucket": 5,
                "level_99999999999_input_tokens": 5
            }
        });
        assert!(extract_token_counts(&usage).above_tiers.is_empty());
    }

    #[test]
    fn codex_cache_write_is_subtracted_from_input_and_populates_5m_bucket() {
        let usage = json!({
            "total_token_usage": {
                "input_tokens": 50_000,
                "cached_input_tokens": 30_000,
                "cache_write_input_tokens": 12_000,
                "output_tokens": 1_500,
                "reasoning_output_tokens": 500,
                "total_tokens": 51_500
            }
        });
        let c = extract_token_counts(&usage);
        assert_eq!(c.input_tokens, 8_000);
        assert_eq!(c.cache_read, 30_000);
        assert_eq!(c.cache_creation, 12_000);
        assert_eq!(c.cache_creation_5m, 12_000);
        assert_eq!(c.cache_creation_1h, 0);
        assert_eq!(c.output_tokens, 1_000);
        assert_eq!(c.reasoning_tokens, 500);
        assert_eq!(c.total, 51_500);

        let clamped = json!({
            "total_token_usage": {
                "input_tokens": 1_000,
                "cached_input_tokens": 700,
                "cache_write_input_tokens": 600,
                "output_tokens": 100
            }
        });
        let c = extract_token_counts(&clamped);
        assert_eq!(c.input_tokens, 0);
        assert_eq!(c.cache_read, 700);
        assert_eq!(c.cache_creation, 600);
        assert_eq!(c.cache_creation_5m, 600);
        assert_eq!(c.total, 1_400);
    }

    #[test]
    fn copilot_reasoning_output_tokens_populate_reasoning_bucket() {
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
