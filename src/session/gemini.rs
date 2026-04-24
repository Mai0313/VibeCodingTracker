use crate::constants::FastHashMap;
use crate::models::*;
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_git_remote_url, parse_iso_timestamp, process_gemini_usage};
use anyhow::Result;
use serde_json::Value;

/// Analyze Gemini conversations from the JSONL event stream.
///
/// `session` carries the first-line meta record (`sessionId` etc.), and
/// `events` yields one parsed JSON value per subsequent line. The analyzer
/// filters down to `type == "gemini"` events and deserialises those into
/// [`GeminiMessage`] individually; everything else (`type == "user"`,
/// `"info"`, `$set` meta-update records, …) is silently skipped.
///
/// This is the only supported Gemini entry point — legacy single-object
/// exports (`chats/<session>.json` with an inline `messages` array) are no
/// longer handled.
pub fn analyze_gemini_events<I>(
    session: GeminiSession,
    events: I,
    mode: ParseMode,
) -> Result<CodeAnalysis>
where
    I: IntoIterator<Item = Value>,
{
    let mut state = SessionParseState::with_mode(mode);
    let mut conversation_usage: FastHashMap<String, Value> = FastHashMap::with_capacity(3);

    for event in events {
        // Skip lines without a type tag (e.g. `{"$set": {...}}` meta updates
        // that Gemini CLI interleaves to keep `lastUpdated` fresh).
        let Some(event_type) = event.get("type").and_then(|t| t.as_str()) else {
            continue;
        };

        if event_type != "gemini" {
            continue;
        }

        // Parse only the assistant events into the typed shape — cheaper
        // than eagerly typing every line, and resilient to new event types
        // that may be added in future Gemini CLI releases.
        let Ok(message) = serde_json::from_value::<GeminiMessage>(event) else {
            continue;
        };

        process_gemini_message(&mut state, &mut conversation_usage, &message);
    }

    Ok(finalize_record(
        state,
        conversation_usage,
        session.session_id,
    ))
}

fn finalize_record(
    mut state: SessionParseState,
    conversation_usage: FastHashMap<String, Value>,
    session_id: String,
) -> CodeAnalysis {
    // Gemini CLI does not record the invoking `cwd` in its log format today;
    // fall back to querying git from the process's current dir so the usage
    // report still stamps a remote URL when running inside a repo.
    if state.git_remote.is_empty() {
        state.git_remote = get_git_remote_url(&state.folder_path);
    }

    let last_ts = state.last_ts;
    let mut record = state.into_record(conversation_usage);
    record.task_id = session_id;
    record.timestamp = last_ts;

    CodeAnalysis {
        user: String::new(),
        extension_name: String::new(),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    }
}

fn process_gemini_message(
    state: &mut SessionParseState,
    conversation_usage: &mut FastHashMap<String, Value>,
    message: &GeminiMessage,
) {
    let ts = parse_iso_timestamp(&message.timestamp);
    if ts > state.last_ts {
        state.last_ts = ts;
    }

    if message.message_type != "gemini" {
        return;
    }

    if let (Some(tokens), Some(model)) = (&message.tokens, &message.model) {
        process_gemini_usage(conversation_usage, model, tokens);
    }

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

                // Content sits at result[0].functionResponse.response.output
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
            // Current Gemini CLI emits `replace`; `edit_file` /
            // `replace_in_file` were the historical names and are kept
            // here as best-effort aliases in case older sessions are
            // still being replayed through `vct analysis --path`.
            "edit_file" | "replace_in_file" | "replace" => {
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
            // Meta tools like `update_topic` / `task_complete` carry no
            // file-operation data; ignore them silently.
            _ => {}
        }
    }
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
