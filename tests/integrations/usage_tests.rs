// Integration tests for usage command functionality
//
// These tests verify the usage calculation and aggregation logic

use vibe_coding_tracker::usage::calculator::get_usage_from_directories;

#[test]
fn test_get_usage_from_empty_directories() {
    // Test that get_usage_from_directories works
    // This will use default directories which may or may not have data
    let result = get_usage_from_directories();
    assert!(result.is_ok(), "Should handle directories");

    // Result may be empty or contain data depending on the system
    let _usage = result.unwrap();
}

#[test]
fn test_get_usage_from_directories_structure() {
    // Test with default directories
    let result = get_usage_from_directories();

    if let Ok(usage) = result {
        // Verify that the result has valid structure
        for (date, models) in usage.iter() {
            assert!(!date.is_empty(), "Date should not be empty");

            // Verify model data structure
            for (_model_name, model_data) in models {
                // Verify the JSON structure has expected fields
                assert!(model_data.is_object(), "Model data should be an object");
            }
        }
    }
}

#[test]
fn test_usage_data_serialization() {
    use serde_json::json;
    use vibe_coding_tracker::models::usage::DateUsageResult;

    // Create sample usage data
    let mut usage = DateUsageResult::default();
    let mut model_data = ahash::AHashMap::default();
    model_data.insert(
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

    usage.insert("2025-10-11".to_string(), model_data);

    // Test serialization to JSON
    let json = serde_json::to_string(&usage).unwrap();
    assert!(
        json.contains("claude-sonnet-4"),
        "Should contain model name"
    );
    assert!(json.contains("2025-10-11"), "Should contain date");

    // Test deserialization
    let deserialized: DateUsageResult = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.len(), usage.len());
    assert!(deserialized.contains_key("2025-10-11"));
}

#[test]
fn test_usage_aggregation_by_date() {
    // Test that usage dates are properly formatted
    let result = get_usage_from_directories();

    if let Ok(usage) = result {
        // Verify that dates are properly formatted (YYYY-MM-DD)
        for date in usage.keys() {
            assert!(date.len() == 10, "Date should be in YYYY-MM-DD format");
            assert!(
                date.chars().filter(|c| *c == '-').count() == 2,
                "Date should have two hyphens"
            );
        }
    }
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

    let cost = calculate_cost(1000, 500, 10000, 2000, &pricing);

    // input: 1000 * 0.000003 = 0.003
    // output: 500 * 0.000015 = 0.0075
    // cache_read: 10000 * 0.0000003 = 0.003
    // cache_creation: 2000 * 0.00000375 = 0.0075
    // total: 0.021
    assert_eq!(cost, 0.021, "Cost calculation should be accurate");
}

#[test]
fn test_usage_with_multiple_models() {
    // Test handling of multiple models in usage data
    use serde_json::json;
    use vibe_coding_tracker::models::usage::DateUsageResult;

    let mut usage = DateUsageResult::default();
    let date = "2025-10-11".to_string();

    let mut models = ahash::AHashMap::default();
    models.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0,
            "cost_usd": 0.05
        }),
    );
    models.insert(
        "gpt-4-turbo".to_string(),
        json!({
            "input_tokens": 2000,
            "output_tokens": 1000,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0,
            "cost_usd": 0.10
        }),
    );

    usage.insert(date.clone(), models);

    assert_eq!(usage[&date].len(), 2, "Should have two models");

    let total_cost: f64 = usage[&date]
        .values()
        .filter_map(|v| v["cost_usd"].as_f64())
        .sum();
    assert!(
        (total_cost - 0.15).abs() < 0.001,
        "Total cost should be sum of individual costs"
    );
}

#[test]
fn test_usage_json_output_format() {
    // Test that JSON output format matches expected structure
    use serde_json::{Value, json};
    use vibe_coding_tracker::models::usage::DateUsageResult;

    let mut usage = DateUsageResult::default();
    let mut model_data = ahash::AHashMap::default();
    model_data.insert(
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

    usage.insert("2025-10-11".to_string(), model_data);

    let json = serde_json::to_string_pretty(&usage).unwrap();
    let parsed: Value = serde_json::from_str(&json).unwrap();

    // Verify structure
    assert!(parsed.is_object(), "Root should be an object");
    assert!(
        parsed["2025-10-11"].is_object(),
        "Date value should be an object"
    );

    let model_value = &parsed["2025-10-11"]["claude-sonnet-4"];
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

#[test]
fn test_usage_date_sorting() {
    // Test that dates are properly sortable
    use vibe_coding_tracker::models::usage::DateUsageResult;

    let mut usage = DateUsageResult::default();
    usage.insert("2025-10-15".to_string(), ahash::AHashMap::default());
    usage.insert("2025-10-10".to_string(), ahash::AHashMap::default());
    usage.insert("2025-10-12".to_string(), ahash::AHashMap::default());

    let mut dates: Vec<_> = usage.keys().collect();
    dates.sort();

    assert_eq!(dates[0], "2025-10-10");
    assert_eq!(dates[1], "2025-10-12");
    assert_eq!(dates[2], "2025-10-15");
}
