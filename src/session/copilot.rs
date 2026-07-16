//! Parser for GitHub Copilot CLI session events
//! (`~/.copilot/session-state/<sessionId>/events.jsonl`).
//!
//! One [`CopilotEvent`] per line, dispatched on `event_type`. Token usage is
//! taken from the authoritative `session.shutdown` record when present, with
//! streamed `assistant.message.outputTokens` as a partial fallback for
//! sessions that never shut down cleanly. File operations are paired across
//! `tool.execution_start` / `tool.execution_complete` by `toolCallId` and
//! only counted on success. See the table below for the full event map.
use crate::constants::{FastHashMap, capacity};
use crate::models::*;
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_git_remote_url, parse_iso_timestamp};
use anyhow::Result;
use serde_json::{Value, json};

// =============================================================================
// Copilot CLI `events.jsonl` streaming parser
// =============================================================================
//
// Copilot CLI stores every session as
// `~/.copilot/session-state/<sessionId>/events.jsonl`. Each line is a single
// `CopilotEvent` whose `event_type` decides how to interpret `data`:
//
//   session.start            → session-scoped context (sessionId, cwd, …)
//   session.model_change     → tracks the currently active model
//   session.task_complete    → task summary, informational only
//   session.shutdown         → authoritative per-model token usage
//   system.message           → system prompt; ignored
//   user.message             → user-turn content; ignored
//   assistant.message        → streaming output; only outputTokens is reliable
//   assistant.turn_start/end → turn bookkeeping; ignored
//   tool.execution_start     → paired with the matching complete event
//   tool.execution_complete  → fires the analyzer's file-op handlers
//
// Legacy single-object dumps under `~/.copilot/history-session-state/` are
// not supported — users with old dumps will see them fall through to the
// Codex default in `detect_extension_type` and fail cleanly rather than
// being mis-parsed.

/// Parse Copilot CLI session events from the JSONL event stream.
///
/// Returns a single-record [`CodeAnalysis`] stamped with the
/// `"Copilot-CLI"` extension name. When the stream lacks a
/// `session.shutdown` record, the per-model usage map is grafted from the
/// streamed output-token fallback and will report `input_tokens: 0` so
/// callers can detect the partial accounting.
///
/// # Errors
///
/// Returns `anyhow::Result` for parity with the other provider parsers, but
/// has no fallible step — events that fail to deserialise into their typed
/// payload are skipped — so it returns `Ok` for any iterator.
pub fn parse_copilot_events<I>(events: I, mode: ParseMode) -> Result<CodeAnalysis>
where
    I: IntoIterator<Item = CopilotEvent>,
{
    Ok(parse_copilot_events_with_diagnostics(events, mode)?.analysis)
}

/// Streaming Copilot parser with event-payload schema diagnostics.
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
    // Pending tool calls indexed by `toolCallId` — each `tool.execution_start`
    // stashes its arguments here until the matching `tool.execution_complete`
    // arrives with the result payload.
    let mut pending_tools: FastHashMap<String, PendingTool> = FastHashMap::with_capacity(32);

    // Fallback accounting used when the session does not reach
    // `session.shutdown` (e.g. crash, SIGKILL, ongoing session). We still
    // want to attribute `assistant.message.outputTokens` to *some* model,
    // so we track the active model switches.
    let mut current_model = String::new();
    // Set to `true` once we consume a `session.shutdown` event. If so, the
    // shutdown record is authoritative and we discard the fallback output
    // tallies built from streamed `assistant.message` events — those would
    // otherwise double-count.
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
                        // Copilot's `outputTokens` follows OpenAI's convention
                        // and already includes `reasoningTokens`, so subtract it
                        // back out to keep each token billed once (the flat token
                        // shape treats output and reasoning as disjoint buckets).
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
                        let tracked = is_tracked_tool(&data.tool_name);
                        let arguments_supported =
                            tracked_tool_arguments_supported(&data.tool_name, &data.arguments);
                        pending_tools.insert(
                            data.tool_call_id,
                            PendingTool {
                                tool_name: data.tool_name,
                                arguments: data.arguments,
                                timestamp: ts,
                                tracked,
                                arguments_supported,
                            },
                        );
                    }
                    Ok(_) | Err(_) => diagnostics.record_relevant(false),
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
                    continue;
                };
                let Some(success) = event.data.get("success").and_then(Value::as_bool) else {
                    if pending.tracked {
                        diagnostics.record_relevant(false);
                    }
                    continue;
                };
                // Only dispatch successful tool calls — failures rarely
                // produce meaningful arguments (e.g. path validation errors)
                // and would skew line-count totals.
                if !success {
                    if pending.tracked {
                        diagnostics.record_relevant(true);
                    }
                    continue;
                }
                let data =
                    match serde_json::from_value::<CopilotToolCompleteData>(event.data.clone()) {
                        Ok(data) => data,
                        Err(_) => {
                            if pending.tracked {
                                diagnostics.record_relevant(false);
                            }
                            continue;
                        }
                    };
                if pending.tracked {
                    let result_supported = tracked_tool_result_supported(
                        &pending.tool_name,
                        &pending.arguments,
                        &data.result,
                    );
                    diagnostics.record_relevant(pending.arguments_supported && result_supported);
                    if !pending.arguments_supported {
                        continue;
                    }
                }
                dispatch_tool(&mut state, &pending, &data);
            }
            _ => {}
        }
    }

    // If `session.shutdown` never arrived, graft the fallback streamed
    // output-token counters into `conversation_usage` so the row still has
    // a non-zero number (callers can tell it's partial by the missing
    // `input_tokens`).
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

    // Fallback git remote lookup when `session.start.context` did not carry
    // a repository string (e.g. running outside a git tree or pre-1.0 CLI).
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

fn is_tracked_tool(name: &str) -> bool {
    matches!(
        name,
        "view"
            | "show_file"
            | "read_file"
            | "rg"
            | "grep"
            | "glob"
            | "web_search"
            | "web_fetch"
            | "create"
            | "write_file"
            | "write"
            | "str_replace"
            | "edit"
            | "replace"
            | "edit_file"
            | "apply_patch"
            | "bash"
            | "shell"
            | "execute"
            | "write_bash"
    )
}

fn tracked_tool_arguments_supported(name: &str, args: &Value) -> bool {
    match name {
        "view" | "show_file" | "read_file" => args
            .get("path")
            .and_then(Value::as_str)
            .is_some_and(|path| !path.is_empty()),
        "rg" | "grep" | "glob" | "web_search" | "web_fetch" => true,
        "create" | "write_file" | "write" => {
            args.get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| !path.is_empty())
                && args
                    .get("file_text")
                    .or_else(|| args.get("content"))
                    .is_some_and(Value::is_string)
        }
        "str_replace" | "edit" | "replace" | "edit_file" => {
            args.get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| !path.is_empty())
                && args
                    .get("old_string")
                    .or_else(|| args.get("old_str"))
                    .or_else(|| args.get("old_text"))
                    .is_some_and(Value::is_string)
                && args
                    .get("new_string")
                    .or_else(|| args.get("new_str"))
                    .or_else(|| args.get("new_text"))
                    .is_some_and(Value::is_string)
        }
        "apply_patch" => extract_apply_patch_text(args).is_some_and(|patch| {
            parse_apply_patch_text(patch)
                .iter()
                .any(|patch| !patch.file_path.is_empty())
        }),
        "bash" | "shell" | "execute" => args
            .get("command")
            .or_else(|| args.get("cmd"))
            .and_then(Value::as_str)
            .is_some_and(|command| !command.trim().is_empty()),
        "write_bash" => args
            .get("input")
            .and_then(Value::as_str)
            .is_some_and(|command| !command.trim().is_empty()),
        _ => false,
    }
}

fn tracked_tool_result_supported(name: &str, args: &Value, result: &Value) -> bool {
    match name {
        "view" | "show_file" | "read_file" => {
            if let Some(range) = args
                .get("view_range")
                .and_then(Value::as_array)
                .filter(|range| range.len() >= 2)
            {
                return range.first().and_then(Value::as_i64).is_some()
                    && range.get(1).and_then(Value::as_i64).is_some();
            }
            result.get("content").is_some_and(Value::is_string)
        }
        _ => true,
    }
}

fn extract_apply_patch_text(args: &Value) -> Option<&str> {
    args.as_str()
        .or_else(|| args.get("input").and_then(Value::as_str))
        .or_else(|| args.get("patch").and_then(Value::as_str))
        .or_else(|| args.get("patchText").and_then(Value::as_str))
        .or_else(|| args.get("string").and_then(Value::as_str))
}

/// A `tool.execution_start` event held until its matching
/// `tool.execution_complete` arrives, keyed by `toolCallId`.
struct PendingTool {
    /// Tool name (e.g. `view`, `create`, `str_replace`, `bash`).
    tool_name: String,
    /// Raw tool arguments object, interpreted lazily by [`dispatch_tool`].
    arguments: Value,
    /// Start-event timestamp in epoch milliseconds, used for the detail record.
    timestamp: i64,
    /// Whether this tool contributes to the analysis projection.
    tracked: bool,
    /// Whether the tracked tool's arguments use a supported schema.
    arguments_supported: bool,
}

/// Routes a completed Copilot tool call to the matching file-operation tally.
///
/// Branches on `pending.tool_name`; unrecognised tools (e.g. `report_intent`,
/// `task_complete`) are silently ignored. Argument field names are probed
/// with historical aliases for forward compatibility across CLI releases.
fn dispatch_tool(
    state: &mut SessionParseState,
    pending: &PendingTool,
    complete: &CopilotToolCompleteData,
) {
    let ts = pending.timestamp;
    let args = &pending.arguments;

    match pending.tool_name.as_str() {
        // Current Copilot CLI exposes `view` for reads. Historical versions
        // used `str_replace_editor` with `command == "view"`, which we no
        // longer attempt to parse.
        "view" | "show_file" | "read_file" => {
            let Some(path) = args.get("path").and_then(|p| p.as_str()) else {
                return;
            };

            state.tool_counts.read += 1;
            let content = extract_view_content(args, &complete.result);
            attach_read_detail(state, path, &content, ts);
        }
        // Search and web tools surface content but do not identify one complete
        // file body, so retain the invocation without inventing line totals.
        "rg" | "grep" | "glob" | "web_search" | "web_fetch" => state.tool_counts.read += 1,
        // `create` is the primary write tool. Historical names are kept for
        // robustness; a future release that renames the tool will still get
        // counted if the argument shape stays similar.
        "create" | "write_file" | "write" => {
            let Some(path) = args.get("path").and_then(|p| p.as_str()) else {
                return;
            };
            let content = args
                .get("file_text")
                .or_else(|| args.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("");
            state.add_write_detail(path, content, ts);
        }
        // Edit-style tool names the CLI is known or likely to emit. Field
        // shape is assumed to stay `{path, old_string|old_str, new_string|new_str}`.
        "str_replace" | "edit" | "replace" | "edit_file" => {
            let Some(path) = args.get("path").and_then(|p| p.as_str()) else {
                return;
            };
            let old_str = args
                .get("old_string")
                .or_else(|| args.get("old_str"))
                .or_else(|| args.get("old_text"))
                .and_then(|s| s.as_str())
                .unwrap_or("");
            let new_str = args
                .get("new_string")
                .or_else(|| args.get("new_str"))
                .or_else(|| args.get("new_text"))
                .and_then(|s| s.as_str())
                .unwrap_or("");
            state.add_edit_detail(path, old_str, new_str, ts);
        }
        "apply_patch" => {
            let patch = extract_apply_patch_text(args).unwrap_or("");
            apply_patch_text(state, patch, ts);
        }
        "bash" | "shell" | "execute" => {
            let command = args
                .get("command")
                .or_else(|| args.get("cmd"))
                .and_then(|c| c.as_str())
                .unwrap_or("");
            let description = args
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("");
            state.add_run_command(command, description, ts);
        }
        "write_bash" => {
            let input = args.get("input").and_then(Value::as_str).unwrap_or("");
            state.add_run_command(input, "", ts);
        }
        // `report_intent`, `task_complete`, `update_topic`, … have no
        // file-operation semantics we care about. Silently ignore.
        _ => {}
    }
}

fn attach_read_detail(state: &mut SessionParseState, path: &str, content: &str, ts: i64) {
    let invocation_count = state.tool_counts.read;
    state.add_read_detail(path, content, ts);
    state.tool_counts.read = invocation_count;
}

/// Folds a successful `apply_patch` call into per-file details.
fn apply_patch_text(state: &mut SessionParseState, patch_text: &str, ts: i64) {
    for patch in parse_apply_patch_text(patch_text) {
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

/// Parses the patch envelope used by the current Copilot CLI.
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

/// Resolve the content a Copilot `view` tool saw.
///
/// Callers can pass us two sources:
///
/// 1. `arguments.view_range` — inclusive `[start, end]` line numbers. When
///    present we synthesise a `line_count`-line placeholder using a
///    non-newline character so `add_read_detail`'s `trim_end_matches('\n')`
///    cannot collapse it back to zero. The actual content is not needed
///    because we only care about the line count.
/// 2. `complete.result.content` — the string the model actually received.
///    Preferred when available and when no `view_range` was supplied.
fn extract_view_content(arguments: &Value, result: &Value) -> String {
    if let Some(range) = arguments.get("view_range").and_then(|v| v.as_array())
        && range.len() >= 2
    {
        let start = range.first().and_then(|v| v.as_i64()).unwrap_or(0);
        let end = range.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
        let line_count = (end - start + 1).max(0) as usize;
        if line_count == 0 {
            return String::new();
        }
        // A pure-newline placeholder ("\n".repeat(N - 1)) would survive
        // `count_lines` on its own, but `add_read_detail` first trims
        // trailing newlines and then the whole thing collapses to an
        // empty string — so the line tally would silently come back as
        // zero. Use single-char "lines" joined by '\n' so the trim is a
        // no-op and `count_lines` recovers exactly `line_count`.
        return vec!["-"; line_count].join("\n");
    }

    result
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string()
}

/// Best-effort reconstruction of a repository's git remote URL from the
/// `session.start.context` fields.
///
/// Copilot writes `{ repository: "owner/repo", repositoryHost: "github.com" }`
/// but does *not* include the full clone URL. We prefix with `https://`
/// because that's the canonical web-facing form; the value is only used for
/// display in the usage report, not for actual git operations, so the
/// SSH-vs-HTTPS distinction does not matter.
fn build_remote_url(host: &str, repository: &str) -> String {
    if host.is_empty() || repository.is_empty() {
        return String::new();
    }
    format!("https://{}/{}", host.trim(), repository.trim())
}

/// Canonicalise a Copilot-supplied model name.
///
/// Copilot CLI writes Anthropic model names with **dot-separated** minor
/// versions (e.g. `claude-sonnet-4.6`, `claude-opus-4.7`), while the
/// LiteLLM pricing table and every other CLI in this tool (Claude Code,
/// Codex) use the **dash-separated** form (`claude-sonnet-4-6`,
/// `claude-opus-4-7`).
///
/// If we leave the Copilot names as-is, two things go wrong:
///
/// 1. `merge_usage_values` keeps Copilot's `claude-sonnet-4.6` separate
///    from Claude Code's `claude-sonnet-4-6`, splitting a single model's
///    usage across two rows.
/// 2. The pricing matcher's substring/fuzzy tier finds no exact key for
///    `claude-sonnet-4.6` and picks the *only* dot-named variant it has
///    — `openrouter/anthropic/claude-sonnet-4.6` — which is an OpenRouter
///    proxy entry with different per-token rates, not the Anthropic
///    native rate the Copilot caller is actually being billed against.
///
/// We limit the rewrite to names starting with `claude-` so OpenAI /
/// Google models whose native form legitimately contains dots (e.g.
/// `gpt-5.1`, `gemini-1.5-pro`) are left untouched.
fn canonicalize_model_name(name: &str) -> String {
    if name.starts_with("claude-") {
        name.replace('.', "-")
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CopilotToolCompleteData, PendingTool, canonicalize_model_name, dispatch_tool,
        extract_view_content, parse_copilot_events_with_diagnostics,
        tracked_tool_arguments_supported, tracked_tool_result_supported,
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

    fn count_lines_after_trim(s: &str) -> usize {
        // Mirror src/session/state.rs count_lines + add_read_detail's
        // trim_end_matches('\n') so the test reflects the actual line
        // tally the analyzer would record.
        let trimmed = s.trim_end_matches('\n');
        if trimmed.is_empty() {
            0
        } else {
            trimmed.chars().filter(|c| *c == '\n').count() + 1
        }
    }

    #[test]
    fn view_range_placeholder_survives_trim_end() {
        // view_range [1, 5] → 5 logical lines. The synthesised
        // placeholder must yield 5 from `count_lines` even after
        // `trim_end_matches('\n')` runs in `add_read_detail`.
        let args = json!({ "view_range": [1, 5], "path": "/tmp/foo" });
        let result = json!({});
        let placeholder = extract_view_content(&args, &result);
        assert_eq!(
            count_lines_after_trim(&placeholder),
            5,
            "view_range [1,5] must count as 5 lines after add_read_detail's trim"
        );
    }

    #[test]
    fn view_range_with_zero_span_returns_empty() {
        // Edge case: empty range produces an empty placeholder so the
        // upstream early-return in `add_read_detail` skips it cleanly.
        let args = json!({ "view_range": [5, 4], "path": "/tmp/foo" });
        let result = json!({});
        assert_eq!(extract_view_content(&args, &result), "");
    }

    #[test]
    fn view_without_range_uses_result_content() {
        let args = json!({ "path": "/tmp/foo" });
        let result = json!({ "content": "alpha\nbeta\ngamma" });
        assert_eq!(extract_view_content(&args, &result), "alpha\nbeta\ngamma");
    }

    fn completed() -> CopilotToolCompleteData {
        CopilotToolCompleteData {
            tool_call_id: "call-1".to_string(),
            success: true,
            result: json!({ "content": "alpha\nbeta" }),
            model: "test".to_string(),
        }
    }

    #[test]
    fn current_show_file_maps_to_read() {
        let pending = PendingTool {
            tool_name: "show_file".to_string(),
            arguments: json!({ "path": "/tmp/a.txt", "view_range": [1, 2] }),
            timestamp: 1,
            tracked: true,
            arguments_supported: true,
        };
        let mut state = SessionParseState::new();
        dispatch_tool(&mut state, &pending, &completed());
        assert_eq!(state.tool_counts.read, 1);
        assert_eq!(state.total_read_lines, 2);
    }

    #[test]
    fn show_file_empty_and_drifted_results_keep_the_known_invocation() {
        let pending = PendingTool {
            tool_name: "show_file".to_string(),
            arguments: json!({ "path": "/tmp/empty.txt" }),
            timestamp: 1,
            tracked: true,
            arguments_supported: true,
        };

        let mut empty = completed();
        empty.result = json!({ "content": "" });
        assert!(tracked_tool_result_supported(
            &pending.tool_name,
            &pending.arguments,
            &empty.result
        ));
        let mut state = SessionParseState::new();
        dispatch_tool(&mut state, &pending, &empty);
        assert_eq!(state.tool_counts.read, 1);
        assert_eq!(state.total_read_lines, 0);

        let mut drifted = completed();
        drifted.result = json!({ "futureContent": "" });
        assert!(!tracked_tool_result_supported(
            &pending.tool_name,
            &pending.arguments,
            &drifted.result
        ));
        let mut state = SessionParseState::new();
        dispatch_tool(&mut state, &pending, &drifted);
        assert_eq!(state.tool_counts.read, 1);
        assert_eq!(state.total_read_lines, 0);
    }

    #[test]
    fn current_search_tools_count_read_invocations_without_fake_lines() {
        let mut state = SessionParseState::new();
        for tool_name in ["rg", "grep", "glob", "web_search", "web_fetch"] {
            let pending = PendingTool {
                tool_name: tool_name.to_string(),
                arguments: json!({ "pattern": "needle", "paths": ["src"] }),
                timestamp: 1,
                tracked: true,
                arguments_supported: true,
            };
            dispatch_tool(&mut state, &pending, &completed());
        }
        assert_eq!(state.tool_counts.read, 5);
        assert_eq!(state.total_read_lines, 0);
    }

    #[test]
    fn apply_patch_arguments_require_a_supported_nonempty_file_header() {
        let drifted = json!("*** Begin Patch\n*** Future File: src/lib.rs\n+new\n*** End Patch");
        assert!(!tracked_tool_arguments_supported("apply_patch", &drifted));

        let empty_body = json!("*** Begin Patch\n*** Add File: empty.txt\n*** End Patch");
        assert!(tracked_tool_arguments_supported("apply_patch", &empty_body));

        let empty_path = json!("*** Begin Patch\n*** Add File:\n+new\n*** End Patch");
        assert!(!tracked_tool_arguments_supported(
            "apply_patch",
            &empty_path
        ));
    }

    #[test]
    fn current_apply_patch_string_maps_to_file_operations() {
        let pending = PendingTool {
            tool_name: "apply_patch".to_string(),
            arguments: json!(
                "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** Add File: notes.txt\n+hello\n*** End Patch"
            ),
            timestamp: 1,
            tracked: true,
            arguments_supported: true,
        };
        let mut state = SessionParseState::new();
        dispatch_tool(&mut state, &pending, &completed());
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.tool_counts.write, 1);
        assert_eq!(state.edit_details.len(), 1);
        assert_eq!(state.write_details.len(), 1);
    }

    #[test]
    fn current_apply_patch_string_field_maps_to_edit() {
        let pending = PendingTool {
            tool_name: "apply_patch".to_string(),
            arguments: json!({
                "string": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
            }),
            timestamp: 1,
            tracked: true,
            arguments_supported: true,
        };
        let mut state = SessionParseState::new();
        dispatch_tool(&mut state, &pending, &completed());
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.edit_details.len(), 1);
    }

    #[test]
    fn current_apply_patch_input_field_maps_to_edit() {
        let pending = PendingTool {
            tool_name: "apply_patch".to_string(),
            arguments: json!({
                "input": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
            }),
            timestamp: 1,
            tracked: true,
            arguments_supported: true,
        };
        let mut state = SessionParseState::new();
        dispatch_tool(&mut state, &pending, &completed());
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.edit_details.len(), 1);
    }

    #[test]
    fn current_write_bash_counts_nonempty_input() {
        let pending = PendingTool {
            tool_name: "write_bash".to_string(),
            arguments: json!({ "input": "yes", "shellId": "shell-1" }),
            timestamp: 1,
            tracked: true,
            arguments_supported: true,
        };
        let mut state = SessionParseState::new();
        dispatch_tool(&mut state, &pending, &completed());
        assert_eq!(state.tool_counts.bash, 1);
        assert_eq!(state.run_details.len(), 1);
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
        // OpenAI / Azure model names use dots natively; do not touch them.
        assert_eq!(canonicalize_model_name("gpt-5.1"), "gpt-5.1");
        assert_eq!(canonicalize_model_name("gpt-4.1-mini"), "gpt-4.1-mini");
        assert_eq!(canonicalize_model_name("gemini-1.5-pro"), "gemini-1.5-pro");
    }
}
