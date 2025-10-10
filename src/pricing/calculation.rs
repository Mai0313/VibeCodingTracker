use super::cache::ModelPricing;

const TOKEN_THRESHOLD: i64 = 200_000;

/// Calculates total cost based on token usage and model pricing
///
/// Each token type (input, output, cache_read, cache_creation) is evaluated independently
/// against the 200K threshold. If a type exceeds 200K tokens, the corresponding above_200k
/// price is used; otherwise, the base price applies.
pub fn calculate_cost(
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    pricing: &ModelPricing,
) -> f64 {
    // Helper function to get the appropriate price based on token count
    // Note: above_200k prices are already normalized to base prices if not provided
    let get_price = |tokens: i64, base_price: f64, above_200k_price: f64| -> f64 {
        if tokens > TOKEN_THRESHOLD {
            above_200k_price
        } else {
            base_price
        }
    };

    // Calculate costs for each token type with appropriate pricing
    let input_price = get_price(
        input_tokens,
        pricing.input_cost_per_token,
        pricing.input_cost_per_token_above_200k_tokens,
    );
    let output_price = get_price(
        output_tokens,
        pricing.output_cost_per_token,
        pricing.output_cost_per_token_above_200k_tokens,
    );
    let cache_read_price = get_price(
        cache_read_tokens,
        pricing.cache_read_input_token_cost,
        pricing.cache_read_input_token_cost_above_200k_tokens,
    );
    let cache_creation_price = get_price(
        cache_creation_tokens,
        pricing.cache_creation_input_token_cost,
        pricing.cache_creation_input_token_cost_above_200k_tokens,
    );

    let input_cost = input_tokens as f64 * input_price;
    let output_cost = output_tokens as f64 * output_price;
    let cache_read_cost = cache_read_tokens as f64 * cache_read_price;
    let cache_creation_cost = cache_creation_tokens as f64 * cache_creation_price;

    input_cost + output_cost + cache_read_cost + cache_creation_cost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_cost() {
        let pricing = ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            cache_read_input_token_cost: 0.0000001,
            cache_creation_input_token_cost: 0.0000005,
            input_cost_per_token_above_200k_tokens: 0.000002,
            output_cost_per_token_above_200k_tokens: 0.000004,
            cache_read_input_token_cost_above_200k_tokens: 0.0000002,
            cache_creation_input_token_cost_above_200k_tokens: 0.000001,
        };

        // Test with tokens below 200K threshold - all use base price
        let cost = calculate_cost(1000, 500, 200, 100, &pricing);
        assert_eq!(cost, 0.001_000 + 0.001_000 + 0.000_020 + 0.000_050);

        // Test with ALL tokens above 200K threshold (should use above_200k pricing)
        let cost_above = calculate_cost(250_000, 250_000, 250_000, 250_000, &pricing);
        let expected = 250_000.0 * 0.000002  // input with above_200k price
            + 250_000.0 * 0.000004           // output with above_200k price
            + 250_000.0 * 0.0000002          // cache_read with above_200k price
            + 250_000.0 * 0.000001; // cache_creation with above_200k price
        assert_eq!(cost_above, expected);
    }

    #[test]
    fn test_calculate_cost_mixed_threshold() {
        // Test: Each token type is checked INDEPENDENTLY against 200K
        let pricing = ModelPricing {
            input_cost_per_token: 0.000003,              // base: $3 per million
            output_cost_per_token: 0.000015,             // base: $15 per million
            cache_read_input_token_cost: 0.0000003,      // base: $0.3 per million
            cache_creation_input_token_cost: 0.00000375, // base: $3.75 per million
            input_cost_per_token_above_200k_tokens: 0.000006, // above: $6 per million (2x)
            output_cost_per_token_above_200k_tokens: 0.0000225, // above: $22.5 per million (1.5x)
            cache_read_input_token_cost_above_200k_tokens: 0.0000006, // above: $0.6 per million (2x)
            cache_creation_input_token_cost_above_200k_tokens: 0.0000075, // above: $7.5 per million (2x)
        };

        // Case 1: Only input_tokens exceeds 200K
        let cost1 = calculate_cost(250_000, 100_000, 150_000, 50_000, &pricing);
        let expected1 = 250_000.0 * 0.000006      // input: above_200k
            + 100_000.0 * 0.000015                // output: base
            + 150_000.0 * 0.0000003               // cache_read: base
            + 50_000.0 * 0.00000375; // cache_creation: base
        assert_eq!(cost1, expected1);

        // Case 2: Only output_tokens exceeds 200K
        let cost2 = calculate_cost(100_000, 250_000, 150_000, 50_000, &pricing);
        let expected2 = 100_000.0 * 0.000003      // input: base
            + 250_000.0 * 0.0000225               // output: above_200k
            + 150_000.0 * 0.0000003               // cache_read: base
            + 50_000.0 * 0.00000375; // cache_creation: base
        assert_eq!(cost2, expected2);

        // Case 3: input and cache_read exceed 200K, others don't
        let cost3 = calculate_cost(300_000, 100_000, 250_000, 50_000, &pricing);
        let expected3 = 300_000.0 * 0.000006      // input: above_200k
            + 100_000.0 * 0.000015                // output: base
            + 250_000.0 * 0.0000006               // cache_read: above_200k
            + 50_000.0 * 0.00000375; // cache_creation: base
        assert_eq!(cost3, expected3);

        // Case 4: Total > 200K but each type < 200K → all use base price
        let cost4 = calculate_cost(50_000, 80_000, 60_000, 40_000, &pricing);
        let expected4 = 50_000.0 * 0.000003       // input: base (< 200K)
            + 80_000.0 * 0.000015                 // output: base (< 200K)
            + 60_000.0 * 0.0000003                // cache_read: base (< 200K)
            + 40_000.0 * 0.00000375; // cache_creation: base (< 200K)
        assert_eq!(cost4, expected4);
    }

    #[test]
    fn test_calculate_cost_exactly_200k() {
        // Test boundary condition: exactly 200K tokens
        let pricing = ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            cache_read_input_token_cost: 0.0000001,
            cache_creation_input_token_cost: 0.0000005,
            input_cost_per_token_above_200k_tokens: 0.000002,
            output_cost_per_token_above_200k_tokens: 0.000004,
            cache_read_input_token_cost_above_200k_tokens: 0.0000002,
            cache_creation_input_token_cost_above_200k_tokens: 0.000001,
        };

        // Exactly 200K should use base price (> 200K triggers above_200k)
        let cost_exact = calculate_cost(200_000, 200_000, 200_000, 200_000, &pricing);
        let expected = 200_000.0 * 0.000001      // base price (not > 200K)
            + 200_000.0 * 0.000002               // base price
            + 200_000.0 * 0.0000001              // base price
            + 200_000.0 * 0.0000005; // base price
        assert_eq!(cost_exact, expected);

        // 200K + 1 should use above_200k price
        let cost_above = calculate_cost(200_001, 200_001, 200_001, 200_001, &pricing);
        let expected_above = 200_001.0 * 0.000002  // above_200k price (> 200K)
            + 200_001.0 * 0.000004                 // above_200k price
            + 200_001.0 * 0.0000002                // above_200k price
            + 200_001.0 * 0.000001; // above_200k price
        assert_eq!(cost_above, expected_above);
    }

    #[test]
    fn test_calculate_cost_fallback_to_base() {
        // Test fallback to base price when above_200k price is not available (0.0)
        let mut pricing = ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            cache_read_input_token_cost: 0.0000001,
            cache_creation_input_token_cost: 0.0000005,
            ..Default::default()
        };

        // Simulate normalization: fill above_200k with base prices
        pricing.input_cost_per_token_above_200k_tokens = pricing.input_cost_per_token;
        pricing.output_cost_per_token_above_200k_tokens = pricing.output_cost_per_token;
        pricing.cache_read_input_token_cost_above_200k_tokens = pricing.cache_read_input_token_cost;
        pricing.cache_creation_input_token_cost_above_200k_tokens =
            pricing.cache_creation_input_token_cost;

        // With tokens above 200K, should use base pricing (since above_200k was filled with base)
        let cost = calculate_cost(250_000, 250_000, 250_000, 250_000, &pricing);
        let expected = 250_000.0 * 0.000001  // input with base price
            + 250_000.0 * 0.000002           // output with base price
            + 250_000.0 * 0.0000001          // cache_read with base price
            + 250_000.0 * 0.0000005; // cache_creation with base price
        assert_eq!(cost, expected);
    }
}
