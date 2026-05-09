use crate::constants::{FastHashMap, capacity};
use crate::models::*;
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
pub fn parse_copilot_events<I>(events: I, mode: ParseMode) -> Result<CodeAnalysis>
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
                            state.git_remote =
                                build_remote_url(&ctx.repository_host, &ctx.repository);
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
                    current_model = canonicalize_model_name(new_model);
                }
            }
            "session.shutdown" => {
                if let Ok(data) = serde_json::from_value::<CopilotShutdownData>(event.data.clone())
                {
                    for (model, metric) in data.model_metrics {
                        if model.is_empty() {
                            continue;
                        }
                        let Some(usage) = metric.usage else {
                            continue;
                        };
                        // Copilot's `reasoningTokens` is the model's
                        // thinking budget emitted before the visible
                        // response. Surface it as its own field so
                        // `calculate_cost` can charge it against the
                        // model's published reasoning rate (when present)
                        // via `output_cost_per_reasoning_token`, instead
                        // of folding it into `output_tokens` and billing
                        // every thinking token at the flat output rate.
                        let usage_json = json!({
                            "input_tokens": usage.input_tokens,
                            "output_tokens": usage.output_tokens,
                            "reasoning_output_tokens": usage.reasoning_tokens,
                            "cache_read_input_tokens": usage.cache_read_tokens,
                            "cache_creation_input_tokens": usage.cache_write_tokens,
                        });
                        conversation_usage.insert(canonicalize_model_name(&model), usage_json);
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
                if let Ok(data) = serde_json::from_value::<CopilotToolStartData>(event.data.clone())
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
    use super::{canonicalize_model_name, extract_view_content};
    use serde_json::json;

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
