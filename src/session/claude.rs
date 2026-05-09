use crate::constants::{FastHashMap, capacity};
use crate::models::*;
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_git_remote_url, parse_iso_timestamp, process_claude_usage};
use anyhow::Result;
use serde_json::Value;

/// Parse Claude Code session records from a `Vec<Value>` fallback.
///
/// Used by the pretty-printed single-object JSON fallback path in the
/// top-level [`parse_session_file_typed`] dispatcher. Records are moved
/// into the typed iterator form so that the lean [`ClaudeCodeLog`] shape
/// drops unused payloads at deserialisation.
///
/// [`parse_session_file_typed`]: crate::session::parser::parse_session_file_typed
pub fn parse_claude_log_values(records: Vec<Value>, mode: ParseMode) -> Result<CodeAnalysis> {
    let iter = records
        .into_iter()
        .filter_map(|v| serde_json::from_value::<ClaudeCodeLog>(v).ok());
    parse_claude_logs(iter, mode)
}

/// Parse Claude Code session records from any iterator of pre-typed logs.
///
/// This is the streaming entry point: callers that read JSONL one line at a
/// time (see [`crate::session::parser::parse_session_file`]) feed records
/// through here without ever materialising a full `Vec<Value>` of raw JSON.
pub fn parse_claude_logs<I>(logs: I, mode: ParseMode) -> Result<CodeAnalysis>
where
    I: IntoIterator<Item = ClaudeCodeLog>,
{
    let mut state = SessionParseState::with_mode(mode);
    let mut conversation_usage: FastHashMap<String, Value> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);
    // Map `tool_use_id` → `(tool_name, tool_input)` so the user-side
    // tool_result fallback (used by subagent JSONL files that lack the
    // top-level `toolUseResult` field) can recover the original tool name
    // and arguments. Only populated for tools whose result we actually
    // dispatch on (Read / Write / Edit) to keep the map small.
    let mut pending_tool_uses: FastHashMap<String, (String, ClaudeToolInput)> =
        FastHashMap::with_capacity(64);

    for log in logs {
        if state.folder_path.is_empty() && !log.cwd.is_empty() {
            state.folder_path.clone_from(&log.cwd);
        }
        if !log.session_id.is_empty() {
            state.task_id.clone_from(&log.session_id);
        }

        let ts = parse_iso_timestamp(&log.timestamp);
        if ts > state.last_ts {
            state.last_ts = ts;
        }

        if log.log_type == "assistant"
            && let Some(message) = &log.message
        {
            if let (Some(model), Some(usage)) = (&message.model, &message.usage) {
                process_claude_usage(&mut conversation_usage, model, usage);
            }

            for item in &message.content {
                let ClaudeContentItem::ToolUse { id, name, input } = item else {
                    continue;
                };

                match name.as_str() {
                    "Read" => {
                        state.tool_counts.read += 1;
                        if !id.is_empty()
                            && let Some(input) = input.clone()
                        {
                            pending_tool_uses.insert(id.clone(), (name.clone(), input));
                        }
                    }
                    "Write" => {
                        state.tool_counts.write += 1;
                        if !id.is_empty()
                            && let Some(input) = input.clone()
                        {
                            pending_tool_uses.insert(id.clone(), (name.clone(), input));
                        }
                    }
                    "Edit" => {
                        state.tool_counts.edit += 1;
                        if !id.is_empty()
                            && let Some(input) = input.clone()
                        {
                            pending_tool_uses.insert(id.clone(), (name.clone(), input));
                        }
                    }
                    "TodoWrite" => state.tool_counts.todo_write += 1,
                    "Bash" => {
                        if let Some(input) = input {
                            let command = input.command.as_deref().unwrap_or("");
                            let description = input.description.as_deref().unwrap_or("");
                            state.add_run_command(command, description, ts);
                        }
                    }
                    _ => {}
                }
            }
        }

        if let Some(tur) = &log.tool_use_result {
            let tur_type = tur.result_type.as_deref().unwrap_or("");

            if tur_type == "text"
                && let Some(file) = &tur.file
            {
                let file_path = file.file_path.as_deref().unwrap_or("");
                let content = file.content.as_deref().unwrap_or("");
                state.add_read_detail(file_path, content, ts);
            }

            if tur_type == "create" {
                let file_path = tur.file_path.as_deref().unwrap_or("");
                let content = tur.content.as_deref().unwrap_or("");
                state.add_write_detail(file_path, content, ts);
            }

            if let Some(file_path) = tur.file_path.as_deref()
                && let Some(new_string) = tur.new_string.as_deref()
            {
                let old_string = tur.old_string.as_deref().unwrap_or("");
                state.add_edit_detail(file_path, old_string, new_string, ts);
            }
        } else if log.log_type == "user"
            && log.is_sidechain
            && let Some(message) = &log.message
        {
            // Subagent JSONL fallback: tool results live inside
            // `message.content[].tool_result` instead of the top-level
            // `toolUseResult` field. We gate this on `is_sidechain` because
            // main-session user records can also legitimately have content
            // tool_result blocks alongside a non-dict (string-shaped)
            // `toolUseResult` (e.g. user-rejection messages); without the
            // sidechain guard those would double-count against the
            // toolUseResult path that runs above.
            for item in &message.content {
                let ClaudeContentItem::ToolResult {
                    tool_use_id,
                    content,
                } = item
                else {
                    continue;
                };
                let Some((name, input)) = pending_tool_uses.remove(tool_use_id) else {
                    continue;
                };
                dispatch_subagent_tool_result(&mut state, &name, &input, content, ts);
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

/// Dispatch a Read / Write / Edit tool result that came from a subagent
/// JSONL `message.content[].tool_result` block (no top-level `toolUseResult`).
///
/// `result_content` is what the model received back: for Read it's the
/// numbered file contents, for Write / Edit it's a confirmation string we
/// don't actually need (the line count comes from the original tool input).
fn dispatch_subagent_tool_result(
    state: &mut SessionParseState,
    tool_name: &str,
    input: &ClaudeToolInput,
    result_content: &str,
    ts: i64,
) {
    let Some(file_path) = input.file_path.as_deref() else {
        return;
    };

    match tool_name {
        "Read" => {
            // The numbered file contents the subagent saw — drives the
            // read-line tally. Use the result body, not the input, since
            // Read's input only carries the path.
            state.add_read_detail(file_path, result_content, ts);
        }
        "Write" => {
            // Write's input carries the full file body it intended to write.
            let body = input.content.as_deref().unwrap_or("");
            state.add_write_detail(file_path, body, ts);
        }
        "Edit" => {
            let new_string = input.new_string.as_deref().unwrap_or("");
            let old_string = input.old_string.as_deref().unwrap_or("");
            state.add_edit_detail(file_path, old_string, new_string, ts);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_log(ts: &str, model: &str, content: serde_json::Value) -> ClaudeCodeLog {
        let raw = serde_json::json!({
            "type": "assistant",
            "timestamp": ts,
            "isSidechain": true,
            "message": { "model": model, "usage": {}, "content": content }
        });
        serde_json::from_value(raw).unwrap()
    }

    fn user_log(ts: &str, content: serde_json::Value) -> ClaudeCodeLog {
        let raw = serde_json::json!({
            "type": "user",
            "timestamp": ts,
            "isSidechain": true,
            "message": { "content": content }
        });
        serde_json::from_value(raw).unwrap()
    }

    #[test]
    fn subagent_user_message_tool_result_is_dispatched_via_fallback() {
        // Simulate a subagent JSONL: assistant calls Read, user record
        // returns the content inside `message.content[].tool_result`
        // (no top-level `toolUseResult`). Expect read_lines to be counted.
        let logs = vec![
            assistant_log(
                "2025-01-01T00:00:00Z",
                "claude-haiku-4-5",
                serde_json::json!([
                    {
                        "type": "tool_use",
                        "id": "toolu_abc",
                        "name": "Read",
                        "input": { "file_path": "/tmp/foo.txt" }
                    }
                ]),
            ),
            user_log(
                "2025-01-01T00:00:01Z",
                serde_json::json!([
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_abc",
                        "content": "1\tline-one\n2\tline-two\n3\tline-three"
                    }
                ]),
            ),
        ];
        let analysis = parse_claude_logs(logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.total_read_lines, 3);
        // Read assistant tool_use also bumps tool_counts.read once, and
        // add_read_detail bumps it again — same convention as the main
        // session toolUseResult path.
        assert_eq!(record.tool_call_counts.read, 2);
    }

    #[test]
    fn main_session_string_tool_use_result_does_not_double_count() {
        // Main session records can have a string-shaped toolUseResult
        // (rejection messages) alongside a content tool_result block.
        // Without `is_sidechain`, the fallback would double-count. Here
        // we simulate `isSidechain == false` and expect zero read lines
        // (the string `toolUseResult` carries no file content).
        let raw_assistant = serde_json::json!({
            "type": "assistant",
            "timestamp": "2025-01-01T00:00:00Z",
            "isSidechain": false,
            "message": {
                "model": "claude-opus-4-7",
                "usage": {},
                "content": [
                    { "type": "tool_use", "id": "toolu_xyz", "name": "Read",
                      "input": { "file_path": "/tmp/bar.txt" } }
                ]
            }
        });
        let raw_user = serde_json::json!({
            "type": "user",
            "timestamp": "2025-01-01T00:00:01Z",
            "isSidechain": false,
            "toolUseResult": "User rejected the tool call",
            "message": {
                "content": [
                    { "type": "tool_result", "tool_use_id": "toolu_xyz",
                      "content": "1\tshould-not-count\n2\tshould-not-count" }
                ]
            }
        });
        let logs = vec![
            serde_json::from_value::<ClaudeCodeLog>(raw_assistant).unwrap(),
            serde_json::from_value::<ClaudeCodeLog>(raw_user).unwrap(),
        ];
        let analysis = parse_claude_logs(logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(
            record.total_read_lines, 0,
            "main-session string toolUseResult must not trigger fallback"
        );
        assert_eq!(record.tool_call_counts.read, 1, "tool_use bump only");
    }
}
