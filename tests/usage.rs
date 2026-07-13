// Integration tests for usage aggregation.
//
// These drive `get_usage_from_paths` against a `TempHome` (fixture session files
// under a temp directory) so the real aggregation runs hermetically: no
// process-global env is mutated, no machine files are read, and no external API
// is reached. The remaining tests are pure in-memory cost / JSON math.

mod common;

use common::{TempHome, fixture_str};
use vibe_coding_tracker::cli::TimeRange;
use vibe_coding_tracker::config::ProvidersConfig;
use vibe_coding_tracker::usage::calculator::{get_usage_from_paths, get_usage_from_paths_with};

#[test]
fn empty_home_yields_no_usage() {
    let home = TempHome::new();
    let data = get_usage_from_paths(&home.paths, TimeRange::All).expect("aggregate empty home");
    assert!(data.models.is_empty(), "empty home has no models");
    assert_eq!(data.provider_days.total, 0);
}

#[test]
fn aggregates_claude_session_from_paths() {
    let home = TempHome::new();
    home.put_claude_session(
        "test-project",
        "session.jsonl",
        &fixture_str("test_conversation_claude_code.jsonl"),
    );

    let data = get_usage_from_paths(&home.paths, TimeRange::All).expect("aggregate claude");

    assert!(
        data.models.contains_key("claude-sonnet-4-20250514"),
        "the Claude fixture's model should appear in the merged table, got: {:?}",
        data.models.keys().collect::<Vec<_>>()
    );
    assert!(
        data.per_provider
            .claude
            .contains_key("claude-sonnet-4-20250514"),
        "and be attributed to the Claude provider bucket"
    );
    assert!(
        data.provider_days.claude >= 1,
        "at least one active Claude day"
    );
}

#[test]
fn merges_multiple_providers_from_paths() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "session.jsonl",
        &fixture_str("test_conversation_claude_code.jsonl"),
    );
    home.put_gemini_session(
        "proj-hash",
        "chat.jsonl",
        &fixture_str("test_conversation_gemini.jsonl"),
    );

    let data = get_usage_from_paths(&home.paths, TimeRange::All).expect("aggregate multi");

    assert!(data.models.contains_key("claude-sonnet-4-20250514"));
    assert!(
        data.models.keys().any(|m| m.starts_with("gemini-3")),
        "a Gemini model should be present, got: {:?}",
        data.models.keys().collect::<Vec<_>>()
    );
    assert!(data.provider_days.claude >= 1);
    assert!(data.provider_days.gemini >= 1);
}

#[test]
fn aggregates_grok_context_estimate_without_model_or_compaction_duplication() {
    let home = TempHome::new();
    home.put_grok_fixture_session("workspace", "grok-session");

    let data = get_usage_from_paths(&home.paths, TimeRange::All).expect("aggregate Grok");
    let usage = data.models.get("grok-test").expect("resolved Grok model");

    assert_eq!(usage["input_tokens"], 0);
    assert_eq!(usage["cache_read_input_tokens"], 12_345);
    assert!(
        !data.models.contains_key("grok-secondary"),
        "session aggregates must not be copied to every model in modelsUsed"
    );
    assert_eq!(data.per_provider.grok.get("grok-test"), Some(usage));
    assert_eq!(data.provider_days.grok, 1);
    assert_eq!(data.provider_days.total, 1);
}

#[test]
fn disabled_grok_provider_is_not_scanned() {
    let home = TempHome::new();
    home.put_grok_fixture_session("workspace", "grok-session");
    let providers = ProvidersConfig {
        grok: false,
        ..ProvidersConfig::default()
    };

    let data = get_usage_from_paths_with(&home.paths, TimeRange::All, providers)
        .expect("aggregate with Grok disabled");

    assert!(data.models.is_empty());
    assert!(data.per_provider.grok.is_empty());
    assert_eq!(data.provider_days.grok, 0);
}

#[test]
fn disabled_provider_is_dropped_from_usage_rollup() {
    let home = TempHome::new();
    home.put_claude_session(
        "proj",
        "session.jsonl",
        &fixture_str("test_conversation_claude_code.jsonl"),
    );
    home.put_gemini_session(
        "proj-hash",
        "chat.jsonl",
        &fixture_str("test_conversation_gemini.jsonl"),
    );

    // Turn Gemini off in `[providers]`: it must be skipped entirely.
    let providers = ProvidersConfig {
        gemini: false,
        ..ProvidersConfig::default()
    };
    let data = get_usage_from_paths_with(&home.paths, TimeRange::All, providers)
        .expect("aggregate with gemini disabled");

    assert!(
        data.models.contains_key("claude-sonnet-4-20250514"),
        "the enabled Claude provider is still aggregated"
    );
    assert!(
        !data.models.keys().any(|m| m.starts_with("gemini-3")),
        "the disabled Gemini provider must not appear, got: {:?}",
        data.models.keys().collect::<Vec<_>>()
    );
    assert_eq!(data.provider_days.gemini, 0, "no active Gemini days");
}

#[test]
fn test_usage_data_serialization() {
    use serde_json::json;
    use vibe_coding_tracker::models::usage::UsageResult;

    // Create sample usage data
    let mut usage = UsageResult::default();
    usage.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 2000,
            "cache_creation_input_tokens": 1000,
            "cost_usd": 0.05,
            "matched_model": "claude-sonnet-4"
        }),
    );

    // Test serialization to JSON
    let json = serde_json::to_string(&usage).unwrap();
    assert!(
        json.contains("claude-sonnet-4"),
        "Should contain model name"
    );

    // Test deserialization
    let deserialized: UsageResult = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.len(), usage.len());
    assert!(deserialized.contains_key("claude-sonnet-4"));
}

#[test]
fn test_usage_calculation_cost_accuracy() {
    use vibe_coding_tracker::pricing::{ModelPricing, calculate_cost};

    let pricing = ModelPricing {
        input_cost_per_token: 0.000003,
        output_cost_per_token: 0.000015,
        cache_read_input_token_cost: 0.0000003,
        cache_creation_input_token_cost: 0.00000375,
        ..Default::default()
    };

    // 2000 cache_creation tokens, all default (5 minute) TTL, no reasoning.
    let cost = calculate_cost(1000, 500, 0, 10000, 2000, 0, &pricing);

    // input: 1000 * 0.000003 = 0.003
    // output: 500 * 0.000015 = 0.0075
    // cache_read: 10000 * 0.0000003 = 0.003
    // cache_creation (5m): 2000 * 0.00000375 = 0.0075
    // total: 0.021
    assert_eq!(cost, 0.021, "Cost calculation should be accurate");
}

#[test]
fn test_usage_with_multiple_models() {
    // Test handling of multiple models in usage data
    use serde_json::json;
    use vibe_coding_tracker::models::usage::UsageResult;

    let mut usage = UsageResult::default();
    usage.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0,
            "cost_usd": 0.05
        }),
    );
    usage.insert(
        "gpt-4-turbo".to_string(),
        json!({
            "input_tokens": 2000,
            "output_tokens": 1000,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0,
            "cost_usd": 0.10
        }),
    );

    assert_eq!(usage.len(), 2, "Should have two models");

    let total_cost: f64 = usage.values().filter_map(|v| v["cost_usd"].as_f64()).sum();
    assert!(
        (total_cost - 0.15).abs() < 0.001,
        "Total cost should be sum of individual costs"
    );
}

#[test]
fn test_usage_json_output_format() {
    // Test that JSON output format matches expected structure
    use serde_json::{Value, json};
    use vibe_coding_tracker::models::usage::UsageResult;

    let mut usage = UsageResult::default();
    usage.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 2000,
            "cache_creation_input_tokens": 1000,
            "cost_usd": 0.05123456789,
            "matched_model": "claude-sonnet-4"
        }),
    );

    let json = serde_json::to_string_pretty(&usage).unwrap();
    let parsed: Value = serde_json::from_str(&json).unwrap();

    // Verify structure
    assert!(parsed.is_object(), "Root should be an object");

    let model_value = &parsed["claude-sonnet-4"];
    assert!(
        model_value["input_tokens"].is_number(),
        "input_tokens should be number"
    );
    assert!(
        model_value["output_tokens"].is_number(),
        "output_tokens should be number"
    );
    assert!(
        model_value["cost_usd"].is_number(),
        "cost_usd should be number"
    );
}

#[test]
fn test_usage_handles_missing_cache_tokens() {
    // Test that usage calculations work when cache tokens are 0
    use serde_json::json;

    let usage_value = json!({
        "model": "test-model",
        "input_tokens": 1000,
        "output_tokens": 500,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cost_usd": 0.05
    });

    assert_eq!(usage_value["input_tokens"].as_i64().unwrap(), 1000);
    assert_eq!(usage_value["cache_read_input_tokens"].as_i64().unwrap(), 0);
    assert_eq!(
        usage_value["cache_creation_input_tokens"].as_i64().unwrap(),
        0
    );
}
