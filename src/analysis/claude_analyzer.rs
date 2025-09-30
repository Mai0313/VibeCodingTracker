use crate::models::*;
use crate::utils::{count_lines, get_git_remote_url, parse_iso_timestamp};
use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Analyze Claude Code conversations
pub fn analyze_claude_conversations(records: &[Value]) -> Result<CodeAnalysis> {
    let mut write_details = Vec::new();
    let mut read_details = Vec::new();
    let mut edit_details = Vec::new();
    let mut run_details = Vec::new();

    let mut tool_counts = CodeAnalysisToolCalls::default();
    let mut conversation_usage: HashMap<String, Value> = HashMap::new();
    let mut unique_files = HashSet::new();

    let mut total_write_lines = 0;
    let mut total_read_lines = 0;
    let mut total_read_characters = 0;
    let mut total_write_characters = 0;
    let mut total_edit_characters = 0;
    let mut total_edit_lines = 0;

    let mut folder_path = String::new();
    let mut task_id = String::new();
    let mut last_timestamp = 0i64;

    for record in records {
        let log: ClaudeCodeLog = match serde_json::from_value(record.clone()) {
            Ok(log) => log,
            Err(_) => continue,
        };

        if folder_path.is_empty() {
            folder_path = log.cwd.clone();
        }
        task_id = log.session_id.clone();

        let ts = parse_iso_timestamp(&log.timestamp);
        if ts > last_timestamp {
            last_timestamp = ts;
        }

        // Process assistant messages
        if log.log_type == "assistant" {
            if let Some(message) = &log.message {
                if let Some(msg_obj) = message.as_object() {
                    // Process usage data
                    if let (Some(model), Some(usage)) = (msg_obj.get("model"), msg_obj.get("usage"))
                    {
                        if let Some(model_str) = model.as_str() {
                            process_claude_usage(&mut conversation_usage, model_str, usage);
                        }
                    }

                    // Count tool calls
                    if let Some(content_array) = msg_obj.get("content").and_then(|c| c.as_array()) {
                        for item in content_array {
                            if let Some(item_obj) = item.as_object() {
                                if let Some(item_type) =
                                    item_obj.get("type").and_then(|t| t.as_str())
                                {
                                    if item_type == "tool_use" {
                                        if let Some(name) =
                                            item_obj.get("name").and_then(|n| n.as_str())
                                        {
                                            match name {
                                                "Read" => tool_counts.read += 1,
                                                "Write" => tool_counts.write += 1,
                                                "Edit" => tool_counts.edit += 1,
                                                "TodoWrite" => tool_counts.todo_write += 1,
                                                "Bash" => {
                                                    tool_counts.bash += 1;
                                                    if let Some(input) = item_obj.get("input") {
                                                        let command = input
                                                            .get("command")
                                                            .and_then(|c| c.as_str())
                                                            .unwrap_or("")
                                                            .to_string();
                                                        let description = input
                                                            .get("description")
                                                            .and_then(|d| d.as_str())
                                                            .unwrap_or("")
                                                            .to_string();

                                                        run_details.push(
                                                            CodeAnalysisRunCommandDetail {
                                                                base: CodeAnalysisDetailBase {
                                                                    file_path: log.cwd.clone(),
                                                                    line_count: 0,
                                                                    character_count: command.len(),
                                                                    timestamp: ts,
                                                                },
                                                                command,
                                                                description,
                                                            },
                                                        );
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Process tool use results
        if let Some(tur) = &log.tool_use_result {
            if let Some(tur_obj) = tur.as_object() {
                let tur_type = tur_obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

                // Read operations
                if tur_type == "text" {
                    if let Some(file_map) = tur_obj.get("file").and_then(|f| f.as_object()) {
                        let file_path = file_map
                            .get("filePath")
                            .and_then(|p| p.as_str())
                            .unwrap_or("")
                            .to_string();
                        let content = file_map
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or("");
                        let num_lines = file_map
                            .get("numLines")
                            .and_then(|n| n.as_u64())
                            .unwrap_or(0) as usize;

                        read_details.push(CodeAnalysisReadDetail {
                            base: CodeAnalysisDetailBase {
                                file_path: file_path.clone(),
                                line_count: num_lines,
                                character_count: content.chars().count(),
                                timestamp: ts,
                            },
                        });

                        unique_files.insert(file_path);
                        total_read_characters += content.chars().count();
                        total_read_lines += num_lines;
                    }
                }

                // Write operations
                if tur_type == "create" {
                    let file_path = tur_obj
                        .get("filePath")
                        .and_then(|p| p.as_str())
                        .unwrap_or("")
                        .to_string();
                    let content = tur_obj
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    let line_count = count_lines(&content);

                    write_details.push(CodeAnalysisWriteDetail {
                        base: CodeAnalysisDetailBase {
                            file_path: file_path.clone(),
                            line_count,
                            character_count: content.chars().count(),
                            timestamp: ts,
                        },
                        content: content.clone(),
                    });

                    unique_files.insert(file_path);
                    total_write_lines += line_count;
                    total_write_characters += content.chars().count();
                }

                // Edit operations
                if let Some(file_path) = tur_obj.get("filePath").and_then(|p| p.as_str()) {
                    if let Some(new_string) = tur_obj.get("newString").and_then(|s| s.as_str()) {
                        let old_string = tur_obj
                            .get("oldString")
                            .and_then(|s| s.as_str())
                            .unwrap_or("");
                        let line_count = count_lines(new_string);

                        edit_details.push(CodeAnalysisApplyDiffDetail {
                            base: CodeAnalysisDetailBase {
                                file_path: file_path.to_string(),
                                line_count,
                                character_count: new_string.chars().count(),
                                timestamp: ts,
                            },
                            old_string: old_string.to_string(),
                            new_string: new_string.to_string(),
                        });

                        unique_files.insert(file_path.to_string());
                        total_edit_characters += new_string.chars().count();
                        total_edit_lines += line_count;
                    }
                }
            }
        }
    }

    let git_remote_url = get_git_remote_url(&folder_path);

    let record = CodeAnalysisRecord {
        total_unique_files: unique_files.len(),
        total_write_lines,
        total_read_lines,
        total_read_characters,
        total_write_characters,
        total_edit_characters,
        total_edit_lines,
        write_file_details: write_details,
        read_file_details: read_details,
        edit_file_details: edit_details,
        run_command_details: run_details,
        tool_call_counts: tool_counts,
        conversation_usage,
        task_id,
        timestamp: last_timestamp,
        folder_path,
        git_remote_url,
    };

    Ok(CodeAnalysis {
        user: String::new(),
        extension_name: String::new(),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    })
}

/// Process Claude usage data
fn process_claude_usage(
    conversation_usage: &mut HashMap<String, Value>,
    model: &str,
    usage: &Value,
) {
    let usage_obj = match usage.as_object() {
        Some(obj) => obj,
        None => return,
    };

    // Get or create usage entry
    let existing = conversation_usage
        .entry(model.to_string())
        .or_insert_with(|| {
            serde_json::json!({
                "input_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "cache_creation": {},
                "output_tokens": 0,
                "service_tier": ""
            })
        });

    let existing_obj = existing.as_object_mut().unwrap();

    // Add numeric fields
    if let Some(input_tokens) = usage_obj.get("input_tokens").and_then(|v| v.as_i64()) {
        let current = existing_obj
            .get("input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        existing_obj.insert("input_tokens".to_string(), (current + input_tokens).into());
    }

    if let Some(cache_creation) = usage_obj
        .get("cache_creation_input_tokens")
        .and_then(|v| v.as_i64())
    {
        let current = existing_obj
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        existing_obj.insert(
            "cache_creation_input_tokens".to_string(),
            (current + cache_creation).into(),
        );
    }

    if let Some(cache_read) = usage_obj
        .get("cache_read_input_tokens")
        .and_then(|v| v.as_i64())
    {
        let current = existing_obj
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        existing_obj.insert(
            "cache_read_input_tokens".to_string(),
            (current + cache_read).into(),
        );
    }

    if let Some(output_tokens) = usage_obj.get("output_tokens").and_then(|v| v.as_i64()) {
        let current = existing_obj
            .get("output_tokens")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        existing_obj.insert(
            "output_tokens".to_string(),
            (current + output_tokens).into(),
        );
    }

    // Handle service_tier
    if let Some(service_tier) = usage_obj.get("service_tier").and_then(|v| v.as_str()) {
        existing_obj.insert("service_tier".to_string(), service_tier.into());
    }

    // Handle cache_creation nested object
    if let Some(cache_creation) = usage_obj.get("cache_creation").and_then(|v| v.as_object()) {
        let existing_cache = existing_obj
            .entry("cache_creation".to_string())
            .or_insert_with(|| serde_json::json!({}));

        if let Some(existing_cache_obj) = existing_cache.as_object_mut() {
            for (key, value) in cache_creation {
                if let Some(v) = value.as_i64() {
                    let current = existing_cache_obj
                        .get(key)
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    existing_cache_obj.insert(key.clone(), (current + v).into());
                }
            }
        }
    }
}
