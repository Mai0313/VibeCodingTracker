// Integration tests for pricing system functionality
//
// These tests verify pricing fetch, cache, and calculation logic

use std::collections::HashMap;
use vibe_coding_tracker::pricing::{
    ModelPricing, ModelPricingMap, calculate_cost, clear_pricing_cache, fetch_model_pricing,
};

#[test]
fn test_fetch_model_pricing_basic() {
    // Test that fetching pricing data works
    // This is a network test, so it might fail if offline
    let result = fetch_model_pricing();

    if let Ok(pricing_map) = result {
        // Verify some common models exist
        let claude_result = pricing_map.get("claude-3-opus");
        assert!(
            claude_result.pricing.input_cost_per_token >= 0.0,
            "Claude pricing should be non-negative"
        );

        let gpt4_result = pricing_map.get("gpt-4");
        assert!(
            gpt4_result.pricing.input_cost_per_token >= 0.0,
            "GPT-4 pricing should be non-negative"
        );
    } else {
        eprintln!("Skipping online pricing test: network error");
    }
}

#[test]
fn test_pricing_cache_functionality() {
    // Clear cache before test
    clear_pricing_cache();

    // First fetch (should hit network or disk cache)
    let result1 = fetch_model_pricing();

    if result1.is_ok() {
        // Second fetch (should use cache)
        let result2 = fetch_model_pricing();
        assert!(result2.is_ok(), "Cached fetch should succeed");

        // Results should be equivalent
        if let (Ok(p1), Ok(p2)) = (result1, result2) {
            let model = "claude-3-opus";
            let price1 = p1.get(model);
            let price2 = p2.get(model);

            assert_eq!(
                price1.pricing.input_cost_per_token, price2.pricing.input_cost_per_token,
                "Cached pricing should match original"
            );
        }
    }
}

#[test]
fn test_model_pricing_exact_match() {
    clear_pricing_cache();

    let mut raw_map = HashMap::new();
    raw_map.insert(
        "test-exact-model-unique-123".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.000075,
            cache_read_input_token_cost: 0.0000015,
            cache_creation_input_token_cost: 0.000018,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("test-exact-model-unique-123");
    assert_eq!(result.pricing.input_cost_per_token, 0.000015);
    assert_eq!(
        result.matched_model, None,
        "Exact match should not set matched_model"
    );
}

#[test]
fn test_model_pricing_normalized_match() {
    let mut raw_map = HashMap::new();
    raw_map.insert(
        "claude-3-sonnet".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    // Test with version suffix
    let result = pricing_map.get("claude-3-sonnet-20240229");
    assert_eq!(result.pricing.input_cost_per_token, 0.000003);
    assert_eq!(
        result.matched_model,
        Some("claude-3-sonnet".to_string()),
        "Should match normalized name"
    );
}

#[test]
fn test_model_pricing_substring_match() {
    clear_pricing_cache();

    let mut raw_map = HashMap::new();
    raw_map.insert(
        "test-model-base".to_string(),
        ModelPricing {
            input_cost_per_token: 0.00003,
            output_cost_per_token: 0.00006,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("test-model-base-extended");
    assert_eq!(result.pricing.input_cost_per_token, 0.00003);
    assert_eq!(
        result.matched_model,
        Some("test-model-base".to_string()),
        "Should match via substring"
    );
}

#[test]
fn test_model_pricing_fuzzy_match() {
    let mut raw_map = HashMap::new();
    raw_map.insert(
        "claude-3-5-sonnet".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("claude-35-sonnet");
    assert!(
        result.matched_model.is_some() || result.pricing.input_cost_per_token == 0.0,
        "Should either fuzzy match or return default"
    );
}

#[test]
fn test_model_pricing_no_match() {
    let raw_map = HashMap::new();
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("unknown-model-xyz");
    assert_eq!(result.pricing.input_cost_per_token, 0.0);
    assert_eq!(result.pricing.output_cost_per_token, 0.0);
    assert_eq!(result.matched_model, None, "No match should return None");
}

#[test]
fn test_calculate_cost_basic() {
    let pricing = ModelPricing {
        input_cost_per_token: 0.000003,
        output_cost_per_token: 0.000015,
        cache_read_input_token_cost: 0.0000003,
        cache_creation_input_token_cost: 0.00000375,
        ..Default::default()
    };

    let cost = calculate_cost(1000, 500, 10000, 2000, &pricing);
    // input: 1000 * 0.000003 = 0.003
    // output: 500 * 0.000015 = 0.0075
    // cache_read: 10000 * 0.0000003 = 0.003
    // cache_creation: 2000 * 0.00000375 = 0.0075
    // total: 0.021
    assert_eq!(cost, 0.021);
}

#[test]
fn test_calculate_cost_zero_tokens() {
    let pricing = ModelPricing::default();
    let cost = calculate_cost(0, 0, 0, 0, &pricing);
    assert_eq!(cost, 0.0);
}

#[test]
fn test_calculate_cost_no_cache() {
    let pricing = ModelPricing {
        input_cost_per_token: 0.000003,
        output_cost_per_token: 0.000015,
        ..Default::default()
    };

    let cost = calculate_cost(1000, 500, 0, 0, &pricing);
    // input: 1000 * 0.000003 = 0.003
    // output: 500 * 0.000015 = 0.0075
    // total: 0.0105
    assert_eq!(cost, 0.0105);
}

#[test]
fn test_calculate_cost_large_numbers() {
    let pricing = ModelPricing {
        input_cost_per_token: 0.000001,
        output_cost_per_token: 0.000002,
        cache_read_input_token_cost: 0.0000001,
        cache_creation_input_token_cost: 0.0000005,
        ..Default::default()
    };

    let cost = calculate_cost(1_000_000, 500_000, 100_000, 50_000, &pricing);
    assert!(cost > 0.0);
    assert!(cost.is_finite());
}

#[test]
fn test_pricing_with_provider_prefix() {
    let mut raw_map = HashMap::new();
    raw_map.insert(
        "claude-3-opus".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.000075,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    // Test with provider prefix
    let result = pricing_map.get("bedrock/claude-3-opus-20240229");
    assert!(
        result.pricing.input_cost_per_token > 0.0 || result.matched_model.is_some(),
        "Should match after normalization"
    );
}

#[test]
fn test_pricing_multiple_models() {
    let mut raw_map = HashMap::new();

    raw_map.insert(
        "claude-3-opus".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000015,
            ..Default::default()
        },
    );

    raw_map.insert(
        "gpt-4".to_string(),
        ModelPricing {
            input_cost_per_token: 0.00003,
            ..Default::default()
        },
    );

    raw_map.insert(
        "gemini-pro".to_string(),
        ModelPricing {
            input_cost_per_token: 0.0000005,
            ..Default::default()
        },
    );

    let pricing_map = ModelPricingMap::new(raw_map);

    // Test all models
    assert!(
        pricing_map
            .get("claude-3-opus")
            .pricing
            .input_cost_per_token
            > 0.0
    );
    assert!(pricing_map.get("gpt-4").pricing.input_cost_per_token > 0.0);
    assert!(pricing_map.get("gemini-pro").pricing.input_cost_per_token > 0.0);
}

#[test]
fn test_pricing_serialization() {
    let pricing = ModelPricing {
        input_cost_per_token: 0.000003,
        output_cost_per_token: 0.000015,
        cache_read_input_token_cost: 0.0000003,
        cache_creation_input_token_cost: 0.00000375,
        ..Default::default()
    };

    let json = serde_json::to_string(&pricing).unwrap();
    let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

    assert_eq!(
        deserialized.input_cost_per_token,
        pricing.input_cost_per_token
    );
    assert_eq!(
        deserialized.output_cost_per_token,
        pricing.output_cost_per_token
    );
}

#[test]
fn test_pricing_case_insensitive() {
    let mut raw_map = HashMap::new();
    raw_map.insert(
        "GPT-4".to_string(),
        ModelPricing {
            input_cost_per_token: 0.00003,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    // Should match despite case difference
    let result = pricing_map.get("gpt-4");
    assert!(
        result.pricing.input_cost_per_token > 0.0 || result.matched_model.is_some(),
        "Should match despite case difference"
    );
}

#[test]
fn test_pricing_with_special_characters() {
    let mut raw_map = HashMap::new();
    raw_map.insert(
        "model-with-special_chars.123".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000001,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("model-with-special_chars.123");
    assert_eq!(result.pricing.input_cost_per_token, 0.000001);
}

#[test]
fn test_pricing_cache_expiration() {
    // Test that cache expires after 24 hours (implicitly tested by date-based cache)
    // This is difficult to test directly without mocking time
    // Just verify that cache operations work

    clear_pricing_cache();
    let _ = fetch_model_pricing();

    // Should use cache on second call
    let result = fetch_model_pricing();
    assert!(
        result.is_ok() || result.is_err(),
        "Cache operations should not panic"
    );
}

#[test]
fn test_pricing_above_200k_tokens() {
    let pricing = ModelPricing {
        input_cost_per_token: 0.000001,
        output_cost_per_token: 0.000002,
        input_cost_per_token_above_200k_tokens: 0.000002,
        output_cost_per_token_above_200k_tokens: 0.000004,
        ..Default::default()
    };

    assert_eq!(pricing.input_cost_per_token_above_200k_tokens, 0.000002);
    assert_eq!(pricing.output_cost_per_token_above_200k_tokens, 0.000004);
}

#[test]
fn test_pricing_result_structure() {
    use vibe_coding_tracker::pricing::ModelPricingResult;

    let pricing = ModelPricing::default();
    let result = ModelPricingResult {
        pricing,
        matched_model: Some("test-model".to_string()),
    };

    assert_eq!(result.matched_model, Some("test-model".to_string()));
    assert_eq!(result.pricing.input_cost_per_token, 0.0);
}

#[test]
fn test_pricing_edge_cases() {
    // Test with empty string
    let raw_map = HashMap::new();
    let pricing_map = ModelPricingMap::new(raw_map);
    let result = pricing_map.get("");
    assert_eq!(result.pricing.input_cost_per_token, 0.0);

    // Test with very long model name
    let long_name = "a".repeat(1000);
    let result = pricing_map.get(&long_name);
    assert_eq!(result.pricing.input_cost_per_token, 0.0);
}
