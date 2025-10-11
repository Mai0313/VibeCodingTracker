// Integration tests to verify analysis output matches expected results
//
// This test compares the actual analysis output with expected results from example files,
// while ignoring certain fields that can vary between environments:
// - insightsVersion: may differ based on build
// - machineId: machine-specific identifier
// - user: username may differ

use serde_json::{Value, json};
use std::path::PathBuf;
use vibe_coding_tracker::analysis::analyzer::analyze_jsonl_file;

/// Compare two JSON values while ignoring specific fields
///
/// This function recursively compares two JSON values, ignoring the specified fields
/// at any level of nesting.
fn compare_json_ignore_fields(actual: &Value, expected: &Value, ignore_fields: &[&str]) -> bool {
    match (actual, expected) {
        (Value::Object(actual_map), Value::Object(expected_map)) => {
            // Get all keys from both objects (excluding ignored fields)
            let actual_keys: std::collections::HashSet<_> = actual_map
                .keys()
                .filter(|k| !ignore_fields.contains(&k.as_str()))
                .collect();

            let expected_keys: std::collections::HashSet<_> = expected_map
                .keys()
                .filter(|k| !ignore_fields.contains(&k.as_str()))
                .collect();

            // Check that both have the same keys (excluding ignored fields)
            if actual_keys != expected_keys {
                eprintln!("Key mismatch:");
                eprintln!("  Actual keys: {:?}", actual_keys);
                eprintln!("  Expected keys: {:?}", expected_keys);
                return false;
            }

            // Recursively compare each value
            for key in actual_keys {
                let actual_value = &actual_map[key.as_str()];
                let expected_value = &expected_map[key.as_str()];

                if !compare_json_ignore_fields(actual_value, expected_value, ignore_fields) {
                    eprintln!("Mismatch at key: {}", key);
                    return false;
                }
            }

            true
        }
        (Value::Array(actual_arr), Value::Array(expected_arr)) => {
            if actual_arr.len() != expected_arr.len() {
                eprintln!(
                    "Array length mismatch: {} vs {}",
                    actual_arr.len(),
                    expected_arr.len()
                );
                return false;
            }

            for (i, (actual_item, expected_item)) in
                actual_arr.iter().zip(expected_arr.iter()).enumerate()
            {
                if !compare_json_ignore_fields(actual_item, expected_item, ignore_fields) {
                    eprintln!("Mismatch at array index: {}", i);
                    return false;
                }
            }

            true
        }
        _ => {
            if actual != expected {
                eprintln!("Value mismatch:");
                eprintln!("  Actual: {}", actual);
                eprintln!("  Expected: {}", expected);
                false
            } else {
                true
            }
        }
    }
}

#[test]
fn test_claude_code_analysis_matches_expected() {
    let input_file = PathBuf::from("examples/test_conversation.jsonl");
    let expected_file = PathBuf::from("examples/analysis_result.json");

    // Skip test if files don't exist
    if !input_file.exists() {
        eprintln!("Input file not found: {:?}", input_file);
        return;
    }

    if !expected_file.exists() {
        eprintln!("Expected result file not found: {:?}", expected_file);
        return;
    }

    // Read expected result
    let expected_content =
        std::fs::read_to_string(&expected_file).expect("Failed to read expected result file");
    let expected_json: Value =
        serde_json::from_str(&expected_content).expect("Failed to parse expected result JSON");

    // Analyze the input file
    let actual_result = analyze_jsonl_file(&input_file);
    assert!(
        actual_result.is_ok(),
        "Failed to analyze Claude Code conversation: {:?}",
        actual_result.err()
    );

    let actual_json = actual_result.unwrap();

    // Compare results, ignoring specific fields
    let ignore_fields = ["insightsVersion", "machineId", "user", "gitRemoteUrl"];
    let matches = compare_json_ignore_fields(&actual_json, &expected_json, &ignore_fields);

    if !matches {
        // Print detailed comparison for debugging
        eprintln!("\n=== ACTUAL OUTPUT ===");
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&actual_json)
                .unwrap_or_else(|_| "Invalid JSON".to_string())
        );
        eprintln!("\n=== EXPECTED OUTPUT ===");
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&expected_json)
                .unwrap_or_else(|_| "Invalid JSON".to_string())
        );
    }

    assert!(
        matches,
        "Claude Code analysis output does not match expected result (ignoring insightsVersion, machineId, user, gitRemoteUrl)"
    );
}

#[test]
fn test_codex_analysis_matches_expected() {
    let input_file = PathBuf::from("examples/test_conversation_oai.jsonl");
    let expected_file = PathBuf::from("examples/analysis_result_oai.json");

    // Skip test if files don't exist
    if !input_file.exists() {
        eprintln!("Input file not found: {:?}", input_file);
        return;
    }

    if !expected_file.exists() {
        eprintln!("Expected result file not found: {:?}", expected_file);
        return;
    }

    // Read expected result
    let expected_content =
        std::fs::read_to_string(&expected_file).expect("Failed to read expected result file");
    let expected_json: Value =
        serde_json::from_str(&expected_content).expect("Failed to parse expected result JSON");

    // Analyze the input file
    let actual_result = analyze_jsonl_file(&input_file);
    assert!(
        actual_result.is_ok(),
        "Failed to analyze Codex conversation: {:?}",
        actual_result.err()
    );

    let actual_json = actual_result.unwrap();

    // Compare results, ignoring specific fields
    let ignore_fields = ["insightsVersion", "machineId", "user", "gitRemoteUrl"];
    let matches = compare_json_ignore_fields(&actual_json, &expected_json, &ignore_fields);

    if !matches {
        // Print detailed comparison for debugging
        eprintln!("\n=== ACTUAL OUTPUT ===");
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&actual_json)
                .unwrap_or_else(|_| "Invalid JSON".to_string())
        );
        eprintln!("\n=== EXPECTED OUTPUT ===");
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&expected_json)
                .unwrap_or_else(|_| "Invalid JSON".to_string())
        );
    }

    assert!(
        matches,
        "Codex analysis output does not match expected result (ignoring insightsVersion, machineId, user, gitRemoteUrl)"
    );
}

#[test]
fn test_copilot_analysis_matches_expected() {
    let input_file = PathBuf::from("examples/test_conversation_copilot.json");
    let expected_file = PathBuf::from("examples/analysis_result_copilot.json");

    // Skip test if files don't exist
    if !input_file.exists() {
        eprintln!("Input file not found: {:?}", input_file);
        return;
    }

    if !expected_file.exists() {
        eprintln!("Expected result file not found: {:?}", expected_file);
        return;
    }

    // Read expected result
    let expected_content =
        std::fs::read_to_string(&expected_file).expect("Failed to read expected result file");
    let expected_json: Value =
        serde_json::from_str(&expected_content).expect("Failed to parse expected result JSON");

    // Analyze the input file
    let actual_result = analyze_jsonl_file(&input_file);
    assert!(
        actual_result.is_ok(),
        "Failed to analyze Copilot conversation: {:?}",
        actual_result.err()
    );

    let actual_json = actual_result.unwrap();

    // Compare results, ignoring specific fields
    let ignore_fields = ["insightsVersion", "machineId", "user", "gitRemoteUrl"];
    let matches = compare_json_ignore_fields(&actual_json, &expected_json, &ignore_fields);

    if !matches {
        // Print detailed comparison for debugging
        eprintln!("\n=== ACTUAL OUTPUT ===");
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&actual_json)
                .unwrap_or_else(|_| "Invalid JSON".to_string())
        );
        eprintln!("\n=== EXPECTED OUTPUT ===");
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&expected_json)
                .unwrap_or_else(|_| "Invalid JSON".to_string())
        );
    }

    assert!(
        matches,
        "Copilot analysis output does not match expected result (ignoring insightsVersion, machineId, user, gitRemoteUrl)"
    );
}

#[test]
fn test_gemini_analysis_matches_expected() {
    let input_file = PathBuf::from("examples/test_conversation_gemini.json");
    let expected_file = PathBuf::from("examples/analysis_result_gemini.json");

    // Skip test if files don't exist
    if !input_file.exists() {
        eprintln!("Input file not found: {:?}", input_file);
        return;
    }

    if !expected_file.exists() {
        eprintln!("Expected result file not found: {:?}", expected_file);
        return;
    }

    // Read expected result
    let expected_content =
        std::fs::read_to_string(&expected_file).expect("Failed to read expected result file");
    let expected_json: Value =
        serde_json::from_str(&expected_content).expect("Failed to parse expected result JSON");

    // Analyze the input file
    let actual_result = analyze_jsonl_file(&input_file);
    assert!(
        actual_result.is_ok(),
        "Failed to analyze Gemini conversation: {:?}",
        actual_result.err()
    );

    let actual_json = actual_result.unwrap();

    // Compare results, ignoring specific fields
    let ignore_fields = ["insightsVersion", "machineId", "user", "gitRemoteUrl"];
    let matches = compare_json_ignore_fields(&actual_json, &expected_json, &ignore_fields);

    if !matches {
        // Print detailed comparison for debugging
        eprintln!("\n=== ACTUAL OUTPUT ===");
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&actual_json)
                .unwrap_or_else(|_| "Invalid JSON".to_string())
        );
        eprintln!("\n=== EXPECTED OUTPUT ===");
        eprintln!(
            "{}",
            serde_json::to_string_pretty(&expected_json)
                .unwrap_or_else(|_| "Invalid JSON".to_string())
        );
    }

    assert!(
        matches,
        "Gemini analysis output does not match expected result (ignoring insightsVersion, machineId, user, gitRemoteUrl)"
    );
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn test_compare_json_ignore_fields_simple() {
        let actual = json!({
            "name": "test",
            "value": 123,
            "machineId": "different-id"
        });

        let expected = json!({
            "name": "test",
            "value": 123,
            "machineId": "original-id"
        });

        let ignore_fields = ["machineId"];
        assert!(compare_json_ignore_fields(
            &actual,
            &expected,
            &ignore_fields
        ));
    }

    #[test]
    fn test_compare_json_ignore_fields_nested() {
        let actual = json!({
            "data": {
                "name": "test",
                "user": "different-user"
            },
            "insightsVersion": "1.0.0"
        });

        let expected = json!({
            "data": {
                "name": "test",
                "user": "original-user"
            },
            "insightsVersion": "2.0.0"
        });

        let ignore_fields = ["user", "insightsVersion"];
        assert!(compare_json_ignore_fields(
            &actual,
            &expected,
            &ignore_fields
        ));
    }

    #[test]
    fn test_compare_json_ignore_fields_mismatch() {
        let actual = json!({
            "name": "test1",
            "value": 123
        });

        let expected = json!({
            "name": "test2",
            "value": 123
        });

        let ignore_fields = ["machineId"];
        assert!(!compare_json_ignore_fields(
            &actual,
            &expected,
            &ignore_fields
        ));
    }

    #[test]
    fn test_compare_json_ignore_fields_array() {
        let actual = json!({
            "items": [
                {"id": 1, "machineId": "abc"},
                {"id": 2, "machineId": "def"}
            ]
        });

        let expected = json!({
            "items": [
                {"id": 1, "machineId": "xyz"},
                {"id": 2, "machineId": "uvw"}
            ]
        });

        let ignore_fields = ["machineId"];
        assert!(compare_json_ignore_fields(
            &actual,
            &expected,
            &ignore_fields
        ));
    }
}
