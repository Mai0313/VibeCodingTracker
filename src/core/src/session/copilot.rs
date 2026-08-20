//! Parser for GitHub Copilot CLI session events
//! (`~/.copilot/session-state/<sessionId>/events.jsonl`).
//!
//! One [`CopilotEvent`] per line, dispatched on `event_type`. Token usage is
//! taken from the authoritative `session.shutdown` record when present, with
//! streamed `assistant.message.outputTokens` as a partial fallback for
//! sessions that never shut down cleanly. File operations are paired across
//! `tool.execution_start` / `tool.execution_complete` by `toolCallId` and
//! only counted on success. See the table below for the event map.
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
// Each line is a single `CopilotEvent` whose `event_type` decides how to
// interpret `data`:
//
//   session.start            → session-scoped context (sessionId, cwd, …)
//   session.model_change     → tracks the currently active model
//   session.shutdown         → authoritative per-model token usage
//   assistant.message        → streaming output; only outputTokens is reliable
//   tool.execution_start     → paired with the matching complete event
//   tool.execution_complete  → fires the analyzer's file-op handlers
//
// Every other entry in the `recognized` list below (system / user messages,
// turn and hook bookkeeping, subagents, aborts, …) is known-and-ignored; an
// `event_type` outside that list counts as schema drift.
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
    // Pending tool calls indexed by `toolCallId`.
    let mut pending_tools: FastHashMap<String, PendingTool> = FastHashMap::with_capacity(32);

    // Fallback accounting used when the session does not reach
    // `session.shutdown` (e.g. crash, SIGKILL, ongoing session). We still
    // want to attribute `assistant.message.outputTokens` to *some* model,
    // so we track the active model switches.
    let mut current_model = String::new();
    // A consumed `session.shutdown` record is authoritative. The fallback
    // graft below inserts on the same canonicalized model key, so without
    // this guard it would replace that row with a zero-`input_tokens` one.
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
                // A session may switch models at any point, so the streamed
                // `assistant.message` tokens below are attributed to whichever
                // model was current when they arrived.
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
                        // The verdict for this call is already taken. Holding
                        // the id keeps its completion pairing, so the same
                        // drift is not counted a second time as an orphan.
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
                    // Without the start event there is no tool name, so there
                    // is no `tracked` flag to gate on. Only a completion that
                    // reported success could have carried a file operation —
                    // a failed one is skipped even when it does pair — so that
                    // is the orphan counted as a record left unnormalized.
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
                // Only dispatch successful tool calls — failures rarely
                // produce meaningful arguments (e.g. path validation errors)
                // and would skew line-count totals.
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

/// A Copilot tool this parser tracks, resolved from the name in the log.
///
/// One variant per metric the tool folds into. [`TrackedTool::read_args`] and
/// [`ToolArgs::record`] match exhaustively on their enum, so a variant one of
/// them forgets is a build error rather than a silently dropped metric. What
/// the compiler cannot check is [`TrackedTool::from_name`] itself, which is why
/// every tool name is spelled there and nowhere else.
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
    /// Resolves a log name, or `None` for one this table does not list
    /// (`report_intent`, `task_complete`, `update_topic`, … among them). An
    /// unsupported payload on a listed tool is schema drift; on any other tool
    /// it is not.
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            // Historical releases exposed reads as `str_replace_editor` with
            // `command == "view"`, which we no longer attempt to parse.
            "view" | "show_file" | "read_file" => Self::Read,
            // Search and web tools surface content but do not identify one
            // complete file body, so they retain the invocation without
            // inventing line totals.
            "rg" | "grep" | "glob" | "web_search" | "web_fetch" => Self::Search,
            // `create` is the primary write tool; the other names are ones the
            // CLI is known or likely to emit. A tool name outside this table is
            // ignored, however similar its argument shape.
            "create" | "write_file" | "write" => Self::Write,
            // Edit-style tool names the CLI is known or likely to emit.
            "str_replace" | "edit" | "replace" | "edit_file" => Self::Edit,
            "apply_patch" => Self::ApplyPatch,
            "bash" | "shell" | "execute" => Self::Shell,
            "write_bash" => Self::WriteBash,
            _ => return None,
        })
    }

    /// Reads what a successful call folds into metrics, or `None` when this
    /// parser cannot read its arguments — that call is schema drift and folds
    /// nothing. Every argument key looked up by name, historical alias
    /// spellings included, is listed here and nowhere else; the one exception
    /// is the patch envelope, whose own spellings live in
    /// [`extract_apply_patch_text`].
    ///
    /// `result` arrives with the completion and only [`TrackedTool::Read`] uses
    /// it; a result that does not read leaves the call foldable, so it is the
    /// variant's business rather than this `Option`'s.
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
                // A patch envelope naming no file at all folds nothing, so it
                // reads as unsupported rather than as an empty success.
                if patches.iter().all(|patch| patch.file_path.is_empty()) {
                    return None;
                }
                ToolArgs::ApplyPatch { patches }
            }
            Self::Shell => ToolArgs::Shell {
                // A blank command would be dropped by `add_run_command`,
                // leaving the call with no metric at all, so it counts as
                // unreadable here.
                command: arg_str(args, &["command", "cmd"])
                    .filter(|command| !command.trim().is_empty())?,
                // Optional, and absent from plenty of real calls: it is
                // reported alongside the command, not part of what makes the
                // call foldable.
                description: arg_str(args, &["description"]).unwrap_or(""),
            },
            Self::WriteBash => ToolArgs::WriteBash {
                input: arg_str(args, &["input"]).filter(|input| !input.trim().is_empty())?,
            },
        })
    }
}

/// The payload of a successful [`TrackedTool`] call, read once.
///
/// Building one *is* the schema check, so a key validation accepts cannot be a
/// key the fold then fails to read: there is no second reader to disagree with.
/// The two used to be separate `match` arms over the tool name, each restating
/// the whole name table and the alias set of every argument that has one.
enum ToolArgs<'a> {
    /// `content` is `None` when the completion carried no body this parser
    /// could read. The arguments were fine, so the call still counts as one
    /// read invocation, but it contributes no lines and reads as schema drift.
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
    /// Whether the whole payload read. Only [`ToolArgs::Read`] can be built
    /// from arguments this parser understands and still answer `false`.
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

/// The string under the first of `keys` that `args` carries.
///
/// A key present but holding something other than a string ends the search: the
/// log did spell the argument, just not in a shape this parser can read.
fn arg_str<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| args.get(*key))?.as_str()
}

/// The `path` argument, which every variant carrying one requires to be
/// non-empty; an operation against no path records nothing downstream.
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

/// A `tool.execution_start` event held until its matching
/// `tool.execution_complete` arrives, keyed by `toolCallId`.
struct PendingTool {
    /// The tracked tool this call names, or `None` for a name outside
    /// [`TrackedTool::from_name`] — such a call folds nothing and is not
    /// schema drift either.
    tool: Option<TrackedTool>,
    /// Raw tool arguments object, read by [`TrackedTool::read_args`] once the
    /// completion supplies the result half of the payload.
    arguments: Value,
    /// Start-event timestamp in epoch milliseconds, used for the detail record.
    timestamp: i64,
}

/// Attaches the read body without double-counting the invocation.
///
/// The caller has already bumped `tool_counts.read` so an empty or
/// undecodable body still shows up, and `add_read_detail` bumps it again for
/// a non-empty body, so the count is saved and restored around the call.
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

/// Resolve the content a Copilot `view`-family call saw, or `None` when
/// neither source reads — the arguments were still fine, so the call keeps its
/// invocation and only its body is reported as drift.
///
/// Two sources, in order:
///
/// 1. `arguments.view_range` — inclusive `[start, end]` line numbers. Only
///    the line count matters downstream, so a placeholder of that many lines
///    stands in for the body (see the comment on its shape below). A bound
///    that is not an integer reads as nothing, rather than as the `[0, 0]`
///    span an `unwrap_or(0)` would invent for it.
/// 2. `result.content` — the string the model actually received. Used when
///    no `view_range` was supplied.
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
        // A pure-newline placeholder ("\n".repeat(N - 1)) would survive
        // `count_lines` on its own, but `add_read_detail` first trims
        // trailing newlines and then the whole thing collapses to an
        // empty string — so the line tally would silently come back as
        // zero. Use single-char "lines" joined by '\n' so the trim is a
        // no-op and `count_lines` recovers exactly `line_count`.
        return Some(vec!["-"; line_count].join("\n"));
    }

    Some(result.get("content")?.as_str()?.to_string())
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

    /// Reads a completed call's payload the way the completion arm does.
    ///
    /// Panics on a name outside the tracked table so a test cannot silently
    /// assert about a tool the parser stopped resolving.
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

    /// What one folded call moved, so a tool filed under the wrong variant
    /// fails on the whole shape rather than on one counter it happens to share.
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

    /// An arguments object built from string pairs, for the alias walk below
    /// (whose keys are loop variables rather than literals).
    fn args_of(pairs: &[(&str, &str)]) -> Value {
        Value::Object(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), json!(value)))
                .collect(),
        )
    }

    fn count_lines_after_trim(s: &str) -> usize {
        // Mirror `add_read_detail`'s `trim_end_matches('\n')` + `count_lines`
        // so the test reflects the actual line tally the analyzer would
        // record.
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
        let placeholder = read_view_content(&args, &result).expect("a readable range");
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
        // A bound that is not an integer used to be called drift by the schema
        // check and read anyway by the fold, whose `unwrap_or(0)` turned it
        // into the span `[0, 0]` — one fabricated line for a range nobody
        // could read.
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
        // `TrackedTool::from_name` is the only place these names are spelled
        // now, and a list of strings is not something the compiler checks, so
        // every one is walked here: a name dropped from the table stops folding
        // altogether rather than merely losing its detail.
        let read_args = json!({ "path": "/tmp/a.txt", "view_range": [1, 2] });
        let search_args = json!({ "pattern": "needle" });
        let write_args = json!({ "path": "/tmp/a.txt", "content": "one\ntwo" });
        let edit_args = json!({ "path": "/tmp/a.txt", "old_string": "one", "new_string": "two" });
        let patch_args =
            json!("*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch");
        let shell_args = json!({ "command": "ls" });
        let write_bash_args = json!({ "input": "yes" });

        // Read lines are what separate the two families sharing the read
        // counter. Every other pair needs different arguments, so a name filed
        // under the wrong variant reads as nothing and folds nothing.
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

        // A name outside the table is not tracked, so it is neither folded nor
        // reported as drift.
        assert!(TrackedTool::from_name("report_intent").is_none());
    }

    #[test]
    fn an_edit_that_replaces_nothing_is_reclassified_as_a_write() {
        // Copilot folds edits through `add_edit_detail`, not the `_raw` form
        // its Gemini counterpart uses: an empty `old_string` here is a new file
        // expressed as a diff, and lands as a write. Nothing pinned that before
        // the two readers became one, and the schema check has always let an
        // empty string through.
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
        // The alias sets live in `TrackedTool::read_args` alone now, but a list
        // of strings is not something the compiler checks either. Each case
        // asserts on the folded *content*, not on a counter: an alias that
        // resolved to nothing would leave the counter right and the detail
        // empty.
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
        // `arg_str` resolves an alias list in order, which is what the chain of
        // `.or_else(|| args.get(..))` calls it replaced did. Reordering a list
        // is the one edit to `read_args` that changes an answer without
        // changing what the parser accepts.
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

        // A failed tool produces no metric even when it does pair, so its
        // orphan lost nothing and must not read as a gap.
        let failed =
            parse_copilot_events_with_diagnostics(vec![complete(false)], ParseMode::Full).unwrap();
        assert_eq!(failed.diagnostics.failed_relevant_records, 0);
        assert_eq!(failed.diagnostics.partial_failure_count(), 0);

        // A start whose schema drifted already recorded the failure, so its
        // completion must not record a second one for the same call.
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
        // OpenAI / Azure model names use dots natively; do not touch them.
        assert_eq!(canonicalize_model_name("gpt-5.1"), "gpt-5.1");
        assert_eq!(canonicalize_model_name("gpt-4.1-mini"), "gpt-4.1-mini");
        assert_eq!(canonicalize_model_name("gemini-1.5-pro"), "gemini-1.5-pro");
    }
}
