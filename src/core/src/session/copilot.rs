//! Parser for GitHub Copilot CLI session events (`~/.copilot/session-state/<sessionId>/events.jsonl`).
//!
//! Token usage comes from `session.shutdown` when present, falling back to streamed `assistant.message.outputTokens` (with `input_tokens: 0`) for sessions that did not shut down cleanly. File operations are paired across `tool.execution_start` and `tool.execution_complete` by `toolCallId` and counted only on success. Legacy single-object dumps under `~/.copilot/history-session-state/` are not supported.
use crate::constants::{FastHashMap, capacity};
use crate::models::*;
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_git_remote_url, parse_iso_timestamp};
use anyhow::Result;
use serde_json::{Value, json};

/// Parses Copilot CLI session `events` into a single-record [`CodeAnalysis`].
///
/// Stamps `extension_name` as `"Copilot-CLI"` and leaves the remaining runtime metadata fields blank. When no `session.shutdown` event is present, per-model usage falls back to streamed `outputTokens` with `input_tokens: 0`.
///
/// # Errors
///
/// Never returns `Err`; returns `Result` for parity with other provider parsers.
pub fn parse_copilot_events<I>(events: I, mode: ParseMode) -> Result<CodeAnalysis>
where
    I: IntoIterator<Item = CopilotEvent>,
{
    Ok(parse_copilot_events_with_diagnostics(events, mode)?.analysis)
}

/// Parses Copilot CLI session `events` with schema diagnostics.
pub(crate) fn parse_copilot_events_with_diagnostics<I>(
    events: I,
    mode: ParseMode,
) -> Result<ParsedAnalysis>
where
    I: IntoIterator<Item = CopilotEvent>,
{
    let mut state = SessionParseState::with_mode(mode);
    let mut conversation_usage: FastHashMap<String, Value> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);
    // Pending tool calls indexed by `toolCallId`.
    let mut pending_tools: FastHashMap<String, PendingTool> = FastHashMap::with_capacity(32);

    // Active model for fallback `assistant.message.outputTokens` attribution when `session.shutdown` never arrives.
    let mut current_model = String::new();
    // Guards against the fallback overwriting authoritative `session.shutdown` rows on the same canonicalized model key.
    let mut shutdown_seen = false;
    let mut pending_output_tokens: FastHashMap<String, i64> = FastHashMap::with_capacity(3);
    let mut diagnostics = ParseDiagnostics::default();

    for event in events {
        let recognized = matches!(
            event.event_type.as_str(),
            "session.start"
                | "session.model_change"
                | "session.task_complete"
                | "session.shutdown"
                | "session.info"
                | "session.mode_changed"
                | "system.message"
                | "user.message"
                | "assistant.message"
                | "assistant.turn_start"
                | "assistant.turn_end"
                | "tool.execution_start"
                | "tool.execution_complete"
                | "hook.start"
                | "hook.end"
                | "abort"
                | "subagent.started"
                | "subagent.completed"
                | "system.notification"
                | "session.resume"
        );
        if recognized {
            diagnostics.record_recognized_source();
        } else {
            diagnostics.record_unrecognized();
        }
        let ts = parse_iso_timestamp(&event.timestamp);
        if ts > state.last_ts {
            state.last_ts = ts;
        }

        match event.event_type.as_str() {
            "session.start" => {
                if let Ok(data) =
                    serde_json::from_value::<CopilotSessionStartData>(event.data.clone())
                {
                    if state.task_id.is_empty() && !data.session_id.is_empty() {
                        state.task_id = data.session_id;
                    }
                    if let Some(ctx) = data.context {
                        if state.folder_path.is_empty() {
                            if !ctx.cwd.is_empty() {
                                state.folder_path = ctx.cwd;
                            } else if !ctx.git_root.is_empty() {
                                state.folder_path = ctx.git_root;
                            }
                        }
                        if state.git_remote.is_empty() {
                            state.git_remote =
                                build_remote_url(&ctx.repository_host, &ctx.repository);
                        }
                    }
                }
            }
            "session.model_change" => {
                let new_model = event
                    .data
                    .get("newModel")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                if let Some(new_model) = new_model {
                    current_model = canonicalize_model_name(new_model);
                }
            }
            "session.shutdown" => {
                let payload_supported = shutdown_payload_supported(&event.data);
                if payload_supported
                    && let Ok(data) =
                        serde_json::from_value::<CopilotShutdownData>(event.data.clone())
                {
                    diagnostics.record_relevant(true);
                    for (model, metric) in data.model_metrics {
                        if model.is_empty() {
                            continue;
                        }
                        let Some(usage) = metric.usage else {
                            continue;
                        };
                        // Copilot's `outputTokens` includes `reasoningTokens`, whereas the flat usage shape keeps output and reasoning disjoint.
                        let output_tokens =
                            usage.output_tokens.saturating_sub(usage.reasoning_tokens);
                        let usage_json = json!({
                            "input_tokens": usage.input_tokens,
                            "output_tokens": output_tokens,
                            "reasoning_output_tokens": usage.reasoning_tokens,
                            "cache_read_input_tokens": usage.cache_read_tokens,
                            "cache_creation_input_tokens": usage.cache_write_tokens,
                        });
                        conversation_usage.insert(canonicalize_model_name(&model), usage_json);
                    }
                    shutdown_seen = true;
                } else {
                    diagnostics.record_relevant(false);
                }
            }
            "assistant.message" => {
                // Only used as a fallback when no `session.shutdown` arrives.
                if let Some(output_tokens) = event.data.get("outputTokens") {
                    let output_tokens = output_tokens.as_i64();
                    diagnostics
                        .record_relevant(output_tokens.is_some() && !current_model.is_empty());
                    if let Some(output_tokens) = output_tokens.filter(|&t| t > 0)
                        && !current_model.is_empty()
                    {
                        *pending_output_tokens
                            .entry(current_model.clone())
                            .or_insert(0) += output_tokens;
                    }
                }
            }
            "tool.execution_start" => {
                match serde_json::from_value::<CopilotToolStartData>(event.data.clone()) {
                    Ok(data) if !data.tool_call_id.is_empty() && !data.tool_name.is_empty() => {
                        pending_tools.insert(
                            data.tool_call_id,
                            PendingTool {
                                tool: TrackedTool::from_name(&data.tool_name),
                                arguments: data.arguments,
                                timestamp: ts,
                            },
                        );
                    }
                    Ok(data) => {
                        diagnostics.record_relevant(false);
                        // Keep the `toolCallId` registered so its completion pairs cleanly instead of double-counting the drift as an orphan.
                        if !data.tool_call_id.is_empty() {
                            pending_tools.insert(
                                data.tool_call_id,
                                PendingTool {
                                    tool: None,
                                    arguments: Value::Null,
                                    timestamp: ts,
                                },
                            );
                        }
                    }
                    Err(_) => diagnostics.record_relevant(false),
                }
            }
            "tool.execution_complete" => {
                let Some(tool_call_id) = event
                    .data
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                else {
                    diagnostics.record_relevant(false);
                    continue;
                };
                let Some(pending) = pending_tools.remove(tool_call_id) else {
                    // Only successful orphans count as drift because failed tool calls are ignored even when paired.
                    if event.data.get("success").and_then(Value::as_bool) == Some(true) {
                        diagnostics.record_relevant(false);
                    }
                    continue;
                };
                let Some(success) = event.data.get("success").and_then(Value::as_bool) else {
                    if pending.tool.is_some() {
                        diagnostics.record_relevant(false);
                    }
                    continue;
                };
                // Failed tool calls do not apply file operations; count them as valid lifecycle records without dispatching.
                if !success {
                    if pending.tool.is_some() {
                        diagnostics.record_relevant(true);
                    }
                    continue;
                }
                let data =
                    match serde_json::from_value::<CopilotToolCompleteData>(event.data.clone()) {
                        Ok(data) => data,
                        Err(_) => {
                            if pending.tool.is_some() {
                                diagnostics.record_relevant(false);
                            }
                            continue;
                        }
                    };
                let folded = pending
                    .tool
                    .and_then(|tool| tool.read_args(&pending.arguments, &data.result));
                if pending.tool.is_some() {
                    diagnostics.record_relevant(folded.as_ref().is_some_and(ToolArgs::is_complete));
                }
                if let Some(folded) = folded {
                    folded.record(&mut state, pending.timestamp);
                }
            }
            _ => {}
        }
    }

    if !shutdown_seen {
        for (model, output_tokens) in pending_output_tokens {
            conversation_usage.insert(
                model,
                json!({
                    "input_tokens": 0,
                    "output_tokens": output_tokens,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                }),
            );
        }
    }

    if state.git_remote.is_empty() {
        state.git_remote = get_git_remote_url(&state.folder_path);
    }

    let record = state.into_record(conversation_usage);

    let analysis = CodeAnalysis {
        user: String::new(),
        extension_name: String::from("Copilot-CLI"),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    };
    Ok(ParsedAnalysis::new(analysis, diagnostics))
}

fn shutdown_payload_supported(data: &Value) -> bool {
    let Some(metrics) = data.get("modelMetrics").and_then(Value::as_object) else {
        return false;
    };
    metrics.values().all(|metric| {
        let Some(metric) = metric.as_object() else {
            return false;
        };
        match metric.get("usage") {
            None | Some(Value::Null) => true,
            Some(usage) => copilot_usage_supported(usage),
        }
    })
}

fn copilot_usage_supported(usage: &Value) -> bool {
    const TOKEN_FIELDS: &[&str] = &[
        "inputTokens",
        "outputTokens",
        "cacheReadTokens",
        "cacheWriteTokens",
        "reasoningTokens",
    ];
    let Some(usage) = usage.as_object() else {
        return false;
    };
    if usage.is_empty() {
        return true;
    }

    let mut recognized = false;
    for field in TOKEN_FIELDS {
        if let Some(value) = usage.get(*field) {
            recognized = true;
            if value.as_i64().is_none() {
                return false;
            }
        }
    }
    recognized
}

/// Tracked Copilot CLI tool category, resolved from the log's tool name.
#[derive(Clone, Copy)]
enum TrackedTool {
    Read,
    Search,
    Write,
    Edit,
    ApplyPatch,
    Shell,
    WriteBash,
}

impl TrackedTool {
    /// Maps a Copilot tool `name` to its tracked category, or `None` for untracked tools whose payloads are ignored.
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "view" | "show_file" | "read_file" => Self::Read,
            // Search/web tools count as read invocations without line totals because they do not return a single file body.
            "rg" | "grep" | "glob" | "web_search" | "web_fetch" => Self::Search,
            "create" | "write_file" | "write" => Self::Write,
            "str_replace" | "edit" | "replace" | "edit_file" => Self::Edit,
            "apply_patch" => Self::ApplyPatch,
            "bash" | "shell" | "execute" => Self::Shell,
            "write_bash" => Self::WriteBash,
            _ => return None,
        })
    }

    /// Extracts the foldable payload for a completed tool call, returning `None` if required arguments are missing or unreadable (schema drift).
    fn read_args<'a>(self, args: &'a Value, result: &Value) -> Option<ToolArgs<'a>> {
        Some(match self {
            Self::Read => ToolArgs::Read {
                path: arg_path(args)?,
                content: read_view_content(args, result),
            },
            Self::Search => ToolArgs::Search,
            Self::Write => ToolArgs::Write {
                path: arg_path(args)?,
                content: arg_str(args, &["file_text", "content"])?,
            },
            Self::Edit => ToolArgs::Edit {
                path: arg_path(args)?,
                old_string: arg_str(args, &["old_string", "old_str", "old_text"])?,
                new_string: arg_str(args, &["new_string", "new_str", "new_text"])?,
            },
            Self::ApplyPatch => {
                let patches = parse_apply_patch_text(extract_apply_patch_text(args)?);
                // Treat a patch envelope naming no file as unsupported rather than an empty success.
                if patches.iter().all(|patch| patch.file_path.is_empty()) {
                    return None;
                }
                ToolArgs::ApplyPatch { patches }
            }
            Self::Shell => ToolArgs::Shell {
                // Reject blank commands here since `add_run_command` would silently drop them.
                command: arg_str(args, &["command", "cmd"])
                    .filter(|command| !command.trim().is_empty())?,
                description: arg_str(args, &["description"]).unwrap_or(""),
            },
            Self::WriteBash => ToolArgs::WriteBash {
                input: arg_str(args, &["input"]).filter(|input| !input.trim().is_empty())?,
            },
        })
    }
}

/// Validated payload of a successful [`TrackedTool`] call.
enum ToolArgs<'a> {
    /// `content` is `None` when the completion body is unreadable; the read invocation still counts, but records schema drift.
    Read {
        path: &'a str,
        content: Option<String>,
    },
    Search,
    Write {
        path: &'a str,
        content: &'a str,
    },
    Edit {
        path: &'a str,
        old_string: &'a str,
        new_string: &'a str,
    },
    ApplyPatch {
        patches: Vec<CopilotPatch>,
    },
    Shell {
        command: &'a str,
        description: &'a str,
    },
    WriteBash {
        input: &'a str,
    },
}

impl ToolArgs<'_> {
    /// Returns whether the payload is complete (`false` only for [`ToolArgs::Read`] with an unreadable result body).
    fn is_complete(&self) -> bool {
        !matches!(self, Self::Read { content: None, .. })
    }

    /// Folds the call into the file-operation metrics.
    fn record(self, state: &mut SessionParseState, ts: i64) {
        match self {
            Self::Read { path, content } => {
                state.tool_counts.read += 1;
                attach_read_detail(state, path, content.as_deref().unwrap_or(""), ts);
            }
            Self::Search => state.tool_counts.read += 1,
            Self::Write { path, content } => state.add_write_detail(path, content, ts),
            Self::Edit {
                path,
                old_string,
                new_string,
            } => state.add_edit_detail(path, old_string, new_string, ts),
            Self::ApplyPatch { patches } => record_patches(state, patches, ts),
            Self::Shell {
                command,
                description,
            } => state.add_run_command(command, description, ts),
            Self::WriteBash { input } => state.add_run_command(input, "", ts),
        }
    }
}

/// Returns the string value of the first matching key in `keys`, returning `None` immediately if that key is present with a non-string value.
fn arg_str<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| args.get(*key))?.as_str()
}

/// Returns the non-empty `path` string argument, if present.
fn arg_path(args: &Value) -> Option<&str> {
    arg_str(args, &["path"]).filter(|path| !path.is_empty())
}

fn extract_apply_patch_text(args: &Value) -> Option<&str> {
    args.as_str()
        .or_else(|| args.get("input").and_then(Value::as_str))
        .or_else(|| args.get("patch").and_then(Value::as_str))
        .or_else(|| args.get("patchText").and_then(Value::as_str))
        .or_else(|| args.get("string").and_then(Value::as_str))
}

/// A `tool.execution_start` event awaiting its matching `tool.execution_complete`.
struct PendingTool {
    /// Tracked tool category, or `None` for untracked tools.
    tool: Option<TrackedTool>,
    /// Raw tool arguments from `tool.execution_start`.
    arguments: Value,
    /// Start-event timestamp in epoch milliseconds.
    timestamp: i64,
}

/// Attaches read line/file details without incrementing `tool_counts.read` a second time.
fn attach_read_detail(state: &mut SessionParseState, path: &str, content: &str, ts: i64) {
    let invocation_count = state.tool_counts.read;
    state.add_read_detail(path, content, ts);
    state.tool_counts.read = invocation_count;
}

/// Folds a successful `apply_patch` call's parsed patches into per-file details.
fn record_patches(state: &mut SessionParseState, patches: Vec<CopilotPatch>, ts: i64) {
    for patch in patches {
        let (old_string, new_string) = extract_patch_strings(&patch.lines);
        match patch.action.as_str() {
            "add" => state.add_write_detail(&patch.file_path, &new_string, ts),
            "delete" => state.add_edit_detail_raw(&patch.file_path, &old_string, "", ts),
            _ => state.add_edit_detail_raw(&patch.file_path, &old_string, &new_string, ts),
        }
    }
}

struct CopilotPatch {
    action: String,
    file_path: String,
    lines: Vec<String>,
}

/// Parses a `*** Begin Patch` envelope into per-file hunks.
fn parse_apply_patch_text(patch_text: &str) -> Vec<CopilotPatch> {
    let Some(start) = patch_text.find("*** Begin Patch") else {
        return Vec::new();
    };

    let mut patches = Vec::with_capacity(3);
    let mut current: Option<CopilotPatch> = None;
    for line in patch_text[start..].lines() {
        let line = line.trim_end_matches('\r');
        let header = [
            ("*** Update File:", "update"),
            ("*** Add File:", "add"),
            ("*** Delete File:", "delete"),
        ]
        .into_iter()
        .find_map(|(prefix, action)| line.strip_prefix(prefix).map(|path| (action, path)));

        if line.starts_with("*** End Patch") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            break;
        } else if line.starts_with("*** Begin Patch") {
            continue;
        } else if let Some((action, path)) = header {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            current = Some(CopilotPatch {
                action: action.to_string(),
                file_path: path.trim().to_string(),
                lines: Vec::with_capacity(20),
            });
        } else if let Some(path) = line.strip_prefix("*** Move to:") {
            if let Some(patch) = &mut current {
                patch.file_path = path.trim().to_string();
            }
        } else if let Some(patch) = &mut current {
            patch.lines.push(line.to_string());
        }
    }

    if let Some(patch) = current {
        patches.push(patch);
    }
    patches
}

/// Splits one patch body's `-` / `+` lines into its removed and added text.
fn extract_patch_strings(lines: &[String]) -> (String, String) {
    let mut old_string = String::new();
    let mut new_string = String::new();
    for line in lines {
        if line.starts_with("@@") || line.starts_with('\\') {
            continue;
        }
        if let Some(line) = line.strip_prefix('+') {
            new_string.push_str(line);
            new_string.push('\n');
        } else if let Some(line) = line.strip_prefix('-') {
            old_string.push_str(line);
            old_string.push('\n');
        }
    }
    old_string.truncate(old_string.trim_end_matches('\n').len());
    new_string.truncate(new_string.trim_end_matches('\n').len());
    (old_string, new_string)
}

/// Returns the read body for a `view`-family call from `arguments.view_range` (synthesized line placeholder) or `result.content`, or `None` if neither is readable.
fn read_view_content(arguments: &Value, result: &Value) -> Option<String> {
    if let Some(range) = arguments
        .get("view_range")
        .and_then(|v| v.as_array())
        .filter(|range| range.len() >= 2)
    {
        let start = range.first()?.as_i64()?;
        let end = range.get(1)?.as_i64()?;
        let line_count = (end - start + 1).max(0) as usize;
        if line_count == 0 {
            return Some(String::new());
        }
        // Use non-empty lines so `add_read_detail`'s trailing-newline trim cannot collapse the placeholder to empty.
        return Some(vec!["-"; line_count].join("\n"));
    }

    Some(result.get("content")?.as_str()?.to_string())
}

/// Builds an `https://{host}/{repository}` remote URL, or an empty string if either part is empty.
fn build_remote_url(host: &str, repository: &str) -> String {
    if host.is_empty() || repository.is_empty() {
        return String::new();
    }
    format!("https://{}/{}", host.trim(), repository.trim())
}

/// Rewrites dots to dashes in `claude-*` model names while leaving other providers' names unchanged.
fn canonicalize_model_name(name: &str) -> String {
    // Copilot emits `claude-sonnet-4.6`, whereas LiteLLM native keys and other CLIs use `claude-sonnet-4-6` (OpenAI/Google models natively use dots).
    if name.starts_with("claude-") {
        name.replace('.', "-")
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ToolArgs, TrackedTool, canonicalize_model_name, parse_copilot_events_with_diagnostics,
        read_view_content,
    };
    use crate::models::CopilotEvent;
    use crate::session::state::ParseMode;
    use crate::session::state::SessionParseState;
    use serde_json::{Value, json};

    fn event(event_type: &str, data: Value) -> CopilotEvent {
        CopilotEvent {
            event_type: event_type.to_string(),
            data,
            id: String::new(),
            timestamp: "2026-07-12T00:00:00Z".to_string(),
            parent_id: None,
        }
    }

    /// Resolves `tool_name` and reads its payload, panicking if `tool_name` is untracked.
    fn read<'a>(tool_name: &str, arguments: &'a Value, result: &Value) -> Option<ToolArgs<'a>> {
        TrackedTool::from_name(tool_name)
            .unwrap_or_else(|| panic!("`{tool_name}` must resolve to a tracked tool"))
            .read_args(arguments, result)
    }

    /// Folds one successful call and hands back the state it produced.
    fn fold(tool_name: &str, arguments: Value, result: Value) -> SessionParseState {
        let mut state = SessionParseState::new();
        if let Some(payload) = read(tool_name, &arguments, &result) {
            payload.record(&mut state, 1);
        }
        state
    }

    /// Subset of [`SessionParseState`] counters asserted together per folded tool call.
    #[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
    struct Counters {
        read: usize,
        write: usize,
        edit: usize,
        bash: usize,
        read_lines: usize,
    }

    impl Counters {
        fn of(state: &SessionParseState) -> Self {
            Self {
                read: state.tool_counts.read,
                write: state.tool_counts.write,
                edit: state.tool_counts.edit,
                bash: state.tool_counts.bash,
                read_lines: state.total_read_lines,
            }
        }
    }

    /// Builds a JSON object from string key-value pairs.
    fn args_of(pairs: &[(&str, &str)]) -> Value {
        Value::Object(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), json!(value)))
                .collect(),
        )
    }

    fn count_lines_after_trim(s: &str) -> usize {
        // Mirrors `add_read_detail`'s trailing-newline trim before line counting.
        let trimmed = s.trim_end_matches('\n');
        if trimmed.is_empty() {
            0
        } else {
            trimmed.chars().filter(|c| *c == '\n').count() + 1
        }
    }

    #[test]
    fn view_range_placeholder_survives_trim_end() {
        let args = json!({ "view_range": [1, 5], "path": "/tmp/foo" });
        let result = json!({});
        let placeholder = read_view_content(&args, &result).expect("a readable range");
        assert_eq!(
            count_lines_after_trim(&placeholder),
            5,
            "view_range [1,5] must count as 5 lines after add_read_detail's trim"
        );
    }

    #[test]
    fn view_range_with_zero_span_returns_empty() {
        let args = json!({ "view_range": [5, 4], "path": "/tmp/foo" });
        let result = json!({});
        assert_eq!(read_view_content(&args, &result).as_deref(), Some(""));
    }

    #[test]
    fn view_without_range_uses_result_content() {
        let args = json!({ "path": "/tmp/foo" });
        let result = json!({ "content": "alpha\nbeta\ngamma" });
        assert_eq!(
            read_view_content(&args, &result).as_deref(),
            Some("alpha\nbeta\ngamma")
        );
    }

    fn completed_result() -> Value {
        json!({ "content": "alpha\nbeta" })
    }

    #[test]
    fn current_show_file_maps_to_read() {
        let state = fold(
            "show_file",
            json!({ "path": "/tmp/a.txt", "view_range": [1, 2] }),
            completed_result(),
        );
        assert_eq!(state.tool_counts.read, 1);
        assert_eq!(state.total_read_lines, 2);
    }

    #[test]
    fn show_file_empty_and_drifted_results_keep_the_known_invocation() {
        let args = json!({ "path": "/tmp/empty.txt" });

        let empty = json!({ "content": "" });
        assert!(
            read("show_file", &args, &empty)
                .expect("readable arguments")
                .is_complete()
        );
        let state = fold("show_file", args.clone(), empty);
        assert_eq!(state.tool_counts.read, 1);
        assert_eq!(state.total_read_lines, 0);

        let drifted = json!({ "futureContent": "" });
        assert!(
            !read("show_file", &args, &drifted)
                .expect("readable arguments")
                .is_complete()
        );
        let state = fold("show_file", args, drifted);
        assert_eq!(state.tool_counts.read, 1);
        assert_eq!(state.total_read_lines, 0);
    }

    #[test]
    fn an_unreadable_view_range_counts_the_read_without_inventing_lines() {
        let args = json!({ "path": "/tmp/a.txt", "view_range": ["1", "5"] });
        let result = json!({ "content": "alpha\nbeta" });
        assert!(
            !read("show_file", &args, &result)
                .expect("readable arguments")
                .is_complete()
        );

        let state = fold("show_file", args, result);
        assert_eq!(state.tool_counts.read, 1);
        assert_eq!(state.total_read_lines, 0);
    }

    #[test]
    fn current_search_tools_count_read_invocations_without_fake_lines() {
        let mut state = SessionParseState::new();
        for tool_name in ["rg", "grep", "glob", "web_search", "web_fetch"] {
            let args = json!({ "pattern": "needle", "paths": ["src"] });
            read(tool_name, &args, &completed_result())
                .expect("search arguments are always readable")
                .record(&mut state, 1);
        }
        assert_eq!(state.tool_counts.read, 5);
        assert_eq!(state.total_read_lines, 0);
    }

    #[test]
    fn apply_patch_arguments_require_a_supported_nonempty_file_header() {
        let drifted = json!("*** Begin Patch\n*** Future File: src/lib.rs\n+new\n*** End Patch");
        assert!(read("apply_patch", &drifted, &Value::Null).is_none());

        let empty_body = json!("*** Begin Patch\n*** Add File: empty.txt\n*** End Patch");
        assert!(read("apply_patch", &empty_body, &Value::Null).is_some());

        let empty_path = json!("*** Begin Patch\n*** Add File:\n+new\n*** End Patch");
        assert!(read("apply_patch", &empty_path, &Value::Null).is_none());
    }

    #[test]
    fn current_apply_patch_string_maps_to_file_operations() {
        let state = fold(
            "apply_patch",
            json!(
                "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** Add File: notes.txt\n+hello\n*** End Patch"
            ),
            completed_result(),
        );
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.tool_counts.write, 1);
        assert_eq!(state.edit_details.len(), 1);
        assert_eq!(state.write_details.len(), 1);
    }

    #[test]
    fn current_apply_patch_string_field_maps_to_edit() {
        let state = fold(
            "apply_patch",
            json!({
                "string": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
            }),
            completed_result(),
        );
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.edit_details.len(), 1);
    }

    #[test]
    fn current_apply_patch_input_field_maps_to_edit() {
        let state = fold(
            "apply_patch",
            json!({
                "input": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
            }),
            completed_result(),
        );
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.edit_details.len(), 1);
    }

    #[test]
    fn current_write_bash_counts_nonempty_input() {
        let state = fold(
            "write_bash",
            json!({ "input": "yes", "shellId": "shell-1" }),
            completed_result(),
        );
        assert_eq!(state.tool_counts.bash, 1);
        assert_eq!(state.run_details.len(), 1);
    }

    #[test]
    fn every_tracked_tool_name_folds_into_its_metric() {
        let read_args = json!({ "path": "/tmp/a.txt", "view_range": [1, 2] });
        let search_args = json!({ "pattern": "needle" });
        let write_args = json!({ "path": "/tmp/a.txt", "content": "one\ntwo" });
        let edit_args = json!({ "path": "/tmp/a.txt", "old_string": "one", "new_string": "two" });
        let patch_args =
            json!("*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch");
        let shell_args = json!({ "command": "ls" });
        let write_bash_args = json!({ "input": "yes" });

        // `read_lines` distinguishes `Read` from `Search`, which both increment `read`.
        let families: &[(&[&str], &Value, Counters)] = &[
            (
                &["view", "show_file", "read_file"],
                &read_args,
                Counters {
                    read: 1,
                    read_lines: 2,
                    ..Counters::default()
                },
            ),
            (
                &["rg", "grep", "glob", "web_search", "web_fetch"],
                &search_args,
                Counters {
                    read: 1,
                    ..Counters::default()
                },
            ),
            (
                &["create", "write_file", "write"],
                &write_args,
                Counters {
                    write: 1,
                    ..Counters::default()
                },
            ),
            (
                &["str_replace", "edit", "replace", "edit_file"],
                &edit_args,
                Counters {
                    edit: 1,
                    ..Counters::default()
                },
            ),
            (
                &["apply_patch"],
                &patch_args,
                Counters {
                    edit: 1,
                    ..Counters::default()
                },
            ),
            (
                &["bash", "shell", "execute"],
                &shell_args,
                Counters {
                    bash: 1,
                    ..Counters::default()
                },
            ),
            (
                &["write_bash"],
                &write_bash_args,
                Counters {
                    bash: 1,
                    ..Counters::default()
                },
            ),
        ];

        for (names, args, expected) in families {
            for name in *names {
                let state = fold(name, (*args).clone(), completed_result());
                assert_eq!(
                    Counters::of(&state),
                    *expected,
                    "`{name}` must fold into its metric"
                );
            }
        }

        assert!(TrackedTool::from_name("report_intent").is_none());
    }

    #[test]
    fn an_edit_that_replaces_nothing_is_reclassified_as_a_write() {
        let state = fold(
            "str_replace",
            json!({ "path": "/tmp/a.txt", "old_string": "", "new_string": "one\ntwo" }),
            Value::Null,
        );
        assert_eq!(state.tool_counts.write, 1);
        assert_eq!(state.tool_counts.edit, 0);
        assert_eq!(state.total_write_lines, 2);
    }

    #[test]
    fn every_argument_alias_reaches_the_fold() {
        for old_key in ["old_string", "old_str", "old_text"] {
            for new_key in ["new_string", "new_str", "new_text"] {
                let state = fold(
                    "str_replace",
                    args_of(&[
                        ("path", "/tmp/a.txt"),
                        (old_key, "before"),
                        (new_key, "after"),
                    ]),
                    Value::Null,
                );
                let detail = state.edit_details.first().expect("one folded edit");
                assert_eq!(detail.old_string, "before", "`{old_key}` reaches the fold");
                assert_eq!(detail.new_string, "after", "`{new_key}` reaches the fold");
            }
        }

        for content_key in ["file_text", "content"] {
            let state = fold(
                "create",
                args_of(&[("path", "/tmp/a.txt"), (content_key, "one\ntwo")]),
                Value::Null,
            );
            let detail = state.write_details.first().expect("one folded write");
            assert_eq!(
                detail.content, "one\ntwo",
                "`{content_key}` reaches the fold"
            );
        }

        for command_key in ["command", "cmd"] {
            let state = fold(
                "bash",
                args_of(&[(command_key, "ls -l"), ("description", "list")]),
                Value::Null,
            );
            let detail = state.run_details.first().expect("one folded command");
            assert_eq!(detail.command, "ls -l", "`{command_key}` reaches the fold");
            assert_eq!(detail.description, "list");
        }
    }

    #[test]
    fn a_log_spelling_two_aliases_takes_the_earlier_one() {
        let state = fold(
            "str_replace",
            json!({
                "path": "/tmp/a.txt",
                "old_text": "historical",
                "old_str": "middle",
                "old_string": "current",
                "new_string": "after",
            }),
            Value::Null,
        );
        let detail = state.edit_details.first().expect("one folded edit");
        assert_eq!(detail.old_string, "current");
    }

    #[test]
    fn shutdown_rejects_unknown_only_usage_keys_without_inventing_zero_usage() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "future-model": {
                            "usage": {
                                "promptTokens": 123,
                                "completionTokens": 45
                            }
                        }
                    }
                }),
            )],
            ParseMode::Full,
        )
        .unwrap();

        assert!(parsed.diagnostics.is_complete_failure());
        assert!(
            parsed.analysis.records[0].conversation_usage.is_empty(),
            "unknown token keys must not become a successful all-zero usage row"
        );
    }

    #[test]
    fn shutdown_accepts_partial_current_usage_even_when_the_value_is_zero() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "current-model": {
                            "usage": { "inputTokens": 0 }
                        }
                    }
                }),
            )],
            ParseMode::Full,
        )
        .unwrap();

        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(
            parsed.analysis.records[0].conversation_usage["current-model"]["input_tokens"],
            0
        );
    }

    #[test]
    fn missing_tool_success_is_schema_drift_but_explicit_false_is_a_known_failure() {
        let start = || {
            event(
                "tool.execution_start",
                json!({
                    "toolCallId": "call-1",
                    "toolName": "show_file",
                    "arguments": { "path": "/tmp/a.txt" }
                }),
            )
        };

        let missing = parse_copilot_events_with_diagnostics(
            vec![
                start(),
                event(
                    "tool.execution_complete",
                    json!({
                        "toolCallId": "call-1",
                        "status": "success",
                        "result": { "content": "one\ntwo" }
                    }),
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();
        assert!(missing.diagnostics.is_complete_failure());
        assert_eq!(missing.analysis.records[0].tool_call_counts.read, 0);

        let failed = parse_copilot_events_with_diagnostics(
            vec![
                start(),
                event(
                    "tool.execution_complete",
                    json!({
                        "toolCallId": "call-1",
                        "success": false,
                        "result": { "error": "invalid path" }
                    }),
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();
        assert!(!failed.diagnostics.is_complete_failure());
        assert_eq!(failed.diagnostics.partial_failure_count(), 0);
        assert_eq!(failed.analysis.records[0].tool_call_counts.read, 0);
    }

    #[test]
    fn an_orphaned_successful_completion_is_not_dropped_silently() {
        let complete = |success: bool| {
            event(
                "tool.execution_complete",
                json!({
                    "toolCallId": "never-started",
                    "success": success,
                    "result": { "content": "one\ntwo" }
                }),
            )
        };

        let orphan =
            parse_copilot_events_with_diagnostics(vec![complete(true)], ParseMode::Full).unwrap();
        assert_eq!(orphan.diagnostics.failed_relevant_records, 1);

        // Failed completions produce no metrics even when paired, so an orphan failure is not drift.
        let failed =
            parse_copilot_events_with_diagnostics(vec![complete(false)], ParseMode::Full).unwrap();
        assert_eq!(failed.diagnostics.failed_relevant_records, 0);
        assert_eq!(failed.diagnostics.partial_failure_count(), 0);

        // A drifted start already recorded the failure, so its completion must not double-count it.
        let drifted = parse_copilot_events_with_diagnostics(
            vec![
                event(
                    "tool.execution_start",
                    json!({ "toolCallId": "never-started", "renamedTool": "show_file" }),
                ),
                complete(true),
            ],
            ParseMode::Full,
        )
        .unwrap();
        assert_eq!(drifted.diagnostics.failed_relevant_records, 1);
    }

    #[test]
    fn claude_dot_version_rewrites_to_dash() {
        assert_eq!(
            canonicalize_model_name("claude-sonnet-4.6"),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            canonicalize_model_name("claude-opus-4.7"),
            "claude-opus-4-7"
        );
    }

    #[test]
    fn claude_dash_version_is_unchanged() {
        assert_eq!(
            canonicalize_model_name("claude-sonnet-4-6"),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn non_claude_models_keep_dots() {
        assert_eq!(canonicalize_model_name("gpt-5.1"), "gpt-5.1");
        assert_eq!(canonicalize_model_name("gpt-4.1-mini"), "gpt-4.1-mini");
        assert_eq!(canonicalize_model_name("gemini-1.5-pro"), "gemini-1.5-pro");
    }
}
