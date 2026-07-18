use super::cache::{ModelPricing, TierRange};
use crate::utils::TokenCounts;

/// One resolved set of per-token prices (base level or one tier).
struct PriceLevel {
    input: f64,
    output: f64,
    /// `0.0` means "no dedicated reasoning rate at this level" — billed at
    /// this level's output rate instead, so reasoning never prices at $0.
    reasoning_raw: f64,
    cache_read: f64,
    cc_5m: f64,
    /// `0.0` means the level publishes no extended-TTL price — 1h cache
    /// writes fall back to the 5m rate (under-bill rather than fabricate).
    cc_1h_raw: f64,
}

impl PriceLevel {
    fn base(pricing: &ModelPricing) -> Self {
        Self {
            input: pricing.input_cost_per_token,
            output: pricing.output_cost_per_token,
            reasoning_raw: pricing.output_cost_per_reasoning_token,
            cache_read: pricing.cache_read_input_token_cost,
            cc_5m: pricing.cache_creation_input_token_cost,
            cc_1h_raw: pricing.cache_creation_input_token_cost_above_1hr,
        }
    }

    fn bill(
        &self,
        input: i64,
        output: i64,
        reasoning: i64,
        cache_read: i64,
        cc_5m: i64,
        cc_1h: i64,
    ) -> f64 {
        let reasoning_price = if self.reasoning_raw > 0.0 {
            self.reasoning_raw
        } else {
            self.output
        };
        let cc_1h_price = if self.cc_1h_raw > 0.0 {
            self.cc_1h_raw
        } else {
            self.cc_5m
        };
        input as f64 * self.input
            + output as f64 * self.output
            + reasoning as f64 * reasoning_price
            + cache_read as f64 * self.cache_read
            + cc_5m as f64 * self.cc_5m
            + cc_1h as f64 * cc_1h_price
    }
}

/// Calculates total cost for normalized token counts and the model's pricing.
///
/// Strategy (highest priority first):
/// 1. If `pricing.ranges` is `Some`, selects a `TierRange` by `input_tokens`
///    (Qwen / doubao style volume tiers) — tier prices apply standalone.
/// 2. Otherwise, base prices bill the base slice of every bucket, and the
///    `above_*` slices (accumulated per request by the usage parsers for
///    requests whose own prompt context crossed the model's tier threshold)
///    bill at the lowest tier's prices. Tier selection against summed counts
///    was removed: LiteLLM's "above Nk tokens" semantics are per request, so
///    comparing the threshold against cross-session aggregates promoted whole
///    months of small requests to the elevated rate.
/// 3. Counts without `above_*` slices (analysis paths, offline scans,
///    providers without per-request granularity) therefore bill entirely at
///    base rates — a deliberate lower bound.
///
/// `reasoning_tokens` covers the model's "thinking" budget (Gemini
/// `thoughts_tokens`, Codex `reasoning_output_tokens`, Copilot
/// `reasoningTokens`). When the active price level publishes
/// `output_cost_per_reasoning_token`, reasoning is billed at that rate;
/// otherwise it falls back to the active `output_cost_per_token` rate so
/// providers that don't split reasoning (all Anthropic models, GPT-5.x,
/// Grok, …) continue to bill correctly.
///
/// `cache_creation_5m` and `cache_creation_1h` are priced separately
/// (5-minute default TTL vs 1-hour extended TTL). When a model doesn't
/// publish a 1hr price (value is 0.0), the 5m price is used for both buckets
/// — matching current behaviour for providers that don't split TTL.
///
/// # Examples
///
/// ```
/// use vct_core::pricing::ModelPricing;
/// use vct_core::pricing::calculate_cost;
/// use vct_core::utils::TokenCounts;
///
/// let pricing = ModelPricing {
///     input_cost_per_token: 3e-6,
///     output_cost_per_token: 1.5e-5,
///     ..Default::default()
/// };
/// // 1000 input + 500 output tokens, no cache or reasoning.
/// let counts = TokenCounts {
///     input_tokens: 1000,
///     output_tokens: 500,
///     ..Default::default()
/// };
/// let cost = calculate_cost(&counts, &pricing);
/// assert_eq!(cost, 1000.0 * 3e-6 + 500.0 * 1.5e-5);
/// ```
pub fn calculate_cost(counts: &TokenCounts, pricing: &ModelPricing) -> f64 {
    if let Some(ranges) = &pricing.ranges {
        // Range-based pricing dispatches on input volume and has no
        // cache_creation fields on the range rows (LiteLLM doesn't publish
        // them for Qwen / doubao), so cache writes stay on base prices.
        let r = select_range(ranges, counts.input_tokens);
        let level = PriceLevel {
            input: r.map(|r| r.input_cost_per_token).unwrap_or(0.0),
            output: r.map(|r| r.output_cost_per_token).unwrap_or(0.0),
            reasoning_raw: r.map(|r| r.output_cost_per_reasoning_token).unwrap_or(0.0),
            cache_read: r.map(|r| r.cache_read_input_token_cost).unwrap_or(0.0),
            cc_5m: pricing.cache_creation_input_token_cost,
            cc_1h_raw: pricing.cache_creation_input_token_cost_above_1hr,
        };
        return level.bill(
            counts.input_tokens,
            counts.output_tokens,
            counts.reasoning_tokens,
            counts.cache_read,
            counts.cache_creation_5m,
            counts.cache_creation_1h,
        );
    }

    let base = PriceLevel::base(pricing);

    // The above-threshold slices are subsets of the totals; the base slice is
    // the remainder. Clamp defensively so a malformed merge can never bill
    // negative tokens.
    let base_slice = |total: i64, above: i64| (total - above).max(0);
    let mut cost = base.bill(
        base_slice(counts.input_tokens, counts.above_input),
        base_slice(counts.output_tokens, counts.above_output),
        base_slice(counts.reasoning_tokens, counts.above_reasoning),
        base_slice(counts.cache_read, counts.above_cache_read),
        base_slice(counts.cache_creation_5m, counts.above_cache_creation_5m),
        base_slice(counts.cache_creation_1h, counts.above_cache_creation_1h),
    );

    let has_above = counts.above_input != 0
        || counts.above_output != 0
        || counts.above_reasoning != 0
        || counts.above_cache_read != 0
        || counts.above_cache_creation_5m != 0
        || counts.above_cache_creation_1h != 0;
    if has_above {
        // Classification uses the lowest threshold, so the lowest tier's
        // prices apply. A tier field the model doesn't publish (0.0) falls
        // back to the base price for that bucket rather than billing $0.
        let tier = match pricing.tiers.first() {
            Some(tier) => PriceLevel {
                input: positive_or(tier.input_cost_per_token, base.input),
                output: positive_or(tier.output_cost_per_token, base.output),
                // LiteLLM publishes no tier-specific reasoning rate; billing
                // tier reasoning at the tier output rate matches "once you're
                // in the tier, everything is more expensive".
                reasoning_raw: 0.0,
                cache_read: positive_or(tier.cache_read_input_token_cost, base.cache_read),
                cc_5m: positive_or(tier.cache_creation_input_token_cost, base.cc_5m),
                cc_1h_raw: tier.cache_creation_input_token_cost_above_1hr,
            },
            // Above-slices without a published tier (e.g. thresholds derived
            // from a newer pricing snapshot than this entry): bill at base
            // rates verbatim, keeping the model's dedicated reasoning rate.
            None => base,
        };
        cost += tier.bill(
            counts.above_input,
            counts.above_output,
            counts.above_reasoning,
            counts.above_cache_read,
            counts.above_cache_creation_5m,
            counts.above_cache_creation_1h,
        );
    }

    cost
}

fn positive_or(value: f64, fallback: f64) -> f64 {
    if value > 0.0 { value } else { fallback }
}

/// Selects a `TierRange` for range-based pricing.
///
/// Ranges are sorted by `min_tokens` ascending at parse time, so the **last**
/// range whose `min_tokens <= input_tokens` is the right match — this naturally
/// handles both in-range hits and over-cap inputs (where `input_tokens` exceeds
/// every defined `max_tokens`) with a single pass. Inputs below the lowest
/// range's `min_tokens` (unexpected for LiteLLM data, which starts at 0) fall
/// back to the first range so we still bill rather than silently return $0.
fn select_range(ranges: &[TierRange], input_tokens: i64) -> Option<&TierRange> {
    ranges
        .iter()
        .rev()
        .find(|r| r.min_tokens <= input_tokens)
        .or_else(|| ranges.first())
}

#[cfg(test)]
mod tests {
    use super::super::cache::ThresholdTier;
    use super::*;

    fn counts(
        input: i64,
        output: i64,
        reasoning: i64,
        cache_read: i64,
        cc_5m: i64,
        cc_1h: i64,
    ) -> TokenCounts {
        TokenCounts {
            input_tokens: input,
            output_tokens: output,
            reasoning_tokens: reasoning,
            cache_read,
            cache_creation: cc_5m + cc_1h,
            cache_creation_5m: cc_5m,
            cache_creation_1h: cc_1h,
            ..Default::default()
        }
    }

    fn flat_pricing() -> ModelPricing {
        ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_read_input_token_cost: 0.0000003,
            cache_creation_input_token_cost: 0.00000375,
            ..Default::default()
        }
    }

    fn sonnet_like_pricing() -> ModelPricing {
        // Mimics Claude Sonnet 4.5: base + 200k tier (2x).
        ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_read_input_token_cost: 0.0000003,
            cache_creation_input_token_cost: 0.00000375,
            tiers: vec![ThresholdTier {
                threshold_tokens: 200_000,
                input_cost_per_token: 0.000006,
                output_cost_per_token: 0.0000225,
                cache_read_input_token_cost: 0.0000006,
                cache_creation_input_token_cost: 0.0000075,
                ..Default::default()
            }],
            ranges: None,
            ..Default::default()
        }
    }

    #[test]
    fn test_flat_pricing_applies_base() {
        let p = flat_pricing();
        // 200 of cache_creation goes into the 5m bucket (no TTL split available).
        let cost = calculate_cost(&counts(1000, 500, 0, 200, 100, 0), &p);
        let expected =
            1000.0 * 0.000003 + 500.0 * 0.000015 + 200.0 * 0.0000003 + 100.0 * 0.00000375;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_no_above_slice_stays_on_base() {
        let p = sonnet_like_pricing();
        let cost = calculate_cost(&counts(1000, 500, 0, 200, 100, 0), &p);
        let expected =
            1000.0 * 0.000003 + 500.0 * 0.000015 + 200.0 * 0.0000003 + 100.0 * 0.00000375;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_aggregate_volume_alone_never_promotes_to_tier() {
        // Regression guard for the aggregate-tier bug: a month of small
        // requests sums far past the threshold, but with no per-request
        // above slice everything must stay on base prices.
        let p = sonnet_like_pricing();
        let cost = calculate_cost(&counts(5_000_000, 250_000, 0, 2_000_000, 0, 0), &p);
        let expected = 5_000_000.0 * 0.000003 + 250_000.0 * 0.000015 + 2_000_000.0 * 0.0000003;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_above_slice_bills_at_tier_and_remainder_at_base() {
        let p = sonnet_like_pricing();
        let mut c = counts(300_000, 1_000, 0, 100_000, 10_000, 0);
        c.above_input = 250_000;
        c.above_output = 600;
        c.above_cache_read = 80_000;
        c.above_cache_creation_5m = 10_000;
        let cost = calculate_cost(&c, &p);
        let base_part = 50_000.0 * 0.000003 + 400.0 * 0.000015 + 20_000.0 * 0.0000003;
        let tier_part =
            250_000.0 * 0.000006 + 600.0 * 0.0000225 + 80_000.0 * 0.0000006 + 10_000.0 * 0.0000075;
        assert_eq!(cost, base_part + tier_part);
    }

    #[test]
    fn test_fully_above_request_bills_everything_at_tier() {
        let p = sonnet_like_pricing();
        let mut c = counts(250_000, 1_000, 0, 0, 0, 0);
        c.above_input = 250_000;
        c.above_output = 1_000;
        let cost = calculate_cost(&c, &p);
        assert_eq!(cost, 250_000.0 * 0.000006 + 1_000.0 * 0.0000225);
    }

    #[test]
    fn test_above_slice_without_published_tier_bills_at_base() {
        // Thresholds can come from a newer pricing snapshot than this entry;
        // the tokens must still be billed exactly once, at base rates.
        let p = flat_pricing();
        let mut c = counts(300_000, 1_000, 0, 0, 0, 0);
        c.above_input = 300_000;
        c.above_output = 1_000;
        let cost = calculate_cost(&c, &p);
        assert_eq!(cost, 300_000.0 * 0.000003 + 1_000.0 * 0.000015);
    }

    #[test]
    fn test_above_slice_without_tier_keeps_dedicated_reasoning_rate() {
        // A model with a dedicated reasoning rate but no context tier: an
        // above-slice (from a normalized collision or stale threshold) must
        // still bill reasoning at that dedicated rate, not the output rate.
        let p = ModelPricing {
            input_cost_per_token: 1e-6,
            output_cost_per_token: 8e-6,
            output_cost_per_reasoning_token: 3e-6,
            ..Default::default()
        };
        let mut c = counts(1_000, 200, 500, 0, 0, 0);
        c.above_input = 1_000;
        c.above_output = 200;
        c.above_reasoning = 500;
        let cost = calculate_cost(&c, &p);
        let expected = 1_000.0 * 1e-6 + 200.0 * 8e-6 + 500.0 * 3e-6;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_unpublished_tier_field_falls_back_to_base_price() {
        // The tier row only publishes input/output; its cache_read must fall
        // back to the base cache_read price rather than billing $0.
        let p = ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            cache_read_input_token_cost: 0.0000001,
            tiers: vec![ThresholdTier {
                threshold_tokens: 128_000,
                input_cost_per_token: 0.000002,
                output_cost_per_token: 0.000004,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut c = counts(0, 0, 0, 200_000, 0, 0);
        c.above_cache_read = 200_000;
        let cost = calculate_cost(&c, &p);
        assert_eq!(cost, 200_000.0 * 0.0000001);
    }

    #[test]
    fn test_above_reasoning_uses_tier_output_rate() {
        // No tier-specific reasoning rate exists; above-slice reasoning bills
        // at the tier output rate ("once you're in the tier, everything is
        // more expensive"), never at the base reasoning rate.
        let p = ModelPricing {
            input_cost_per_token: 3e-6,
            output_cost_per_token: 1.5e-5,
            output_cost_per_reasoning_token: 1e-6,
            tiers: vec![ThresholdTier {
                threshold_tokens: 200_000,
                input_cost_per_token: 6e-6,
                output_cost_per_token: 2.25e-5,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut c = counts(250_000, 1_000, 500, 0, 0, 0);
        c.above_input = 250_000;
        c.above_output = 1_000;
        c.above_reasoning = 500;
        let cost = calculate_cost(&c, &p);
        let expected = 250_000.0 * 6e-6 + 1_000.0 * 2.25e-5 + 500.0 * 2.25e-5;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_negative_base_slice_clamps_to_zero() {
        // A malformed merge could leave above > total; never bill negative
        // base tokens.
        let p = sonnet_like_pricing();
        let mut c = counts(100, 0, 0, 0, 0, 0);
        c.above_input = 500;
        let cost = calculate_cost(&c, &p);
        assert_eq!(cost, 500.0 * 0.000006);
    }

    #[test]
    fn test_range_based_pricing_dispatches_by_input() {
        // Mimics dashscope/qwen3-coder-plus tiers.
        let p = ModelPricing {
            // Base prices are ignored when ranges is Some.
            input_cost_per_token: 999.0,
            output_cost_per_token: 999.0,
            ranges: Some(vec![
                TierRange {
                    min_tokens: 0,
                    max_tokens: 32_000,
                    input_cost_per_token: 0.000001,
                    output_cost_per_token: 0.000005,
                    ..Default::default()
                },
                TierRange {
                    min_tokens: 32_000,
                    max_tokens: 128_000,
                    input_cost_per_token: 0.0000018,
                    output_cost_per_token: 0.000009,
                    ..Default::default()
                },
                TierRange {
                    min_tokens: 128_000,
                    max_tokens: 256_000,
                    input_cost_per_token: 0.000003,
                    output_cost_per_token: 0.000015,
                    ..Default::default()
                },
                TierRange {
                    min_tokens: 256_000,
                    max_tokens: 1_000_000,
                    input_cost_per_token: 0.000006,
                    output_cost_per_token: 0.00006,
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        let c_low = calculate_cost(&counts(20_000, 5_000, 0, 0, 0, 0), &p);
        assert_eq!(c_low, 20_000.0 * 0.000001 + 5_000.0 * 0.000005);

        let c_hi = calculate_cost(&counts(500_000, 5_000, 0, 0, 0, 0), &p);
        assert_eq!(c_hi, 500_000.0 * 0.000006 + 5_000.0 * 0.00006);
    }

    #[test]
    fn test_range_based_falls_back_to_last_range_for_overflow() {
        let p = ModelPricing {
            ranges: Some(vec![TierRange {
                min_tokens: 0,
                max_tokens: 100_000,
                input_cost_per_token: 0.000001,
                output_cost_per_token: 0.000002,
                ..Default::default()
            }]),
            ..Default::default()
        };

        // 200K exceeds every defined range — fall back to the last one.
        let cost = calculate_cost(&counts(200_000, 0, 0, 0, 0, 0), &p);
        assert_eq!(cost, 200_000.0 * 0.000001);
    }

    #[test]
    fn test_zero_tokens() {
        let p = sonnet_like_pricing();
        assert_eq!(calculate_cost(&TokenCounts::default(), &p), 0.0);
    }

    #[test]
    fn test_1hr_cache_creation_billed_at_extended_rate() {
        // Opus 4.7-like: base cache_creation $6.25/M, above_1hr $10/M.
        let p = ModelPricing {
            input_cost_per_token: 5e-6,
            output_cost_per_token: 2.5e-5,
            cache_read_input_token_cost: 5e-7,
            cache_creation_input_token_cost: 6.25e-6,
            cache_creation_input_token_cost_above_1hr: 1e-5,
            ..Default::default()
        };

        // 10_000 tokens at 1hr TTL should cost the extended rate, not the 5m rate.
        let cost = calculate_cost(&counts(0, 0, 0, 0, 0, 10_000), &p);
        assert_eq!(cost, 10_000.0 * 1e-5);

        // Mixed: 1_000 at 5m + 10_000 at 1h.
        let cost_mixed = calculate_cost(&counts(0, 0, 0, 0, 1_000, 10_000), &p);
        assert_eq!(cost_mixed, 1_000.0 * 6.25e-6 + 10_000.0 * 1e-5);
    }

    #[test]
    fn test_1hr_falls_back_to_5m_when_model_has_no_extended_price() {
        // A model with only a 5m price — 1h tokens should still be billed
        // (at the 5m rate) rather than silently costing $0.
        let p = ModelPricing {
            cache_creation_input_token_cost: 6.25e-6,
            cache_creation_input_token_cost_above_1hr: 0.0,
            ..Default::default()
        };

        let cost = calculate_cost(&counts(0, 0, 0, 0, 0, 10_000), &p);
        assert_eq!(cost, 10_000.0 * 6.25e-6);
    }

    #[test]
    fn test_1hr_above_slice_uses_tier_price_when_published() {
        // Tier carries its own 1hr price (Claude 3.5 Sonnet-style); the
        // above slice of both TTL buckets bills at the tier's rates.
        let p = ModelPricing {
            input_cost_per_token: 3e-6,
            cache_creation_input_token_cost: 3.75e-6,
            cache_creation_input_token_cost_above_1hr: 6e-6,
            tiers: vec![ThresholdTier {
                threshold_tokens: 200_000,
                input_cost_per_token: 6e-6,
                cache_creation_input_token_cost: 7.5e-6,
                cache_creation_input_token_cost_above_1hr: 1.2e-5,
                ..Default::default()
            }],
            ..Default::default()
        };

        let mut c = counts(250_000, 0, 0, 0, 5_000, 5_000);
        c.above_input = 250_000;
        c.above_cache_creation_5m = 5_000;
        c.above_cache_creation_1h = 5_000;
        let cost = calculate_cost(&c, &p);
        let expected = 250_000.0 * 6e-6 + 5_000.0 * 7.5e-6 + 5_000.0 * 1.2e-5;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_reasoning_billed_at_dedicated_rate_when_published() {
        // Gemini 2.5 flash-lite publishes a dedicated
        // `output_cost_per_reasoning_token` that happens to match its
        // output rate; perplexity/sonar-deep-research pays $3/M for
        // reasoning vs $8/M for output. Use the synthetic latter shape
        // to prove the reasoning price is not being silently coerced
        // back to the output rate.
        let p = ModelPricing {
            input_cost_per_token: 1e-6,
            output_cost_per_token: 8e-6,
            output_cost_per_reasoning_token: 3e-6,
            ..Default::default()
        };

        let cost = calculate_cost(&counts(1_000, 200, 500, 0, 0, 0), &p);
        let expected = 1_000.0 * 1e-6 + 200.0 * 8e-6 + 500.0 * 3e-6;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_reasoning_falls_back_to_output_rate_when_not_published() {
        // Claude has no reasoning price published at all. Sessions that
        // still report reasoning tokens (e.g. Copilot driving a Claude
        // model) should bill them at the output rate rather than $0.
        let p = ModelPricing {
            input_cost_per_token: 3e-6,
            output_cost_per_token: 1.5e-5,
            ..Default::default()
        };

        let cost = calculate_cost(&counts(1_000, 500, 200, 0, 0, 0), &p);
        let expected = 1_000.0 * 3e-6 + 500.0 * 1.5e-5 + 200.0 * 1.5e-5;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_reasoning_uses_range_reasoning_rate_when_published() {
        // dashscope qwen-plus ships a per-range reasoning rate ($4/M)
        // that's substantially higher than the per-range output rate
        // ($1.2/M). The range-based path must route reasoning through
        // that field.
        let p = ModelPricing {
            ranges: Some(vec![TierRange {
                min_tokens: 0,
                max_tokens: 32_000,
                input_cost_per_token: 8e-7,
                output_cost_per_token: 1.2e-6,
                output_cost_per_reasoning_token: 4e-6,
                ..Default::default()
            }]),
            ..Default::default()
        };

        let cost = calculate_cost(&counts(10_000, 500, 200, 0, 0, 0), &p);
        let expected = 10_000.0 * 8e-7 + 500.0 * 1.2e-6 + 200.0 * 4e-6;
        assert_eq!(cost, expected);
    }
}
