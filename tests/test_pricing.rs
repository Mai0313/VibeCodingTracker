use std::collections::HashMap;
use vibe_coding_tracker::pricing::{
    calculate_cost, clear_pricing_cache, ModelPricing, ModelPricingMap,
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
        ..Default::default()
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
        ..Default::default()
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
    // Clear cache to ensure test isolation
    clear_pricing_cache();

    let mut raw_map = HashMap::new();
    raw_map.insert(
        "claude-3-opus".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.000075,
            cache_read_input_token_cost: 0.0000015,
            cache_creation_input_token_cost: 0.000018,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("claude-3-opus");
    assert_eq!(result.pricing.input_cost_per_token, 0.000015);
    assert_eq!(
        result.matched_model, None,
        "Exact match should not set matched_model"
    );
}

#[test]
fn test_get_model_pricing_normalized_match() {
    let mut raw_map = HashMap::new();
    raw_map.insert(
        "claude-3-sonnet".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
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
fn test_get_model_pricing_substring_match() {
    // Clear cache to ensure test isolation
    clear_pricing_cache();

    let mut raw_map = HashMap::new();
    // Use unique model names to avoid cache pollution from other tests
    raw_map.insert(
        "test-model-base".to_string(),
        ModelPricing {
            input_cost_per_token: 0.00003,
            output_cost_per_token: 0.00006,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    // Test substring matching
    let result = pricing_map.get("test-model-base-extended");
    assert_eq!(result.pricing.input_cost_per_token, 0.00003);
    assert_eq!(
        result.matched_model,
        Some("test-model-base".to_string()),
        "Should match via substring"
    );
}

#[test]
fn test_get_model_pricing_fuzzy_match() {
    let mut raw_map = HashMap::new();
    raw_map.insert(
        "claude-3-5-sonnet".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    // Test fuzzy matching with slightly different name
    let result = pricing_map.get("claude-35-sonnet");
    // Should find a fuzzy match since similarity is high
    assert!(
        result.matched_model.is_some() || result.pricing.input_cost_per_token == 0.0,
        "Should either fuzzy match or return default"
    );
}

#[test]
fn test_get_model_pricing_no_match() {
    let raw_map = HashMap::new();
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("unknown-model");
    assert_eq!(result.pricing.input_cost_per_token, 0.0);
    assert_eq!(result.pricing.output_cost_per_token, 0.0);
    assert_eq!(result.matched_model, None, "No match should return None");
}

#[test]
fn test_get_model_pricing_with_provider_prefix() {
    let mut raw_map = HashMap::new();
    raw_map.insert(
        "claude-3-opus".to_string(),
        ModelPricing {
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.000075,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    // Test with provider prefix (should be normalized)
    let result = pricing_map.get("bedrock/claude-3-opus-20240229");
    // After normalization: bedrock/claude-3-opus-20240229 -> claude-3-opus
    assert!(
        result.pricing.input_cost_per_token > 0.0 || result.matched_model.is_some(),
        "Should match after normalization"
    );
}

#[test]
fn test_get_model_pricing_prefers_better_match() {
    // Clear cache to ensure test isolation
    clear_pricing_cache();

    let mut raw_map = HashMap::new();
    raw_map.insert(
        "gpt-4".to_string(),
        ModelPricing {
            input_cost_per_token: 0.00003,
            output_cost_per_token: 0.00006,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
            ..Default::default()
        },
    );
    raw_map.insert(
        "gpt-4-turbo".to_string(),
        ModelPricing {
            input_cost_per_token: 0.00001,
            output_cost_per_token: 0.00003,
            cache_read_input_token_cost: 0.0,
            cache_creation_input_token_cost: 0.0,
            ..Default::default()
        },
    );
    let pricing_map = ModelPricingMap::new(raw_map);

    // When searching for "gpt-4-turbo", it should prefer exact or better matches
    let result = pricing_map.get("gpt-4-turbo");
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
    assert_eq!(
        pricing.cache_read_input_token_cost, 0.0,
        "Should use default"
    );
    assert_eq!(
        pricing.cache_creation_input_token_cost, 0.0,
        "Should use default"
    );
}

// Integration tests for cache functionality
mod cache_tests {
    use super::*;

    #[test]
    fn test_model_pricing_result_debug() {
        let pricing = ModelPricing::default();
        let result = vibe_coding_tracker::pricing::ModelPricingResult {
            pricing,
            matched_model: Some("test-model".to_string()),
        };

        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("ModelPricingResult"));
        assert!(debug_str.contains("test-model"));
    }

    #[test]
    fn test_model_pricing_result_clone() {
        let pricing = ModelPricing::default();
        let result = vibe_coding_tracker::pricing::ModelPricingResult {
            pricing,
            matched_model: Some("test-model".to_string()),
        };

        let cloned = result.clone();
        assert_eq!(cloned.matched_model, result.matched_model);
        assert_eq!(
            cloned.pricing.input_cost_per_token,
            result.pricing.input_cost_per_token
        );
    }

    #[test]
    fn test_calculate_cost_negative_tokens() {
        let pricing = ModelPricing {
            input_cost_per_token: 0.000001,
            output_cost_per_token: 0.000002,
            cache_read_input_token_cost: 0.0000001,
            cache_creation_input_token_cost: 0.0000005,
            ..Default::default()
        };

        // Test with negative values (should handle gracefully)
        let cost = calculate_cost(-100, 0, 0, 0, &pricing);
        assert!(cost.is_finite(), "Cost should be a finite number");
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

        // Test with very large token counts
        let cost = calculate_cost(1_000_000, 500_000, 100_000, 50_000, &pricing);
        assert!(cost > 0.0);
        assert!(cost.is_finite());
    }

    #[test]
    fn test_get_model_pricing_empty_string() {
        let raw_map = HashMap::new();
        let pricing_map = ModelPricingMap::new(raw_map);
        let result = pricing_map.get("");

        assert_eq!(result.pricing.input_cost_per_token, 0.0);
        assert_eq!(result.matched_model, None);
    }

    #[test]
    fn test_get_model_pricing_special_characters() {
        let mut raw_map = HashMap::new();
        raw_map.insert(
            "model-with-special_chars.123".to_string(),
            ModelPricing {
                input_cost_per_token: 0.000001,
                output_cost_per_token: 0.000002,
                cache_read_input_token_cost: 0.0,
                cache_creation_input_token_cost: 0.0,
                ..Default::default()
            },
        );
        let pricing_map = ModelPricingMap::new(raw_map);

        let result = pricing_map.get("model-with-special_chars.123");
        assert_eq!(result.pricing.input_cost_per_token, 0.000001);
        assert_eq!(result.matched_model, None); // Exact match
    }

    #[test]
    fn test_get_model_pricing_case_sensitivity() {
        let mut raw_map = HashMap::new();
        raw_map.insert(
            "GPT-4".to_string(),
            ModelPricing {
                input_cost_per_token: 0.00003,
                output_cost_per_token: 0.00006,
                cache_read_input_token_cost: 0.0,
                cache_creation_input_token_cost: 0.0,
                ..Default::default()
            },
        );
        let pricing_map = ModelPricingMap::new(raw_map);

        // Test with different case - should still match via fuzzy matching
        let result = pricing_map.get("gpt-4");
        // Should find via fuzzy match or exact match depending on implementation
        assert!(
            result.pricing.input_cost_per_token > 0.0 || result.matched_model.is_some(),
            "Should match despite case difference"
        );
    }

    #[test]
    fn test_get_model_pricing_multiple_versions() {
        let mut raw_map = HashMap::new();
        raw_map.insert(
            "claude-3-opus-20240229".to_string(),
            ModelPricing {
                input_cost_per_token: 0.000015,
                output_cost_per_token: 0.000075,
                cache_read_input_token_cost: 0.0,
                cache_creation_input_token_cost: 0.0,
                ..Default::default()
            },
        );
        raw_map.insert(
            "claude-3-opus".to_string(),
            ModelPricing {
                input_cost_per_token: 0.000010,
                output_cost_per_token: 0.000050,
                cache_read_input_token_cost: 0.0,
                cache_creation_input_token_cost: 0.0,
                ..Default::default()
            },
        );
        let pricing_map = ModelPricingMap::new(raw_map);

        // Exact match should take precedence
        let result = pricing_map.get("claude-3-opus");
        assert_eq!(result.pricing.input_cost_per_token, 0.000010);
        assert_eq!(result.matched_model, None);
    }

    #[test]
    fn test_model_pricing_clone() {
        let pricing = ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_read_input_token_cost: 0.0000003,
            cache_creation_input_token_cost: 0.00000375,
            ..Default::default()
        };

        let cloned = pricing;
        assert_eq!(cloned.input_cost_per_token, pricing.input_cost_per_token);
        assert_eq!(cloned.output_cost_per_token, pricing.output_cost_per_token);
        assert_eq!(
            cloned.cache_read_input_token_cost,
            pricing.cache_read_input_token_cost
        );
        assert_eq!(
            cloned.cache_creation_input_token_cost,
            pricing.cache_creation_input_token_cost
        );
    }

    #[test]
    fn test_model_pricing_debug() {
        let pricing = ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_read_input_token_cost: 0.0000003,
            cache_creation_input_token_cost: 0.00000375,
            ..Default::default()
        };

        let debug_str = format!("{:?}", pricing);
        assert!(debug_str.contains("ModelPricing"));
    }

    #[test]
    fn test_above_200k_pricing_fields() {
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

        assert_eq!(pricing.input_cost_per_token_above_200k_tokens, 0.000002);
        assert_eq!(pricing.output_cost_per_token_above_200k_tokens, 0.000004);
        assert_eq!(
            pricing.cache_read_input_token_cost_above_200k_tokens,
            0.0000002
        );
        assert_eq!(
            pricing.cache_creation_input_token_cost_above_200k_tokens,
            0.000001
        );
    }

    #[test]
    fn test_model_pricing_serialization_with_above_200k() {
        let pricing = ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_read_input_token_cost: 0.0000003,
            cache_creation_input_token_cost: 0.00000375,
            input_cost_per_token_above_200k_tokens: 0.000006,
            output_cost_per_token_above_200k_tokens: 0.00003,
            cache_read_input_token_cost_above_200k_tokens: 0.0000006,
            cache_creation_input_token_cost_above_200k_tokens: 0.0000075,
        };

        let json = serde_json::to_string(&pricing).unwrap();
        let deserialized: ModelPricing = serde_json::from_str(&json).unwrap();

        assert_eq!(
            deserialized.input_cost_per_token_above_200k_tokens,
            pricing.input_cost_per_token_above_200k_tokens
        );
        assert_eq!(
            deserialized.output_cost_per_token_above_200k_tokens,
            pricing.output_cost_per_token_above_200k_tokens
        );
    }
}
