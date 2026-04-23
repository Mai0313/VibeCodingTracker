use crate::parser::common_state::{ParseMode, ParserState};
use crate::constants::{FastHashMap, capacity};
use crate::models::*;
use crate::utils::{get_git_remote_url, parse_iso_timestamp, process_claude_usage};
use anyhow::Result;
use serde_json::Value;

/// Analyze Claude Code conversations from pre-parsed `Value` records.
///
/// Records are moved into the typed iterator form so that the lean
/// [`ClaudeCodeLog`] shape drops unused payloads at deserialisation.
pub fn analyze_claude_conversations(records: Vec<Value>) -> Result<CodeAnalysis> {
    analyze_claude_conversations_with_mode(records, ParseMode::Full)
}

pub fn analyze_claude_conversations_with_mode(
    records: Vec<Value>,
    mode: ParseMode,
) -> Result<CodeAnalysis> {
    let iter = records
        .into_iter()
        .filter_map(|v| serde_json::from_value::<ClaudeCodeLog>(v).ok());
    analyze_claude_logs(iter, mode)
}

/// Analyze Claude Code conversations from any iterator of pre-parsed logs.
///
/// This is the streaming entry point: callers that read JSONL one line at a
/// time (see [`crate::parser::analyzer::analyze_session_file_typed_as`]) feed
/// records through here without ever materialising a full `Vec<Value>` of raw
/// JSON.
pub fn analyze_claude_logs<I>(logs: I, mode: ParseMode) -> Result<CodeAnalysis>
where
    I: IntoIterator<Item = ClaudeCodeLog>,
{
    let mut state = ParserState::with_mode(mode);
    let mut conversation_usage: FastHashMap<String, Value> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);

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
                let ClaudeContentItem::ToolUse { name, input } = item else {
                    continue;
                };

                match name.as_str() {
                    "Read" => state.tool_counts.read += 1,
                    "Write" => state.tool_counts.write += 1,
                    "Edit" => state.tool_counts.edit += 1,
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
