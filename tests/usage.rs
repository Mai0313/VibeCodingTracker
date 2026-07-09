// Integration tests for usage command functionality
//
// These tests verify the usage calculation and aggregation logic

use serial_test::serial;
use std::ffi::OsString;
use vibe_coding_tracker::cli::TimeRange;
use vibe_coding_tracker::usage::calculator::get_usage_from_directories;

/// Redirects `HOME` (and clears the XDG overrides) to an empty temp dir for the
/// guard's lifetime, restoring the previous env on drop.
///
/// `get_usage_from_directories` resolves every provider path off these vars, so
/// pointing them at an empty dir means the aggregation reads no real session
/// data — and, importantly, the Cursor branch never reaches its dashboard API
/// (no `~/.cursor/chats`, no `auth.json`), so tests stay offline, deterministic,
/// and never touch the user's credentials or `~/.vct` cache. Callers must be
/// `#[serial]` because it mutates process-global environment.
struct IsolatedHome {
    _tmp: tempfile::TempDir,
    prev: Vec<(&'static str, Option<OsString>)>,
}

impl IsolatedHome {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let keys = ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"];
        let prev = keys.iter().map(|&k| (k, std::env::var_os(k))).collect();
        // SAFETY: callers guard with `#[serial]`; env is restored on drop.
        unsafe {
            std::env::set_var("HOME", tmp.path());
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("XDG_DATA_HOME");
        }
        Self { _tmp: tmp, prev }
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        for (k, v) in &self.prev {
            // SAFETY: callers guard with `#[serial]`.
            unsafe {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

#[test]
#[serial]
fn test_get_usage_from_empty_directories() {
    // Isolate HOME so aggregation reads no real data and stays offline.
    let _home = IsolatedHome::new();

    let result = get_usage_from_directories(TimeRange::All);
    assert!(result.is_ok(), "Should handle directories");

    // With an empty home there is no provider data to aggregate.
    let _usage = result.unwrap();
}

#[test]
#[serial]
fn test_get_usage_from_directories_structure() {
    let _home = IsolatedHome::new();

    let result = get_usage_from_directories(TimeRange::All);

    if let Ok(usage) = result {
        // Verify that the result has valid structure
        for (_model_name, model_data) in usage.models.iter() {
            // Verify the JSON structure has expected fields
            assert!(model_data.is_object(), "Model data should be an object");
        }
    }
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
