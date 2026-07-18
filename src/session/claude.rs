//! Parser for Claude Code session logs (`~/.claude/projects/**/*.jsonl`).
//!
//! Claude writes one record per line; assistant records carry token `usage`
//! and `tool_use` blocks, while the file operation results arrive either in a
//! top-level `toolUseResult` field (main sessions) or, for subagent JSONL
//! files that omit it, inside the following user record's
//! `message.content[].tool_result` block. The parser keys those two shapes
//! together by `tool_use_id` and routes both to [`SessionParseState`].
use crate::constants::{FastHashMap, capacity};
use crate::models::*;
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{
    claude_request_context, get_git_remote_url, parse_iso_timestamp, process_claude_usage,
};
use anyhow::Result;
use serde_json::Value;

/// Parse Claude Code session records from a `Vec<Value>` fallback.
///
/// Used by the pretty-printed single-object JSON fallback path in the
/// top-level [`parse_session_file_typed`] dispatcher. Records are moved
/// into the typed iterator form so that the lean [`ClaudeCodeLog`] shape
/// drops unused payloads at deserialisation.
///
/// Records that fail to deserialise into [`ClaudeCodeLog`] are skipped rather
/// than aborting the parse.
///
/// # Errors
///
/// Returns `Err` only if the underlying [`parse_claude_logs`] does; in
/// practice the Claude parse never fails (malformed records are dropped),
/// so this is `Ok` for any input.
///
/// [`parse_session_file_typed`]: crate::session::parser::parse_session_file_typed
pub fn parse_claude_log_values(records: Vec<Value>, mode: ParseMode) -> Result<CodeAnalysis> {
    let iter = records
        .into_iter()
        .filter_map(|value| serde_json::from_value::<ClaudeCodeLog>(value).ok());
    parse_claude_logs(iter, mode)
}

/// Parse Claude Code session records from any iterator of pre-typed logs.
///
/// This is the streaming entry point: callers that read JSONL one line at a
/// time (see [`crate::session::parser::parse_session_file_to_value`]) feed records
/// through here without ever materialising a full `Vec<Value>` of raw JSON.
///
/// The returned [`CodeAnalysis`] always holds exactly one record; the
/// runtime metadata fields (`user`, `extension_name`, …) are left blank here
/// and filled in by the caller's `finalize` step.
///
/// # Errors
///
/// Returns `anyhow::Result` for signature uniformity with the other provider
/// parsers, but the Claude path has no fallible step — every per-record
/// failure is tolerated by skipping that record — so it returns `Ok` for any
/// iterator.
pub fn parse_claude_logs<I>(logs: I, mode: ParseMode) -> Result<CodeAnalysis>
where
    I: IntoIterator<Item = ClaudeCodeLog>,
{
    Ok(parse_claude_logs_with_diagnostics(logs, mode, None)?.analysis)
}

/// Streaming Claude parser with parser-only schema diagnostics.
///
/// `tiers` enables per-request context-tier classification (usage scans
/// only); `None` skips classification entirely.
pub(crate) fn parse_claude_logs_with_diagnostics<I>(
    logs: I,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<ParsedAnalysis>
where
    I: IntoIterator<Item = ClaudeCodeLog>,
{
    let mut classifier = tiers.map(TierClassifier::new);
    let mut state = SessionParseState::with_mode(mode);
    let mut conversation_usage: FastHashMap<String, Value> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);
    // Advisor-message token usage is kept separate from `conversation_usage`
    // so the `analysis` aggregator never attributes the main model's file-op
    // counts to an advisor model. The `usage` path merges this in.
    let mut advisor_usage: FastHashMap<String, Value> = FastHashMap::default();
    // Keep the originating tool name so polymorphic top-level results can be
    // interpreted by lifecycle rather than by ambiguous fields such as
    // `filePath` (which ExitPlanMode also carries).
    let mut pending_tool_uses: FastHashMap<String, PendingClaudeTool> =
        FastHashMap::with_capacity(64);
    let mut diagnostics = ParseDiagnostics::default();

    for log in logs {
        let recognized = matches!(
            log.log_type.as_str(),
            "assistant"
                | "user"
                | "system"
                | "summary"
                | "progress"
                | "file-history-snapshot"
                | "file-history-delta"
                | "queue-operation"
                | "attachment"
                | "bridge-session"
                | "permission-mode"
                | "mode"
                | "last-prompt"
                | "ai-title"
                | "agent-name"
                | "pr-link"
                | "started"
                | "result"
                | "agent-setting"
                | "frame-link"
        ) || log.tool_use_result.is_some();
        if recognized {
            diagnostics.record_recognized_source();
        } else {
            diagnostics.record_unrecognized();
        }
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

        if log.log_type == "assistant" && log.message.is_none() {
            diagnostics.record_relevant(false);
        }

        if log.log_type == "assistant"
            && let Some(message) = &log.message
        {
            if let Some(usage) = &message.usage {
                let model = message.model.as_deref().filter(|model| !model.is_empty());
                let normalized = is_supported_claude_usage(usage) && model.is_some();
                diagnostics.record_relevant(normalized);
                if normalized && let Some(model) = model {
                    // One assistant record is one billed request; classify its
                    // own prompt context against the model's tier threshold.
                    let above = classifier.as_mut().is_some_and(|classifier| {
                        usage.as_object().is_some_and(|usage_obj| {
                            classifier.is_above(model, claude_request_context(usage_obj))
                        })
                    });
                    process_claude_usage(&mut conversation_usage, model, usage, above);

                    // Claude Code's top-level `usage` is the sum of the
                    // `message`-type entries in `usage.iterations` and EXCLUDES any
                    // `advisor_message` iteration (a secondary inference Claude Code
                    // runs but keeps off its own /cost accounting). Capture those
                    // advisor tokens — under the advisor's own model so they price
                    // correctly — in the separate `advisor_usage` map so the
                    // `usage` cost reflects them without the `analysis` aggregator
                    // crediting the advisor with the main model's file operations.
                    if let Some(iters) = usage.get("iterations").and_then(|v| v.as_array()) {
                        for iter in iters {
                            if iter.get("type").and_then(|t| t.as_str()) == Some("advisor_message")
                            {
                                let adv_model =
                                    iter.get("model").and_then(|m| m.as_str()).unwrap_or(model);
                                let normalized =
                                    !adv_model.is_empty() && is_supported_claude_usage(iter);
                                diagnostics.record_relevant(normalized);
                                if normalized {
                                    let above = classifier.as_mut().is_some_and(|classifier| {
                                        iter.as_object().is_some_and(|usage_obj| {
                                            classifier.is_above(
                                                adv_model,
                                                claude_request_context(usage_obj),
                                            )
                                        })
                                    });
                                    process_claude_usage(
                                        &mut advisor_usage,
                                        adv_model,
                                        iter,
                                        above,
                                    );
                                }
                            }
                        }
                    }
                }
            }

            for item in &message.content {
                let ClaudeContentItem::ToolUse { id, name, input } = item else {
                    continue;
                };

                let tracked_file_tool = is_tracked_file_tool(name);
                let input_supported =
                    tracked_file_tool && tracked_tool_input_supported(name, input.as_ref());
                if !id.is_empty() {
                    pending_tool_uses.insert(
                        id.clone(),
                        PendingClaudeTool {
                            name: name.clone(),
                            input: input.clone(),
                            input_supported,
                        },
                    );
                }

                match name.as_str() {
                    "Read" => {
                        if input_supported {
                            diagnostics.record_relevant(true);
                            state.tool_counts.read += 1;
                        } else if id.is_empty() {
                            diagnostics.record_relevant(false);
                        }
                    }
                    "Write" => {
                        if input_supported {
                            diagnostics.record_relevant(true);
                            state.tool_counts.write += 1;
                        } else if id.is_empty() {
                            diagnostics.record_relevant(false);
                        }
                    }
                    "Edit" => {
                        if input_supported {
                            diagnostics.record_relevant(true);
                            state.tool_counts.edit += 1;
                        } else if id.is_empty() {
                            diagnostics.record_relevant(false);
                        }
                    }
                    "TodoWrite" | "TaskCreate" | "TaskUpdate" | "TaskStop" => {
                        diagnostics.record_relevant(true);
                        state.tool_counts.todo_write += 1
                    }
                    "Bash" => {
                        diagnostics
                            .record_relevant(tracked_tool_input_supported(name, input.as_ref()));
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
            let correlated =
                first_tool_result(log.message.as_ref()).and_then(|(tool_use_id, _, is_error)| {
                    pending_tool_uses
                        .remove(tool_use_id)
                        .map(|pending| (pending, is_error))
                });

            if let Some((pending, true)) = correlated.as_ref() {
                if is_tracked_file_tool(&pending.name) && !pending.input_supported {
                    // The provider understood the envelope and explicitly
                    // rejected the bad arguments. No file operation ran.
                    diagnostics.record_relevant(true);
                }
            } else {
                let expected_tool = correlated
                    .as_ref()
                    .map(|(pending, _)| pending.name.as_str());
                match validate_top_level_tool_result_for(tur, expected_tool) {
                    TopLevelToolResult::Irrelevant => {}
                    TopLevelToolResult::Unsupported => diagnostics.record_relevant(false),
                    TopLevelToolResult::NonTextRead => {
                        diagnostics.record_relevant(true);
                        if let Some(path) = correlated
                            .as_ref()
                            .and_then(|(pending, _)| pending.input.as_ref())
                            .and_then(|input| input.file_path.as_deref())
                        {
                            state.add_non_text_read_path(path);
                        }
                    }
                    TopLevelToolResult::Supported(kind) => {
                        diagnostics.record_relevant(true);
                        dispatch_top_level_tool_result(&mut state, tur, kind, ts);
                    }
                }
            }
        } else if log.log_type == "user"
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
                    is_error,
                } = item
                else {
                    continue;
                };
                let Some(pending) = pending_tool_uses.remove(tool_use_id) else {
                    continue;
                };

                if *is_error {
                    if is_tracked_file_tool(&pending.name) && !pending.input_supported {
                        diagnostics.record_relevant(true);
                    }
                    continue;
                }
                if !log.is_sidechain || !is_tracked_file_tool(&pending.name) {
                    continue;
                }

                let normalized = pending.input_supported;
                diagnostics.record_relevant(normalized);
                if normalized && let Some(input) = pending.input.as_ref() {
                    dispatch_subagent_tool_result(&mut state, &pending.name, input, content, ts);
                }
            }
        }
    }

    for pending in pending_tool_uses.into_values() {
        if is_tracked_file_tool(&pending.name) && !pending.input_supported {
            diagnostics.record_relevant(false);
        }
    }

    if state.git_remote.is_empty() {
        state.git_remote = get_git_remote_url(&state.folder_path);
    }

    let mut record = state.into_record(conversation_usage);
    record.advisor_usage = advisor_usage;

    let analysis = CodeAnalysis {
        user: String::new(),
        extension_name: String::new(),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    };
    Ok(ParsedAnalysis::new(analysis, diagnostics))
}

#[derive(Clone)]
struct PendingClaudeTool {
    name: String,
    input: Option<ClaudeToolInput>,
    input_supported: bool,
}

fn is_tracked_file_tool(name: &str) -> bool {
    matches!(name, "Read" | "Write" | "Edit")
}

fn first_tool_result(message: Option<&ClaudeMessage>) -> Option<(&str, &str, bool)> {
    message?.content.iter().find_map(|item| {
        let ClaudeContentItem::ToolResult {
            tool_use_id,
            content,
            is_error,
        } = item
        else {
            return None;
        };
        Some((tool_use_id.as_str(), content.as_str(), *is_error))
    })
}

fn tracked_tool_input_supported(name: &str, input: Option<&ClaudeToolInput>) -> bool {
    let Some(input) = input else {
        return false;
    };
    let has_path = input
        .file_path
        .as_deref()
        .is_some_and(|path| !path.trim().is_empty());

    match name {
        "Read" => has_path,
        "Write" => has_path && input.content.is_some(),
        "Edit" => has_path && input.old_string.is_some() && input.new_string.is_some(),
        "Bash" => input
            .command
            .as_deref()
            .is_some_and(|command| !command.trim().is_empty()),
        _ => false,
    }
}

fn is_supported_claude_usage(usage: &Value) -> bool {
    let Some(usage) = usage.as_object() else {
        return false;
    };
    if usage.is_empty() {
        return true;
    }

    let mut recognized = false;
    for key in [
        "input_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "output_tokens",
    ] {
        if let Some(value) = usage.get(key) {
            if value.as_i64().is_none() {
                return false;
            }
            recognized = true;
        }
    }

    for (object_key, numeric_keys) in [
        (
            "cache_creation",
            &["ephemeral_5m_input_tokens", "ephemeral_1h_input_tokens"][..],
        ),
        (
            "server_tool_use",
            &["web_search_requests", "web_fetch_requests"][..],
        ),
    ] {
        let Some(nested) = usage.get(object_key) else {
            continue;
        };
        let Some(nested) = nested.as_object() else {
            return false;
        };
        if nested.is_empty() {
            recognized = true;
            continue;
        }
        for key in numeric_keys {
            if let Some(value) = nested.get(*key) {
                if value.as_i64().is_none() {
                    return false;
                }
                recognized = true;
            }
        }
    }

    recognized
}

#[derive(Clone, Copy)]
enum FileToolResultKind {
    Read,
    Write,
    Edit,
}

enum TopLevelToolResult {
    Irrelevant,
    Unsupported,
    NonTextRead,
    Supported(FileToolResultKind),
}

#[cfg(test)]
fn validate_top_level_tool_result(result: &ClaudeToolUseResult) -> TopLevelToolResult {
    validate_top_level_tool_result_for(result, None)
}

fn validate_top_level_tool_result_for(
    result: &ClaudeToolUseResult,
    expected_tool: Option<&str>,
) -> TopLevelToolResult {
    if let Some(expected_tool) = expected_tool {
        return match expected_tool {
            "Read" => validate_read_result(result),
            "Write" => validate_write_result(result),
            "Edit" => validate_edit_result(result),
            _ => TopLevelToolResult::Irrelevant,
        };
    }

    if result.result_type.as_deref() == Some("image") {
        return if result.file.is_some() {
            TopLevelToolResult::NonTextRead
        } else {
            TopLevelToolResult::Unsupported
        };
    }
    if result.result_type.as_deref() == Some("text") || result.file.is_some() {
        return validate_read_result(result);
    }
    if matches!(result.result_type.as_deref(), Some("create" | "update"))
        || (result.file_path.is_some() && result.content.is_some())
    {
        return validate_write_result(result);
    }
    if result.file_path.is_some() || result.old_string.is_some() || result.new_string.is_some() {
        // ExitPlanMode carries a plan file path but no file-operation body.
        if result.result_type.is_none()
            && result.content.is_none()
            && result.old_string.is_none()
            && result.new_string.is_none()
        {
            return TopLevelToolResult::Irrelevant;
        }
        return validate_edit_result(result);
    }

    TopLevelToolResult::Irrelevant
}

fn validate_read_result(result: &ClaudeToolUseResult) -> TopLevelToolResult {
    if result.result_type.as_deref() == Some("image") {
        return if result.file.is_some() {
            TopLevelToolResult::NonTextRead
        } else {
            TopLevelToolResult::Unsupported
        };
    }

    if result.result_type.as_deref() == Some("text") || result.file.is_some() {
        let supported = result.result_type.as_deref() == Some("text")
            && result.file.as_ref().is_some_and(|file| {
                file.file_path
                    .as_deref()
                    .is_some_and(|path| !path.trim().is_empty())
                    && file.content.is_some()
            });
        return if supported {
            TopLevelToolResult::Supported(FileToolResultKind::Read)
        } else {
            TopLevelToolResult::Unsupported
        };
    }

    TopLevelToolResult::Unsupported
}

fn validate_write_result(result: &ClaudeToolUseResult) -> TopLevelToolResult {
    if matches!(result.result_type.as_deref(), Some("create" | "update"))
        || (result.file_path.is_some() && result.content.is_some())
    {
        let supported = matches!(result.result_type.as_deref(), Some("create" | "update"))
            && result
                .file_path
                .as_deref()
                .is_some_and(|path| !path.trim().is_empty())
            && result.content.is_some();
        return if supported {
            TopLevelToolResult::Supported(FileToolResultKind::Write)
        } else {
            TopLevelToolResult::Unsupported
        };
    }

    TopLevelToolResult::Unsupported
}

fn validate_edit_result(result: &ClaudeToolUseResult) -> TopLevelToolResult {
    if result.file_path.is_some() || result.old_string.is_some() || result.new_string.is_some() {
        let supported = result
            .file_path
            .as_deref()
            .is_some_and(|path| !path.trim().is_empty())
            && result.old_string.is_some()
            && result.new_string.is_some();
        return if supported {
            TopLevelToolResult::Supported(FileToolResultKind::Edit)
        } else {
            TopLevelToolResult::Unsupported
        };
    }

    TopLevelToolResult::Unsupported
}

fn dispatch_top_level_tool_result(
    state: &mut SessionParseState,
    result: &ClaudeToolUseResult,
    kind: FileToolResultKind,
    ts: i64,
) {
    match kind {
        FileToolResultKind::Read => {
            let file = result.file.as_ref().expect("validated read result");
            let file_path = file.file_path.as_deref().expect("validated read path");
            let content = file.content.as_deref().expect("validated read content");
            preserve_file_tool_counts(state, |state| {
                state.add_read_detail(file_path, content, ts);
            });
        }
        FileToolResultKind::Write => {
            let file_path = result.file_path.as_deref().expect("validated write path");
            let content = result.content.as_deref().expect("validated write content");
            preserve_file_tool_counts(state, |state| {
                state.add_write_detail(file_path, content, ts);
            });
        }
        FileToolResultKind::Edit => {
            let file_path = result.file_path.as_deref().expect("validated edit path");
            let old_string = result.old_string.as_deref().expect("validated old string");
            let new_string = result.new_string.as_deref().expect("validated new string");
            preserve_file_tool_counts(state, |state| {
                state.add_edit_detail(file_path, old_string, new_string, ts);
            });
        }
    }
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
            preserve_file_tool_counts(state, |state| {
                state.add_read_detail(file_path, result_content, ts);
            });
        }
        "Write" => {
            // Write's input carries the full file body it intended to write.
            let body = input.content.as_deref().unwrap_or("");
            preserve_file_tool_counts(state, |state| {
                state.add_write_detail(file_path, body, ts);
            });
        }
        "Edit" => {
            let new_string = input.new_string.as_deref().unwrap_or("");
            let old_string = input.old_string.as_deref().unwrap_or("");
            preserve_file_tool_counts(state, |state| {
                state.add_edit_detail(file_path, old_string, new_string, ts);
            });
        }
        _ => {}
    }
}

/// Adds file-operation details without counting a second tool invocation.
fn preserve_file_tool_counts(
    state: &mut SessionParseState,
    add_detail: impl FnOnce(&mut SessionParseState),
) {
    let counts = (
        state.tool_counts.read,
        state.tool_counts.write,
        state.tool_counts.edit,
    );
    add_detail(state);
    state.tool_counts.read = counts.0;
    state.tool_counts.write = counts.1;
    state.tool_counts.edit = counts.2;
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
    fn per_request_tier_classification_splits_above_slice() {
        let usage_log = |ts: &str, input: i64, cache_read: i64, output: i64| -> ClaudeCodeLog {
            let raw = serde_json::json!({
                "type": "assistant",
                "timestamp": ts,
                "message": {
                    "model": "claude-sonnet-5",
                    "usage": {
                        "input_tokens": input,
                        "cache_read_input_tokens": cache_read,
                        "output_tokens": output
                    },
                    "content": []
                }
            });
            serde_json::from_value(raw).unwrap()
        };
        // One request below the 200k threshold, one above it.
        let logs = vec![
            usage_log("2026-07-01T00:00:00Z", 1_000, 50_000, 200),
            usage_log("2026-07-01T00:01:00Z", 2_000, 220_000, 300),
        ];

        let tiers = crate::pricing::TierThresholds::from_entries(
            [("claude-sonnet-5", 200_000)].into_iter(),
        );
        let parsed =
            parse_claude_logs_with_diagnostics(logs.clone(), ParseMode::UsageOnly, Some(&tiers))
                .unwrap();
        let usage = &parsed.analysis.records[0].conversation_usage["claude-sonnet-5"];
        // Totals cover both requests; the above_tier slice only the second.
        assert_eq!(usage["input_tokens"], 3_000);
        assert_eq!(usage["output_tokens"], 500);
        assert_eq!(usage["above_tier"]["input_tokens"], 2_000);
        assert_eq!(usage["above_tier"]["cache_read_tokens"], 220_000);
        assert_eq!(usage["above_tier"]["output_tokens"], 300);

        // Without thresholds nothing is classified and the shape is unchanged.
        let parsed = parse_claude_logs_with_diagnostics(logs, ParseMode::UsageOnly, None).unwrap();
        let usage = &parsed.analysis.records[0].conversation_usage["claude-sonnet-5"];
        assert!(usage.get("above_tier").is_none());
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
        assert_eq!(record.tool_call_counts.read, 1);
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

    #[test]
    fn current_task_mutations_count_as_todo_writes() {
        let log = assistant_log(
            "2026-07-12T00:00:00Z",
            "claude-opus-4-7",
            serde_json::json!([
                { "type": "tool_use", "id": "task-1", "name": "TaskCreate", "input": {} },
                { "type": "tool_use", "id": "task-2", "name": "TaskUpdate", "input": {} },
                { "type": "tool_use", "id": "task-3", "name": "TaskStop", "input": {} }
            ]),
        );

        let analysis = parse_claude_logs(vec![log], ParseMode::Full).unwrap();
        assert_eq!(analysis.records[0].tool_call_counts.todo_write, 3);
    }

    #[test]
    fn update_write_results_recover_details_without_double_counting() {
        for original_file in [serde_json::Value::Null, serde_json::json!("old body")] {
            let assistant = assistant_log(
                "2026-07-12T00:00:00Z",
                "claude-opus-4-7",
                serde_json::json!([{
                    "type": "tool_use",
                    "id": "write-1",
                    "name": "Write",
                    "input": { "file_path": "/tmp/update.txt", "content": "one\ntwo\n" }
                }]),
            );
            let user: ClaudeCodeLog = serde_json::from_value(serde_json::json!({
                "type": "user",
                "timestamp": "2026-07-12T00:00:01Z",
                "message": {
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "write-1",
                        "content": "updated"
                    }]
                },
                "toolUseResult": {
                    "type": "update",
                    "filePath": "/tmp/update.txt",
                    "content": "one\ntwo\n",
                    "originalFile": original_file,
                    "structuredPatch": []
                }
            }))
            .unwrap();

            let parsed =
                parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full, None)
                    .unwrap();
            assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
            let record = &parsed.analysis.records[0];
            assert_eq!(record.tool_call_counts.write, 1);
            assert_eq!(record.total_write_lines, 2);
            assert_eq!(record.total_unique_files, 1);
            assert_eq!(record.write_file_details.len(), 1);
        }
    }

    #[test]
    fn image_read_result_is_a_successful_zero_line_read() {
        let assistant = assistant_log(
            "2026-07-12T00:00:00Z",
            "claude-opus-4-7",
            serde_json::json!([{
                "type": "tool_use",
                "id": "read-image",
                "name": "Read",
                "input": { "file_path": "/tmp/image.png" }
            }]),
        );
        let user: ClaudeCodeLog = serde_json::from_value(serde_json::json!({
            "type": "user",
            "timestamp": "2026-07-12T00:00:01Z",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "read-image",
                    "content": [{ "type": "image", "source": { "type": "base64" } }]
                }]
            },
            "toolUseResult": {
                "type": "image",
                "file": {
                    "type": "image/png",
                    "base64": "AA==",
                    "originalSize": 1,
                    "dimensions": { "width": 1, "height": 1 }
                }
            }
        }))
        .unwrap();

        let parsed =
            parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full, None).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 0);
        assert_eq!(record.total_unique_files, 1);
        assert!(record.read_file_details.is_empty());
    }

    #[test]
    fn exit_plan_mode_file_path_is_not_a_file_operation() {
        let assistant = assistant_log(
            "2026-07-12T00:00:00Z",
            "claude-opus-4-7",
            serde_json::json!([{
                "type": "tool_use",
                "id": "plan-1",
                "name": "ExitPlanMode",
                "input": { "plan": "steps", "planFilePath": "/tmp/plan.md" }
            }]),
        );
        let user: ClaudeCodeLog = serde_json::from_value(serde_json::json!({
            "type": "user",
            "timestamp": "2026-07-12T00:00:01Z",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "plan-1",
                    "content": "approved"
                }]
            },
            "toolUseResult": {
                "filePath": "/tmp/plan.md",
                "plan": "steps",
                "isAgent": false,
                "hasTaskTool": true,
                "planWasEdited": false
            }
        }))
        .unwrap();

        let parsed =
            parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full, None).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.read, 0);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.tool_call_counts.edit, 0);
        assert_eq!(record.tool_call_counts.todo_write, 0);
        assert_eq!(record.tool_call_counts.bash, 0);
        assert_eq!(record.total_unique_files, 0);
    }

    #[test]
    fn explicitly_errored_invalid_reads_are_metric_free_and_supported() {
        for input in [
            serde_json::json!({}),
            serde_json::json!({ "__unparsedToolInput": { "file_path": 42 } }),
        ] {
            let assistant = assistant_log(
                "2026-07-12T00:00:00Z",
                "claude-opus-4-7",
                serde_json::json!([{
                    "type": "tool_use",
                    "id": "bad-read",
                    "name": "Read",
                    "input": input
                }]),
            );
            let user = user_log(
                "2026-07-12T00:00:01Z",
                serde_json::json!([{
                    "type": "tool_result",
                    "tool_use_id": "bad-read",
                    "content": "input validation failed",
                    "is_error": true
                }]),
            );

            let parsed =
                parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full, None)
                    .unwrap();
            assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
            assert!(!parsed.diagnostics.is_complete_failure());
            assert_eq!(parsed.analysis.records[0].tool_call_counts.read, 0);
        }
    }

    #[test]
    fn unresolved_invalid_read_remains_a_schema_failure() {
        let assistant = assistant_log(
            "2026-07-12T00:00:00Z",
            "claude-opus-4-7",
            serde_json::json!([{
                "type": "tool_use",
                "id": "unresolved-read",
                "name": "Read",
                "input": {}
            }]),
        );

        let parsed =
            parse_claude_logs_with_diagnostics([assistant], ParseMode::Full, None).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert_eq!(parsed.analysis.records[0].tool_call_counts.read, 0);
    }

    #[test]
    fn tracked_tool_validation_checks_required_fields_and_allows_empty_bodies() {
        let input = |value| serde_json::from_value::<ClaudeToolInput>(value).unwrap();

        assert!(tracked_tool_input_supported(
            "Read",
            Some(&input(serde_json::json!({ "file_path": "/tmp/a" })))
        ));
        assert!(!tracked_tool_input_supported(
            "Read",
            Some(&input(serde_json::json!({ "future_path": "/tmp/a" })))
        ));
        assert!(tracked_tool_input_supported(
            "Write",
            Some(&input(
                serde_json::json!({ "file_path": "/tmp/a", "content": "" })
            ))
        ));
        assert!(!tracked_tool_input_supported(
            "Write",
            Some(&input(
                serde_json::json!({ "file_path": "/tmp/a", "future_content": "" })
            ))
        ));
        assert!(tracked_tool_input_supported(
            "Edit",
            Some(&input(serde_json::json!({
                "file_path": "/tmp/a",
                "old_string": "",
                "new_string": ""
            })))
        ));
        assert!(!tracked_tool_input_supported(
            "Edit",
            Some(&input(serde_json::json!({
                "file_path": "/tmp/a",
                "future_old": "",
                "new_string": ""
            })))
        ));
        assert!(tracked_tool_input_supported(
            "Bash",
            Some(&input(serde_json::json!({ "command": "true" })))
        ));
        assert!(!tracked_tool_input_supported(
            "Bash",
            Some(&input(serde_json::json!({ "future_command": "true" })))
        ));

        let result = |value| serde_json::from_value::<ClaudeToolUseResult>(value).unwrap();
        assert!(matches!(
            validate_top_level_tool_result(&result(serde_json::json!({
                "type": "text",
                "file": { "filePath": "/tmp/a", "content": "" }
            }))),
            TopLevelToolResult::Supported(FileToolResultKind::Read)
        ));
        assert!(matches!(
            validate_top_level_tool_result(&result(serde_json::json!({
                "type": "create",
                "filePath": "/tmp/a",
                "content": ""
            }))),
            TopLevelToolResult::Supported(FileToolResultKind::Write)
        ));
        assert!(matches!(
            validate_top_level_tool_result(&result(serde_json::json!({
                "type": "update",
                "filePath": "/tmp/a",
                "content": "updated"
            }))),
            TopLevelToolResult::Supported(FileToolResultKind::Write)
        ));
        assert!(matches!(
            validate_top_level_tool_result(&result(serde_json::json!({
                "filePath": "/tmp/a",
                "oldString": "",
                "newString": ""
            }))),
            TopLevelToolResult::Supported(FileToolResultKind::Edit)
        ));
        assert!(matches!(
            validate_top_level_tool_result(&result(serde_json::json!({
                "type": "text",
                "futureFile": { "filePath": "/tmp/a", "content": "text" }
            }))),
            TopLevelToolResult::Unsupported
        ));
    }

    #[test]
    fn usage_validation_rejects_unknown_only_and_wrong_typed_token_payloads() {
        assert!(is_supported_claude_usage(&serde_json::json!({})));
        assert!(is_supported_claude_usage(&serde_json::json!({
            "input_tokens": 0
        })));
        assert!(is_supported_claude_usage(&serde_json::json!({
            "cache_creation": {}
        })));
        assert!(is_supported_claude_usage(&serde_json::json!({
            "output_tokens": 4,
            "future_metric": 9
        })));
        assert!(is_supported_claude_usage(&serde_json::json!({
            "server_tool_use": { "web_search_requests": 0, "future_request": 3 }
        })));

        assert!(!is_supported_claude_usage(&serde_json::json!({
            "prompt_tokens": 4,
            "completion_tokens": 2
        })));
        assert!(!is_supported_claude_usage(&serde_json::json!({
            "input_tokens": "4"
        })));
        assert!(!is_supported_claude_usage(&serde_json::json!({
            "input_tokens": 4,
            "cache_creation": { "ephemeral_5m_input_tokens": null }
        })));
    }

    #[test]
    fn unknown_only_usage_is_diagnosed_without_creating_a_zero_row() {
        let log: ClaudeCodeLog = serde_json::from_value(serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-07-12T00:00:00Z",
            "message": {
                "model": "claude-opus-4-7",
                "usage": { "prompt_tokens": 4, "completion_tokens": 2 },
                "content": []
            }
        }))
        .unwrap();

        let parsed = parse_claude_logs_with_diagnostics([log], ParseMode::Full, None).unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }

    #[test]
    fn advisor_message_usage_is_separated_from_conversation_usage() {
        // Top-level usage already sums the `message`-type iterations and omits
        // the `advisor_message` one. The advisor tokens land in `advisor_usage`
        // (under the advisor's own model), NOT in `conversation_usage`, so the
        // analysis aggregator never credits the advisor with the main model's
        // file operations. The advisor here uses a *different* model than the
        // main turn to make the separation observable.
        let raw = serde_json::json!({
            "type": "assistant",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "model": "claude-haiku-4-5",
                "content": [],
                "usage": {
                    "input_tokens": 4,
                    "output_tokens": 7440,
                    "cache_read_input_tokens": 68709,
                    "cache_creation_input_tokens": 18687,
                    "iterations": [
                        { "type": "message", "input_tokens": 2, "output_tokens": 6397 },
                        { "type": "advisor_message", "model": "claude-opus-4-8",
                          "input_tokens": 47579, "output_tokens": 10521 },
                        { "type": "message", "input_tokens": 2, "output_tokens": 1043 }
                    ]
                }
            }
        });
        let log: ClaudeCodeLog = serde_json::from_value(raw).unwrap();
        let analysis = parse_claude_logs(vec![log], ParseMode::Full).unwrap();
        let record = &analysis.records[0];

        // `conversation_usage` (what `analysis` reads) carries only the main
        // model with its top-level totals — no advisor key.
        let conv = &record.conversation_usage;
        assert_eq!(conv.len(), 1);
        let main = conv.get("claude-haiku-4-5").unwrap();
        assert_eq!(main["input_tokens"].as_i64().unwrap(), 4);
        assert_eq!(main["output_tokens"].as_i64().unwrap(), 7440);
        assert!(conv.get("claude-opus-4-8").is_none());

        // `advisor_usage` (what `usage` merges) carries the advisor tokens
        // under its own model for correct pricing.
        let advisor = record.advisor_usage.get("claude-opus-4-8").unwrap();
        assert_eq!(advisor["input_tokens"].as_i64().unwrap(), 47579);
        assert_eq!(advisor["output_tokens"].as_i64().unwrap(), 10521);
    }

    #[test]
    fn unknown_only_advisor_usage_does_not_create_a_zero_row() {
        let raw = serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-07-12T00:00:00Z",
            "message": {
                "model": "claude-haiku-4-5",
                "content": [],
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 1,
                    "iterations": [{
                        "type": "advisor_message",
                        "model": "claude-opus-4-7",
                        "prompt_tokens": 10,
                        "completion_tokens": 2
                    }]
                }
            }
        });
        let log: ClaudeCodeLog = serde_json::from_value(raw).unwrap();
        let parsed = parse_claude_logs_with_diagnostics([log], ParseMode::Full, None).unwrap();

        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert!(parsed.analysis.records[0].advisor_usage.is_empty());
        assert_eq!(parsed.analysis.records[0].conversation_usage.len(), 1);
    }

    #[test]
    fn message_only_iterations_leave_advisor_usage_empty() {
        // Without an advisor_message iteration, usage equals the top-level
        // values and `advisor_usage` stays empty.
        let raw = serde_json::json!({
            "type": "assistant",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "model": "claude-opus-4-8",
                "content": [],
                "usage": {
                    "input_tokens": 6527,
                    "output_tokens": 764,
                    "iterations": [
                        { "type": "message", "input_tokens": 6527, "output_tokens": 764 }
                    ]
                }
            }
        });
        let log: ClaudeCodeLog = serde_json::from_value(raw).unwrap();
        let analysis = parse_claude_logs(vec![log], ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.conversation_usage.len(), 1);
        assert!(record.advisor_usage.is_empty());
        let main = record.conversation_usage.get("claude-opus-4-8").unwrap();
        assert_eq!(main["input_tokens"].as_i64().unwrap(), 6527);
        assert_eq!(main["output_tokens"].as_i64().unwrap(), 764);
    }
}
