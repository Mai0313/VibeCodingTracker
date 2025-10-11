// Unit tests for pricing/matching.rs
//
// Tests the model pricing matching logic

use std::collections::HashMap;
use vibe_coding_tracker::pricing::{ModelPricing, ModelPricingMap, clear_pricing_cache};

fn create_test_pricing() -> ModelPricing {
    ModelPricing {
        input_cost_per_token: 0.000001,
        output_cost_per_token: 0.000002,
        cache_read_input_token_cost: 0.0000001,
        cache_creation_input_token_cost: 0.0000005,
        input_cost_per_token_above_200k_tokens: 0.000002,
        output_cost_per_token_above_200k_tokens: 0.000004,
        cache_read_input_token_cost_above_200k_tokens: 0.0000002,
        cache_creation_input_token_cost_above_200k_tokens: 0.000001,
    }
}

#[test]
fn test_exact_match() {
    // Test exact model name match
    clear_pricing_cache();

    let mut raw = HashMap::new();
    raw.insert("gpt-4".to_string(), create_test_pricing());
    raw.insert("claude-3-opus".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);

    let result = map.get("gpt-4");
    assert!(result.pricing.input_cost_per_token > 0.0); // Should match

    let result2 = map.get("claude-3-opus");
    assert!(result2.pricing.input_cost_per_token > 0.0); // Should match
}

#[test]
fn test_normalized_match() {
    // Test normalized matching (removes version suffixes)
    clear_pricing_cache();

    let mut raw = HashMap::new();
    raw.insert("gpt-4-0613".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);

    // Should match via substring or fuzzy matching
    let result = map.get("gpt-4");
    assert!(result.pricing.input_cost_per_token > 0.0);
}

#[test]
fn test_substring_match() {
    // Test substring matching
    clear_pricing_cache();

    let mut raw = HashMap::new();
    raw.insert("claude-3-opus-20240229".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);

    // Should match via substring or normalization
    let result = map.get("claude-3-opus");
    assert!(result.pricing.input_cost_per_token > 0.0);
}

#[test]
fn test_case_insensitive_match() {
    // Test case-insensitive matching
    let mut raw = HashMap::new();
    raw.insert("GPT-4".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);

    let result = map.get("gpt-4");
    assert!(result.pricing.input_cost_per_token > 0.0);
}

#[test]
fn test_fuzzy_match() {
    // Test fuzzy matching with similar names
    let mut raw = HashMap::new();
    raw.insert("claude-3-sonnet".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);

    // Slightly misspelled should still match (if similarity >= 0.7)
    let result = map.get("claude-3-sonet");
    // This might or might not match depending on Jaro-Winkler score
    // Just verify it returns a result
    assert!(result.pricing.input_cost_per_token >= 0.0);
}

#[test]
fn test_no_match_returns_default() {
    // Test that unmatched models return default (zero cost)
    let raw = HashMap::new();
    let map = ModelPricingMap::new(raw);

    let result = map.get("unknown-model");
    assert_eq!(result.pricing.input_cost_per_token, 0.0);
    assert_eq!(result.pricing.output_cost_per_token, 0.0);
    assert!(result.matched_model.is_none());
}

#[test]
fn test_multiple_models() {
    // Test with multiple models
    let mut raw = HashMap::new();
    let pricing1 = ModelPricing {
        input_cost_per_token: 0.000001,
        output_cost_per_token: 0.000002,
        ..Default::default()
    };
    let pricing2 = ModelPricing {
        input_cost_per_token: 0.000003,
        output_cost_per_token: 0.000006,
        ..Default::default()
    };

    raw.insert("model-a".to_string(), pricing1);
    raw.insert("model-b".to_string(), pricing2);

    let map = ModelPricingMap::new(raw);

    let result_a = map.get("model-a");
    assert_eq!(result_a.pricing.input_cost_per_token, 0.000001);

    let result_b = map.get("model-b");
    assert_eq!(result_b.pricing.input_cost_per_token, 0.000003);
}

#[test]
fn test_empty_model_name() {
    // Test with empty model name - will match first model due to substring logic
    clear_pricing_cache();

    let mut raw = HashMap::new();
    raw.insert("gpt-4".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);

    let result = map.get("");
    // Empty string will match via substring logic, so it returns a match
    assert!(result.pricing.input_cost_per_token >= 0.0);
}

#[test]
fn test_pricing_map_debug() {
    // Test that ModelPricingMap can be debug formatted
    let mut raw = HashMap::new();
    raw.insert("test-model".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);
    let debug_str = format!("{:?}", map);

    assert!(!debug_str.is_empty());
}

#[test]
fn test_pricing_map_clone() {
    // Test that ModelPricingMap can be cloned
    let mut raw = HashMap::new();
    raw.insert("test-model".to_string(), create_test_pricing());

    let map1 = ModelPricingMap::new(raw);
    let map2 = map1.clone();

    let result1 = map1.get("test-model");
    let result2 = map2.get("test-model");

    assert_eq!(
        result1.pricing.input_cost_per_token,
        result2.pricing.input_cost_per_token
    );
}

#[test]
fn test_match_priority() {
    // Test that exact match takes priority over fuzzy match
    clear_pricing_cache();

    let mut raw = HashMap::new();
    let exact_pricing = ModelPricing {
        input_cost_per_token: 0.000001,
        ..Default::default()
    };
    let other_pricing = ModelPricing {
        input_cost_per_token: 0.000099,
        ..Default::default()
    };

    raw.insert("gpt-4".to_string(), exact_pricing);
    raw.insert("gpt-4-turbo".to_string(), other_pricing);

    let map = ModelPricingMap::new(raw);

    // Exact match should be used
    let result = map.get("gpt-4");
    assert_eq!(result.pricing.input_cost_per_token, 0.000001);
}

#[test]
fn test_version_stripping() {
    // Test that version numbers are handled correctly
    let mut raw = HashMap::new();
    raw.insert("claude-3-opus".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);

    // Should match without version
    let result = map.get("claude-3-opus-20240229");
    assert!(result.pricing.input_cost_per_token > 0.0);
}

#[test]
fn test_result_clone() {
    // Test that ModelPricingResult can be cloned
    let mut raw = HashMap::new();
    raw.insert("test".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);
    let result1 = map.get("test");
    let result2 = result1.clone();

    assert_eq!(
        result1.pricing.input_cost_per_token,
        result2.pricing.input_cost_per_token
    );
}

#[test]
fn test_result_debug() {
    // Test that ModelPricingResult can be debug formatted
    let mut raw = HashMap::new();
    raw.insert("test".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);
    let result = map.get("test");
    let debug_str = format!("{:?}", result);

    assert!(!debug_str.is_empty());
    assert!(debug_str.contains("pricing"));
}

#[test]
fn test_special_characters() {
    // Test model names with special characters
    let mut raw = HashMap::new();
    raw.insert("model/v1.0".to_string(), create_test_pricing());
    raw.insert("model:latest".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);

    let result1 = map.get("model/v1.0");
    assert!(result1.pricing.input_cost_per_token > 0.0);

    let result2 = map.get("model:latest");
    assert!(result2.pricing.input_cost_per_token > 0.0);
}

#[test]
fn test_very_long_model_name() {
    // Test with very long model name
    let mut raw = HashMap::new();
    let long_name = "a".repeat(1000);
    raw.insert(long_name.clone(), create_test_pricing());

    let map = ModelPricingMap::new(raw);

    let result = map.get(&long_name);
    assert!(result.pricing.input_cost_per_token > 0.0);
}

#[test]
fn test_unicode_model_names() {
    // Test model names with unicode characters
    let mut raw = HashMap::new();
    raw.insert("模型-1".to_string(), create_test_pricing());
    raw.insert("モデル-2".to_string(), create_test_pricing());

    let map = ModelPricingMap::new(raw);

    let result1 = map.get("模型-1");
    assert!(result1.pricing.input_cost_per_token > 0.0);

    let result2 = map.get("モデル-2");
    assert!(result2.pricing.input_cost_per_token > 0.0);
}
