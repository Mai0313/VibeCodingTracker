use crate::analysis::common_state::{AnalysisMode, AnalysisState};
use crate::constants::FastHashMap;
use crate::models::*;
use crate::utils::{get_git_remote_url, parse_iso_timestamp, process_gemini_usage};
use anyhow::Result;
use serde_json::Value;

/// Analyze Gemini conversations from a pre-parsed `Vec<Value>`.
///
/// Thin wrapper around [`analyze_gemini_session`] kept for the
/// `analyze_jsonl_file` dispatch path that still materialises the full file as
/// `Vec<Value>` when the input is pretty-printed JSON.
pub fn analyze_gemini_conversations(data: Vec<Value>) -> Result<CodeAnalysis> {
    analyze_gemini_conversations_with_mode(data, AnalysisMode::Full)
}

pub fn analyze_gemini_conversations_with_mode(
    mut data: Vec<Value>,
    mode: AnalysisMode,
) -> Result<CodeAnalysis> {
    if data.is_empty() {
        return Ok(CodeAnalysis {
            user: String::new(),
            extension_name: String::new(),
            insights_version: String::new(),
            machine_id: String::new(),
            records: vec![],
        });
    }

    // Parse the Gemini session
    let session: GeminiSession = serde_json::from_value(data.remove(0))?;
    analyze_gemini_session(session, mode)
}

/// Analyze Gemini conversations from an already-deserialised [`GeminiSession`].
///
/// This is the entry point used by the streaming JSONL path — it skips the
/// `Vec<Value>` intermediate and runs straight off the typed shape.
pub fn analyze_gemini_session(session: GeminiSession, mode: AnalysisMode) -> Result<CodeAnalysis> {
    let mut state = AnalysisState::with_mode(mode);
    // Pre-allocate FastHashMap with typical capacity (1-3 models per conversation)
    let mut conversation_usage: FastHashMap<String, Value> = FastHashMap::with_capacity(3);
    let mut last_timestamp = 0i64;
    let folder_path = String::new();

    // Process messages to extract token usage and tool call details
    for message in &session.messages {
        let ts = parse_iso_timestamp(&message.timestamp);
        if ts > last_timestamp {
            last_timestamp = ts;
        }

        // Only process gemini messages (not user messages)
        if message.message_type != "gemini" {
            continue;
        }

        if let (Some(tokens), Some(model)) = (&message.tokens, &message.model) {
            process_gemini_usage(&mut conversation_usage, model, tokens);
        }

        // Process tool calls to extract file operations
        for tool_call in &message.tool_calls {
            let Some(name) = tool_call.get("name").and_then(|n| n.as_str()) else {
                continue;
            };

            let args = tool_call.get("args");

            match name {
                "read_file" => {
                    let file_path = args
                        .and_then(|a| a.get("file_path"))
                        .and_then(|p| p.as_str())
                        .unwrap_or("");

                    // Extract content from result[].functionResponse.response.output
                    let content = extract_tool_result_output(tool_call);
                    state.add_read_detail(file_path, &content, ts);
                }
                "write_file" | "create_file" => {
                    let file_path = args
                        .and_then(|a| a.get("file_path"))
                        .and_then(|p| p.as_str())
                        .unwrap_or("");
                    let content = args
                        .and_then(|a| a.get("content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("");

                    state.add_write_detail(file_path, content, ts);
                }
                "edit_file" | "replace_in_file" => {
                    let file_path = args
                        .and_then(|a| a.get("file_path"))
                        .and_then(|p| p.as_str())
                        .unwrap_or("");
                    let old_string = args
                        .and_then(|a| a.get("old_string").or_else(|| a.get("old_text")))
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    let new_string = args
                        .and_then(|a| a.get("new_string").or_else(|| a.get("new_text")))
                        .and_then(|s| s.as_str())
                        .unwrap_or("");

                    state.add_edit_detail(file_path, old_string, new_string, ts);
                }
                "run_command" | "execute_command" | "shell" => {
                    let command = args
                        .and_then(|a| a.get("command").or_else(|| a.get("cmd")))
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    let description = args
                        .and_then(|a| a.get("description"))
                        .and_then(|d| d.as_str())
                        .unwrap_or("");

                    state.add_run_command(command, description, ts);
                }
                _ => {}
            }
        }
    }

    // Try to get git remote URL from current directory
    let git_remote_url = get_git_remote_url(&folder_path);

    let mut record = state.into_record(conversation_usage);
    record.task_id = session.session_id;
    record.git_remote_url = git_remote_url;
    record.timestamp = last_timestamp;

    Ok(CodeAnalysis {
        user: String::new(),
        extension_name: String::new(),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    })
}

/// Extract output text from Gemini tool call result
///
/// Gemini result structure: `[{ "functionResponse": { "response": { "output": "..." } } }]`
fn extract_tool_result_output(tool_call: &Value) -> String {
    tool_call
        .get("result")
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("functionResponse"))
        .and_then(|fr| fr.get("response"))
        .and_then(|resp| resp.get("output"))
        .and_then(|o| o.as_str())
        .unwrap_or("")
        .to_string()
}
