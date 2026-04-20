use super::cache::{ModelPricing, TierRange};

/// Calculates total cost for a request given token counts and the model's pricing.
///
/// Strategy (highest priority first):
/// 1. If `pricing.ranges` is `Some`, selects a `TierRange` by `input_tokens`
///    (Qwen / doubao style volume tiers) — tier prices apply standalone.
/// 2. Otherwise, if `pricing.tiers` is non-empty, picks the highest tier whose
///    `threshold_tokens` is exceeded by total input context
///    (input + cache_read + cache_creation). All four token types are charged
///    at that tier's prices — matching Anthropic / Google "above Nk tokens"
///    semantics where prompt size promotes the entire request to a higher rate.
/// 3. Otherwise, uses flat base prices for every token type.
///
/// `cache_creation_5m_tokens` and `cache_creation_1h_tokens` are priced
/// separately (5-minute default TTL vs 1-hour extended TTL). When a model
/// doesn't publish a 1hr price (value is 0.0), the 5m price is used for both
/// buckets — matching current behaviour for providers that don't split TTL.
pub fn calculate_cost(
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_5m_tokens: i64,
    cache_creation_1h_tokens: i64,
    pricing: &ModelPricing,
) -> f64 {
    let total_cache_creation = cache_creation_5m_tokens + cache_creation_1h_tokens;

    if let Some(ranges) = &pricing.ranges {
        // Range-based pricing is Qwen-only; it doesn't have a 1hr concept, so
        // collapse both TTL buckets into the total for rate selection.
        return calculate_cost_ranges(
            input_tokens,
            output_tokens,
            cache_read_tokens,
            ranges,
        );
    }

    let total_input_context = input_tokens + cache_read_tokens + total_cache_creation;
    let active_tier = pricing
        .tiers
        .iter()
        .rev()
        .find(|t| total_input_context > t.threshold_tokens);

    let (input_price, output_price, cache_read_price, cc_5m_price, cc_1h_price_raw) =
        match active_tier {
            Some(t) => (
                t.input_cost_per_token,
                t.output_cost_per_token,
                t.cache_read_input_token_cost,
                t.cache_creation_input_token_cost,
                t.cache_creation_input_token_cost_above_1hr,
            ),
            None => (
                pricing.input_cost_per_token,
                pricing.output_cost_per_token,
                pricing.cache_read_input_token_cost,
                pricing.cache_creation_input_token_cost,
                pricing.cache_creation_input_token_cost_above_1hr,
            ),
        };

    // If the model doesn't publish a 1hr price, bill 1h tokens at the 5m rate
    // (safer to under-bill than fabricate a rate — and matches legacy behaviour
    // for models / providers with no TTL split).
    let cc_1h_price = if cc_1h_price_raw > 0.0 {
        cc_1h_price_raw
    } else {
        cc_5m_price
    };

    input_tokens as f64 * input_price
        + output_tokens as f64 * output_price
        + cache_read_tokens as f64 * cache_read_price
        + cache_creation_5m_tokens as f64 * cc_5m_price
        + cache_creation_1h_tokens as f64 * cc_1h_price
}

/// Range-based pricing: `input_tokens` selects the matching `TierRange`.
///
/// Falls back to the last (highest) range for over-cap usage so e.g. a Qwen
/// call beyond the advertised max still gets charged the top-tier rate rather
/// than silently priced at $0.
fn calculate_cost_ranges(
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    ranges: &[TierRange],
) -> f64 {
    let range = ranges
        .iter()
        .find(|r| input_tokens >= r.min_tokens && input_tokens < r.max_tokens)
        .or_else(|| ranges.last());

    match range {
        Some(r) => {
            input_tokens as f64 * r.input_cost_per_token
                + output_tokens as f64 * r.output_cost_per_token
                + cache_read_tokens as f64 * r.cache_read_input_token_cost
        }
        None => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::cache::ThresholdTier;

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
        let cost = calculate_cost(1000, 500, 200, 100, 0, &p);
        let expected = 1000.0 * 0.000003
            + 500.0 * 0.000015
            + 200.0 * 0.0000003
            + 100.0 * 0.00000375;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_threshold_tier_below_threshold_uses_base() {
        let p = sonnet_like_pricing();
        // Total input context = 1000 + 200 + 100 = 1300 ≤ 200K → base prices
        let cost = calculate_cost(1000, 500, 200, 100, 0, &p);
        let expected = 1000.0 * 0.000003
            + 500.0 * 0.000015
            + 200.0 * 0.0000003
            + 100.0 * 0.00000375;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_threshold_tier_above_threshold_applies_tier() {
        let p = sonnet_like_pricing();
        // Total input context = 250K + 250K + 250K = 750K > 200K → tier prices for ALL types
        let cost = calculate_cost(250_000, 250_000, 250_000, 250_000, 0, &p);
        let expected = 250_000.0 * 0.000006
            + 250_000.0 * 0.0000225
            + 250_000.0 * 0.0000006
            + 250_000.0 * 0.0000075;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_threshold_uses_total_input_context_not_output() {
        let p = sonnet_like_pricing();
        // Small input context (50K) but massive output (500K) → still base prices
        let cost = calculate_cost(50_000, 500_000, 0, 0, 0, &p);
        let expected = 50_000.0 * 0.000003 + 500_000.0 * 0.000015;
        assert_eq!(cost, expected);
    }

    #[test]
    fn test_exact_200k_stays_on_base() {
        let p = sonnet_like_pricing();
        let cost_exact = calculate_cost(200_000, 50_000, 0, 0, 0, &p);
        assert_eq!(cost_exact, 200_000.0 * 0.000003 + 50_000.0 * 0.000015);

        let cost_above = calculate_cost(200_001, 50_000, 0, 0, 0, &p);
        assert_eq!(cost_above, 200_001.0 * 0.000006 + 50_000.0 * 0.0000225);
    }

    #[test]
    fn test_multi_tier_picks_highest_exceeded() {
        // Synthetic model with 128k and 272k tiers (as GPT-5.x does).
        let p = ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            tiers: vec![
                ThresholdTier {
                    threshold_tokens: 128_000,
                    input_cost_per_token: 0.000002,
                    output_cost_per_token: 0.000004,
                    ..Default::default()
                },
                ThresholdTier {
                    threshold_tokens: 272_000,
                    input_cost_per_token: 0.000004,
                    output_cost_per_token: 0.000008,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        // 100K: below both tiers → base prices
        let c1 = calculate_cost(100_000, 10_000, 0, 0, 0, &p);
        assert_eq!(c1, 100_000.0 * 0.000001 + 10_000.0 * 0.000002);

        // 200K: above 128k, below 272k → first tier
        let c2 = calculate_cost(200_000, 10_000, 0, 0, 0, &p);
        assert_eq!(c2, 200_000.0 * 0.000002 + 10_000.0 * 0.000004);

        // 300K: above both → second (highest) tier
        let c3 = calculate_cost(300_000, 10_000, 0, 0, 0, &p);
        assert_eq!(c3, 300_000.0 * 0.000004 + 10_000.0 * 0.000008);
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

        let c_low = calculate_cost(20_000, 5_000, 0, 0, 0, &p);
        assert_eq!(c_low, 20_000.0 * 0.000001 + 5_000.0 * 0.000005);

        let c_hi = calculate_cost(500_000, 5_000, 0, 0, 0, &p);
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
        let cost = calculate_cost(200_000, 0, 0, 0, 0, &p);
        assert_eq!(cost, 200_000.0 * 0.000001);
    }

    #[test]
    fn test_zero_tokens() {
        let p = sonnet_like_pricing();
        assert_eq!(calculate_cost(0, 0, 0, 0, 0, &p), 0.0);
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
        let cost = calculate_cost(0, 0, 0, 0, 10_000, &p);
        assert_eq!(cost, 10_000.0 * 1e-5);

        // Mixed: 1_000 at 5m + 10_000 at 1h.
        let cost_mixed = calculate_cost(0, 0, 0, 1_000, 10_000, &p);
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

        let cost = calculate_cost(0, 0, 0, 0, 10_000, &p);
        assert_eq!(cost, 10_000.0 * 6.25e-6);
    }

    #[test]
    fn test_1hr_uses_tier_price_when_tier_active() {
        // Tier carries its own 1hr price (Claude 3.5 Sonnet-style).
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

        // Input context = 250K → tier applies.
        // 5_000 @ 5m tier, 5_000 @ 1hr tier.
        let cost = calculate_cost(250_000, 0, 0, 5_000, 5_000, &p);
        let expected = 250_000.0 * 6e-6 + 5_000.0 * 7.5e-6 + 5_000.0 * 1.2e-5;
        assert_eq!(cost, expected);
    }
}
