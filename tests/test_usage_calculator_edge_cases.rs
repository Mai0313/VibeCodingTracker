use serde_json::json;
use std::fs;
use vibe_coding_tracker::usage::calculator::calculate_usage_from_jsonl;

#[test]
fn test_calculate_usage_empty_file() {
    // Create a temporary empty JSONL file
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("empty_test.jsonl");
    fs::write(&temp_file, "").unwrap();

    let result = calculate_usage_from_jsonl(&temp_file);
    assert!(result.is_ok(), "Should handle empty file gracefully");

    let usage = result.unwrap();
    assert_eq!(
        usage.conversation_usage.len(),
        0,
        "Empty file should have no usage"
    );

    // Cleanup
    let _ = fs::remove_file(&temp_file);
}

#[test]
fn test_calculate_usage_with_synthetic_model() {
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("synthetic_model_test.jsonl");

    // Create test data with synthetic model
    let data = [
        json!({
            "type": "assistant",
            "message": {
                "model": "<synthetic>test-model",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50
                }
            },
            "parentUuid": "test-uuid"
        }),
        json!({
            "type": "assistant",
            "message": {
                "model": "claude-3-opus",
                "usage": {
                    "input_tokens": 200,
                    "output_tokens": 100
                }
            },
            "parentUuid": "test-uuid"
        }),
    ];

    let jsonl_content = data
        .iter()
        .map(|v| serde_json::to_string(v).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&temp_file, jsonl_content).unwrap();

    let result = calculate_usage_from_jsonl(&temp_file);
    assert!(result.is_ok());

    let usage = result.unwrap();
    // Synthetic model should be skipped
    assert!(
        !usage
            .conversation_usage
            .contains_key("<synthetic>test-model"),
        "Synthetic model should be skipped"
    );
    assert!(
        usage.conversation_usage.contains_key("claude-3-opus"),
        "Real model should be tracked"
    );

    // Cleanup
    let _ = fs::remove_file(&temp_file);
}

#[test]
fn test_calculate_usage_with_cache_creation() {
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("cache_creation_test.jsonl");

    let data = [json!({
        "type": "assistant",
        "message": {
            "model": "claude-3-opus",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_creation_input_tokens": 1000,
                "cache_read_input_tokens": 500,
                "cache_creation": {
                    "prompt": 800,
                    "system": 200
                }
            }
        },
        "parentUuid": "test-uuid"
    })];

    let jsonl_content = data
        .iter()
        .map(|v| serde_json::to_string(v).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&temp_file, jsonl_content).unwrap();

    let result = calculate_usage_from_jsonl(&temp_file);
    assert!(result.is_ok());

    let usage = result.unwrap();
    let claude_usage = usage.conversation_usage.get("claude-3-opus").unwrap();

    assert_eq!(
        claude_usage["cache_creation_input_tokens"]
            .as_i64()
            .unwrap(),
        1000
    );
    assert_eq!(
        claude_usage["cache_read_input_tokens"].as_i64().unwrap(),
        500
    );

    // Check cache_creation nested object
    let cache_creation = claude_usage["cache_creation"].as_object().unwrap();
    assert_eq!(cache_creation["prompt"].as_i64().unwrap(), 800);
    assert_eq!(cache_creation["system"].as_i64().unwrap(), 200);

    // Cleanup
    let _ = fs::remove_file(&temp_file);
}

#[test]
fn test_calculate_usage_codex_with_reasoning_tokens() {
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("codex_reasoning_test.jsonl");

    let data = [
        json!({
            "type": "turn_context",
            "payload": {
                "model": "gpt-4o"
            }
        }),
        json!({
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": 1000,
                        "output_tokens": 500,
                        "reasoning_output_tokens": 200
                    },
                    "model_context_window": 128000
                }
            }
        }),
    ];

    let jsonl_content = data
        .iter()
        .map(|v| serde_json::to_string(v).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&temp_file, jsonl_content).unwrap();

    let result = calculate_usage_from_jsonl(&temp_file);
    assert!(result.is_ok());

    let usage = result.unwrap();
    let gpt_usage = usage.conversation_usage.get("gpt-4o").unwrap();

    let total_token_usage = gpt_usage["total_token_usage"].as_object().unwrap();
    assert_eq!(total_token_usage["input_tokens"].as_i64().unwrap(), 1000);
    assert_eq!(total_token_usage["output_tokens"].as_i64().unwrap(), 500);
    assert_eq!(
        total_token_usage["reasoning_output_tokens"]
            .as_i64()
            .unwrap(),
        200
    );

    // Cleanup
    let _ = fs::remove_file(&temp_file);
}

#[test]
fn test_calculate_usage_tool_call_counts() {
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("tool_calls_test.jsonl");

    let data = [json!({
        "type": "assistant",
        "message": {
            "model": "claude-3-opus",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50
            },
            "content": [
                {
                    "type": "tool_use",
                    "name": "Read",
                    "input": {}
                },
                {
                    "type": "tool_use",
                    "name": "Write",
                    "input": {}
                },
                {
                    "type": "tool_use",
                    "name": "Read",
                    "input": {}
                },
                {
                    "type": "text",
                    "text": "Some text"
                }
            ]
        },
        "parentUuid": "test-uuid"
    })];

    let jsonl_content = data
        .iter()
        .map(|v| serde_json::to_string(v).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&temp_file, jsonl_content).unwrap();

    let result = calculate_usage_from_jsonl(&temp_file);
    assert!(result.is_ok());

    let usage = result.unwrap();
    assert_eq!(
        *usage.tool_call_counts.get("Read").unwrap(),
        2,
        "Should count 2 Read calls"
    );
    assert_eq!(
        *usage.tool_call_counts.get("Write").unwrap(),
        1,
        "Should count 1 Write call"
    );

    // Cleanup
    let _ = fs::remove_file(&temp_file);
}

#[test]
fn test_calculate_usage_codex_shell_calls() {
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("codex_shell_test.jsonl");

    let data = [
        json!({
            "type": "turn_context",
            "payload": {
                "model": "gpt-4"
            }
        }),
        json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell"
            }
        }),
        json!({
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell"
            }
        }),
    ];

    let jsonl_content = data
        .iter()
        .map(|v| serde_json::to_string(v).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&temp_file, jsonl_content).unwrap();

    let result = calculate_usage_from_jsonl(&temp_file);
    assert!(result.is_ok());

    let usage = result.unwrap();
    assert_eq!(
        *usage.tool_call_counts.get("Bash").unwrap(),
        2,
        "Should count 2 shell calls as Bash"
    );

    // Cleanup
    let _ = fs::remove_file(&temp_file);
}

#[test]
fn test_calculate_usage_mixed_models() {
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("mixed_models_test.jsonl");

    let data = [
        json!({
            "type": "assistant",
            "message": {
                "model": "claude-3-opus",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50
                }
            },
            "parentUuid": "test-uuid"
        }),
        json!({
            "type": "assistant",
            "message": {
                "model": "claude-3-sonnet",
                "usage": {
                    "input_tokens": 200,
                    "output_tokens": 100
                }
            },
            "parentUuid": "test-uuid"
        }),
        json!({
            "type": "assistant",
            "message": {
                "model": "claude-3-opus",
                "usage": {
                    "input_tokens": 150,
                    "output_tokens": 75
                }
            },
            "parentUuid": "test-uuid"
        }),
    ];

    let jsonl_content = data
        .iter()
        .map(|v| serde_json::to_string(v).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&temp_file, jsonl_content).unwrap();

    let result = calculate_usage_from_jsonl(&temp_file);
    assert!(result.is_ok());

    let usage = result.unwrap();

    // Check opus aggregation
    let opus_usage = usage.conversation_usage.get("claude-3-opus").unwrap();
    assert_eq!(opus_usage["input_tokens"].as_i64().unwrap(), 250); // 100 + 150
    assert_eq!(opus_usage["output_tokens"].as_i64().unwrap(), 125); // 50 + 75

    // Check sonnet
    let sonnet_usage = usage.conversation_usage.get("claude-3-sonnet").unwrap();
    assert_eq!(sonnet_usage["input_tokens"].as_i64().unwrap(), 200);
    assert_eq!(sonnet_usage["output_tokens"].as_i64().unwrap(), 100);

    // Cleanup
    let _ = fs::remove_file(&temp_file);
}

#[test]
fn test_calculate_usage_non_assistant_messages() {
    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join("non_assistant_test.jsonl");

    let data = [
        json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": "Hello"
            },
            "parentUuid": "test-uuid"
        }),
        json!({
            "type": "system",
            "message": {
                "role": "system",
                "content": "System message"
            },
            "parentUuid": "test-uuid"
        }),
    ];

    let jsonl_content = data
        .iter()
        .map(|v| serde_json::to_string(v).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&temp_file, jsonl_content).unwrap();

    let result = calculate_usage_from_jsonl(&temp_file);
    assert!(result.is_ok());

    let usage = result.unwrap();
    // Non-assistant messages should not be counted
    assert_eq!(
        usage.conversation_usage.len(),
        0,
        "Only assistant messages should be counted"
    );

    // Cleanup
    let _ = fs::remove_file(&temp_file);
}
