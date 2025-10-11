// Unit tests for pricing/cache.rs
//
// Tests pricing cache operations

use std::collections::HashMap;
use vibe_coding_tracker::pricing::ModelPricing;

#[test]
fn test_model_pricing_default() {
    // Test ModelPricing default values
    let pricing = ModelPricing::default();

    assert_eq!(pricing.input_cost_per_token, 0.0);
    assert_eq!(pricing.output_cost_per_token, 0.0);
    assert_eq!(pricing.cache_read_input_token_cost, 0.0);
    assert_eq!(pricing.cache_creation_input_token_cost, 0.0);
    assert_eq!(pricing.input_cost_per_token_above_200k_tokens, 0.0);
    assert_eq!(pricing.output_cost_per_token_above_200k_tokens, 0.0);
    assert_eq!(pricing.cache_read_input_token_cost_above_200k_tokens, 0.0);
    assert_eq!(
        pricing.cache_creation_input_token_cost_above_200k_tokens,
        0.0
    );
}

#[test]
fn test_model_pricing_serialization() {
    // Test ModelPricing can be serialized and deserialized
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
fn test_model_pricing_clone() {
    // Test ModelPricing can be cloned
    let pricing1 = ModelPricing {
        input_cost_per_token: 0.000001,
        output_cost_per_token: 0.000002,
        ..Default::default()
    };

    let pricing2 = pricing1;

    assert_eq!(pricing1.input_cost_per_token, pricing2.input_cost_per_token);
    assert_eq!(
        pricing1.output_cost_per_token,
        pricing2.output_cost_per_token
    );
}

#[test]
fn test_model_pricing_debug() {
    // Test ModelPricing debug formatting
    let pricing = ModelPricing::default();
    let debug_str = format!("{:?}", pricing);

    assert!(debug_str.contains("ModelPricing"));
}

#[test]
fn test_model_pricing_with_partial_data() {
    // Test deserializing with partial data (using #[serde(default)])
    let json = r#"{"input_cost_per_token": 0.000001}"#;
    let pricing: ModelPricing = serde_json::from_str(json).unwrap();

    assert_eq!(pricing.input_cost_per_token, 0.000001);
    assert_eq!(pricing.output_cost_per_token, 0.0); // Should use default
}

#[test]
fn test_model_pricing_empty_json() {
    // Test deserializing empty JSON object
    let json = "{}";
    let pricing: ModelPricing = serde_json::from_str(json).unwrap();

    assert_eq!(pricing.input_cost_per_token, 0.0);
    assert_eq!(pricing.output_cost_per_token, 0.0);
}

#[test]
fn test_model_pricing_hashmap_serialization() {
    // Test HashMap<String, ModelPricing> serialization
    let mut pricing_map = HashMap::new();
    pricing_map.insert(
        "gpt-4".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000030,
            output_cost_per_token: 0.000060,
            ..Default::default()
        },
    );
    pricing_map.insert(
        "claude-3".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.000075,
            ..Default::default()
        },
    );

    let json = serde_json::to_string(&pricing_map).unwrap();
    let deserialized: HashMap<String, ModelPricing> = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.len(), 2);
    assert!(deserialized.contains_key("gpt-4"));
    assert!(deserialized.contains_key("claude-3"));
}

#[test]
fn test_model_pricing_all_fields() {
    // Test all fields are properly serialized/deserialized
    let pricing = ModelPricing {
        input_cost_per_token: 1.0,
        output_cost_per_token: 2.0,
        cache_read_input_token_cost: 3.0,
        cache_creation_input_token_cost: 4.0,
        input_cost_per_token_above_200k_tokens: 5.0,
        output_cost_per_token_above_200k_tokens: 6.0,
        cache_read_input_token_cost_above_200k_tokens: 7.0,
        cache_creation_input_token_cost_above_200k_tokens: 8.0,
    };

    let json = serde_json::to_string(&pricing).unwrap();
    let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.input_cost_per_token, 1.0);
    assert_eq!(deserialized.output_cost_per_token, 2.0);
    assert_eq!(deserialized.cache_read_input_token_cost, 3.0);
    assert_eq!(deserialized.cache_creation_input_token_cost, 4.0);
    assert_eq!(deserialized.input_cost_per_token_above_200k_tokens, 5.0);
    assert_eq!(deserialized.output_cost_per_token_above_200k_tokens, 6.0);
    assert_eq!(
        deserialized.cache_read_input_token_cost_above_200k_tokens,
        7.0
    );
    assert_eq!(
        deserialized.cache_creation_input_token_cost_above_200k_tokens,
        8.0
    );
}

#[test]
fn test_model_pricing_zero_values() {
    // Test with all zero values
    let pricing = ModelPricing::default();
    let json = serde_json::to_string(&pricing).unwrap();
    let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

    // All should be zero
    assert_eq!(deserialized.input_cost_per_token, 0.0);
    assert_eq!(deserialized.output_cost_per_token, 0.0);
}

#[test]
fn test_model_pricing_negative_values() {
    // Test that negative values are preserved (although not realistic)
    let pricing = ModelPricing {
        input_cost_per_token: -0.000001,
        output_cost_per_token: -0.000002,
        ..Default::default()
    };

    let json = serde_json::to_string(&pricing).unwrap();
    let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.input_cost_per_token, -0.000001);
    assert_eq!(deserialized.output_cost_per_token, -0.000002);
}

#[test]
fn test_model_pricing_very_small_values() {
    // Test with very small values (scientific notation)
    let pricing = ModelPricing {
        input_cost_per_token: 1e-10,
        output_cost_per_token: 1e-15,
        ..Default::default()
    };

    let json = serde_json::to_string(&pricing).unwrap();
    let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

    assert!((deserialized.input_cost_per_token - 1e-10).abs() < 1e-20);
    assert!((deserialized.output_cost_per_token - 1e-15).abs() < 1e-25);
}

#[test]
fn test_model_pricing_large_values() {
    // Test with large values
    let pricing = ModelPricing {
        input_cost_per_token: 1000000.0,
        output_cost_per_token: 9999999.99,
        ..Default::default()
    };

    let json = serde_json::to_string(&pricing).unwrap();
    let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.input_cost_per_token, 1000000.0);
    assert_eq!(deserialized.output_cost_per_token, 9999999.99);
}
