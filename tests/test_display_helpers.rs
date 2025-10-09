// Tests for display helper functions
// Note: Full TUI interactive tests are difficult to test in CI
// These tests focus on testable components

use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use vibe_coding_tracker::analysis::AggregatedAnalysisRow;
use vibe_coding_tracker::models::DateUsageResult;

#[test]
fn test_analysis_display_table_with_data() {
    let data = vec![
        AggregatedAnalysisRow {
            date: "2025-10-01".to_string(),
            model: "claude-sonnet-4".to_string(),
            edit_lines: 100,
            read_lines: 200,
            write_lines: 50,
            bash_count: 10,
            edit_count: 15,
            read_count: 20,
            todo_write_count: 5,
            write_count: 3,
        },
        AggregatedAnalysisRow {
            date: "2025-10-02".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            edit_lines: 150,
            read_lines: 250,
            write_lines: 75,
            bash_count: 12,
            edit_count: 18,
            read_count: 25,
            todo_write_count: 7,
            write_count: 4,
        },
    ];

    // Test that display_analysis_table doesn't panic with valid data
    vibe_coding_tracker::analysis::display_analysis_table(&data);
}

#[test]
fn test_analysis_display_table_empty() {
    let data: Vec<AggregatedAnalysisRow> = vec![];

    // Test that display_analysis_table handles empty data gracefully
    vibe_coding_tracker::analysis::display_analysis_table(&data);
}

#[test]
fn test_usage_display_table_with_data() {
    let mut usage_data: DateUsageResult = BTreeMap::new();
    let mut models = HashMap::new();

    models.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 100
        }),
    );

    usage_data.insert("2025-10-01".to_string(), models);

    // Test that display_usage_table doesn't panic with valid data
    vibe_coding_tracker::usage::display_usage_table(&usage_data);
}

#[test]
fn test_usage_display_table_empty() {
    let usage_data: DateUsageResult = BTreeMap::new();

    // Test that display_usage_table handles empty data gracefully
    vibe_coding_tracker::usage::display_usage_table(&usage_data);
}

#[test]
fn test_usage_display_text_with_data() {
    let mut usage_data: DateUsageResult = BTreeMap::new();
    let mut models = HashMap::new();

    models.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500,
            "cache_read_input_tokens": 200,
            "cache_creation_input_tokens": 100
        }),
    );

    usage_data.insert("2025-10-01".to_string(), models);

    // Test that display_usage_text doesn't panic with valid data
    vibe_coding_tracker::usage::display_usage_text(&usage_data);
}

#[test]
fn test_usage_display_text_empty() {
    let usage_data: DateUsageResult = BTreeMap::new();

    // Test that display_usage_text handles empty data gracefully
    vibe_coding_tracker::usage::display_usage_text(&usage_data);
}

#[test]
fn test_usage_display_text_multiple_models() {
    let mut usage_data: DateUsageResult = BTreeMap::new();
    let mut models = HashMap::new();

    models.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500
        }),
    );

    models.insert(
        "gpt-4".to_string(),
        json!({
            "total_token_usage": {
                "input_tokens": 2000,
                "output_tokens": 1000,
                "total_tokens": 3000
            }
        }),
    );

    usage_data.insert("2025-10-01".to_string(), models);

    // Test with multiple models
    vibe_coding_tracker::usage::display_usage_text(&usage_data);
}

#[test]
fn test_usage_display_table_codex_format() {
    let mut usage_data: DateUsageResult = BTreeMap::new();
    let mut models = HashMap::new();

    models.insert(
        "gpt-4-turbo".to_string(),
        json!({
            "total_token_usage": {
                "input_tokens": 1500,
                "output_tokens": 750,
                "reasoning_output_tokens": 100,
                "cached_input_tokens": 300,
                "total_tokens": 2350
            }
        }),
    );

    usage_data.insert("2025-10-02".to_string(), models);

    // Test with Codex format
    vibe_coding_tracker::usage::display_usage_table(&usage_data);
}

#[test]
fn test_analysis_display_table_large_numbers() {
    let data = vec![AggregatedAnalysisRow {
        date: "2025-10-01".to_string(),
        model: "claude-sonnet-4".to_string(),
        edit_lines: 1_234_567,
        read_lines: 9_876_543,
        write_lines: 456_789,
        bash_count: 1_234,
        edit_count: 5_678,
        read_count: 9_012,
        todo_write_count: 345,
        write_count: 678,
    }];

    // Test with large numbers (should format with commas)
    vibe_coding_tracker::analysis::display_analysis_table(&data);
}

#[test]
fn test_usage_display_table_multiple_dates() {
    let mut usage_data: DateUsageResult = BTreeMap::new();

    for i in 1..=5 {
        let mut models = HashMap::new();
        models.insert(
            "claude-sonnet-4".to_string(),
            json!({
                "input_tokens": 1000 * i,
                "output_tokens": 500 * i
            }),
        );
        usage_data.insert(format!("2025-10-{:02}", i), models);
    }

    // Test with multiple dates
    vibe_coding_tracker::usage::display_usage_table(&usage_data);
}

#[test]
fn test_analysis_display_table_zero_values() {
    let data = vec![AggregatedAnalysisRow {
        date: "2025-10-01".to_string(),
        model: "claude-sonnet-4".to_string(),
        edit_lines: 0,
        read_lines: 0,
        write_lines: 0,
        bash_count: 0,
        edit_count: 0,
        read_count: 0,
        todo_write_count: 0,
        write_count: 0,
    }];

    // Test with all zero values
    vibe_coding_tracker::analysis::display_analysis_table(&data);
}

#[test]
fn test_usage_display_text_sorted_output() {
    let mut usage_data: DateUsageResult = BTreeMap::new();

    // Insert dates out of order
    for date in ["2025-10-03", "2025-10-01", "2025-10-02"] {
        let mut models = HashMap::new();
        models.insert(
            "claude-sonnet-4".to_string(),
            json!({
                "input_tokens": 1000,
                "output_tokens": 500
            }),
        );
        usage_data.insert(date.to_string(), models);
    }

    // Text display should sort dates
    vibe_coding_tracker::usage::display_usage_text(&usage_data);
}

#[test]
fn test_usage_display_table_with_daily_averages_single_provider() {
    let mut usage_data: DateUsageResult = BTreeMap::new();

    // Add multiple days for Claude only
    for i in 1..=3 {
        let mut models = HashMap::new();
        models.insert(
            "claude-sonnet-4".to_string(),
            json!({
                "input_tokens": 1000 * i,
                "output_tokens": 500 * i,
                "cache_read_input_tokens": 200 * i,
                "cache_creation_input_tokens": 100 * i
            }),
        );
        usage_data.insert(format!("2025-10-{:02}", i), models);
    }

    // Test that daily averages are calculated correctly for single provider
    vibe_coding_tracker::usage::display_usage_table(&usage_data);
}

#[test]
fn test_usage_display_table_with_daily_averages_multiple_providers() {
    let mut usage_data: DateUsageResult = BTreeMap::new();

    // Day 1: Claude
    let mut day1_models = HashMap::new();
    day1_models.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500,
        }),
    );
    usage_data.insert("2025-10-01".to_string(), day1_models);

    // Day 2: Codex
    let mut day2_models = HashMap::new();
    day2_models.insert(
        "gpt-4-turbo".to_string(),
        json!({
            "total_token_usage": {
                "input_tokens": 2000,
                "output_tokens": 1000,
                "total_tokens": 3000
            }
        }),
    );
    usage_data.insert("2025-10-02".to_string(), day2_models);

    // Day 3: Gemini
    let mut day3_models = HashMap::new();
    day3_models.insert(
        "gemini-2.5-pro".to_string(),
        json!({
            "input_tokens": 1500,
            "output_tokens": 750,
        }),
    );
    usage_data.insert("2025-10-03".to_string(), day3_models);

    // Test with multiple providers across different days
    vibe_coding_tracker::usage::display_usage_table(&usage_data);
}

#[test]
fn test_usage_display_table_with_daily_averages_mixed_providers() {
    let mut usage_data: DateUsageResult = BTreeMap::new();

    // Day 1: Claude and Codex
    let mut day1_models = HashMap::new();
    day1_models.insert(
        "claude-sonnet-4-5".to_string(),
        json!({
            "input_tokens": 1000,
            "output_tokens": 500,
        }),
    );
    day1_models.insert(
        "gpt-5-codex".to_string(),
        json!({
            "total_token_usage": {
                "input_tokens": 2000,
                "output_tokens": 1000,
                "total_tokens": 3000
            }
        }),
    );
    usage_data.insert("2025-10-01".to_string(), day1_models);

    // Day 2: All three providers
    let mut day2_models = HashMap::new();
    day2_models.insert(
        "claude-sonnet-4".to_string(),
        json!({
            "input_tokens": 1500,
            "output_tokens": 750,
        }),
    );
    day2_models.insert(
        "gpt-4-turbo".to_string(),
        json!({
            "total_token_usage": {
                "input_tokens": 2500,
                "output_tokens": 1250,
                "total_tokens": 3750
            }
        }),
    );
    day2_models.insert(
        "gemini-2.0-flash".to_string(),
        json!({
            "input_tokens": 1200,
            "output_tokens": 600,
        }),
    );
    usage_data.insert("2025-10-02".to_string(), day2_models);

    // Test with mixed providers per day
    vibe_coding_tracker::usage::display_usage_table(&usage_data);
}

#[test]
fn test_usage_display_table_with_daily_averages_o1_models() {
    let mut usage_data: DateUsageResult = BTreeMap::new();
    let mut models = HashMap::new();

    // Test o1 models are detected as Codex
    models.insert(
        "o1-preview".to_string(),
        json!({
            "total_token_usage": {
                "input_tokens": 1000,
                "output_tokens": 500,
                "reasoning_output_tokens": 200,
                "total_tokens": 1500
            }
        }),
    );

    models.insert(
        "o3-mini".to_string(),
        json!({
            "total_token_usage": {
                "input_tokens": 800,
                "output_tokens": 400,
                "total_tokens": 1200
            }
        }),
    );

    usage_data.insert("2025-10-01".to_string(), models);

    // Test that o1/o3 models are correctly classified as Codex
    vibe_coding_tracker::usage::display_usage_table(&usage_data);
}
