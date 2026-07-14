// Integration tests for pricing system functionality
//
// The fetch/cache path is exercised against a local httpmock server pointed at
// a temp cache dir, so no real LiteLLM request is ever made and the real
// `~/.vct` is never touched. The rest are pure lookup / cost-math tests.

use httpmock::prelude::*;
use serde_json::json;
use std::collections::HashMap;
use tempfile::TempDir;
use vibe_coding_tracker::pricing::{
    ModelPricing, ModelPricingMap, ThresholdTier, TierRange, calculate_cost, clear_pricing_cache,
    fetch_model_pricing_with, normalize_model_name,
};
use vibe_coding_tracker::utils::get_pricing_cache_path_in;

fn pricing_cache_date() -> String {
    chrono::Utc::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string()
}

fn pricing_at(input_cost_per_token: f64) -> ModelPricing {
    ModelPricing {
        input_cost_per_token,
        ..Default::default()
    }
}

#[test]
fn fetch_pricing_from_mock_parses_and_caches() {
    clear_pricing_cache();
    let server = MockServer::start();
    let endpoint = server.mock(|when, then| {
        when.method(GET).path("/pricing");
        then.status(200).json_body(json!({
            "claude-sonnet-4-6": {
                "input_cost_per_token": 3e-6,
                "output_cost_per_token": 1.5e-5,
                "input_cost_per_token_above_200k_tokens": 6e-6,
                "max_input_tokens": 200000,
                "litellm_provider": "anthropic"
            },
            "gpt-5": { "input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6 }
        }));
    });
    let cache_dir = TempDir::new().unwrap();

    let map = fetch_model_pricing_with(&server.url("/pricing"), cache_dir.path())
        .expect("fetch pricing from mock server");

    endpoint.assert(); // the mock endpoint was reached

    let sonnet = map.get("claude-sonnet-4-6");
    assert_eq!(sonnet.pricing.input_cost_per_token, 3e-6);
    // The raw `*_above_200k_tokens` key must be rebuilt into a threshold tier.
    assert_eq!(sonnet.pricing.tiers.len(), 1);
    assert_eq!(sonnet.pricing.tiers[0].threshold_tokens, 200_000);
    assert_eq!(map.get("gpt-5").pricing.input_cost_per_token, 1e-6);

    // The cache lands in the temp dir — never the real `~/.vct`.
    let cache_file = get_pricing_cache_path_in(cache_dir.path(), &pricing_cache_date());
    assert!(
        cache_file.exists(),
        "pricing cache should be written to the temp cache dir"
    );
}

#[test]
fn fetch_pricing_prefers_cache_over_network() {
    clear_pricing_cache();
    let server = MockServer::start();
    // If today's cache is honored, this endpoint is never hit; it 500s so a
    // regression that reached the network would fail loudly instead of silently.
    let endpoint = server.mock(|when, then| {
        when.method(GET).path("/pricing");
        then.status(500);
    });
    let cache_dir = TempDir::new().unwrap();

    // Pre-seed today's cache with a cost-fields JSON (current, non-legacy schema).
    let cache_file = get_pricing_cache_path_in(cache_dir.path(), &pricing_cache_date());
    std::fs::write(
        &cache_file,
        serde_json::to_string(&json!({
            "cached-model": { "input_cost_per_token": 9e-6 }
        }))
        .unwrap(),
    )
    .unwrap();

    let map = fetch_model_pricing_with(&server.url("/pricing"), cache_dir.path())
        .expect("cache hit should succeed without a request");

    assert_eq!(
        endpoint.calls(),
        0,
        "a valid today-cache must short-circuit before any network request"
    );
    assert_eq!(map.get("cached-model").pricing.input_cost_per_token, 9e-6);
}

#[test]
fn fetch_pricing_rejects_http_errors_without_caching() {
    for status in [429, 500] {
        let server = MockServer::start();
        let endpoint = server.mock(|when, then| {
            when.method(GET).path("/pricing");
            then.status(status).json_body(json!({
                "must-not-cache": { "input_cost_per_token": 1e-6 }
            }));
        });
        let cache_dir = TempDir::new().unwrap();

        let error = fetch_model_pricing_with(&server.url("/pricing"), cache_dir.path())
            .expect_err("non-success responses must fail before parsing or caching");

        assert!(error.to_string().contains(&status.to_string()));
        endpoint.assert();
        let cache_file = get_pricing_cache_path_in(cache_dir.path(), &pricing_cache_date());
        assert!(!cache_file.exists());
    }
}

#[test]
fn fetch_pricing_rejects_unpriced_payloads_without_caching() {
    let payloads = [
        json!([]),
        json!({}),
        json!({ "model": { "input_cost_per_token": 0.0 } }),
        json!({ "model": { "input_cost_per_token": "unknown" } }),
    ];

    for payload in payloads {
        let server = MockServer::start();
        let endpoint = server.mock(|when, then| {
            when.method(GET).path("/pricing");
            then.status(200).json_body(payload.clone());
        });
        let cache_dir = TempDir::new().unwrap();

        fetch_model_pricing_with(&server.url("/pricing"), cache_dir.path())
            .expect_err("invalid or unpriced payloads must not be cached");

        endpoint.assert();
        let cache_file = get_pricing_cache_path_in(cache_dir.path(), &pricing_cache_date());
        assert!(!cache_file.exists());
    }
}

#[test]
fn fetch_pricing_rejects_negative_rates_without_caching() {
    let server = MockServer::start();
    let endpoint = server.mock(|when, then| {
        when.method(GET).path("/pricing");
        then.status(200).json_body(json!({
            "valid-model": { "input_cost_per_token": 1e-6 },
            "invalid-model": { "output_cost_per_token": -1e-6 }
        }));
    });
    let cache_dir = TempDir::new().unwrap();

    let error = fetch_model_pricing_with(&server.url("/pricing"), cache_dir.path())
        .expect_err("a negative price must reject the entire payload");

    assert!(error.to_string().contains("negative or non-finite"));
    endpoint.assert();
    let cache_file = get_pricing_cache_path_in(cache_dir.path(), &pricing_cache_date());
    assert!(!cache_file.exists());
}

#[test]
fn fetch_pricing_backs_off_after_failure() {
    let server = MockServer::start();
    let endpoint = server.mock(|when, then| {
        when.method(GET).path("/pricing");
        then.status(500);
    });
    let cache_dir = TempDir::new().unwrap();
    let url = server.url("/pricing");

    fetch_model_pricing_with(&url, cache_dir.path()).expect_err("first request should fail");
    let retry = fetch_model_pricing_with(&url, cache_dir.path())
        .expect_err("an immediate retry should be backed off");

    assert!(retry.to_string().contains("failure backoff"));
    assert_eq!(endpoint.calls(), 1);
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
fn model_name_normalization_only_strips_valid_suffixes() {
    assert_eq!(normalize_model_name("model-12345678"), "model");
    assert_eq!(normalize_model_name("model-2024abcd"), "model-2024abcd");
    assert_eq!(
        normalize_model_name("model-１２３４５６７８"),
        "model-１２３４５６７８"
    );
    assert_eq!(normalize_model_name("model-v2"), "model");
    assert_eq!(normalize_model_name("model-v1.0"), "model");
    assert_eq!(normalize_model_name("model-v2-20240101"), "model");
    assert_eq!(normalize_model_name("model-20240101-v2"), "model");
    assert_eq!(normalize_model_name("model-vision"), "model-vision");
    assert_eq!(normalize_model_name("model-v1..0"), "model-v1..0");
    assert_eq!(normalize_model_name("model-v2beta"), "model-v2beta");
}

#[test]
fn normalized_collisions_prefer_provider_then_unprefixed_base() {
    let mut raw_map = HashMap::new();
    raw_map.insert("openai/model".to_string(), pricing_at(1.0));
    raw_map.insert("openai/model-v1".to_string(), pricing_at(4.0));
    raw_map.insert("azure/model-v1".to_string(), pricing_at(2.0));
    raw_map.insert("model".to_string(), pricing_at(3.0));
    let pricing_map = ModelPricingMap::new(raw_map);

    let provider_match = pricing_map.get("openai/model-v2");
    assert_eq!(provider_match.pricing.input_cost_per_token, 1.0);
    assert_eq!(
        provider_match.matched_model.as_deref(),
        Some("openai/model")
    );

    let base_match = pricing_map.get("bedrock/model-v2");
    assert_eq!(base_match.pricing.input_cost_per_token, 3.0);
    assert_eq!(base_match.matched_model.as_deref(), Some("model"));
}

#[test]
fn normalized_collisions_prefer_unprefixed_base_to_versioned_provider_candidate() {
    let mut raw_map = HashMap::new();
    raw_map.insert("openai/model-20240101".to_string(), pricing_at(1.0));
    raw_map.insert("model".to_string(), pricing_at(2.0));
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("openai/model-20240714");
    assert_eq!(result.pricing.input_cost_per_token, 2.0);
    assert_eq!(result.matched_model.as_deref(), Some("model"));
}

#[test]
fn normalized_collision_fallback_is_deterministic() {
    let mut raw_map = HashMap::new();
    raw_map.insert("zeta/model-v1".to_string(), pricing_at(1.0));
    raw_map.insert("alpha/model-v1".to_string(), pricing_at(2.0));
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("other/model-v2");
    assert_eq!(result.pricing.input_cost_per_token, 2.0);
    assert_eq!(result.matched_model.as_deref(), Some("alpha/model-v1"));
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
fn substring_matching_prefers_the_most_specific_model() {
    let mut raw_map = HashMap::new();
    raw_map.insert("gpt-4".to_string(), pricing_at(4.0));
    raw_map.insert("gpt-4o".to_string(), pricing_at(4.1));
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("openai/gpt-4o-mini");
    assert_eq!(result.pricing.input_cost_per_token, 4.1);
    assert_eq!(result.matched_model.as_deref(), Some("gpt-4o"));
}

#[test]
fn substring_specificity_ignores_provider_prefix_length() {
    let mut raw_map = HashMap::new();
    raw_map.insert("openai/gpt-4".to_string(), pricing_at(4.0));
    raw_map.insert("gpt-4o".to_string(), pricing_at(4.1));
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("openai/gpt-4o-mini");
    assert_eq!(result.pricing.input_cost_per_token, 4.1);
    assert_eq!(result.matched_model.as_deref(), Some("gpt-4o"));
}

#[test]
fn pricing_lookup_caches_are_isolated_per_map() {
    let mut first = HashMap::new();
    first.insert("shared-model".to_string(), pricing_at(1.0));
    let mut second = HashMap::new();
    second.insert("shared-model".to_string(), pricing_at(2.0));

    let first = ModelPricingMap::new(first);
    let second = ModelPricingMap::new(second);

    assert_eq!(first.get("shared-model").pricing.input_cost_per_token, 1.0);
    assert_eq!(second.get("shared-model").pricing.input_cost_per_token, 2.0);
    assert_eq!(first.get("shared-model").pricing.input_cost_per_token, 1.0);
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

    // 2000 cache_creation tokens at default (5 minute) TTL, no reasoning tokens.
    let cost = calculate_cost(1000, 500, 0, 10000, 2000, 0, &pricing);
    // input: 1000 * 0.000003 = 0.003
    // output: 500 * 0.000015 = 0.0075
    // cache_read: 10000 * 0.0000003 = 0.003
    // cache_creation (5m): 2000 * 0.00000375 = 0.0075
    // total: 0.021
    assert_eq!(cost, 0.021);
}

#[test]
fn test_calculate_cost_zero_tokens() {
    let pricing = ModelPricing::default();
    let cost = calculate_cost(0, 0, 0, 0, 0, 0, &pricing);
    assert_eq!(cost, 0.0);
}

#[test]
fn test_calculate_cost_no_cache() {
    let pricing = ModelPricing {
        input_cost_per_token: 0.000003,
        output_cost_per_token: 0.000015,
        ..Default::default()
    };

    let cost = calculate_cost(1000, 500, 0, 0, 0, 0, &pricing);
    // input: 1000 * 0.000003 = 0.003
    // output: 500 * 0.000015 = 0.0075
    // total: 0.0105
    assert_eq!(cost, 0.0105);
}

#[test]
fn test_calculate_cost_large_numbers() {
    // Flat pricing (no tiers): every request uses base prices regardless of size.
    let pricing = ModelPricing {
        input_cost_per_token: 0.000001,
        output_cost_per_token: 0.000002,
        cache_read_input_token_cost: 0.0000001,
        cache_creation_input_token_cost: 0.0000005,
        ..Default::default()
    };

    let cost = calculate_cost(1_000_000, 500_000, 0, 100_000, 50_000, 0, &pricing);
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
    // The lookup match-cache is process-global; clear it so a prior test's
    // (possibly offline / empty) result for these names doesn't bleed in.
    clear_pricing_cache();

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
fn test_pricing_above_200k_tokens_via_tier() {
    let pricing = ModelPricing {
        input_cost_per_token: 0.000001,
        output_cost_per_token: 0.000002,
        tiers: vec![ThresholdTier {
            threshold_tokens: 200_000,
            input_cost_per_token: 0.000002,
            output_cost_per_token: 0.000004,
            ..Default::default()
        }],
        ..Default::default()
    };

    // Below 200K: base prices.
    let below = calculate_cost(100_000, 50_000, 0, 0, 0, 0, &pricing);
    assert_eq!(below, 100_000.0 * 0.000001 + 50_000.0 * 0.000002);

    // Above 200K: tier prices for all tokens.
    let above = calculate_cost(300_000, 50_000, 0, 0, 0, 0, &pricing);
    assert_eq!(above, 300_000.0 * 0.000002 + 50_000.0 * 0.000004);
}

#[test]
fn test_pricing_range_based() {
    // Mimics Qwen-style range-based pricing selected by input_tokens.
    let pricing = ModelPricing {
        input_cost_per_token: 999.0, // Should be ignored — ranges takes priority.
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
        ]),
        ..Default::default()
    };

    let low = calculate_cost(10_000, 1000, 0, 0, 0, 0, &pricing);
    assert_eq!(low, 10_000.0 * 0.000001 + 1000.0 * 0.000005);

    let high = calculate_cost(100_000, 1000, 0, 0, 0, 0, &pricing);
    assert_eq!(high, 100_000.0 * 0.0000018 + 1000.0 * 0.000009);
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
