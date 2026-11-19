//! Parser for Gemini CLI JSONL session logs (`~/.gemini/tmp/<project_hash>/chats/*.jsonl`).
//!
//! Line 1 is a session-meta record and subsequent lines are append-only events. Repeated message ids across incremental updates and `$set.messages` snapshots resolve to the latest revision while preserving already-billed messages across `$rewindTo`; only `type == "gemini"` messages contribute usage and tool metrics.
use crate::constants::FastHashMap;
use crate::models::*;
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{parse_iso_timestamp, process_gemini_usage};
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

/// Parses Gemini CLI JSONL `events` for `session` into a [`CodeAnalysis`].
///
/// `session` is the parsed first-line metadata record and `events` yields each subsequent JSONL record. Repeated message ids across incremental updates and `$set.messages` snapshots resolve to the latest revision while retaining messages hidden by `$rewindTo`; only `type == "gemini"` messages contribute metrics.
///
/// # Errors
///
/// Currently infallible; non-`gemini` events and unsupported message schemas are skipped.
pub fn parse_gemini_events<I>(
    session: GeminiSession,
    events: I,
    mode: ParseMode,
) -> Result<CodeAnalysis>
where
    I: IntoIterator<Item = Value>,
{
    Ok(parse_gemini_events_with_diagnostics(session, events, mode, None)?.analysis)
}

/// Parses Gemini CLI JSONL `events` into a [`ParsedAnalysis`] with schema diagnostics and optional context-tier classification.
pub(crate) fn parse_gemini_events_with_diagnostics<I>(
    session: GeminiSession,
    events: I,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<ParsedAnalysis>
where
    I: IntoIterator<Item = Value>,
{
    let mut classifier = tiers.map(TierClassifier::new);
    let mut state = SessionParseState::with_mode(mode);
    let mut conversation_usage: FastHashMap<String, Value> = FastHashMap::with_capacity(3);
    let mut diagnostics = ParseDiagnostics::default();
    diagnostics.record_recognized_source();

    let messages = deduplicate_messages(events, mode, &mut diagnostics);
    for message in messages {
        diagnostics.merge(message.diagnostics);
        if let (Some(tokens), Some(model)) = (&message.tokens, &message.model) {
            // `tokens.input` already includes cached tokens, so it is the full request prompt size.
            let level = classifier
                .as_mut()
                .map_or(0, |classifier| classifier.level(model, tokens.input));
            process_gemini_usage(&mut conversation_usage, model, tokens, level);
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

/// Minimal Gemini message payload, omitting `content` and `thoughts` to keep deduplication memory bounded.
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

/// Deduplicates append-only `events` by message id (merging `$set.messages` snapshots and retaining `$rewindTo` history), compacting each revision under `mode`.
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
    let mut analysis = match serde_json::from_value::<GeminiAnalysisMessage>(message) {
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
        // Preserve prior billed tokens and model when a later revision omits `tokens`.
        if analysis.tokens.is_none() {
            let prior = &mut messages[index];
            analysis.tokens = prior.tokens.take();
            if analysis.model.is_none() {
                analysis.model = prior.model.take();
            }
        }
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
    // Reject `{}` because all `GeminiTokens` fields default to zero and would silently bill nothing.
    if tokens.is_empty() {
        return false;
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

/// Gemini tools mapped to file-operation or invocation metrics; all tool-name aliases live in [`TrackedTool::from_name`].
#[derive(Clone, Copy)]
enum TrackedTool {
    Read,
    Write,
    Edit,
    Shell,
    TodoWrite,
    ReadManyFiles,
}

impl TrackedTool {
    /// Returns the tracked tool for `name`, or `None` for untracked tools such as `update_topic` and `task_complete`.
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "read_file" => Self::Read,
            "write_file" | "create_file" => Self::Write,
            // `replace` is the current name; `edit_file` and `replace_in_file` support older session logs.
            "edit_file" | "replace_in_file" | "replace" => Self::Edit,
            "run_command" | "run_shell_command" | "execute_command" | "shell" => Self::Shell,
            "write_todos" => Self::TodoWrite,
            // Distinct from `Read` because it reports no single-file content to attach.
            "read_many_files" => Self::ReadManyFiles,
            _ => return None,
        })
    }

    /// Extracts typed arguments for a successful call, or `None` if unreadable (caller falls back to an invocation count).
    fn read_args(self, tool_call: &Value) -> Option<ToolArgs<'_>> {
        let args = tool_call.get("args");
        Some(match self {
            Self::Read => ToolArgs::Read {
                file_path: arg_file_path(args)?,
                content: tool_result_output(tool_call)?,
            },
            Self::Write => ToolArgs::Write {
                file_path: arg_file_path(args)?,
                content: arg_str(args, &["content"])?,
            },
            Self::Edit => ToolArgs::Edit {
                file_path: arg_file_path(args)?,
                old_string: arg_str(args, &["old_string", "old_text"])?,
                new_string: arg_str(args, &["new_string", "new_text"])?,
            },
            Self::Shell => ToolArgs::Shell {
                // Treat blank commands as unreadable because `add_run_command` drops them silently.
                command: arg_str(args, &["command", "cmd"])
                    .filter(|command| !command.trim().is_empty())?,
                description: arg_str(args, &["description"]).unwrap_or(""),
            },
            Self::TodoWrite => ToolArgs::TodoWrite,
            Self::ReadManyFiles => ToolArgs::ReadManyFiles,
        })
    }

    /// Increments the tool's invocation counter without recording file-operation details.
    fn record_invocation(self, state: &mut SessionParseState) {
        match self {
            Self::Read | Self::ReadManyFiles => state.tool_counts.read += 1,
            Self::Write => state.tool_counts.write += 1,
            Self::Edit => state.tool_counts.edit += 1,
            Self::Shell => state.tool_counts.bash += 1,
            Self::TodoWrite => state.tool_counts.todo_write += 1,
        }
    }
}

/// Validated arguments of a successful [`TrackedTool`] call, shared by diagnostics and metric folding.
enum ToolArgs<'a> {
    Read {
        file_path: &'a str,
        content: &'a str,
    },
    Write {
        file_path: &'a str,
        content: &'a str,
    },
    Edit {
        file_path: &'a str,
        old_string: &'a str,
        new_string: &'a str,
    },
    Shell {
        command: &'a str,
        description: &'a str,
    },
    TodoWrite,
    ReadManyFiles,
}

impl ToolArgs<'_> {
    /// Folds the call into the file-operation metrics.
    fn record(self, state: &mut SessionParseState, ts: i64) {
        match self {
            Self::Read { file_path, content } => {
                state.tool_counts.read += 1;
                attach_read_detail(state, file_path, content, ts);
            }
            Self::Write { file_path, content } => state.add_write_detail(file_path, content, ts),
            Self::Edit {
                file_path,
                old_string,
                new_string,
            } => {
                // Use `add_edit_detail_raw` so an empty `old_string` stays an edit instead of converting into a write.
                state.add_edit_detail_raw(file_path, old_string, new_string, ts);
            }
            Self::Shell {
                command,
                description,
            } => state.add_run_command(command, description, ts),
            Self::TodoWrite => state.tool_counts.todo_write += 1,
            Self::ReadManyFiles => state.tool_counts.read += 1,
        }
    }
}

/// Returns the string value of the first matching key in `keys`, returning `None` if that key is present with a non-string value.
fn arg_str<'a>(args: Option<&'a Value>, keys: &[&str]) -> Option<&'a str> {
    let args = args?;
    keys.iter().find_map(|key| args.get(*key))?.as_str()
}

/// Returns the non-empty `file_path` string from `args`, if present.
fn arg_file_path(args: Option<&Value>) -> Option<&str> {
    arg_str(args, &["file_path"]).filter(|path| !path.is_empty())
}

fn record_message_diagnostics(message: &GeminiAnalysisMessage, diagnostics: &mut ParseDiagnostics) {
    for tool_call in &message.tool_calls {
        let Some(tool) = tool_call
            .get("name")
            .and_then(Value::as_str)
            .and_then(TrackedTool::from_name)
        else {
            continue;
        };
        let normalized = match gemini_tool_status(tool_call) {
            GeminiToolStatus::Success => tool.read_args(tool_call).is_some(),
            GeminiToolStatus::Failed => true,
            GeminiToolStatus::Pending | GeminiToolStatus::Unsupported => false,
        };
        diagnostics.record_relevant(normalized);
    }
}

/// Builds a single-record [`CodeAnalysis`] with `session_id` (Gemini CLI logs omit workspace paths, leaving `folder_path` and `git_remote` empty).
fn finalize_record(
    state: SessionParseState,
    conversation_usage: FastHashMap<String, Value>,
    session_id: String,
) -> CodeAnalysis {
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
        let Some(tool) = tool_call
            .get("name")
            .and_then(|n| n.as_str())
            .and_then(TrackedTool::from_name)
        else {
            continue;
        };
        let status = gemini_tool_status(tool_call);
        if status == GeminiToolStatus::Unsupported {
            continue;
        }
        if status == GeminiToolStatus::Success
            && let Some(args) = tool.read_args(tool_call)
        {
            args.record(state, ts);
        } else {
            tool.record_invocation(state);
        }
    }
}

/// Attaches read details while preserving the caller's existing `tool_counts.read`.
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
    fn tokenless_revision_keeps_prior_billed_tokens() {
        let billed = assistant("message-1", "gemini-test", 42, json!([]));
        let mut tokenless = assistant(
            "message-1",
            "gemini-test",
            0,
            json!([{ "name": "write_file", "args": {
                "file_path": "/tmp/a.txt", "content": "x"
            }}]),
        );
        tokenless["tokens"] = json!(null);

        let analysis =
            parse_gemini_events(session(), vec![billed, tokenless], ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.conversation_usage["gemini-test"]["input_tokens"], 42);
        assert_eq!(record.tool_call_counts.write, 1);
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
    fn every_tracked_name_folds_into_its_own_metric() {
        // Counters in order: read, write, edit, bash, todo_write.
        let cases = [
            ("read_file", [1, 0, 0, 0, 0]),
            ("read_many_files", [1, 0, 0, 0, 0]),
            ("write_file", [0, 1, 0, 0, 0]),
            ("create_file", [0, 1, 0, 0, 0]),
            ("edit_file", [0, 0, 1, 0, 0]),
            ("replace_in_file", [0, 0, 1, 0, 0]),
            ("replace", [0, 0, 1, 0, 0]),
            ("run_command", [0, 0, 0, 1, 0]),
            ("run_shell_command", [0, 0, 0, 1, 0]),
            ("execute_command", [0, 0, 0, 1, 0]),
            ("shell", [0, 0, 0, 1, 0]),
            ("write_todos", [0, 0, 0, 0, 1]),
            // A meta tool stays untracked and moves no counter.
            ("update_topic", [0, 0, 0, 0, 0]),
        ];

        // Both `ToolArgs::record` (success) and `record_invocation` (error) must hit the same counter.
        for status in ["success", "error"] {
            for (name, expected) in cases {
                // Provide a superset of args so every tool passes validation and exercises `ToolArgs::record`.
                let message = assistant(
                    "message-1",
                    "gemini-test",
                    10,
                    json!([{
                        "name": name,
                        "status": status,
                        "args": {
                            "file_path": "/tmp/a.txt",
                            "content": "one",
                            "old_string": "one",
                            "new_string": "two",
                            "command": "true"
                        },
                        "result": [{
                            "functionResponse": { "response": { "output": "one" } }
                        }]
                    }]),
                );

                let parsed = parse_gemini_events_with_diagnostics(
                    session(),
                    vec![message],
                    ParseMode::Full,
                    None,
                )
                .unwrap();
                let counts = &parsed.analysis.records[0].tool_call_counts;
                assert_eq!(
                    [
                        counts.read,
                        counts.write,
                        counts.edit,
                        counts.bash,
                        counts.todo_write
                    ],
                    expected,
                    "{name} ({status}) folded into the wrong metric"
                );
                assert_eq!(
                    parsed.diagnostics.partial_failure_count(),
                    0,
                    "{name} ({status}) must not raise schema drift"
                );
            }
        }
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
            parse_gemini_events_with_diagnostics(session(), vec![drifted], ParseMode::Full, None)
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
            parse_gemini_events_with_diagnostics(session(), vec![empty], ParseMode::Full, None)
                .unwrap();
        assert_eq!(empty.diagnostics.partial_failure_count(), 0);
        assert_eq!(empty.analysis.records[0].tool_call_counts.read, 1);
        assert_eq!(empty.analysis.records[0].total_read_lines, 0);
    }

    #[test]
    fn unknown_only_token_keys_do_not_become_zero_usage() {
        let mut message = assistant("drifted", "gemini-test", 10, json!([]));
        message["tokens"] = json!({ "prompt": 123, "completion": 45 });

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
                .unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert!(
            parsed.analysis.records[0].conversation_usage.is_empty(),
            "unknown token keys must not become a successful all-zero usage row"
        );

        let mut current = assistant("current", "gemini-test", 10, json!([]));
        current["tokens"] = json!({ "input": 0 });
        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![current], ParseMode::Full, None)
                .unwrap();
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(
            parsed.analysis.records[0].conversation_usage["gemini-test"]["input_tokens"],
            0
        );
    }

    #[test]
    fn an_empty_token_object_does_not_become_zero_usage() {
        let mut message = assistant("empty-tokens", "gemini-test", 10, json!([]));
        message["tokens"] = json!({});

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
                .unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert!(
            parsed.analysis.records[0].conversation_usage.is_empty(),
            "an empty token object must not become a successful all-zero usage row"
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
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
                .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.total_edit_lines, 0);
        assert_eq!(record.total_write_lines, 0);

        // An empty `old_string` must remain an edit rather than converting into a write.
        let empty_old = assistant(
            "empty-old-edit",
            "gemini-test",
            10,
            json!([{
                "name": "replace",
                "args": {
                    "file_path": "/tmp/a.txt",
                    "old_string": "",
                    "new_string": "new"
                }
            }]),
        );

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![empty_old], ParseMode::Full, None)
                .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.total_edit_lines, 1);
        assert_eq!(record.total_write_lines, 0);
    }

    #[test]
    fn every_argument_alias_reaches_the_fold() {
        // Assert on folded content, not just counters, because an unresolved alias still increments the invocation count.
        for (old_key, new_key) in [("old_string", "new_string"), ("old_text", "new_text")] {
            let mut args = json!({ "file_path": "/tmp/a.txt" });
            args[old_key] = json!("one");
            args[new_key] = json!("two\nthree");
            let message = assistant(
                "edit",
                "gemini-test",
                10,
                json!([{ "name": "replace", "args": args }]),
            );

            let parsed = parse_gemini_events_with_diagnostics(
                session(),
                vec![message],
                ParseMode::Full,
                None,
            )
            .unwrap();
            let record = &parsed.analysis.records[0];
            assert_eq!(
                parsed.diagnostics.partial_failure_count(),
                0,
                "{old_key}/{new_key} must not raise schema drift"
            );
            assert_eq!(record.tool_call_counts.edit, 1);
            assert_eq!(
                record.total_edit_lines, 2,
                "{new_key} did not reach the fold"
            );
            assert_eq!(
                record.edit_file_details[0].old_string, "one",
                "{old_key} did not reach the fold"
            );
        }

        for command_key in ["command", "cmd"] {
            let mut args = json!({ "description": "list the tree" });
            args[command_key] = json!("ls -la");
            let message = assistant(
                "shell",
                "gemini-test",
                10,
                json!([{ "name": "run_shell_command", "args": args }]),
            );

            let parsed = parse_gemini_events_with_diagnostics(
                session(),
                vec![message],
                ParseMode::Full,
                None,
            )
            .unwrap();
            let record = &parsed.analysis.records[0];
            assert_eq!(
                parsed.diagnostics.partial_failure_count(),
                0,
                "{command_key} must not raise schema drift"
            );
            assert_eq!(record.tool_call_counts.bash, 1);
            assert_eq!(
                record.run_command_details[0].command, "ls -la",
                "{command_key} did not reach the fold"
            );
        }
    }

    #[test]
    fn a_blank_command_is_counted_rather_than_dropped() {
        // `add_run_command` drops blank commands, so they must fail argument parsing and fall back to an invocation count.
        let message = assistant(
            "blank-command",
            "gemini-test",
            10,
            json!([{ "name": "run_shell_command", "args": { "command": "   " } }]),
        );

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
                .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert_eq!(record.tool_call_counts.bash, 1);
        assert!(record.run_command_details.is_empty());
    }

    #[test]
    fn an_unsupported_tool_status_records_nothing() {
        let message = assistant(
            "unsupported-status",
            "gemini-test",
            10,
            json!([{
                "name": "write_file",
                "status": "cancelled",
                "args": { "file_path": "/tmp/a.txt", "content": "one" }
            }]),
        );

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
                .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert_eq!(record.tool_call_counts.write, 0);
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
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
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
            None,
        )
        .unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.analysis.records[0].tool_call_counts.read, 1);
        assert_eq!(parsed.analysis.records[0].total_read_lines, 2);
    }
}
