use serde_json::json;
use vibe_coding_tracker::analysis::codex_analyzer::analyze_codex_conversations;
use vibe_coding_tracker::models::CodexLog;

#[test]
fn test_codex_analyzer_with_empty_logs() {
    let logs: Vec<CodexLog> = vec![];
    let result = analyze_codex_conversations(&logs);
    assert!(result.is_ok(), "Should handle empty logs gracefully");

    let analysis = result.unwrap();
    assert_eq!(analysis.records.len(), 1, "Should have one record");
    assert_eq!(
        analysis.records[0].tool_call_counts.bash, 0,
        "Should have no bash calls"
    );
}

#[test]
fn test_codex_analyzer_with_session_meta() {
    let logs = vec![CodexLog {
        timestamp: "2025-10-05T10:00:00.000Z".to_string(),
        log_type: "session_meta".to_string(),
        payload: serde_json::from_value(json!({
            "cwd": "/home/user/project",
            "id": "task-123",
            "git": {
                "repository_url": "https://github.com/user/repo.git"
            }
        }))
        .unwrap(),
    }];

    let result = analyze_codex_conversations(&logs);
    assert!(result.is_ok());

    let analysis = result.unwrap();
    assert_eq!(
        analysis.records[0].folder_path, "/home/user/project",
        "Should extract folder path from session_meta"
    );
    assert_eq!(
        analysis.records[0].task_id, "task-123",
        "Should extract task ID"
    );
    assert_eq!(
        analysis.records[0].git_remote_url, "https://github.com/user/repo.git",
        "Should extract git remote URL"
    );
}

#[test]
fn test_codex_analyzer_with_turn_context() {
    let logs = vec![
        CodexLog {
            timestamp: "2025-10-05T10:00:00.000Z".to_string(),
            log_type: "turn_context".to_string(),
            payload: serde_json::from_value(json!({
                "cwd": "/home/user/project",
                "model": "gpt-4-turbo"
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:01.000Z".to_string(),
            log_type: "event_msg".to_string(),
            payload: serde_json::from_value(json!({
                "type": "token_count",
                "info": {
                    "total_token_usage": {
                        "input_tokens": 1000,
                        "output_tokens": 500
                    },
                    "model_context_window": 128000
                }
            }))
            .unwrap(),
        },
    ];

    let result = analyze_codex_conversations(&logs);
    assert!(result.is_ok());

    let analysis = result.unwrap();
    let conversation_usage = &analysis.records[0].conversation_usage;
    assert!(
        conversation_usage.contains_key("gpt-4-turbo"),
        "Should track usage for gpt-4-turbo model"
    );
}

#[test]
fn test_codex_analyzer_shell_call_basic() {
    let logs = vec![
        CodexLog {
            timestamp: "2025-10-05T10:00:00.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call",
                "name": "shell",
                "call_id": "call-123",
                "arguments": json!({
                    "command": ["bash", "-c", "ls -la"]
                }).to_string()
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:01.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call_output",
                "call_id": "call-123",
                "output": json!({
                    "output": "total 8\ndrwxr-xr-x  2 user user 4096 Oct  5 10:00 ."
                }).to_string()
            }))
            .unwrap(),
        },
    ];

    let result = analyze_codex_conversations(&logs);
    assert!(result.is_ok());

    let analysis = result.unwrap();
    assert_eq!(
        analysis.records[0].tool_call_counts.bash, 1,
        "Should count shell command"
    );
    assert_eq!(
        analysis.records[0].run_command_details.len(),
        1,
        "Should have one run command detail"
    );
}

#[test]
fn test_codex_analyzer_cat_command() {
    let logs = vec![
        CodexLog {
            timestamp: "2025-10-05T10:00:00.000Z".to_string(),
            log_type: "session_meta".to_string(),
            payload: serde_json::from_value(json!({
                "cwd": "/home/user/project"
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:01.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call",
                "name": "shell",
                "call_id": "call-123",
                "arguments": json!({
                    "command": ["bash", "-c", "cat test.txt"]
                }).to_string()
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:02.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call_output",
                "call_id": "call-123",
                "output": json!({
                    "output": "Hello, World!\nThis is a test."
                }).to_string()
            }))
            .unwrap(),
        },
    ];

    let result = analyze_codex_conversations(&logs);
    assert!(result.is_ok());

    let analysis = result.unwrap();
    assert_eq!(
        analysis.records[0].tool_call_counts.read, 1,
        "Should count cat as read operation"
    );
    assert_eq!(
        analysis.records[0].read_file_details.len(),
        1,
        "Should have one read detail"
    );
    assert_eq!(
        analysis.records[0].total_read_lines, 2,
        "Should count 2 lines read"
    );
}

#[test]
fn test_codex_analyzer_sed_command() {
    let logs = vec![
        CodexLog {
            timestamp: "2025-10-05T10:00:00.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call",
                "name": "shell",
                "call_id": "call-123",
                "arguments": json!({
                    "command": ["bash", "-c", "sed -n '1,10p' script.sh"]
                }).to_string()
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:01.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call_output",
                "call_id": "call-123",
                "output": json!({
                    "output": "#!/bin/bash\necho hello"
                }).to_string()
            }))
            .unwrap(),
        },
    ];

    let result = analyze_codex_conversations(&logs);
    assert!(result.is_ok());

    let analysis = result.unwrap();
    assert_eq!(
        analysis.records[0].tool_call_counts.read, 1,
        "Should count sed as read operation"
    );
}

#[test]
fn test_codex_analyzer_applypatch_add_file() {
    let patch_script = r#"
*** Begin Patch
*** Add File: new_file.txt
+Hello, World!
+This is new content.
*** End Patch
"#;

    let logs = vec![
        CodexLog {
            timestamp: "2025-10-05T10:00:00.000Z".to_string(),
            log_type: "session_meta".to_string(),
            payload: serde_json::from_value(json!({
                "cwd": "/home/user/project"
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:01.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call",
                "name": "shell",
                "call_id": "call-123",
                "arguments": json!({
                    "command": ["bash", "-c", format!("applypatch {}", patch_script)]
                }).to_string()
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:02.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call_output",
                "call_id": "call-123",
                "output": json!({
                    "output": "Patch applied successfully"
                }).to_string()
            }))
            .unwrap(),
        },
    ];

    let result = analyze_codex_conversations(&logs);
    assert!(result.is_ok());

    let analysis = result.unwrap();
    assert_eq!(
        analysis.records[0].tool_call_counts.write, 1,
        "Should count add patch as write operation"
    );
    assert_eq!(
        analysis.records[0].total_write_lines, 2,
        "Should count 2 lines written"
    );
}

#[test]
fn test_codex_analyzer_applypatch_delete_file() {
    let patch_script = r#"
*** Begin Patch
*** Delete File: old_file.txt
-Old line 1
-Old line 2
*** End Patch
"#;

    let logs = vec![
        CodexLog {
            timestamp: "2025-10-05T10:00:00.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call",
                "name": "shell",
                "call_id": "call-123",
                "arguments": json!({
                    "command": ["bash", "-c", format!("applypatch {}", patch_script)]
                }).to_string()
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:01.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call_output",
                "call_id": "call-123",
                "output": json!({
                    "output": "Patch applied"
                }).to_string()
            }))
            .unwrap(),
        },
    ];

    let result = analyze_codex_conversations(&logs);
    assert!(result.is_ok());

    let analysis = result.unwrap();
    assert_eq!(
        analysis.records[0].tool_call_counts.edit, 1,
        "Should count delete patch as edit operation"
    );
}

#[test]
fn test_codex_analyzer_applypatch_update_file() {
    let patch_script = r#"
*** Begin Patch
*** Update File: existing.txt
@@ -1,2 +1,2 @@
-old content
+new content
*** End Patch
"#;

    let logs = vec![
        CodexLog {
            timestamp: "2025-10-05T10:00:00.000Z".to_string(),
            log_type: "session_meta".to_string(),
            payload: serde_json::from_value(json!({
                "cwd": "/test"
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:01.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call",
                "name": "shell",
                "call_id": "call-123",
                "arguments": json!({
                    "command": ["bash", "-c", format!("applypatch {}", patch_script)]
                }).to_string()
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:02.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call_output",
                "call_id": "call-123",
                "output": json!({
                    "output": "Success"
                }).to_string()
            }))
            .unwrap(),
        },
    ];

    let result = analyze_codex_conversations(&logs);
    assert!(result.is_ok());

    let analysis = result.unwrap();
    assert_eq!(
        analysis.records[0].tool_call_counts.edit, 1,
        "Should count update patch as edit operation"
    );
}

#[test]
fn test_codex_analyzer_empty_cat_output() {
    let logs = vec![
        CodexLog {
            timestamp: "2025-10-05T10:00:00.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call",
                "name": "shell",
                "call_id": "call-123",
                "arguments": json!({
                    "command": ["bash", "-c", "cat empty.txt"]
                }).to_string()
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:01.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call_output",
                "call_id": "call-123",
                "output": json!({
                    "output": ""
                }).to_string()
            }))
            .unwrap(),
        },
    ];

    let result = analyze_codex_conversations(&logs);
    assert!(result.is_ok());

    let analysis = result.unwrap();
    // Empty output should not be counted as a read
    assert_eq!(
        analysis.records[0].tool_call_counts.read, 0,
        "Empty cat should not count as read"
    );
}

#[test]
fn test_codex_analyzer_unknown_shell_function() {
    let logs = vec![CodexLog {
        timestamp: "2025-10-05T10:00:00.000Z".to_string(),
        log_type: "response_item".to_string(),
        payload: serde_json::from_value(json!({
            "type": "function_call",
            "name": "unknown_function",
            "call_id": "call-123",
            "arguments": "{}"
        }))
        .unwrap(),
    }];

    let result = analyze_codex_conversations(&logs);
    assert!(result.is_ok());

    let analysis = result.unwrap();
    // Unknown function should not be counted
    assert_eq!(
        analysis.records[0].tool_call_counts.bash, 0,
        "Unknown function should not count"
    );
}

#[test]
fn test_codex_analyzer_malformed_shell_output() {
    let logs = vec![
        CodexLog {
            timestamp: "2025-10-05T10:00:00.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call",
                "name": "shell",
                "call_id": "call-123",
                "arguments": "not valid json"
            }))
            .unwrap(),
        },
        CodexLog {
            timestamp: "2025-10-05T10:00:01.000Z".to_string(),
            log_type: "response_item".to_string(),
            payload: serde_json::from_value(json!({
                "type": "function_call_output",
                "call_id": "call-123",
                "output": "not valid json"
            }))
            .unwrap(),
        },
    ];

    let result = analyze_codex_conversations(&logs);
    // Should handle malformed data gracefully
    assert!(result.is_ok());
}
