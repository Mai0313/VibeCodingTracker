use crate::analysis::common_state::AnalysisState;
use crate::constants::capacity;
use crate::models::*;
use crate::utils::{get_git_remote_url, parse_iso_timestamp, process_claude_usage};
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;

/// Analyze Claude Code conversations
pub fn analyze_claude_conversations(records: Vec<Value>) -> Result<CodeAnalysis> {
    let mut state = AnalysisState::new();
    // Pre-allocate HashMap using centralized capacity constant
    let mut conversation_usage: HashMap<String, Value> =
        HashMap::with_capacity(capacity::MODELS_PER_SESSION);

    for record in records {
        let log: ClaudeCodeLog = match serde_json::from_value(record) {
            Ok(log) => log,
            Err(_) => continue,
        };

        if state.folder_path.is_empty() {
            state.folder_path.clone_from(&log.cwd); // More efficient than assignment + clone
        }
        state.task_id.clone_from(&log.session_id); // Reuse existing allocation

        let ts = parse_iso_timestamp(&log.timestamp);
        if ts > state.last_ts {
            state.last_ts = ts;
        }

        if log.log_type == "assistant" {
            if let Some(message) = &log.message {
                if let Some(msg_obj) = message.as_object() {
                    if let (Some(model), Some(usage)) = (msg_obj.get("model"), msg_obj.get("usage"))
                    {
                        if let Some(model_str) = model.as_str() {
                            process_claude_usage(&mut conversation_usage, model_str, usage);
                        }
                    }

                    if let Some(content_array) = msg_obj.get("content").and_then(|c| c.as_array()) {
                        for item in content_array {
                            let Some(item_obj) = item.as_object() else {
                                continue;
                            };

                            let Some(item_type) = item_obj.get("type").and_then(|t| t.as_str())
                            else {
                                continue;
                            };

                            if item_type != "tool_use" {
                                continue;
                            }

                            let Some(name) = item_obj.get("name").and_then(|n| n.as_str()) else {
                                continue;
                            };

                            match name {
                                "Read" => state.tool_counts.read += 1,
                                "Write" => state.tool_counts.write += 1,
                                "Edit" => state.tool_counts.edit += 1,
                                "TodoWrite" => state.tool_counts.todo_write += 1,
                                "Bash" => {
                                    if let Some(input) = item_obj.get("input") {
                                        let command = input
                                            .get("command")
                                            .and_then(|c| c.as_str())
                                            .unwrap_or("");
                                        let description = input
                                            .get("description")
                                            .and_then(|d| d.as_str())
                                            .unwrap_or("");

                                        state.add_run_command(command, description, ts);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }

        if let Some(tur) = &log.tool_use_result {
            if let Some(tur_obj) = tur.as_object() {
                let tur_type = tur_obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

                // Read operations
                if tur_type == "text" {
                    if let Some(file_map) = tur_obj.get("file").and_then(|f| f.as_object()) {
                        let file_path = file_map
                            .get("filePath")
                            .and_then(|p| p.as_str())
                            .unwrap_or("");
                        let content = file_map
                            .get("content")
                            .and_then(|c| c.as_str())
                            .unwrap_or("");

                        state.add_read_detail(file_path, content, ts);
                    }
                }

                // Write operations
                if tur_type == "create" {
                    let file_path = tur_obj
                        .get("filePath")
                        .and_then(|p| p.as_str())
                        .unwrap_or("");
                    let content = tur_obj
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or("");

                    state.add_write_detail(file_path, content, ts);
                }

                // Edit operations
                if let Some(file_path) = tur_obj.get("filePath").and_then(|p| p.as_str()) {
                    if let Some(new_string) = tur_obj.get("newString").and_then(|s| s.as_str()) {
                        let old_string = tur_obj
                            .get("oldString")
                            .and_then(|s| s.as_str())
                            .unwrap_or("");

                        state.add_edit_detail(file_path, old_string, new_string, ts);
                    }
                }
            }
        }
    }

    if state.git_remote.is_empty() {
        state.git_remote = get_git_remote_url(&state.folder_path);
    }

    let record = state.into_record(conversation_usage);

    Ok(CodeAnalysis {
        user: String::new(),
        extension_name: String::new(),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    })
}
