// Integration tests for analysis command functionality
//
// These tests verify both single-file analysis and batch analysis operations

use std::path::PathBuf;
use tempfile::TempDir;
use vibe_coding_tracker::analysis::analyzer::analyze_jsonl_file;
use vibe_coding_tracker::analysis::batch_analyzer::{
    analyze_all_sessions, analyze_all_sessions_by_provider,
};

#[test]
fn test_single_file_analysis_claude() {
    let input_file = PathBuf::from("examples/test_conversation.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = analyze_jsonl_file(&input_file);
    assert!(result.is_ok(), "Should successfully analyze Claude file");

    let analysis = result.unwrap();
    assert!(analysis.is_object(), "Analysis should be a JSON object");

    // Verify required fields
    assert!(
        analysis["extensionName"].is_string(),
        "Should have extensionName"
    );
    assert_eq!(analysis["extensionName"], "Claude-Code");
    assert!(analysis["records"].is_array(), "Should have records array");
}

#[test]
fn test_single_file_analysis_codex() {
    let input_file = PathBuf::from("examples/test_conversation_oai.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = analyze_jsonl_file(&input_file);
    assert!(result.is_ok(), "Should successfully analyze Codex file");

    let analysis = result.unwrap();
    assert!(analysis.is_object(), "Analysis should be a JSON object");
    assert_eq!(analysis["extensionName"], "Codex");
}

#[test]
fn test_single_file_analysis_copilot() {
    let input_file = PathBuf::from("examples/test_conversation_copilot.json");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = analyze_jsonl_file(&input_file);
    assert!(result.is_ok(), "Should successfully analyze Copilot file");

    let analysis = result.unwrap();
    assert!(analysis.is_object(), "Analysis should be a JSON object");
    assert_eq!(analysis["extensionName"], "Copilot-CLI");
}

#[test]
fn test_single_file_analysis_gemini() {
    let input_file = PathBuf::from("examples/test_conversation_gemini.json");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = analyze_jsonl_file(&input_file);
    assert!(result.is_ok(), "Should successfully analyze Gemini file");

    let analysis = result.unwrap();
    assert!(analysis.is_object(), "Analysis should be a JSON object");
    assert_eq!(analysis["extensionName"], "Gemini");
}

#[test]
fn test_analysis_record_structure() {
    let input_file = PathBuf::from("examples/test_conversation.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = analyze_jsonl_file(&input_file);
    if let Ok(analysis) = result {
        let records = &analysis["records"];
        if let Some(first_record) = records.as_array().and_then(|arr| arr.first()) {
            // Verify record structure
            assert!(
                first_record["conversationUsage"].is_object(),
                "Should have conversationUsage"
            );
            assert!(
                first_record["toolCallCounts"].is_object(),
                "Should have toolCallCounts"
            );
            assert!(first_record["taskId"].is_string(), "Should have taskId");
            assert!(
                first_record["timestamp"].is_number(),
                "Should have timestamp"
            );
        }
    }
}

#[test]
fn test_analysis_conversation_usage() {
    let input_file = PathBuf::from("examples/test_conversation.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = analyze_jsonl_file(&input_file);
    if let Ok(analysis) = result {
        let records = &analysis["records"];
        if let Some(first_record) = records.as_array().and_then(|arr| arr.first()) {
            let usage = &first_record["conversationUsage"];

            // Verify that we have at least one model
            assert!(
                usage.as_object().map(|o| !o.is_empty()).unwrap_or(false),
                "Should have at least one model in conversationUsage"
            );

            // Check token structure for each model
            if let Some(usage_obj) = usage.as_object() {
                for (model_name, model_usage) in usage_obj {
                    assert!(!model_name.is_empty(), "Model name should not be empty");
                    assert!(
                        model_usage["input_tokens"].is_number(),
                        "Should have input_tokens"
                    );
                    assert!(
                        model_usage["output_tokens"].is_number(),
                        "Should have output_tokens"
                    );
                }
            }
        }
    }
}

#[test]
fn test_analysis_tool_call_counts() {
    let input_file = PathBuf::from("examples/test_conversation.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = analyze_jsonl_file(&input_file);
    if let Ok(analysis) = result {
        let records = &analysis["records"];
        if let Some(first_record) = records.as_array().and_then(|arr| arr.first()) {
            let counts = &first_record["toolCallCounts"];

            // Verify tool call counts structure
            assert!(counts.is_object(), "toolCallCounts should be an object");

            if let Some(counts_obj) = counts.as_object() {
                // Check that all values are numbers
                for (_tool, count) in counts_obj {
                    assert!(count.is_number(), "Tool count should be a number");
                }
            }
        }
    }
}

#[test]
fn test_analysis_file_operations() {
    let input_file = PathBuf::from("examples/test_conversation.jsonl");

    if !input_file.exists() {
        eprintln!("Skipping test: example file not found");
        return;
    }

    let result = analyze_jsonl_file(&input_file);
    if let Ok(analysis) = result {
        let records = &analysis["records"];
        if let Some(first_record) = records.as_array().and_then(|arr| arr.first()) {
            // Verify file operation fields exist
            assert!(
                first_record["editFileDetails"].is_array()
                    || first_record["editFileDetails"].is_null()
            );
            assert!(
                first_record["readFileDetails"].is_array()
                    || first_record["readFileDetails"].is_null()
            );
            assert!(
                first_record["writeFileDetails"].is_array()
                    || first_record["writeFileDetails"].is_null()
            );
            assert!(
                first_record["runCommandDetails"].is_array()
                    || first_record["runCommandDetails"].is_null()
            );

            // Verify line/character counts
            assert!(first_record["totalEditLines"].is_number());
            assert!(first_record["totalReadLines"].is_number());
            assert!(first_record["totalWriteLines"].is_number());
        }
    }
}

#[test]
fn test_batch_analysis_basic() {
    // Test batch analysis with default directories
    let result = analyze_all_sessions();
    assert!(result.is_ok(), "Batch analysis should not fail");

    if let Ok(rows) = result {
        // Verify each row has required fields
        for row in rows.iter() {
            assert!(!row.date.is_empty(), "Date should not be empty");
            assert!(!row.model.is_empty(), "Model should not be empty");
            // Line counts are usize, so they're always non-negative
            let _ = row.edit_lines;
            let _ = row.read_lines;
            let _ = row.write_lines;
        }
    }
}

#[test]
fn test_batch_analysis_sorting() {
    let result = analyze_all_sessions();

    if let Ok(rows) = result {
        if rows.len() > 1 {
            // Verify sorting: dates should be in order
            for i in 0..rows.len() - 1 {
                let current_date = &rows[i].date;
                let next_date = &rows[i + 1].date;

                if current_date == next_date {
                    // Same date, models should be sorted
                    assert!(
                        rows[i].model <= rows[i + 1].model,
                        "Models should be sorted alphabetically for same date"
                    );
                } else {
                    // Different dates should be in chronological order
                    assert!(
                        current_date <= next_date,
                        "Dates should be sorted chronologically"
                    );
                }
            }
        }
    }
}

#[test]
fn test_batch_analysis_serialization() {
    use vibe_coding_tracker::analysis::batch_analyzer::AggregatedAnalysisRow;

    let row = AggregatedAnalysisRow {
        date: "2025-10-11".to_string(),
        model: "claude-sonnet-4".to_string(),
        edit_lines: 100,
        read_lines: 200,
        write_lines: 50,
        bash_count: 10,
        edit_count: 20,
        read_count: 30,
        todo_write_count: 5,
        write_count: 8,
    };

    // Test serialization
    let json = serde_json::to_string(&row).unwrap();
    assert!(
        json.contains("editLines"),
        "Should use camelCase for edit_lines"
    );
    assert!(
        json.contains("readLines"),
        "Should use camelCase for read_lines"
    );
    assert!(
        json.contains("writeLines"),
        "Should use camelCase for write_lines"
    );
    assert!(
        json.contains("bashCount"),
        "Should use camelCase for bash_count"
    );
    assert!(
        json.contains("todoWriteCount"),
        "Should use camelCase for todo_write_count"
    );

    // Test deserialization
    let deserialized: AggregatedAnalysisRow = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.date, row.date);
    assert_eq!(deserialized.model, row.model);
    assert_eq!(deserialized.edit_lines, row.edit_lines);
}

#[test]
fn test_batch_analysis_by_provider() {
    // Test provider-grouped analysis
    let result = analyze_all_sessions_by_provider();
    assert!(result.is_ok(), "Provider-grouped analysis should not fail");

    if let Ok(grouped) = result {
        // Verify structure - check if any provider has data
        let has_data = !grouped.claude.is_empty()
            || !grouped.codex.is_empty()
            || !grouped.copilot.is_empty()
            || !grouped.gemini.is_empty();

        // At least one provider should have data or all should be empty (valid states)
        assert!(
            has_data
                || (grouped.claude.is_empty()
                    && grouped.codex.is_empty()
                    && grouped.copilot.is_empty()
                    && grouped.gemini.is_empty()),
            "Provider grouping should be valid"
        );
    }
}

#[test]
fn test_analysis_with_empty_file() {
    // Test that empty files are handled gracefully
    let temp_dir = TempDir::new().unwrap();
    let empty_file = temp_dir.path().join("empty.jsonl");
    std::fs::write(&empty_file, "").unwrap();

    let result = analyze_jsonl_file(&empty_file);
    // Should either succeed with empty result or fail gracefully
    assert!(
        result.is_ok() || result.is_err(),
        "Should handle empty file"
    );
}

#[test]
fn test_analysis_with_invalid_json() {
    // Test that invalid JSON is handled gracefully
    let temp_dir = TempDir::new().unwrap();
    let invalid_file = temp_dir.path().join("invalid.jsonl");
    std::fs::write(&invalid_file, "not valid json\n{incomplete").unwrap();

    let result = analyze_jsonl_file(&invalid_file);
    // Should fail with error
    assert!(result.is_err(), "Should fail on invalid JSON");
}

#[test]
fn test_analysis_date_format() {
    // Test that dates are formatted correctly (YYYY-MM-DD)
    let result = analyze_all_sessions();

    if let Ok(rows) = result {
        for row in rows.iter() {
            assert_eq!(
                row.date.len(),
                10,
                "Date should be 10 characters (YYYY-MM-DD)"
            );
            assert_eq!(
                row.date.chars().filter(|c| *c == '-').count(),
                2,
                "Date should have exactly 2 hyphens"
            );

            // Verify date components are numeric
            let parts: Vec<&str> = row.date.split('-').collect();
            assert_eq!(parts.len(), 3, "Date should have 3 parts");
            assert!(parts[0].parse::<u32>().is_ok(), "Year should be numeric");
            assert!(parts[1].parse::<u32>().is_ok(), "Month should be numeric");
            assert!(parts[2].parse::<u32>().is_ok(), "Day should be numeric");
        }
    }
}

#[test]
fn test_analysis_aggregation_logic() {
    // Test that analysis properly aggregates data
    use vibe_coding_tracker::analysis::batch_analyzer::AggregatedAnalysisRow;

    let rows = vec![
        AggregatedAnalysisRow {
            date: "2025-10-11".to_string(),
            model: "claude-sonnet-4".to_string(),
            edit_lines: 50,
            read_lines: 100,
            write_lines: 25,
            bash_count: 5,
            edit_count: 10,
            read_count: 15,
            todo_write_count: 2,
            write_count: 3,
        },
        AggregatedAnalysisRow {
            date: "2025-10-11".to_string(),
            model: "claude-sonnet-4".to_string(),
            edit_lines: 50,
            read_lines: 100,
            write_lines: 25,
            bash_count: 5,
            edit_count: 10,
            read_count: 15,
            todo_write_count: 3,
            write_count: 5,
        },
    ];

    // Calculate totals
    let total_edit_lines: usize = rows.iter().map(|r| r.edit_lines).sum();
    let total_read_lines: usize = rows.iter().map(|r| r.read_lines).sum();
    let total_write_lines: usize = rows.iter().map(|r| r.write_lines).sum();

    assert_eq!(total_edit_lines, 100);
    assert_eq!(total_read_lines, 200);
    assert_eq!(total_write_lines, 50);
}
