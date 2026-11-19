//! Token cost calculation across flat, threshold-tiered, and range-tiered pricing.

use super::cache::{ModelPricing, TierRange};
use crate::utils::{TierSlice, TokenCounts};

/// One resolved set of per-token prices (base level or one tier).
#[derive(Clone, Copy)]
struct PriceLevel {
    input: f64,
    output: f64,
    /// Dedicated reasoning rate, or `0.0` to fall back to `output`.
    reasoning_raw: f64,
    cache_read: f64,
    cc_5m: f64,
    /// Extended 1-hour cache-write rate, or `0.0` to fall back to `cc_5m`.
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

    /// Builds a level from `range`, keeping `pricing`'s base cache-write rates (LiteLLM omits per-range cache-write prices).
    fn range(range: &TierRange, pricing: &ModelPricing) -> Self {
        Self {
            input: range.input_cost_per_token,
            output: range.output_cost_per_token,
            reasoning_raw: range.output_cost_per_reasoning_token,
            cache_read: range.cache_read_input_token_cost,
            cc_5m: pricing.cache_creation_input_token_cost,
            cc_1h_raw: pricing.cache_creation_input_token_cost_above_1hr,
        }
    }

    fn bill(&self, tokens: &TierSlice) -> f64 {
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
        tokens.input_tokens as f64 * self.input
            + tokens.output_tokens as f64 * self.output
            + tokens.reasoning_tokens as f64 * reasoning_price
            + tokens.cache_read as f64 * self.cache_read
            + tokens.cache_creation_5m as f64 * self.cc_5m
            + tokens.cache_creation_1h as f64 * cc_1h_price
    }
}

/// Returns the USD token cost for `counts` under `pricing`.
///
/// Tier selection never inspects aggregated totals in `counts`; callers populate [`TokenCounts::above_tiers`] per request, and any tokens outside those slices bill at the lowest level.
///
/// - Range-based (`pricing.ranges` is `Some`): slice `n` (1-based) bills at `ranges[n]` (clamped to the last row) and unclassified tokens at `ranges[0]`; an empty slice list yields `0.0`. Cache writes always use the model's base rates.
/// - Threshold-based (`pricing.tiers` is non-empty): slice `n` bills at `tiers[n - 1]` (clamped to the last tier) and unclassified tokens at base rates. Reasoning tokens at an elevated threshold tier always use that tier's output rate.
/// - Flat (neither set): all tokens bill at base rates, even if `above_tiers` is populated.
///
/// Unpublished rates (`0.0`) inherit from the level directly below. Reasoning tokens fall back to the active level's output rate when `output_cost_per_reasoning_token` is `0.0`, and `cache_creation_1h` falls back to the active level's 5-minute cache-write rate when `cache_creation_input_token_cost_above_1hr` is `0.0`.
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
        let (Some(first), Some(top)) = (ranges.first(), ranges.last()) else {
            return 0.0;
        };
        return bill_levels(counts, PriceLevel::range(first, pricing), |level, below| {
            // Level `n` maps to `ranges[n]` (clamped to `top`), inheriting unpublished fields from `below`.
            let row = ranges.get(level).unwrap_or(top);
            PriceLevel {
                input: positive_or(row.input_cost_per_token, below.input),
                output: positive_or(row.output_cost_per_token, below.output),
                reasoning_raw: positive_or(
                    row.output_cost_per_reasoning_token,
                    below.reasoning_raw,
                ),
                cache_read: positive_or(row.cache_read_input_token_cost, below.cache_read),
                ..below
            }
        });
    }

    bill_levels(counts, PriceLevel::base(pricing), |level, below| {
        // Level `n` maps to `tiers[n - 1]` (clamped to the last tier), inheriting unpublished fields from `below`.
        match pricing
            .tiers
            .get(level - 1)
            .or_else(|| pricing.tiers.last())
        {
            Some(tier) => PriceLevel {
                input: positive_or(tier.input_cost_per_token, below.input),
                output: positive_or(tier.output_cost_per_token, below.output),
                // LiteLLM publishes no tier-specific reasoning rate; force fallback to this tier's output rate rather than the lower base reasoning rate.
                reasoning_raw: 0.0,
                cache_read: positive_or(tier.cache_read_input_token_cost, below.cache_read),
                cc_5m: positive_or(tier.cache_creation_input_token_cost, below.cc_5m),
                cc_1h_raw: tier.cache_creation_input_token_cost_above_1hr,
            },
            // No published tiers (e.g. stale snapshot boundaries): keep base rates verbatim, including any dedicated reasoning rate.
            None => below,
        }
    })
}

/// Bills unclassified tokens at `base` and each slice in `counts.above_tiers` at the price resolved by `level_price`.
fn bill_levels(
    counts: &TokenCounts,
    base: PriceLevel,
    level_price: impl Fn(usize, PriceLevel) -> PriceLevel,
) -> f64 {
    let mut classified = TierSlice::default();
    for slice in &counts.above_tiers {
        classified.merge(slice);
    }

    // Clamp remainders to zero so a malformed slice merge cannot bill negative base tokens.
    let remainder = |total: i64, classified: i64| (total - classified).max(0);
    let mut cost = base.bill(&TierSlice {
        input_tokens: remainder(counts.input_tokens, classified.input_tokens),
        output_tokens: remainder(counts.output_tokens, classified.output_tokens),
        reasoning_tokens: remainder(counts.reasoning_tokens, classified.reasoning_tokens),
        cache_read: remainder(counts.cache_read, classified.cache_read),
        cache_creation_5m: remainder(counts.cache_creation_5m, classified.cache_creation_5m),
        cache_creation_1h: remainder(counts.cache_creation_1h, classified.cache_creation_1h),
    });

    // Resolve every level in order even for empty slices so unpublished prices inherit from the level below.
    let mut prices = base;
    for (index, slice) in counts.above_tiers.iter().enumerate() {
        prices = level_price(index + 1, prices);
        if !slice.is_empty() {
            cost += prices.bill(slice);
        }
    }

    cost
}

fn positive_or(value: f64, fallback: f64) -> f64 {
    if value > 0.0 { value } else { fallback }
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

    /// Sets the slice for 1-based `level`, zero-filling lower levels.
    fn classify(counts: &mut TokenCounts, level: usize, slice: TierSlice) {
        if counts.above_tiers.len() < level {
            counts.above_tiers.resize(level, TierSlice::default());
        }
        counts.above_tiers[level - 1] = slice;
    }

    fn slice(input: i64, output: i64) -> TierSlice {
        TierSlice {
            input_tokens: input,
            output_tokens: output,
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
        let p = sonnet_like_pricing();
        let cost = calculate_cost(&counts(5_000_000, 250_000, 0, 2_000_000, 0, 0), &p);
        let expected = 5_000_000.0 * 0.000003 + 250_000.0 * 0.000015 + 2_000_000.0 * 0.0000003;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_above_slice_bills_at_tier_and_remainder_at_base() {
        let p = sonnet_like_pricing();
        let mut c = counts(300_000, 1_000, 0, 100_000, 10_000, 0);
        classify(
            &mut c,
            1,
            TierSlice {
                input_tokens: 250_000,
                output_tokens: 600,
                cache_read: 80_000,
                cache_creation_5m: 10_000,
                ..Default::default()
            },
        );
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
        classify(&mut c, 1, slice(250_000, 1_000));
        let cost = calculate_cost(&c, &p);
        assert_eq!(cost, 250_000.0 * 0.000006 + 1_000.0 * 0.0000225);
    }

    #[test]
    fn test_above_slice_without_published_tier_bills_at_base() {
        // Thresholds can come from a newer pricing snapshot than this entry; the tokens must still be billed exactly once, at base rates.
        let p = flat_pricing();
        let mut c = counts(300_000, 1_000, 0, 0, 0, 0);
        classify(&mut c, 1, slice(300_000, 1_000));
        let cost = calculate_cost(&c, &p);
        assert_eq!(cost, 300_000.0 * 0.000003 + 1_000.0 * 0.000015);
    }

    #[test]
    fn test_above_slice_without_tier_keeps_dedicated_reasoning_rate() {
        let p = ModelPricing {
            input_cost_per_token: 1e-6,
            output_cost_per_token: 8e-6,
            output_cost_per_reasoning_token: 3e-6,
            ..Default::default()
        };
        let mut c = counts(1_000, 200, 500, 0, 0, 0);
        classify(
            &mut c,
            1,
            TierSlice {
                input_tokens: 1_000,
                output_tokens: 200,
                reasoning_tokens: 500,
                ..Default::default()
            },
        );
        let cost = calculate_cost(&c, &p);
        let expected = 1_000.0 * 1e-6 + 200.0 * 8e-6 + 500.0 * 3e-6;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_unpublished_tier_field_falls_back_to_base_price() {
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
        classify(
            &mut c,
            1,
            TierSlice {
                cache_read: 200_000,
                ..Default::default()
            },
        );
        let cost = calculate_cost(&c, &p);
        assert_eq!(cost, 200_000.0 * 0.0000001);
    }

    #[test]
    fn test_above_reasoning_uses_tier_output_rate() {
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
        classify(
            &mut c,
            1,
            TierSlice {
                input_tokens: 250_000,
                output_tokens: 1_000,
                reasoning_tokens: 500,
                ..Default::default()
            },
        );
        let cost = calculate_cost(&c, &p);
        let expected = 250_000.0 * 6e-6 + 1_000.0 * 2.25e-5 + 500.0 * 2.25e-5;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_negative_base_slice_clamps_to_zero() {
        // A malformed merge could leave above > total; never bill negative base tokens.
        let p = sonnet_like_pricing();
        let mut c = counts(100, 0, 0, 0, 0, 0);
        classify(&mut c, 1, slice(500, 0));
        let cost = calculate_cost(&c, &p);
        assert_eq!(cost, 500.0 * 0.000006);
    }

    fn qwen_like_pricing() -> ModelPricing {
        // Mimics dashscope/qwen3-coder-plus: four contiguous volume rows.
        ModelPricing {
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
        }
    }

    #[test]
    fn test_range_aggregate_volume_alone_never_promotes_to_a_higher_row() {
        let p = qwen_like_pricing();

        let c_low = calculate_cost(&counts(20_000, 5_000, 0, 0, 0, 0), &p);
        assert_eq!(c_low, 20_000.0 * 0.000001 + 5_000.0 * 0.000005);

        let c_hi = calculate_cost(&counts(500_000, 5_000, 0, 0, 0, 0), &p);
        assert_eq!(c_hi, 500_000.0 * 0.000001 + 5_000.0 * 0.000005);
    }

    #[test]
    fn test_range_above_slice_bills_at_row_one_and_remainder_at_row_zero() {
        let p = qwen_like_pricing();
        let mut c = counts(500_000, 5_000, 0, 0, 0, 0);
        classify(&mut c, 1, slice(300_000, 4_000));
        let cost = calculate_cost(&c, &p);
        let base_part = 200_000.0 * 0.000001 + 1_000.0 * 0.000005;
        let above_part = 300_000.0 * 0.0000018 + 4_000.0 * 0.000009;
        assert_eq!(cost, base_part + above_part);
    }

    #[test]
    fn test_range_above_slice_reaches_the_row_it_classified_into() {
        let p = qwen_like_pricing();
        let mut c = counts(500_000, 5_000, 0, 0, 0, 0);
        classify(&mut c, 3, slice(500_000, 5_000));
        let cost = calculate_cost(&c, &p);
        assert_eq!(cost, 500_000.0 * 0.000006 + 5_000.0 * 0.00006);
    }

    #[test]
    fn test_every_range_row_bills_its_own_slice() {
        let p = qwen_like_pricing();
        let mut c = counts(400_000, 4_000, 0, 0, 0, 0);
        classify(&mut c, 1, slice(100_000, 1_000));
        classify(&mut c, 2, slice(100_000, 1_000));
        classify(&mut c, 3, slice(100_000, 1_000));
        let cost = calculate_cost(&c, &p);
        let expected = 100_000.0 * 0.000001
            + 1_000.0 * 0.000005
            + 100_000.0 * 0.0000018
            + 1_000.0 * 0.000009
            + 100_000.0 * 0.000003
            + 1_000.0 * 0.000015
            + 100_000.0 * 0.000006
            + 1_000.0 * 0.00006;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_each_threshold_tier_bills_its_own_slice() {
        // LiteLLM currently publishes single-tier threshold models; pin multi-tier `tiers[n - 1]` indexing.
        let p = ModelPricing {
            input_cost_per_token: 1e-6,
            output_cost_per_token: 2e-6,
            tiers: vec![
                ThresholdTier {
                    threshold_tokens: 200_000,
                    input_cost_per_token: 2e-6,
                    output_cost_per_token: 4e-6,
                    ..Default::default()
                },
                ThresholdTier {
                    threshold_tokens: 500_000,
                    input_cost_per_token: 4e-6,
                    output_cost_per_token: 8e-6,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let mut c = counts(900_000, 9_000, 0, 0, 0, 0);
        classify(&mut c, 1, slice(300_000, 3_000));
        classify(&mut c, 2, slice(400_000, 4_000));
        let expected = 200_000.0 * 1e-6
            + 2_000.0 * 2e-6
            + 300_000.0 * 2e-6
            + 3_000.0 * 4e-6
            + 400_000.0 * 4e-6
            + 4_000.0 * 8e-6;
        assert_eq!(calculate_cost(&c, &p), expected);
    }

    #[test]
    fn test_a_level_past_the_last_row_bills_at_that_row() {
        // Stale boundaries from a newer snapshot clamp to the dearest row rather than row 0.
        let p = qwen_like_pricing();
        let mut c = counts(10_000, 0, 0, 0, 0, 0);
        classify(&mut c, 7, slice(10_000, 0));
        assert_eq!(calculate_cost(&c, &p), 10_000.0 * 0.000006);
    }

    #[test]
    fn test_higher_range_row_inherits_prices_from_the_row_below_it() {
        let p = ModelPricing {
            ranges: Some(vec![
                TierRange {
                    min_tokens: 0,
                    max_tokens: 32_000,
                    input_cost_per_token: 1e-6,
                    output_cost_per_token: 5e-6,
                    ..Default::default()
                },
                TierRange {
                    min_tokens: 32_000,
                    max_tokens: 128_000,
                    input_cost_per_token: 1.8e-6,
                    output_cost_per_token: 9e-6,
                    ..Default::default()
                },
                TierRange {
                    min_tokens: 128_000,
                    max_tokens: 256_000,
                    input_cost_per_token: 3e-6,
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        let mut c = counts(200_000, 2_000, 0, 0, 0, 0);
        classify(&mut c, 2, slice(200_000, 2_000));
        assert_eq!(calculate_cost(&c, &p), 200_000.0 * 3e-6 + 2_000.0 * 9e-6);
    }

    #[test]
    fn test_range_row_one_falls_back_to_row_zero_for_unpublished_prices() {
        let p = ModelPricing {
            ranges: Some(vec![
                TierRange {
                    min_tokens: 0,
                    max_tokens: 32_000,
                    input_cost_per_token: 0.000001,
                    output_cost_per_token: 0.000005,
                    cache_read_input_token_cost: 0.0000001,
                    ..Default::default()
                },
                TierRange {
                    min_tokens: 32_000,
                    max_tokens: 128_000,
                    input_cost_per_token: 0.0000018,
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        let mut c = counts(10_000, 2_000, 0, 5_000, 0, 0);
        classify(
            &mut c,
            1,
            TierSlice {
                input_tokens: 10_000,
                output_tokens: 2_000,
                cache_read: 5_000,
                ..Default::default()
            },
        );
        let cost = calculate_cost(&c, &p);
        assert_eq!(
            cost,
            10_000.0 * 0.0000018 + 2_000.0 * 0.000005 + 5_000.0 * 0.0000001
        );
    }

    #[test]
    fn test_single_row_range_prices_every_volume_at_that_row() {
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

        // A single-row model has no threshold, so volumes past `max_tokens` still bill at row 0.
        let cost = calculate_cost(&counts(200_000, 0, 0, 0, 0, 0), &p);
        assert_eq!(cost, 200_000.0 * 0.000001);

        // An above-slice inherited from a stale threshold bills there too.
        let mut c = counts(200_000, 0, 0, 0, 0, 0);
        classify(&mut c, 1, slice(200_000, 0));
        assert_eq!(calculate_cost(&c, &p), 200_000.0 * 0.000001);
    }

    #[test]
    fn test_range_cache_writes_stay_on_base_prices() {
        let p = ModelPricing {
            cache_creation_input_token_cost: 0.00000375,
            cache_creation_input_token_cost_above_1hr: 0.000006,
            ranges: Some(vec![
                TierRange {
                    min_tokens: 0,
                    max_tokens: 32_000,
                    input_cost_per_token: 0.000001,
                    ..Default::default()
                },
                TierRange {
                    min_tokens: 32_000,
                    max_tokens: 128_000,
                    input_cost_per_token: 0.0000018,
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        let mut c = counts(0, 0, 0, 0, 4_000, 2_000);
        classify(
            &mut c,
            1,
            TierSlice {
                cache_creation_5m: 1_000,
                cache_creation_1h: 500,
                ..Default::default()
            },
        );
        let cost = calculate_cost(&c, &p);
        assert_eq!(cost, 4_000.0 * 0.00000375 + 2_000.0 * 0.000006);
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

        let cost = calculate_cost(&counts(0, 0, 0, 0, 0, 10_000), &p);
        assert_eq!(cost, 10_000.0 * 1e-5);

        let cost_mixed = calculate_cost(&counts(0, 0, 0, 0, 1_000, 10_000), &p);
        assert_eq!(cost_mixed, 1_000.0 * 6.25e-6 + 10_000.0 * 1e-5);
    }

    #[test]
    fn test_1hr_falls_back_to_5m_when_model_has_no_extended_price() {
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
        classify(
            &mut c,
            1,
            TierSlice {
                input_tokens: 250_000,
                cache_creation_5m: 5_000,
                cache_creation_1h: 5_000,
                ..Default::default()
            },
        );
        let cost = calculate_cost(&c, &p);
        let expected = 250_000.0 * 6e-6 + 5_000.0 * 7.5e-6 + 5_000.0 * 1.2e-5;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_reasoning_billed_at_dedicated_rate_when_published() {
        // Distinct reasoning and output rates verify reasoning is not coerced to the output rate.
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
