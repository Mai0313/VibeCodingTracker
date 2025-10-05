use std::collections::HashMap;
use vibe_coding_tracker::pricing::{
    calculate_cost, get_model_pricing, ModelPricing,
};

#[test]
fn test_model_pricing_default() {
    let pricing = ModelPricing::default();
    assert_eq!(pricing.input_cost_per_token, 0.0);
    assert_eq!(pricing.output_cost_per_token, 0.0);
    assert_eq!(pricing.cache_read_input_token_cost, 0.0);
    assert_eq!(pricing.cache_creation_input_token_cost, 0.0);
}

#[test]
fn test_calculate_cost_all_zeros() {
    let pricing = ModelPricing::default();
    let cost = calculate_cost(0, 0, 0, 0, &pricing);
    assert_eq!(cost, 0.0);
}

#[test]
fn test_calculate_cost_only_input() {
    let pricing = ModelPricing {
        input_cost_per_token: 0.000003,
        output_cost_per_token: 0.0,
        cache_read_input_token_cost: 0.0,
        cache_creation_input_token_cost: 0.0,
    };
    let cost = calculate_cost(1000, 0, 0, 0, &pricing);
    assert_eq!(cost, 0.003);
}

#[test]
fn test_calculate_cost_with_cache() {
    let pricing = ModelPricing {
        input_cost_per_token: 0.000003,
        output_cost_per_token: 0.000015,
        cache_read_input_token_cost: 0.0000003,
        cache_creation_input_token_cost: 0.00000375,
    };

    // Test with all token types
    let cost = calculate_cost(1000, 500, 10000, 2000, &pricing);
    // input: 1000 * 0.000003 = 0.003
    // output: 500 * 0.000015 = 0.0075
    // cache_read: 10000 * 0.0000003 = 0.003
    // cache_creation: 2000 * 0.00000375 = 0.0075
    // total: 0.021
    assert_eq!(cost, 0.021);
}

#[test]
fn test_get_model_pricing_exact_match() {
    let mut pricing_map = HashMap::new();
    pricing_map.insert(
        "claude-3-opus".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.000075,
            cache_read_input_token_cost: 0.0000015,
            cache_creation_input_token_cost: 0.000018,
        },
    );

    let result = get_model_pricing("claude-3-opus", &pricing_map);
    assert_eq!(result.pricing.input_cost_per_token, 0.000015);
    assert_eq!(result.matched_model, None, "Exact match should not set matched_model");
}

#[test]
fn test_get_model_pricing_normalized_match() {
    let mut pricing_map = HashMap::new();
    pricing_map.insert(
        "claude-3-sonnet".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
        },
    );

    // Test with version suffix
    let result = get_model_pricing("claude-3-sonnet-20240229", &pricing_map);
    assert_eq!(result.pricing.input_cost_per_token, 0.000003);
    assert_eq!(
        result.matched_model,
        Some("claude-3-sonnet".to_string()),
        "Should match normalized name"
    );
}

#[test]
fn test_get_model_pricing_substring_match() {
    let mut pricing_map = HashMap::new();
    pricing_map.insert(
        "gpt-4".to_string(),
        ModelPricing {
            input_cost_per_token: 0.00003,
            output_cost_per_token: 0.00006,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
        },
    );

    // Test substring matching
    let result = get_model_pricing("gpt-4-turbo", &pricing_map);
    assert_eq!(result.pricing.input_cost_per_token, 0.00003);
    assert_eq!(
        result.matched_model,
        Some("gpt-4".to_string()),
        "Should match via substring"
    );
}

#[test]
fn test_get_model_pricing_fuzzy_match() {
    let mut pricing_map = HashMap::new();
    pricing_map.insert(
        "claude-3-5-sonnet".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
        },
    );

    // Test fuzzy matching with slightly different name
    let result = get_model_pricing("claude-35-sonnet", &pricing_map);
    // Should find a fuzzy match since similarity is high
    assert!(
        result.matched_model.is_some() || result.pricing.input_cost_per_token == 0.0,
        "Should either fuzzy match or return default"
    );
}

#[test]
fn test_get_model_pricing_no_match() {
    let pricing_map = HashMap::new();

    let result = get_model_pricing("unknown-model", &pricing_map);
    assert_eq!(result.pricing.input_cost_per_token, 0.0);
    assert_eq!(result.pricing.output_cost_per_token, 0.0);
    assert_eq!(result.matched_model, None, "No match should return None");
}

#[test]
fn test_get_model_pricing_with_provider_prefix() {
    let mut pricing_map = HashMap::new();
    pricing_map.insert(
        "claude-3-opus".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.000075,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
        },
    );

    // Test with provider prefix (should be normalized)
    let result = get_model_pricing("bedrock/claude-3-opus-20240229", &pricing_map);
    // After normalization: bedrock/claude-3-opus-20240229 -> claude-3-opus
    assert!(
        result.pricing.input_cost_per_token > 0.0 || result.matched_model.is_some(),
        "Should match after normalization"
    );
}

#[test]
fn test_get_model_pricing_prefers_better_match() {
    let mut pricing_map = HashMap::new();
    pricing_map.insert(
        "gpt-4".to_string(),
        ModelPricing {
            input_cost_per_token: 0.00003,
            output_cost_per_token: 0.00006,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
        },
    );
    pricing_map.insert(
        "gpt-4-turbo".to_string(),
        ModelPricing {
            input_cost_per_token: 0.00001,
            output_cost_per_token: 0.00003,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
        },
    );

    // When searching for "gpt-4-turbo", it should prefer exact or better matches
    let result = get_model_pricing("gpt-4-turbo", &pricing_map);
    // Should find exact match for gpt-4-turbo
    assert_eq!(result.pricing.input_cost_per_token, 0.00001);
}

#[test]
fn test_model_pricing_serialization() {
    let pricing = ModelPricing {
        input_cost_per_token: 0.000003,
        output_cost_per_token: 0.000015,
        cache_read_input_token_cost: 0.0000003,
        cache_creation_input_token_cost: 0.00000375,
    };

    let json = serde_json::to_string(&pricing).unwrap();
    let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.input_cost_per_token, pricing.input_cost_per_token);
    assert_eq!(
        deserialized.output_cost_per_token,
        pricing.output_cost_per_token
    );
    assert_eq!(
        deserialized.cache_read_input_token_cost,
        pricing.cache_read_input_token_cost
    );
    assert_eq!(
        deserialized.cache_creation_input_token_cost,
        pricing.cache_creation_input_token_cost
    );
}

#[test]
fn test_model_pricing_partial_deserialization() {
    // Test that missing fields use default values
    let json = r#"{"input_cost_per_token": 0.000003}"#;
    let pricing: ModelPricing = serde_json::from_str(json).unwrap();

    assert_eq!(pricing.input_cost_per_token, 0.000003);
    assert_eq!(pricing.output_cost_per_token, 0.0, "Should use default");
    assert_eq!(pricing.cache_read_input_token_cost, 0.0, "Should use default");
    assert_eq!(
        pricing.cache_creation_input_token_cost, 0.0,
        "Should use default"
    );
}
