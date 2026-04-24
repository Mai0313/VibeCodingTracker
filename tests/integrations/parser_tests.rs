// Integration tests to verify parser output matches expected results
//
// This test compares the actual analysis output with expected results from example files,
// while ignoring certain fields that can vary between environments:
// - insightsVersion: may differ based on build
// - machineId: machine-specific identifier
// - user: username may differ
// - gitRemoteUrl: git remote URL may differ

use serde_json::Value;
use std::path::PathBuf;
use vibe_coding_tracker::session::parser::parse_session_file;

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
fn test_claude_code_parser() {
    let input_file = PathBuf::from("examples/test_conversation_claude_code.jsonl");
    let expected_file = PathBuf::from("examples/analysis_result_claude_code.json");

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
    let actual_result = parse_session_file(&input_file);
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
fn test_codex_parser() {
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
    let actual_result = parse_session_file(&input_file);
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
fn test_copilot_parser() {
    let input_file = PathBuf::from("examples/test_conversation_copilot.jsonl");
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
    let actual_result = parse_session_file(&input_file);
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
fn test_gemini_parser() {
    let input_file = PathBuf::from("examples/test_conversation_gemini.jsonl");
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
    let actual_result = parse_session_file(&input_file);
    assert!(
        actual_result.is_ok(),
        "Failed to analyze Gemini conversation: {:?}",
        actual_result.err()
    );

    let actual_json = actual_result.unwrap();

    // Compare results, ignoring specific fields. `folderPath` is included
    // because Gemini session logs do not carry a cwd in the meta record, so
    // the analyzer leaves it empty and the git-remote lookup falls back to
    // the current working directory — both of which are environment-
    // specific and will differ between CI and a local developer machine.
    let ignore_fields = [
        "insightsVersion",
        "machineId",
        "user",
        "gitRemoteUrl",
        "folderPath",
    ];
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
        "Gemini analysis output does not match expected result (ignoring insightsVersion, machineId, user, gitRemoteUrl, folderPath)"
    );
}

/// Inline-fixture smoke test for the Gemini JSONL parser.
///
/// Complements `test_gemini_parser` (which uses a real-world session dump)
/// by exercising narrow edge cases that the real fixture may not hit:
/// ignored `user` / `info` events, a `$set` meta-update line the analyzer
/// must silently skip, and one assistant `gemini` event carrying token
/// usage plus a `toolCalls[]` entry for a `replace` edit.
#[test]
fn test_gemini_parser_jsonl() {
    use std::io::Write;

    let tempdir = tempfile::tempdir().expect("failed to create tempdir");
    // `is_gemini_session_file` requires the parent directory to be named
    // `chats`, so honour that even for the inline fixture.
    let chats_dir = tempdir.path().join("project-hash").join("chats");
    std::fs::create_dir_all(&chats_dir).expect("failed to mkdir -p chats");
    let input_file = chats_dir.join("session-fixture.jsonl");

    let fixture = r#"{"sessionId":"fixture-session","projectHash":"abc","startTime":"2026-04-23T00:00:00.000Z","lastUpdated":"2026-04-23T00:00:10.000Z","kind":"main"}
{"id":"i1","timestamp":"2026-04-23T00:00:01.000Z","type":"info","content":"kicked off"}
{"id":"u1","timestamp":"2026-04-23T00:00:02.000Z","type":"user","content":[{"text":"hi"}]}
{"$set":{"lastUpdated":"2026-04-23T00:00:02.500Z"}}
{"id":"g1","timestamp":"2026-04-23T00:00:05.000Z","type":"gemini","model":"gemini-3-flash-preview","tokens":{"input":100,"output":50,"cached":10,"thoughts":5,"tool":0,"total":165},"content":"done","toolCalls":[{"id":"t1","name":"replace","args":{"file_path":"README.md","old_string":"old","new_string":"new"}}]}
"#;

    {
        let mut f = std::fs::File::create(&input_file).expect("failed to create fixture");
        f.write_all(fixture.as_bytes())
            .expect("failed to write fixture");
    }

    let actual = parse_session_file(&input_file).expect("parse Gemini fixture session file");

    assert_eq!(actual["extensionName"], "Gemini");
    let record = &actual["records"][0];
    assert_eq!(record["taskId"], "fixture-session");

    // Token usage attributed to the sole assistant model in the fixture.
    // Gemini's `tokens.input` (100) is the full prompt including the
    // cached subset (10), so `process_gemini_usage` stores it as
    // non-cached (90) — mirroring Claude's "input ⊥ cache_read"
    // convention and preventing `calculate_cost` from double-billing.
    let usage = &record["conversationUsage"]["gemini-3-flash-preview"];
    assert_eq!(usage["input_tokens"], 90);
    assert_eq!(usage["output_tokens"], 50);
    assert_eq!(usage["cache_read_input_tokens"], 10);
    assert_eq!(usage["thoughts_tokens"], 5);
    assert_eq!(usage["total_tokens"], 165);

    // `replace` tool call should land in edit_file_details.
    assert_eq!(record["editFileDetails"][0]["oldString"], "old");
    assert_eq!(record["editFileDetails"][0]["newString"], "new");
    assert_eq!(record["toolCallCounts"]["Edit"], 1);
}

#[cfg(test)]
mod helper_tests {
    use super::*;
    use serde_json::json;

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
