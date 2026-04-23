use crate::analysis::common_state::{AnalysisMode, AnalysisState};
use crate::constants::{FastHashMap, capacity};
use crate::models::*;
use crate::utils::{get_git_remote_url, parse_iso_timestamp};
use anyhow::Result;
use serde_json::{Value, json};

// =============================================================================
// Legacy single-object Copilot CLI sessions
// =============================================================================
//
// Historical `~/.copilot/history-session-state/<sessionId>.json` dumps have
// a flat `timeline` of tool-call events and no token accounting at all.
// The code below preserves that pipeline for backward compatibility — the
// streaming parser for modern `events.jsonl` files lives further down.

/// Analyze Copilot CLI conversations from a legacy single-object dump
pub fn analyze_copilot_conversations(session: CopilotSession) -> Result<CodeAnalysis> {
    analyze_copilot_conversations_with_mode(session, AnalysisMode::Full)
}

pub fn analyze_copilot_conversations_with_mode(
    session: CopilotSession,
    mode: AnalysisMode,
) -> Result<CodeAnalysis> {
    let mut state = AnalysisState::with_mode(mode);

    // The legacy format carries no token accounting; seed a zero-usage
    // entry under the generic `copilot` model so downstream pricing logic
    // still has a row to attach to.
    let mut conversation_usage: FastHashMap<String, Value> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);
    conversation_usage.insert(
        "copilot".to_string(),
        json!({
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_input_tokens": 0,
            "cache_creation_input_tokens": 0
        }),
    );

    for event in session.timeline {
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

        match tool_title.as_str() {
            "str_replace_editor" => {
                if let Ok(args) = serde_json::from_value::<StrReplaceEditorArgs>(arguments.clone())
                {
                    match args.command.as_str() {
                        "view" => {
                            let content = if let Some(view_range) = args.view_range {
                                // Synthesize a newline-only string whose line
                                // count matches the requested range so the
                                // read-line counter gets a plausible value.
                                let start = view_range.first().copied().unwrap_or(0);
                                let end = view_range.get(1).copied().unwrap_or(0);
                                let line_count = (end - start + 1).max(0) as usize;
                                "\n".repeat(line_count.saturating_sub(1))
                            } else if let Some(result) = &event.result
                                && let Some(log) = result.get("log").and_then(|l| l.as_str())
                            {
                                log.to_string()
                            } else {
                                String::new()
                            };

                            state.add_read_detail(&args.path, &content, ts);
                        }
                        "str_replace" => {
                            let old_str = args.old_str.as_deref().unwrap_or("");
                            let new_str = args.new_str.as_deref().unwrap_or("");
                            state.add_edit_detail(&args.path, old_str, new_str, ts);
                        }
                        "create" => {
                            let content = args.file_text.as_deref().unwrap_or("");
                            state.add_write_detail(&args.path, content, ts);
                        }
                        _ => {}
                    }
                }
            }
            "bash" => {
                if let Ok(args) = serde_json::from_value::<BashArgs>(arguments.clone()) {
                    let command = args.command.as_deref().unwrap_or("");
                    let description = args.description.as_deref().unwrap_or("");
                    state.add_run_command(command, description, ts);
                }
            }
            _ => {}
        }
    }

    state.task_id = session.session_id;

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

// =============================================================================
// Modern Copilot CLI `events.jsonl` streaming parser
// =============================================================================
//
// The new layout stores every session as
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

/// Analyze Copilot CLI conversations from the modern JSONL event stream.
pub fn analyze_copilot_events<I>(events: I, mode: AnalysisMode) -> Result<CodeAnalysis>
where
    I: IntoIterator<Item = CopilotEvent>,
{
    let mut state = AnalysisState::with_mode(mode);
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

    for event in events {
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
                            state.git_remote = build_remote_url(&ctx.repository_host, &ctx.repository);
                        }
                    }
                }
            }
            "session.model_change" => {
                if let Some(new_model) = event
                    .data
                    .get("newModel")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    current_model = new_model.to_string();
                }
            }
            "session.shutdown" => {
                if let Ok(data) = serde_json::from_value::<CopilotShutdownData>(event.data.clone()) {
                    for (model, metric) in data.model_metrics {
                        if model.is_empty() {
                            continue;
                        }
                        let Some(usage) = metric.usage else {
                            continue;
                        };
                        // Copilot's `reasoningTokens` is a subset of the
                        // "thinking" budget a model burns before emitting
                        // visible tokens. Other providers fold it into the
                        // output bucket, so we do the same here for
                        // consistency (see
                        // `utils::token_extractor::extract_token_counts`
                        // for Codex's treatment of `reasoning_output_tokens`).
                        let usage_json = json!({
                            "input_tokens": usage.input_tokens,
                            "output_tokens": usage.output_tokens + usage.reasoning_tokens,
                            "cache_read_input_tokens": usage.cache_read_tokens,
                            "cache_creation_input_tokens": usage.cache_write_tokens,
                        });
                        conversation_usage.insert(model, usage_json);
                    }
                    shutdown_seen = true;
                }
            }
            "assistant.message" => {
                // Only used as a fallback when no `session.shutdown` arrives.
                if let Some(output_tokens) = event
                    .data
                    .get("outputTokens")
                    .and_then(|v| v.as_i64())
                    .filter(|&t| t > 0)
                    && !current_model.is_empty()
                {
                    *pending_output_tokens
                        .entry(current_model.clone())
                        .or_insert(0) += output_tokens;
                }
            }
            "tool.execution_start" => {
                if let Ok(data) =
                    serde_json::from_value::<CopilotToolStartData>(event.data.clone())
                    && !data.tool_call_id.is_empty()
                {
                    pending_tools.insert(
                        data.tool_call_id,
                        PendingTool {
                            tool_name: data.tool_name,
                            arguments: data.arguments,
                            timestamp: ts,
                        },
                    );
                }
            }
            "tool.execution_complete" => {
                let Ok(data) =
                    serde_json::from_value::<CopilotToolCompleteData>(event.data.clone())
                else {
                    continue;
                };
                if data.tool_call_id.is_empty() {
                    continue;
                }
                let Some(pending) = pending_tools.remove(&data.tool_call_id) else {
                    continue;
                };
                // Only dispatch successful tool calls — failures rarely
                // produce meaningful arguments (e.g. path validation errors)
                // and would skew line-count totals.
                if !data.success {
                    continue;
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

    Ok(CodeAnalysis {
        user: String::new(),
        extension_name: String::from("Copilot-CLI"),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    })
}

struct PendingTool {
    tool_name: String,
    arguments: Value,
    timestamp: i64,
}

fn dispatch_tool(
    state: &mut AnalysisState,
    pending: &PendingTool,
    complete: &CopilotToolCompleteData,
) {
    let ts = pending.timestamp;
    let args = &pending.arguments;

    match pending.tool_name.as_str() {
        // Current Copilot CLI exposes `view` for reads. Historical versions
        // used `str_replace_editor` with `command == "view"` — that path is
        // still handled by the legacy analyzer above.
        "view" | "read_file" => {
            let Some(path) = args.get("path").and_then(|p| p.as_str()) else {
                return;
            };

            let content = extract_view_content(args, &complete.result);
            state.add_read_detail(path, &content, ts);
        }
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
        // `glob`, `report_intent`, `task_complete`, `update_topic`, … have
        // no file-operation semantics we care about. Silently ignore.
        _ => {}
    }
}

/// Resolve the content a Copilot `view` tool saw.
///
/// Callers can pass us two sources:
///
/// 1. `arguments.view_range` — inclusive `[start, end]` line numbers. When
///    present we synthesise a newline-only placeholder so `count_lines`
///    still gets the right number of lines; the actual content is not
///    needed because we only care about the line count.
/// 2. `complete.result.content` — the string the model actually received.
///    Preferred when available and when no `view_range` was supplied.
fn extract_view_content(arguments: &Value, result: &Value) -> String {
    if let Some(range) = arguments.get("view_range").and_then(|v| v.as_array())
        && range.len() >= 2
    {
        let start = range.first().and_then(|v| v.as_i64()).unwrap_or(0);
        let end = range.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
        let line_count = (end - start + 1).max(0) as usize;
        return "\n".repeat(line_count.saturating_sub(1));
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
