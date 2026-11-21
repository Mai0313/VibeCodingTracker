// Integration tests for pricing system functionality.
//
// The fetch/cache path runs against a local `httpmock` server pointed at a temp cache dir, so no real LiteLLM request is made and `~/.vct` is never touched. The rest are pure lookup and cost-math tests.

use httpmock::prelude::*;
use serde_json::json;
use std::collections::HashMap;
use tempfile::TempDir;
use vct_core::pricing::{
    ModelPricing, ModelPricingMap, ThresholdTier, TierRange, calculate_cost, clear_pricing_cache,
    fetch_model_pricing_with, normalize_model_name,
};
use vct_core::utils::{TierSlice, get_pricing_cache_path_in};

/// Builds a `TokenCounts` with `cache_creation` set to `cc_5m + cc_1h`.
fn tc(
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cc_5m: i64,
    cc_1h: i64,
) -> vct_core::utils::TokenCounts {
    vct_core::utils::TokenCounts {
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

    endpoint.assert();

    let sonnet = map.get("claude-sonnet-4-6");
    assert_eq!(sonnet.pricing.input_cost_per_token, 3e-6);
    // The raw `*_above_200k_tokens` key must be rebuilt into a threshold tier.
    assert_eq!(sonnet.pricing.tiers.len(), 1);
    assert_eq!(sonnet.pricing.tiers[0].threshold_tokens, 200_000);
    assert_eq!(map.get("gpt-5").pricing.input_cost_per_token, 1e-6);

    let cache_file = get_pricing_cache_path_in(cache_dir.path(), &pricing_cache_date());
    assert!(
        cache_file.exists(),
        "pricing cache should be written to the temp cache dir"
    );
    let cache_text = std::fs::read_to_string(&cache_file).expect("read written cache");
    assert!(
        !cache_text.contains('\n'),
        "pricing cache should be written as compact JSON"
    );
}

#[test]
fn fetch_pricing_refetches_when_cache_is_legacy_or_has_negative_rate() {
    clear_pricing_cache();
    let bad_caches = [
        json!({
            "stale": {
                "input_cost_per_token": 1e-6,
                "tiers": [{ "threshold_tokens": 200_000, "input_cost_per_token": 2e-6 }]
            }
        }),
        json!({
            "good": { "input_cost_per_token": 1e-6 },
            "bad": { "output_cost_per_token": -1e-6 }
        }),
        json!([]),
    ];

    for bad_cache in bad_caches {
        let server = MockServer::start();
        let endpoint = server.mock(|when, then| {
            when.method(GET).path("/pricing");
            then.status(200).json_body(json!({
                "fresh-model": { "input_cost_per_token": 4e-6 }
            }));
        });
        let cache_dir = TempDir::new().unwrap();
        let cache_file = get_pricing_cache_path_in(cache_dir.path(), &pricing_cache_date());
        std::fs::write(&cache_file, serde_json::to_string(&bad_cache).unwrap()).unwrap();

        let map = fetch_model_pricing_with(&server.url("/pricing"), cache_dir.path())
            .expect("invalid or legacy cache must fall back to remote fetch");

        endpoint.assert();
        assert_eq!(map.get("fresh-model").pricing.input_cost_per_token, 4e-6);
    }
}

#[test]
fn fetch_pricing_prefers_cache_over_network() {
    clear_pricing_cache();
    let server = MockServer::start();
    // If today's cache is honored, this endpoint is never hit; it returns 500 so a regression that reaches the network fails loudly.
    let endpoint = server.mock(|when, then| {
        when.method(GET).path("/pricing");
        then.status(500);
    });
    let cache_dir = TempDir::new().unwrap();

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
fn normalized_collisions_prefer_official_vendor_over_reseller_prefixes() {
    clear_pricing_cache();

    let mut raw_map = HashMap::new();
    raw_map.insert("kyma/gemini-3.5-pro".to_string(), pricing_at(5.4));
    raw_map.insert("gemini/gemini-3.5-pro".to_string(), pricing_at(2.0));
    raw_map.insert("vertex_ai/gemini-3.5-pro".to_string(), pricing_at(2.1));
    raw_map.insert("azure/mistral-large-latest".to_string(), pricing_at(8.0));
    raw_map.insert("mistral/mistral-large-latest".to_string(), pricing_at(0.7));
    let pricing_map = ModelPricingMap::new(raw_map);

    let gemini = pricing_map.get("gemini-3.5-pro");
    assert_eq!(gemini.pricing.input_cost_per_token, 2.0);
    assert_eq!(
        gemini.matched_model.as_deref(),
        Some("gemini/gemini-3.5-pro")
    );

    let mistral = pricing_map.get("mistral-large-latest");
    assert_eq!(mistral.pricing.input_cost_per_token, 0.7);
    assert_eq!(
        mistral.matched_model.as_deref(),
        Some("mistral/mistral-large-latest")
    );
}

#[test]
fn multi_vendor_cloud_prefixes_only_count_as_official_for_their_own_models() {
    clear_pricing_cache();

    let mut raw_map = HashMap::new();
    raw_map.insert("dashscope/glm-5.2".to_string(), pricing_at(1.8));
    raw_map.insert("mistral/glm-5.2".to_string(), pricing_at(1.9));
    raw_map.insert("perplexity/glm-5.2".to_string(), pricing_at(2.0));
    raw_map.insert("tencent/glm-5.2".to_string(), pricing_at(2.1));
    raw_map.insert("vertex_ai/glm-5.2".to_string(), pricing_at(2.2));
    raw_map.insert("zai/glm-5.2".to_string(), pricing_at(1.4));
    raw_map.insert("dashscope/qwen3.8-max".to_string(), pricing_at(2.0));
    raw_map.insert("aihubmix/qwen3.8-max".to_string(), pricing_at(1.69));
    raw_map.insert("mistral/codestral-2508".to_string(), pricing_at(0.3));
    raw_map.insert("aihubmix/codestral-2508".to_string(), pricing_at(0.5));
    raw_map.insert("vertex_ai/gemma-4-26b-a4b-it".to_string(), pricing_at(0.15));
    raw_map.insert("aihubmix/gemma-4-26b-a4b-it".to_string(), pricing_at(0.14));
    raw_map.insert("perplexity/sonar-pro".to_string(), pricing_at(3.0));
    raw_map.insert("kyma/sonar-pro".to_string(), pricing_at(4.05));
    raw_map.insert("tencent/hy4-preview".to_string(), pricing_at(0.417));
    raw_map.insert("aihubmix/hy4-preview".to_string(), pricing_at(0.845));
    let pricing_map = ModelPricingMap::new(raw_map);

    // Every multi-vendor prefix sorts before `zai/` lexically, so any of them qualifying on a non-owned model would steal `glm-5.2` from `zai/`.
    assert_eq!(
        pricing_map.get("glm-5.2").matched_model.as_deref(),
        Some("zai/glm-5.2")
    );
    assert_eq!(
        pricing_map.get("qwen3.8-max").matched_model.as_deref(),
        Some("dashscope/qwen3.8-max")
    );
    assert_eq!(
        pricing_map.get("codestral-2508").matched_model.as_deref(),
        Some("mistral/codestral-2508")
    );
    assert_eq!(
        pricing_map
            .get("gemma-4-26b-a4b-it")
            .matched_model
            .as_deref(),
        Some("vertex_ai/gemma-4-26b-a4b-it")
    );
    assert_eq!(
        pricing_map.get("sonar-pro").matched_model.as_deref(),
        Some("perplexity/sonar-pro")
    );
    assert_eq!(
        pricing_map.get("hy4-preview").matched_model.as_deref(),
        Some("tencent/hy4-preview")
    );
}

#[test]
fn versioned_normalized_collisions_preserve_same_unprefixed_and_official_order() {
    clear_pricing_cache();

    // All candidates are versioned (`tier_group == 1`), with `anthropic/` < `claude-...` < `cloudflare/` < `openrouter/` lexically so lexical order opposes `same_provider` (0) < `unprefixed` (1) < `official` (2).
    let mut same_vs_unprefixed = HashMap::new();
    same_vs_unprefixed.insert(
        "anthropic/claude-3-7-sonnet-20250219".to_string(),
        pricing_at(3.1),
    );
    same_vs_unprefixed.insert("claude-3-7-sonnet-20250219".to_string(), pricing_at(3.0));
    same_vs_unprefixed.insert(
        "cloudflare/claude-3-7-sonnet-20250219".to_string(),
        pricing_at(3.4),
    );
    same_vs_unprefixed.insert(
        "openrouter/claude-3-7-sonnet-20250219".to_string(),
        pricing_at(3.2),
    );
    let same_vs_unprefixed = ModelPricingMap::new(same_vs_unprefixed);
    assert_eq!(
        same_vs_unprefixed
            .get("openrouter/claude-3-7-sonnet-20250301")
            .matched_model
            .as_deref(),
        Some("openrouter/claude-3-7-sonnet-20250219")
    );
    assert_eq!(
        same_vs_unprefixed
            .get("bedrock/claude-3-7-sonnet-20250301")
            .matched_model
            .as_deref(),
        Some("claude-3-7-sonnet-20250219")
    );

    // Versioned unprefixed canonical entry beats unsuffixed reseller.
    let mut unprefixed = HashMap::new();
    unprefixed.insert("aihubmix/claude-3-7-sonnet".to_string(), pricing_at(3.5));
    unprefixed.insert("claude-3-7-sonnet-20250219".to_string(), pricing_at(3.0));
    let unprefixed = ModelPricingMap::new(unprefixed);
    assert_eq!(
        unprefixed.get("claude-3-7-sonnet").matched_model.as_deref(),
        Some("claude-3-7-sonnet-20250219")
    );

    // Versioned official vendor beats lexically-earlier reseller.
    let mut official = HashMap::new();
    official.insert("aihubmix/deepseek-v3.1".to_string(), pricing_at(0.58));
    official.insert("deepseek/deepseek-v3.2".to_string(), pricing_at(0.28));
    let official = ModelPricingMap::new(official);
    assert_eq!(
        official.get("deepseek-v3").matched_model.as_deref(),
        Some("deepseek/deepseek-v3.2")
    );
}

#[test]
fn normalized_collisions_ignore_provider_prefix_length_and_prefer_unsuffixed_base() {
    clear_pricing_cache();

    let mut raw_map = HashMap::new();
    raw_map.insert("kyma/claude-opus-5.5".to_string(), pricing_at(5.4));
    raw_map.insert(
        "github_copilot/claude-opus-5.5".to_string(),
        pricing_at(4.0),
    );
    raw_map.insert("llamagate/mistral-7b-v0.3".to_string(), pricing_at(0.1));
    raw_map.insert("snowflake/mistral-7b".to_string(), pricing_at(0.15));
    let pricing_map = ModelPricingMap::new(raw_map);

    let opus = pricing_map.get("claude-opus-5.5");
    assert_eq!(opus.pricing.input_cost_per_token, 4.0);
    assert_eq!(
        opus.matched_model.as_deref(),
        Some("github_copilot/claude-opus-5.5")
    );

    let dated = pricing_map.get("mistral-7b-20260101");
    assert_eq!(dated.pricing.input_cost_per_token, 0.15);
    assert_eq!(dated.matched_model.as_deref(), Some("snowflake/mistral-7b"));
}

#[test]
fn substring_matching_prefers_official_vendor_over_reseller() {
    clear_pricing_cache();

    let mut raw_map = HashMap::new();
    raw_map.insert("aihubmix/grok-4.6".to_string(), pricing_at(2.5));
    raw_map.insert("kyma/grok-4.6".to_string(), pricing_at(2.758));
    raw_map.insert("xai/grok-4.6".to_string(), pricing_at(2.0));
    let pricing_map = ModelPricingMap::new(raw_map);

    let result = pricing_map.get("grok-4.6-fast");
    assert_eq!(result.pricing.input_cost_per_token, 2.0);
    assert_eq!(result.matched_model.as_deref(), Some("xai/grok-4.6"));
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

    let cost = calculate_cost(&tc(1000, 500, 0, 10000, 2000, 0), &pricing);
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
    let cost = calculate_cost(&tc(0, 0, 0, 0, 0, 0), &pricing);
    assert_eq!(cost, 0.0);
}

#[test]
fn test_calculate_cost_no_cache() {
    let pricing = ModelPricing {
        input_cost_per_token: 0.000003,
        output_cost_per_token: 0.000015,
        ..Default::default()
    };

    let cost = calculate_cost(&tc(1000, 500, 0, 0, 0, 0), &pricing);
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

    let cost = calculate_cost(&tc(1_000_000, 500_000, 0, 100_000, 50_000, 0), &pricing);
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

    let result = pricing_map.get("bedrock/claude-3-opus-20240229");
    assert!(
        result.pricing.input_cost_per_token > 0.0 || result.matched_model.is_some(),
        "Should match after normalization"
    );
}

#[test]
fn test_pricing_multiple_models() {
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
    let below = calculate_cost(&tc(100_000, 50_000, 0, 0, 0, 0), &pricing);
    assert_eq!(below, 100_000.0 * 0.000001 + 50_000.0 * 0.000002);

    // Aggregated volume alone never promotes to the tier; only the per-request `above_tiers` slice does.
    let aggregate = calculate_cost(&tc(300_000, 50_000, 0, 0, 0, 0), &pricing);
    assert_eq!(aggregate, 300_000.0 * 0.000001 + 50_000.0 * 0.000002);

    // A request classified above 200K bills its slice at tier prices.
    let mut above = tc(300_000, 50_000, 0, 0, 0, 0);
    above.above_tiers = vec![TierSlice {
        input_tokens: 300_000,
        output_tokens: 50_000,
        ..Default::default()
    }];
    let above = calculate_cost(&above, &pricing);
    assert_eq!(above, 300_000.0 * 0.000002 + 50_000.0 * 0.000004);
}

#[test]
fn test_pricing_range_based() {
    // Mimics Qwen-style range-based pricing.
    let pricing = ModelPricing {
        input_cost_per_token: 999.0, // Ignored when `ranges` is set.
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

    let low = calculate_cost(&tc(10_000, 1000, 0, 0, 0, 0), &pricing);
    assert_eq!(low, 10_000.0 * 0.000001 + 1000.0 * 0.000005);

    // Aggregated volume alone never promotes to the upper range; only per-request `above_tiers` slices do.
    let aggregate = calculate_cost(&tc(100_000, 1000, 0, 0, 0, 0), &pricing);
    assert_eq!(aggregate, 100_000.0 * 0.000001 + 1000.0 * 0.000005);

    let mut above = tc(100_000, 1000, 0, 0, 0, 0);
    above.above_tiers = vec![TierSlice {
        input_tokens: 100_000,
        output_tokens: 1000,
        ..Default::default()
    }];
    let above = calculate_cost(&above, &pricing);
    assert_eq!(above, 100_000.0 * 0.0000018 + 1000.0 * 0.000009);
}

#[test]
fn test_pricing_result_structure() {
    use vct_core::pricing::ModelPricingResult;

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
    let raw_map = HashMap::new();
    let pricing_map = ModelPricingMap::new(raw_map);
    let result = pricing_map.get("");
    assert_eq!(result.pricing.input_cost_per_token, 0.0);

    let long_name = "a".repeat(1000);
    let result = pricing_map.get(&long_name);
    assert_eq!(result.pricing.input_cost_per_token, 0.0);
}
