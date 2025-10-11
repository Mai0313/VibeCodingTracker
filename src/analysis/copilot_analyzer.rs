use crate::analysis::common_state::AnalysisState;
use crate::constants::{FastHashMap, capacity};
use crate::models::*;
use crate::utils::{get_git_remote_url, parse_iso_timestamp};
use anyhow::Result;
use serde_json::{Value, json};

/// Analyze Copilot CLI conversations
pub fn analyze_copilot_conversations(session: CopilotSession) -> Result<CodeAnalysis> {
    let mut state = AnalysisState::new();

    // Pre-allocate FastHashMap using centralized capacity constant
    // Copilot usage is currently unknown, so we initialize an empty map
    let mut conversation_usage: FastHashMap<String, Value> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);

    // Initialize with zero usage for the hardcoded "copilot" model
    // As requested by the user, usage data is not available from Copilot CLI logs
    conversation_usage.insert(
        "copilot".to_string(),
        json!({
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0
        }),
    );

    // Process timeline events
    for event in session.timeline {
        // Only process tool_call_completed events
        if event.event_type != "tool_call_completed" {
            continue;
        }

        let Some(tool_title) = &event.tool_title else {
            continue;
        };

        let Some(arguments) = &event.arguments else {
            continue;
        };

        let ts = parse_iso_timestamp(&event.timestamp);
        if ts > state.last_ts {
            state.last_ts = ts;
        }

        // Parse based on toolTitle
        match tool_title.as_str() {
            "str_replace_editor" => {
                // Try to parse as StrReplaceEditorArgs
                if let Ok(args) = serde_json::from_value::<StrReplaceEditorArgs>(arguments.clone())
                {
                    match args.command.as_str() {
                        "view" => {
                            // Read operation
                            let content = if let Some(view_range) = args.view_range {
                                // If view_range is present, calculate line count
                                let start = view_range.first().copied().unwrap_or(0);
                                let end = view_range.get(1).copied().unwrap_or(0);
                                let line_count = (end - start + 1).max(0) as usize;
                                // Generate placeholder content based on line count
                                "\n".repeat(line_count.saturating_sub(1))
                            } else {
                                // No view_range means reading entire file
                                // Try to extract content from result if available
                                if let Some(result) = &event.result {
                                    if let Some(log) = result.get("log").and_then(|l| l.as_str()) {
                                        log.to_string()
                                    } else {
                                        String::new()
                                    }
                                } else {
                                    String::new()
                                }
                            };

                            state.add_read_detail(&args.path, &content, ts);
                            state.tool_counts.read += 1;
                        }
                        "str_replace" => {
                            // Edit operation
                            let old_str = args.old_str.as_deref().unwrap_or("");
                            let new_str = args.new_str.as_deref().unwrap_or("");

                            state.add_edit_detail(&args.path, old_str, new_str, ts);
                            state.tool_counts.edit += 1;
                        }
                        "create" => {
                            // Write operation
                            let content = args.file_text.as_deref().unwrap_or("");

                            state.add_write_detail(&args.path, content, ts);
                            state.tool_counts.write += 1;
                        }
                        _ => {
                            // Unknown command, skip
                        }
                    }
                }
            }
            "bash" => {
                // Bash command execution
                if let Ok(args) = serde_json::from_value::<BashArgs>(arguments.clone()) {
                    let command = args.command.as_deref().unwrap_or("");
                    let description = args.description.as_deref().unwrap_or("");

                    state.add_run_command(command, description, ts);
                    state.tool_counts.bash += 1;
                }
            }
            _ => {
                // Unknown tool, skip
            }
        }
    }

    // Set task_id from session_id
    state.task_id = session.session_id;

    // Git remote URL is not available in Copilot CLI logs
    // We could potentially try to detect it from folder_path if needed
    if state.git_remote.is_empty() {
        state.git_remote = get_git_remote_url(&state.folder_path);
    }

    let record = state.into_record(conversation_usage);

    Ok(CodeAnalysis {
        user: String::new(),
        extension_name: String::from("Copilot-CLI"),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    })
}
