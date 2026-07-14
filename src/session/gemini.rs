//! Parser for Gemini CLI session logs
//! (`~/.gemini/tmp/<project_hash>/chats/*.jsonl`).
//!
//! The first line is a session-meta record; every subsequent line is an
//! event. The parser deduplicates repeated message ids across incremental
//! updates and snapshots, while retaining messages later hidden by a rewind
//! because their usage was billed and their tools already ran. It then processes
//! only `type == "gemini"` messages, which carry usage and tool calls.
use crate::constants::FastHashMap;
use crate::models::*;
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_git_remote_url, parse_iso_timestamp, process_gemini_usage};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

/// Parse Gemini CLI session events from the JSONL event stream.
///
/// `session` carries the first-line meta record (`sessionId` etc.), and
/// `events` yields one parsed JSON value per subsequent line. The parser
/// deduplicates append-only revisions into the historical message set, then
/// deserializes each `type == "gemini"` message into [`GeminiMessage`]. User
/// and info messages remain in the set for ordering but do not contribute
/// metrics.
///
/// This is the only supported Gemini entry point — legacy single-object
/// exports (`chats/<session>.json` with an inline `messages` array) are no
/// longer handled.
///
/// # Errors
///
/// Returns `anyhow::Result` for parity with the other provider parsers, but
/// has no fallible step. Non-`gemini` events are skipped, and unsupported
/// Gemini message schemas are logged before being skipped.
pub fn parse_gemini_events<I>(
    session: GeminiSession,
    events: I,
    mode: ParseMode,
) -> Result<CodeAnalysis>
where
    I: IntoIterator<Item = Value>,
{
    Ok(parse_gemini_events_with_diagnostics(session, events, mode)?.analysis)
}

/// Streaming Gemini parser with event-payload schema diagnostics.
pub(crate) fn parse_gemini_events_with_diagnostics<I>(
    session: GeminiSession,
    events: I,
    mode: ParseMode,
) -> Result<ParsedAnalysis>
where
    I: IntoIterator<Item = Value>,
{
    let mut state = SessionParseState::with_mode(mode);
    let mut conversation_usage: FastHashMap<String, Value> = FastHashMap::with_capacity(3);
    let mut diagnostics = ParseDiagnostics::default();
    diagnostics.record_recognized_source();

    let messages = deduplicate_messages(events, mode, &mut diagnostics);
    for message in messages {
        diagnostics.merge(message.diagnostics);
        if let (Some(tokens), Some(model)) = (&message.tokens, &message.model) {
            process_gemini_usage(&mut conversation_usage, model, tokens);
        }
        state.merge(message.state);
    }

    let analysis = finalize_record(state, conversation_usage, session.session_id);
    Ok(ParsedAnalysis::new(analysis, diagnostics))
}

/// One compacted assistant-message revision retained for historical metrics.
struct GeminiMessageAnalysis {
    state: SessionParseState,
    tokens: Option<GeminiTokens>,
    model: Option<String>,
    diagnostics: ParseDiagnostics,
}

/// Minimal Gemini message shape needed by analysis.
///
/// Deliberately omits user content and thoughts so `UsageOnly` never retains
/// those large fields while deduplicating revisions.
#[derive(Deserialize)]
struct GeminiAnalysisMessage {
    #[serde(default)]
    timestamp: String,
    #[serde(rename = "type", default)]
    message_type: String,
    tokens: Option<GeminiTokens>,
    model: Option<String>,
    #[serde(rename = "toolCalls", default)]
    tool_calls: Vec<Value>,
}

/// Deduplicates Gemini's append-only chat log into its historical message set.
///
/// A message id may be appended repeatedly as tokens and tool results arrive,
/// so only its latest revision is retained. `$set.messages` entries are merged
/// by id instead of counted again. `$rewindTo` changes the CLI's visible chat,
/// but does not undo already billed model calls or executed tools, so it does
/// not remove historical analysis records. Each revision is compacted before
/// storage, preserving the [`ParseMode::UsageOnly`] memory boundary.
fn deduplicate_messages<I>(
    events: I,
    mode: ParseMode,
    diagnostics: &mut ParseDiagnostics,
) -> Vec<GeminiMessageAnalysis>
where
    I: IntoIterator<Item = Value>,
{
    let mut messages = Vec::new();
    let mut positions: FastHashMap<String, usize> = FastHashMap::default();

    for event in events {
        if event.get("$rewindTo").is_some() {
            diagnostics.record_recognized_source();
            continue;
        }

        if event.get("id").and_then(Value::as_str).is_some()
            && let Some(message_type) = event.get("type").and_then(Value::as_str)
        {
            if matches!(message_type, "gemini" | "user" | "info" | "error") {
                diagnostics.record_recognized_source();
            } else {
                diagnostics.record_unrecognized();
            }
            upsert_message(&mut messages, &mut positions, event, mode, diagnostics);
            continue;
        }

        let Some(set) = event.get("$set") else {
            diagnostics.record_unrecognized();
            continue;
        };
        diagnostics.record_recognized_source();
        let Some(messages_value) = set.get("messages") else {
            continue;
        };
        let Some(snapshot) = messages_value.as_array() else {
            diagnostics.record_relevant(false);
            continue;
        };

        for message in snapshot {
            upsert_message(
                &mut messages,
                &mut positions,
                message.clone(),
                mode,
                diagnostics,
            );
        }
    }

    messages
}

/// Inserts a new message or replaces the latest revision for an existing id.
fn upsert_message(
    messages: &mut Vec<GeminiMessageAnalysis>,
    positions: &mut FastHashMap<String, usize>,
    message: Value,
    mode: ParseMode,
    diagnostics: &mut ParseDiagnostics,
) {
    if message.get("type").and_then(Value::as_str) != Some("gemini") {
        return;
    }
    let Some(id) = message
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
    else {
        diagnostics.record_relevant(false);
        return;
    };
    let raw_tokens = message
        .get("tokens")
        .filter(|tokens| !tokens.is_null())
        .cloned();
    let analysis = match serde_json::from_value::<GeminiAnalysisMessage>(message) {
        Ok(mut message) => {
            let mut message_diagnostics = ParseDiagnostics::default();
            if let Some(raw_tokens) = raw_tokens.as_ref() {
                let normalized = message
                    .model
                    .as_deref()
                    .is_some_and(|model| !model.is_empty())
                    && gemini_tokens_supported(raw_tokens);
                message_diagnostics.record_relevant(normalized);
                if !normalized {
                    message.tokens = None;
                }
            }
            record_message_diagnostics(&message, &mut message_diagnostics);
            let mut state = SessionParseState::with_mode(mode);
            process_gemini_message(&mut state, &message);
            GeminiMessageAnalysis {
                state,
                tokens: message.tokens,
                model: message.model,
                diagnostics: message_diagnostics,
            }
        }
        Err(_) => {
            let mut message_diagnostics = ParseDiagnostics::default();
            message_diagnostics.record_relevant(false);
            GeminiMessageAnalysis {
                state: SessionParseState::with_mode(mode),
                tokens: None,
                model: None,
                diagnostics: message_diagnostics,
            }
        }
    };

    if let Some(&index) = positions.get(&id) {
        messages[index] = analysis;
    } else {
        positions.insert(id, messages.len());
        messages.push(analysis);
    }
}

fn gemini_tokens_supported(tokens: &Value) -> bool {
    const TOKEN_FIELDS: &[&str] = &["input", "output", "cached", "thoughts", "tool", "total"];
    let Some(tokens) = tokens.as_object() else {
        return false;
    };
    if tokens.is_empty() {
        return true;
    }

    let mut recognized = false;
    for field in TOKEN_FIELDS {
        if let Some(value) = tokens.get(*field) {
            recognized = true;
            if value.as_i64().is_none() {
                return false;
            }
        }
    }
    recognized
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GeminiToolStatus {
    Success,
    Failed,
    Pending,
    Unsupported,
}

fn gemini_tool_status(tool_call: &Value) -> GeminiToolStatus {
    match tool_call.get("status").and_then(Value::as_str) {
        Some("success") => GeminiToolStatus::Success,
        Some("error" | "failed") => GeminiToolStatus::Failed,
        Some("pending" | "running") => GeminiToolStatus::Pending,
        None if tool_call.get("result").is_none_or(|result| {
            result.is_null() || result.as_array().is_some_and(Vec::is_empty)
        }) =>
        {
            GeminiToolStatus::Pending
        }
        _ => GeminiToolStatus::Unsupported,
    }
}

fn tracked_tool_schema_supported(name: &str, tool_call: &Value) -> bool {
    let args = tool_call.get("args");
    match name {
        "read_file" => {
            args.and_then(|args| args.get("file_path"))
                .and_then(Value::as_str)
                .is_some_and(|path| !path.is_empty())
                && tool_result_output(tool_call).is_some()
        }
        "write_file" | "create_file" => {
            args.and_then(|args| args.get("file_path"))
                .and_then(Value::as_str)
                .is_some_and(|path| !path.is_empty())
                && args
                    .and_then(|args| args.get("content"))
                    .is_some_and(Value::is_string)
        }
        "edit_file" | "replace_in_file" | "replace" => {
            args.and_then(|args| args.get("file_path"))
                .and_then(Value::as_str)
                .is_some_and(|path| !path.is_empty())
                && args
                    .and_then(|args| args.get("old_string").or_else(|| args.get("old_text")))
                    .is_some_and(Value::is_string)
                && args
                    .and_then(|args| args.get("new_string").or_else(|| args.get("new_text")))
                    .is_some_and(Value::is_string)
        }
        "run_command" | "run_shell_command" | "execute_command" | "shell" => args
            .and_then(|args| args.get("command").or_else(|| args.get("cmd")))
            .and_then(Value::as_str)
            .is_some_and(|command| !command.trim().is_empty()),
        "write_todos" | "read_many_files" => true,
        _ => false,
    }
}

fn record_message_diagnostics(message: &GeminiAnalysisMessage, diagnostics: &mut ParseDiagnostics) {
    for tool_call in &message.tool_calls {
        let Some(name) = tool_call.get("name").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(
            name,
            "read_file"
                | "write_file"
                | "create_file"
                | "edit_file"
                | "replace_in_file"
                | "replace"
                | "run_command"
                | "run_shell_command"
                | "execute_command"
                | "shell"
                | "write_todos"
                | "read_many_files"
        ) {
            continue;
        }
        let normalized = match gemini_tool_status(tool_call) {
            GeminiToolStatus::Success => tracked_tool_schema_supported(name, tool_call),
            GeminiToolStatus::Failed => true,
            GeminiToolStatus::Pending | GeminiToolStatus::Unsupported => false,
        };
        diagnostics.record_relevant(normalized);
    }
}

/// Converts the accumulated state into a single-record [`CodeAnalysis`],
/// stamping the `task_id` from the session meta and resolving the git remote
/// from the process working directory when none was captured.
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

/// Folds one assistant message's timestamp and tools into a compact state.
fn process_gemini_message(state: &mut SessionParseState, message: &GeminiAnalysisMessage) {
    let ts = parse_iso_timestamp(&message.timestamp);
    if ts > state.last_ts {
        state.last_ts = ts;
    }

    if message.message_type != "gemini" {
        return;
    }

    for tool_call in &message.tool_calls {
        let Some(name) = tool_call.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let status = gemini_tool_status(tool_call);
        if status == GeminiToolStatus::Unsupported {
            continue;
        }
        if status != GeminiToolStatus::Success {
            record_tool_invocation(state, name);
            continue;
        }
        if !tracked_tool_schema_supported(name, tool_call) {
            record_tool_invocation(state, name);
            continue;
        }

        let args = tool_call.get("args");

        match name {
            "read_file" => {
                state.tool_counts.read += 1;
                let file_path = args
                    .and_then(|a| a.get("file_path"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("");

                // Content sits at result[0].functionResponse.response.output
                if let Some(content) = tool_result_output(tool_call) {
                    attach_read_detail(state, file_path, content, ts);
                }
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
            // still being replayed through `vct analysis <file>`.
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

                state.add_edit_detail_raw(file_path, old_string, new_string, ts);
            }
            "run_command" | "run_shell_command" | "execute_command" | "shell" => {
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
            "write_todos" => state.tool_counts.todo_write += 1,
            "read_many_files" => state.tool_counts.read += 1,
            // Meta tools like `update_topic` / `task_complete` carry no
            // file-operation data; ignore them silently.
            _ => {}
        }
    }
}

fn record_tool_invocation(state: &mut SessionParseState, name: &str) {
    match name {
        "read_file" | "read_many_files" => state.tool_counts.read += 1,
        "write_file" | "create_file" => state.tool_counts.write += 1,
        "edit_file" | "replace_in_file" | "replace" => state.tool_counts.edit += 1,
        "run_command" | "run_shell_command" | "execute_command" | "shell" => {
            state.tool_counts.bash += 1;
        }
        "write_todos" => state.tool_counts.todo_write += 1,
        _ => {}
    }
}

fn attach_read_detail(state: &mut SessionParseState, path: &str, content: &str, ts: i64) {
    let invocation_count = state.tool_counts.read;
    state.add_read_detail(path, content, ts);
    state.tool_counts.read = invocation_count;
}

/// Returns the output string from a Gemini tool call result.
fn tool_result_output(tool_call: &Value) -> Option<&str> {
    tool_call
        .get("result")
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("functionResponse"))
        .and_then(|fr| fr.get("response"))
        .and_then(|resp| resp.get("output"))
        .and_then(|o| o.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> GeminiSession {
        GeminiSession {
            session_id: "session-1".to_string(),
            project_hash: "project-1".to_string(),
            start_time: String::new(),
            last_updated: String::new(),
            kind: Some("main".to_string()),
        }
    }

    fn assistant(id: &str, model: &str, input_tokens: i64, mut tool_calls: Value) -> Value {
        if let Some(tool_calls) = tool_calls.as_array_mut() {
            for tool_call in tool_calls {
                if let Some(tool_call) = tool_call.as_object_mut() {
                    tool_call
                        .entry("status")
                        .or_insert_with(|| Value::String("success".to_string()));
                }
            }
        }
        json!({
            "id": id,
            "timestamp": "2026-07-12T00:00:00Z",
            "type": "gemini",
            "model": model,
            "tokens": {
                "input": input_tokens,
                "output": 1,
                "cached": 0,
                "thoughts": 0,
                "tool": 0,
                "total": input_tokens + 1
            },
            "toolCalls": tool_calls
        })
    }

    #[test]
    fn repeated_message_id_uses_only_latest_revision() {
        let first = assistant(
            "message-1",
            "gemini-test",
            10,
            json!([{ "name": "write_file", "args": {
                "file_path": "/tmp/a.txt", "content": "old"
            }}]),
        );
        let latest = assistant(
            "message-1",
            "gemini-test",
            20,
            json!([{ "name": "write_file", "args": {
                "file_path": "/tmp/a.txt", "content": "new"
            }}]),
        );

        let analysis =
            parse_gemini_events(session(), vec![first, latest], ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.conversation_usage["gemini-test"]["input_tokens"], 20);
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.write_file_details.len(), 1);
        assert_eq!(record.write_file_details[0].content, "new");
    }

    #[test]
    fn messages_snapshot_merges_without_recounting_ids() {
        let prior = assistant("prior", "gemini-prior", 10, json!([]));
        let current = assistant("current", "gemini-current", 30, json!([]));
        let snapshot = json!({ "$set": { "messages": [current] } });

        let analysis =
            parse_gemini_events(session(), vec![prior, snapshot], ParseMode::Full).unwrap();
        let usage = &analysis.records[0].conversation_usage;
        assert_eq!(usage["gemini-prior"]["input_tokens"], 10);
        assert_eq!(usage["gemini-current"]["input_tokens"], 30);
    }

    #[test]
    fn rewind_keeps_already_billed_messages() {
        let first = assistant("first", "gemini-first", 10, json!([]));
        let second = assistant("second", "gemini-second", 20, json!([]));
        let third = assistant("third", "gemini-third", 30, json!([]));
        let rewind = json!({ "$rewindTo": "second" });

        let analysis = parse_gemini_events(
            session(),
            vec![first, second, third, rewind],
            ParseMode::Full,
        )
        .unwrap();
        let usage = &analysis.records[0].conversation_usage;
        assert_eq!(usage.len(), 3);
        assert!(usage.contains_key("gemini-first"));
        assert!(usage.contains_key("gemini-second"));
        assert!(usage.contains_key("gemini-third"));
    }

    #[test]
    fn current_tool_names_map_to_existing_metrics() {
        let message = assistant(
            "message-1",
            "gemini-test",
            10,
            json!([
                { "name": "run_shell_command", "args": { "command": "true" } },
                { "name": "write_todos", "args": { "todos": [] } },
                { "name": "read_many_files", "args": { "include": ["src/**"] } }
            ]),
        );

        let analysis = parse_gemini_events(session(), vec![message], ParseMode::Full).unwrap();
        let counts = &analysis.records[0].tool_call_counts;
        assert_eq!(counts.bash, 1);
        assert_eq!(counts.todo_write, 1);
        assert_eq!(counts.read, 1);
    }

    #[test]
    fn read_result_validation_distinguishes_empty_files_from_schema_drift() {
        let drifted = assistant(
            "drifted",
            "gemini-test",
            10,
            json!([{
                "name": "read_file",
                "args": { "file_path": "/tmp/a.txt" },
                "result": [{
                    "functionResponse": { "response": { "futureOutput": "" } }
                }]
            }]),
        );
        let drifted =
            parse_gemini_events_with_diagnostics(session(), vec![drifted], ParseMode::Full)
                .unwrap();
        assert_eq!(drifted.diagnostics.partial_failure_count(), 1);
        assert_eq!(drifted.analysis.records[0].tool_call_counts.read, 1);
        assert_eq!(drifted.analysis.records[0].total_read_lines, 0);

        let empty = assistant(
            "empty",
            "gemini-test",
            10,
            json!([{
                "name": "read_file",
                "args": { "file_path": "/tmp/a.txt" },
                "result": [{
                    "functionResponse": { "response": { "output": "" } }
                }]
            }]),
        );
        let empty =
            parse_gemini_events_with_diagnostics(session(), vec![empty], ParseMode::Full).unwrap();
        assert_eq!(empty.diagnostics.partial_failure_count(), 0);
        assert_eq!(empty.analysis.records[0].tool_call_counts.read, 1);
        assert_eq!(empty.analysis.records[0].total_read_lines, 0);
    }

    #[test]
    fn unknown_only_token_keys_do_not_become_zero_usage() {
        let mut message = assistant("drifted", "gemini-test", 10, json!([]));
        message["tokens"] = json!({ "prompt": 123, "completion": 45 });

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full)
                .unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert!(
            parsed.analysis.records[0].conversation_usage.is_empty(),
            "unknown token keys must not become a successful all-zero usage row"
        );

        let mut current = assistant("current", "gemini-test", 10, json!([]));
        current["tokens"] = json!({ "input": 0 });
        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![current], ParseMode::Full)
                .unwrap();
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(
            parsed.analysis.records[0].conversation_usage["gemini-test"]["input_tokens"],
            0
        );
    }

    #[test]
    fn edit_requires_both_old_and_new_text_without_falling_back_to_write() {
        let message = assistant(
            "drifted-edit",
            "gemini-test",
            10,
            json!([{
                "name": "replace",
                "args": {
                    "file_path": "/tmp/a.txt",
                    "future_old": "old",
                    "new_string": "new"
                }
            }]),
        );

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full)
                .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.total_edit_lines, 0);
        assert_eq!(record.total_write_lines, 0);
    }

    #[test]
    fn failed_write_counts_the_invocation_without_claiming_file_changes() {
        let message = assistant(
            "failed-write",
            "gemini-test",
            10,
            json!([{
                "name": "write_file",
                "status": "error",
                "args": {
                    "file_path": "/tmp/a.txt",
                    "content": "one\ntwo"
                },
                "result": [{ "functionResponse": { "response": { "error": "denied" } } }]
            }]),
        );

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full)
                .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.total_write_lines, 0);
        assert!(record.write_file_details.is_empty());
    }

    #[test]
    fn superseded_pending_revision_does_not_leave_a_false_warning() {
        let pending = assistant(
            "message-1",
            "gemini-test",
            10,
            json!([{
                "name": "read_file",
                "status": "pending",
                "args": { "file_path": "/tmp/a.txt" }
            }]),
        );
        let complete = assistant(
            "message-1",
            "gemini-test",
            20,
            json!([{
                "name": "read_file",
                "status": "success",
                "args": { "file_path": "/tmp/a.txt" },
                "result": [{
                    "functionResponse": { "response": { "output": "one\ntwo" } }
                }]
            }]),
        );

        let parsed = parse_gemini_events_with_diagnostics(
            session(),
            vec![pending, complete],
            ParseMode::Full,
        )
        .unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.analysis.records[0].tool_call_counts.read, 1);
        assert_eq!(parsed.analysis.records[0].total_read_lines, 2);
    }
}
