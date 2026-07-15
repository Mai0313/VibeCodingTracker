//! Parser for GitHub Copilot CLI session events
//! (`~/.copilot/session-state/<sessionId>/events.jsonl`).
//!
//! One [`CopilotEvent`] per line, dispatched on `event_type`. Token usage is
//! taken from the authoritative `session.shutdown` record when present, with
//! streamed `assistant.message.outputTokens` as a partial fallback for
//! sessions that never shut down cleanly. File operations are paired across
//! `tool.execution_start` / `tool.execution_complete` by `toolCallId`.
//! Invocations are counted when they start; only successful completions add
//! file effects. See the table below for the full event map.
use crate::constants::{FastHashMap, capacity};
use crate::models::*;
use crate::session::diagnostics::{
    AnalysisFact, AnalysisFactEffect, AnalysisMetrics, AnalysisStateSnapshot, ParseDiagnostics,
    ParsedAnalysis, PricingGranularity, ToolFactStatus, UsageFact, UsageFactUnit,
};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{accumulate_i64_fields, get_git_remote_url, parse_iso_timestamp};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashSet;

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
    let mut seen_tool_calls = HashSet::with_capacity(32);

    // Fallback accounting used when the session does not reach
    // `session.shutdown` (e.g. crash, SIGKILL, ongoing session). We still
    // want to attribute `assistant.message.outputTokens` to *some* model,
    // so we track the active model switches.
    let mut current_model = String::new();
    // Fallback output belongs only to the currently open epoch. A successful
    // shutdown replaces that epoch, then a later `session.resume` can start a
    // new fallback epoch in the same append-only file.
    let mut pending_output_tokens: FastHashMap<String, i64> = FastHashMap::with_capacity(3);
    let mut diagnostics = ParseDiagnostics::default();
    let mut usage_facts = Vec::new();
    let mut fallback_usage_facts = Vec::new();
    let mut analysis_facts = Vec::new();

    for (source_index, event) in events.into_iter().enumerate() {
        let source_order = source_index + 1;
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
                let mut covered_models = HashSet::new();
                let mut units = Vec::new();
                match event.data.get("modelMetrics").and_then(Value::as_object) {
                    Some(metrics) if metrics.is_empty() => diagnostics.record_relevant(true),
                    Some(metrics) => {
                        for (model, metric) in metrics {
                            if model.is_empty() {
                                diagnostics.record_relevant(false);
                                continue;
                            }
                            let Some(metric) = metric.as_object() else {
                                diagnostics.record_relevant(false);
                                continue;
                            };
                            let Some(usage_value) = metric.get("usage") else {
                                diagnostics.record_relevant(true);
                                continue;
                            };
                            if usage_value.is_null()
                                || usage_value
                                    .as_object()
                                    .is_some_and(serde_json::Map::is_empty)
                            {
                                diagnostics.record_relevant(true);
                                continue;
                            }
                            if !copilot_usage_supported(usage_value) {
                                diagnostics.record_relevant(false);
                                continue;
                            }
                            let Ok(usage) =
                                serde_json::from_value::<CopilotModelUsage>(usage_value.clone())
                            else {
                                diagnostics.record_relevant(false);
                                continue;
                            };

                            let (usage_json, split_valid) = normalize_copilot_usage(&usage);
                            let has_normalized_usage = usage_json
                                .as_object()
                                .is_some_and(|usage| !usage.is_empty());
                            if has_normalized_usage {
                                diagnostics.record_relevant(true);
                            }
                            if !split_valid {
                                diagnostics.record_relevant(false);
                            }
                            if !has_normalized_usage {
                                continue;
                            }
                            let model = canonicalize_model_name(model);
                            covered_models.insert(model.clone());
                            units.push(UsageFactUnit::from_value(
                                model.clone(),
                                &usage_json,
                                PricingGranularity::Aggregate,
                            ));
                            accumulate_copilot_usage(&mut conversation_usage, model, usage_json);
                        }
                    }
                    None => diagnostics.record_relevant(false),
                }

                finalize_copilot_fallback_epoch(
                    &covered_models,
                    &mut pending_output_tokens,
                    &mut fallback_usage_facts,
                    &mut usage_facts,
                    &mut conversation_usage,
                );
                if !units.is_empty() {
                    let stable_id =
                        (!event.id.is_empty()).then(|| format!("copilot-shutdown:{}", event.id));
                    usage_facts.push(UsageFact {
                        stable_id,
                        timestamp_ms: (ts > 0).then_some(ts),
                        observed_at_ms: (ts > 0).then_some(ts),
                        source_order,
                        units,
                    });
                }
            }
            "assistant.message" => {
                // Only used as a fallback when no `session.shutdown` arrives.
                if let Some(message_model) = event
                    .data
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|model| !model.is_empty())
                {
                    current_model = canonicalize_model_name(message_model);
                }
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
                        let usage = json!({
                            "input_tokens": 0,
                            "output_tokens": output_tokens,
                            "cache_read_input_tokens": 0,
                            "cache_creation_input_tokens": 0,
                            "total_tokens": output_tokens,
                        });
                        fallback_usage_facts.push(UsageFact {
                            stable_id: (!event.id.is_empty())
                                .then(|| format!("copilot-message:{}", event.id)),
                            timestamp_ms: (ts > 0).then_some(ts),
                            observed_at_ms: (ts > 0).then_some(ts),
                            source_order,
                            units: vec![UsageFactUnit::from_value(
                                current_model.clone(),
                                &usage,
                                PricingGranularity::Request,
                            )],
                        });
                    }
                }
            }
            "tool.execution_start" => {
                match serde_json::from_value::<CopilotToolStartData>(event.data.clone()) {
                    Ok(data) if !data.tool_name.is_empty() => {
                        let has_correlation_id = !data.tool_call_id.is_empty();
                        let tracked = is_tracked_tool(&data.tool_name);
                        let arguments_supported =
                            tracked_tool_arguments_supported(&data.tool_name, &data.arguments);
                        if tracked
                            && has_correlation_id
                            && !seen_tool_calls.insert(data.tool_call_id.clone())
                        {
                            continue;
                        }
                        let mut pending = PendingTool {
                            tool_call_id: data.tool_call_id,
                            tool_name: data.tool_name,
                            arguments: data.arguments,
                            model: current_model.clone(),
                            timestamp: ts,
                            tracked,
                            arguments_supported,
                            fact_index: None,
                        };
                        if tracked {
                            diagnostics.record_relevant(true);
                            if !arguments_supported {
                                diagnostics.record_relevant(false);
                            }
                            if !has_correlation_id {
                                diagnostics.record_relevant(false);
                            }
                            let before = AnalysisMetrics::from_state(&state);
                            record_tool_invocation(
                                &mut state,
                                &pending.tool_name,
                                &pending.arguments,
                            );
                            let fact_index = analysis_facts.len();
                            analysis_facts.push(AnalysisFact {
                                stable_id: has_correlation_id
                                    .then(|| format!("copilot-tool:{}", pending.tool_call_id)),
                                timestamp_ms: (pending.timestamp > 0).then_some(pending.timestamp),
                                observed_at_ms: (pending.timestamp > 0)
                                    .then_some(pending.timestamp),
                                source_order,
                                model: pending.model.clone(),
                                status: ToolFactStatus::Pending,
                                metrics: AnalysisMetrics::from_state(&state).saturating_sub(before),
                                effect: None,
                            });
                            pending.fact_index = Some(fact_index);
                        }
                        if has_correlation_id {
                            pending_tools.insert(pending.tool_call_id.clone(), pending);
                        } else if !tracked {
                            diagnostics.record_relevant(false);
                        }
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
                        update_copilot_fact(
                            &mut analysis_facts,
                            &pending,
                            ToolFactStatus::Pending,
                            ts,
                            AnalysisMetrics::default(),
                            None,
                        );
                    }
                    continue;
                };
                if !success {
                    if pending.tracked {
                        diagnostics.record_relevant(true);
                        update_copilot_fact(
                            &mut analysis_facts,
                            &pending,
                            ToolFactStatus::Failed,
                            ts,
                            AnalysisMetrics::default(),
                            None,
                        );
                    }
                    continue;
                }
                let data =
                    match serde_json::from_value::<CopilotToolCompleteData>(event.data.clone()) {
                        Ok(data) => data,
                        Err(_) => {
                            if pending.tracked {
                                diagnostics.record_relevant(false);
                                update_copilot_fact(
                                    &mut analysis_facts,
                                    &pending,
                                    ToolFactStatus::Succeeded,
                                    ts,
                                    AnalysisMetrics::default(),
                                    Some(AnalysisFactEffect::default()),
                                );
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
                        update_copilot_fact(
                            &mut analysis_facts,
                            &pending,
                            ToolFactStatus::Succeeded,
                            ts,
                            AnalysisMetrics::default(),
                            Some(AnalysisFactEffect::default()),
                        );
                        continue;
                    }
                }
                let effect_before = AnalysisStateSnapshot::capture(&state);
                let before = AnalysisMetrics::from_state(&state);
                dispatch_tool_effects(&mut state, &pending, &data);
                update_copilot_fact(
                    &mut analysis_facts,
                    &pending,
                    ToolFactStatus::Succeeded,
                    ts,
                    AnalysisMetrics::from_state(&state).saturating_sub(before),
                    Some(effect_before.effect_since(&state, Vec::new())),
                );
            }
            _ => {}
        }
    }

    // Keep streamed output for an epoch that has not reached its own shutdown,
    // even when an earlier epoch in the same resumed session shut down cleanly.
    finalize_copilot_fallback_epoch(
        &HashSet::new(),
        &mut pending_output_tokens,
        &mut fallback_usage_facts,
        &mut usage_facts,
        &mut conversation_usage,
    );

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
    let mut parsed = ParsedAnalysis::new(analysis, diagnostics);
    parsed.usage_facts = usage_facts;
    parsed.analysis_facts = analysis_facts;
    Ok(parsed)
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

fn finalize_copilot_fallback_epoch(
    covered_models: &HashSet<String>,
    pending_output_tokens: &mut FastHashMap<String, i64>,
    fallback_usage_facts: &mut Vec<UsageFact>,
    usage_facts: &mut Vec<UsageFact>,
    conversation_usage: &mut FastHashMap<String, Value>,
) {
    for model in covered_models {
        pending_output_tokens.remove(model);
    }
    fallback_usage_facts.retain_mut(|fact| {
        fact.units
            .retain(|unit| !covered_models.contains(&unit.model));
        !fact.units.is_empty()
    });
    for (model, output_tokens) in pending_output_tokens.drain() {
        accumulate_copilot_usage(
            conversation_usage,
            model,
            json!({
                "input_tokens": 0,
                "output_tokens": output_tokens,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0,
                "total_tokens": output_tokens,
            }),
        );
    }
    usage_facts.append(fallback_usage_facts);
}

fn normalize_copilot_usage(usage: &CopilotModelUsage) -> (Value, bool) {
    let published_total = if usage.input_tokens >= 0 && usage.output_tokens >= 0 {
        usage.input_tokens.checked_add(usage.output_tokens)
    } else {
        None
    };
    let cached_input = usage
        .cache_read_tokens
        .checked_add(usage.cache_write_tokens);
    if let (Some(cached_input), Some(published_total)) = (cached_input, published_total)
        && usage.cache_read_tokens >= 0
        && usage.cache_write_tokens >= 0
        && usage.reasoning_tokens >= 0
        && cached_input <= usage.input_tokens
        && usage.reasoning_tokens <= usage.output_tokens
    {
        return (
            json!({
                "input_tokens": usage.input_tokens - cached_input,
                "output_tokens": usage.output_tokens - usage.reasoning_tokens,
                "reasoning_output_tokens": usage.reasoning_tokens,
                "cache_read_input_tokens": usage.cache_read_tokens,
                "cache_creation_input_tokens": usage.cache_write_tokens,
                "total_tokens": published_total,
            }),
            true,
        );
    }

    let usage_json = published_total.map_or_else(
        || json!({}),
        |published_total| json!({ "total_tokens": published_total }),
    );
    (usage_json, false)
}

fn accumulate_copilot_usage(
    conversation_usage: &mut FastHashMap<String, Value>,
    model: String,
    usage: Value,
) {
    conversation_usage
        .entry(model)
        .and_modify(|existing| {
            let (Some(existing), Some(usage)) = (existing.as_object_mut(), usage.as_object())
            else {
                return;
            };
            accumulate_i64_fields(
                existing,
                usage,
                &[
                    "input_tokens",
                    "output_tokens",
                    "reasoning_output_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                    "total_tokens",
                ],
            );
        })
        .or_insert(usage);
}

fn is_tracked_tool(name: &str) -> bool {
    matches!(
        name,
        "view"
            | "show_file"
            | "read_file"
            | "rg"
            | "grep"
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
        "rg" | "grep" => true,
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

fn tracked_tool_result_supported(name: &str, _args: &Value, result: &Value) -> bool {
    match name {
        "view" | "show_file" | "read_file" => result.get("content").is_some_and(Value::is_string),
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
    /// Correlation id shared by the start and completion events.
    tool_call_id: String,
    /// Tool name (e.g. `view`, `create`, `str_replace`, `bash`).
    tool_name: String,
    /// Raw tool arguments object, interpreted lazily by [`dispatch_tool_effects`].
    arguments: Value,
    /// Model active when the tool was invoked.
    model: String,
    /// Start-event timestamp in epoch milliseconds, used for the detail record.
    timestamp: i64,
    /// Whether this tool contributes to the analysis projection.
    tracked: bool,
    /// Whether the tracked tool's arguments use a supported schema.
    arguments_supported: bool,
    /// Position of this invocation's private analysis fact.
    fact_index: Option<usize>,
}

fn record_tool_invocation(state: &mut SessionParseState, name: &str, arguments: &Value) {
    match name {
        "view" | "show_file" | "read_file" | "rg" | "grep" => {
            state.tool_counts.read += 1;
        }
        "create" | "write_file" | "write" => state.tool_counts.write += 1,
        "str_replace" | "edit" | "replace" | "edit_file" => {
            state.tool_counts.edit += 1;
        }
        "apply_patch" => {
            let patches = extract_apply_patch_text(arguments)
                .map(parse_apply_patch_text)
                .unwrap_or_default();
            let has_write = patches.iter().any(|patch| patch.action == "add");
            let has_edit = patches.iter().any(|patch| patch.action != "add");
            if has_write {
                state.tool_counts.write += 1;
            }
            if has_edit || !has_write {
                state.tool_counts.edit += 1;
            }
        }
        "bash" | "shell" | "execute" | "write_bash" => state.tool_counts.bash += 1,
        _ => {}
    }
}

fn update_copilot_fact(
    facts: &mut [AnalysisFact],
    pending: &PendingTool,
    status: ToolFactStatus,
    observed_at_ms: i64,
    effects: AnalysisMetrics,
    effect: Option<AnalysisFactEffect>,
) {
    let Some(index) = pending.fact_index else {
        return;
    };
    let Some(fact) = facts.get_mut(index) else {
        return;
    };
    fact.status = status;
    fact.observed_at_ms = (observed_at_ms > 0).then_some(observed_at_ms);
    if status == ToolFactStatus::Succeeded {
        fact.metrics.add_assign(effects);
        fact.effect = effect;
    }
}

/// Routes a completed Copilot tool call to the matching file-operation tally.
///
/// Branches on `pending.tool_name`; unrecognised tools (e.g. `glob`,
/// `task_complete`) are silently ignored. Argument field names are probed
/// with historical aliases for forward compatibility across CLI releases.
#[cfg(test)]
fn dispatch_tool(
    state: &mut SessionParseState,
    pending: &PendingTool,
    complete: &CopilotToolCompleteData,
) {
    record_tool_invocation(state, &pending.tool_name, &pending.arguments);
    dispatch_tool_effects(state, pending, complete);
}

/// Attaches effects from a confirmed successful completion without counting
/// the invocation again.
fn dispatch_tool_effects(
    state: &mut SessionParseState,
    pending: &PendingTool,
    complete: &CopilotToolCompleteData,
) {
    let ts = pending.timestamp;
    let args = &pending.arguments;
    let invocation_counts = state.tool_counts.clone();

    match pending.tool_name.as_str() {
        // Current Copilot CLI exposes `view` for reads. Historical versions
        // used `str_replace_editor` with `command == "view"`, which we no
        // longer attempt to parse.
        "view" | "show_file" | "read_file" => {
            let Some(path) = args.get("path").and_then(|p| p.as_str()) else {
                return;
            };

            let content = extract_view_content(args, &complete.result);
            attach_read_detail(state, path, &content, ts);
        }
        // Search tools read repository content but do not identify one complete
        // file body, so retain the invocation without inventing line totals.
        "rg" | "grep" => {}
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
        // `glob`, `report_intent`, `task_complete`, `update_topic`, … have
        // no file-operation semantics we care about. Silently ignore.
        _ => {}
    }

    state.tool_counts = invocation_counts;
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
/// `view_range` describes the requested slice, but only `result.content`
/// tells us what the model actually received. Counting the requested range
/// would invent lines and characters when the returned content differs.
fn extract_view_content(_arguments: &Value, result: &Value) -> String {
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
    use crate::session::diagnostics::ToolFactStatus;
    use crate::session::state::ParseMode;
    use crate::session::state::SessionParseState;
    use serde_json::{Value, json};

    fn event(event_type: &str, data: Value) -> CopilotEvent {
        event_at(event_type, data, "2026-07-12T00:00:00Z")
    }

    fn event_at(event_type: &str, data: Value, timestamp: &str) -> CopilotEvent {
        CopilotEvent {
            event_type: event_type.to_string(),
            data,
            id: String::new(),
            timestamp: timestamp.to_string(),
            parent_id: None,
        }
    }

    #[test]
    fn view_range_uses_actual_result_content() {
        let args = json!({ "view_range": [1, 5], "path": "/tmp/foo" });
        let result = json!({ "content": "a\nbb" });
        assert_eq!(extract_view_content(&args, &result), "a\nbb");
    }

    #[test]
    fn view_range_without_result_content_is_unsupported() {
        let args = json!({ "view_range": [1, 5], "path": "/tmp/foo" });
        let result = json!({});
        assert_eq!(extract_view_content(&args, &result), "");
        assert!(!tracked_tool_result_supported("view", &args, &result));
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

    fn pending_tool(tool_name: &str, arguments: Value) -> PendingTool {
        PendingTool {
            tool_call_id: "call-1".to_string(),
            tool_name: tool_name.to_string(),
            arguments,
            model: "test".to_string(),
            timestamp: 1,
            tracked: true,
            arguments_supported: true,
            fact_index: None,
        }
    }

    #[test]
    fn current_show_file_maps_to_read() {
        let pending = pending_tool(
            "show_file",
            json!({ "path": "/tmp/a.txt", "view_range": [1, 5] }),
        );
        let mut state = SessionParseState::new();
        dispatch_tool(&mut state, &pending, &completed());
        assert_eq!(state.tool_counts.read, 1);
        assert_eq!(state.total_read_lines, 2);
        assert_eq!(state.total_read_characters, 10);
    }

    #[test]
    fn show_file_empty_and_drifted_results_keep_the_known_invocation() {
        let pending = pending_tool("show_file", json!({ "path": "/tmp/empty.txt" }));

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
        for tool_name in ["rg", "grep"] {
            let pending = pending_tool(tool_name, json!({ "pattern": "needle", "paths": ["src"] }));
            dispatch_tool(&mut state, &pending, &completed());
        }
        assert_eq!(state.tool_counts.read, 2);
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
        let pending = pending_tool(
            "apply_patch",
            json!(
                "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** Add File: notes.txt\n+hello\n*** End Patch"
            ),
        );
        let mut state = SessionParseState::new();
        dispatch_tool(&mut state, &pending, &completed());
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.tool_counts.write, 1);
        assert_eq!(state.edit_details.len(), 1);
        assert_eq!(state.write_details.len(), 1);
    }

    #[test]
    fn current_apply_patch_string_field_maps_to_edit() {
        let pending = pending_tool(
            "apply_patch",
            json!({
                "string": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
            }),
        );
        let mut state = SessionParseState::new();
        dispatch_tool(&mut state, &pending, &completed());
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.edit_details.len(), 1);
    }

    #[test]
    fn current_apply_patch_input_field_maps_to_edit() {
        let pending = pending_tool(
            "apply_patch",
            json!({
                "input": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
            }),
        );
        let mut state = SessionParseState::new();
        dispatch_tool(&mut state, &pending, &completed());
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.edit_details.len(), 1);
    }

    #[test]
    fn current_write_bash_counts_nonempty_input() {
        let pending = pending_tool(
            "write_bash",
            json!({ "input": "yes", "shellId": "shell-1" }),
        );
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
    fn shutdown_keeps_supported_models_and_fallback_for_models_without_usage() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event(
                    "session.model_change",
                    json!({ "newModel": "covered-model" }),
                ),
                event("assistant.message", json!({ "outputTokens": 5 })),
                event("session.model_change", json!({ "newModel": "null-model" })),
                event("assistant.message", json!({ "outputTokens": 7 })),
                event("session.model_change", json!({ "newModel": "empty-model" })),
                event("assistant.message", json!({ "outputTokens": 11 })),
                event(
                    "session.shutdown",
                    json!({
                        "modelMetrics": {
                            "covered-model": {
                                "usage": { "inputTokens": 100, "outputTokens": 20 }
                            },
                            "null-model": { "usage": null },
                            "empty-model": { "usage": {} },
                            "drifted-model": {
                                "usage": { "promptTokens": 123, "completionTokens": 45 }
                            }
                        }
                    }),
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();

        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        let usage = &parsed.analysis.records[0].conversation_usage;
        assert_eq!(usage["covered-model"]["total_tokens"], 120);
        assert_eq!(usage["null-model"]["total_tokens"], 7);
        assert_eq!(usage["empty-model"]["total_tokens"], 11);
        assert!(!usage.contains_key("drifted-model"));
        assert_eq!(parsed.usage_facts.len(), 3);
    }

    #[test]
    fn assistant_message_model_replaces_auto_fallback_before_shutdown() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event("session.model_change", json!({ "newModel": "auto" })),
                event(
                    "assistant.message",
                    json!({ "model": "gpt-5.4-mini", "outputTokens": 20 }),
                ),
                event(
                    "session.shutdown",
                    json!({
                        "modelMetrics": {
                            "gpt-5.4-mini": {
                                "usage": { "inputTokens": 100, "outputTokens": 20 }
                            }
                        }
                    }),
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();

        let usage = &parsed.analysis.records[0].conversation_usage;
        assert!(!usage.contains_key("auto"));
        assert_eq!(usage["gpt-5.4-mini"]["total_tokens"], 120);
        assert_eq!(parsed.usage_facts.len(), 1);
        assert_eq!(parsed.usage_facts[0].units[0].model, "gpt-5.4-mini");
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
    }

    #[test]
    fn unsupported_shutdown_usage_keeps_streamed_fallback_and_diagnostics() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event(
                    "session.model_change",
                    json!({ "newModel": "future-model" }),
                ),
                event("assistant.message", json!({ "outputTokens": 9 })),
                event(
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
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();

        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert_eq!(
            parsed.analysis.records[0].conversation_usage["future-model"],
            json!({
                "input_tokens": 0,
                "output_tokens": 9,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0,
                "total_tokens": 9
            })
        );
        assert_eq!(parsed.usage_facts.len(), 1);
        assert_eq!(parsed.usage_facts[0].units[0].counts.total, 9);
    }

    #[test]
    fn invalid_shutdown_parent_totals_keep_streamed_fallback_and_diagnostics() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event(
                    "session.model_change",
                    json!({ "newModel": "broken-model" }),
                ),
                event("assistant.message", json!({ "outputTokens": 9 })),
                event(
                    "session.shutdown",
                    json!({
                        "modelMetrics": {
                            "broken-model": {
                                "usage": { "inputTokens": -1, "outputTokens": 4 }
                            }
                        }
                    }),
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();

        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert_eq!(
            parsed.analysis.records[0].conversation_usage["broken-model"]["total_tokens"],
            9
        );
        assert_eq!(parsed.usage_facts.len(), 1);
        assert_eq!(parsed.usage_facts[0].units[0].counts.total, 9);
    }

    #[test]
    fn partial_shutdown_seals_uncovered_fallback_before_a_resumed_epoch() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event(
                    "session.model_change",
                    json!({ "newModel": "fallback-model" }),
                ),
                event("assistant.message", json!({ "outputTokens": 7 })),
                event(
                    "session.shutdown",
                    json!({
                        "modelMetrics": {
                            "covered-model": {
                                "usage": { "inputTokens": 100, "outputTokens": 20 }
                            },
                            "fallback-model": { "usage": null }
                        }
                    }),
                ),
                event("session.resume", json!({})),
                event(
                    "session.model_change",
                    json!({ "newModel": "fallback-model" }),
                ),
                event("assistant.message", json!({ "outputTokens": 3 })),
                event(
                    "session.shutdown",
                    json!({
                        "modelMetrics": {
                            "fallback-model": {
                                "usage": { "inputTokens": 10, "outputTokens": 2 }
                            }
                        }
                    }),
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();

        assert_eq!(
            parsed.analysis.records[0].conversation_usage["fallback-model"],
            json!({
                "input_tokens": 10,
                "output_tokens": 9,
                "reasoning_output_tokens": 0,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0,
                "total_tokens": 19
            })
        );
        let fallback_total: i64 = parsed
            .usage_facts
            .iter()
            .flat_map(|fact| &fact.units)
            .filter(|unit| unit.model == "fallback-model")
            .map(|unit| unit.counts.total)
            .sum();
        assert_eq!(fallback_total, 19);
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
    fn shutdown_splits_inclusive_token_parents_into_disjoint_buckets() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "current-model": {
                            "usage": {
                                "inputTokens": 2_000,
                                "outputTokens": 300,
                                "cacheReadTokens": 600,
                                "cacheWriteTokens": 100,
                                "reasoningTokens": 50
                            }
                        }
                    }
                }),
            )],
            ParseMode::Full,
        )
        .unwrap();

        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(
            parsed.analysis.records[0].conversation_usage["current-model"],
            json!({
                "input_tokens": 1_300,
                "output_tokens": 250,
                "reasoning_output_tokens": 50,
                "cache_read_input_tokens": 600,
                "cache_creation_input_tokens": 100,
                "total_tokens": 2_300
            })
        );
    }

    #[test]
    fn shutdown_preserves_total_but_drops_invalid_pricing_splits() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "bad-cache": {
                            "usage": {
                                "inputTokens": 100,
                                "outputTokens": 40,
                                "cacheReadTokens": 80,
                                "cacheWriteTokens": 30,
                                "reasoningTokens": 10
                            }
                        },
                        "bad-reasoning": {
                            "usage": {
                                "inputTokens": 100,
                                "outputTokens": 40,
                                "cacheReadTokens": 20,
                                "cacheWriteTokens": 0,
                                "reasoningTokens": 41
                            }
                        }
                    }
                }),
            )],
            ParseMode::Full,
        )
        .unwrap();

        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 2);
        for model in ["bad-cache", "bad-reasoning"] {
            assert_eq!(
                parsed.analysis.records[0].conversation_usage[model],
                json!({ "total_tokens": 140 }),
                "invalid subsets must not become priced token buckets"
            );
        }
    }

    #[test]
    fn resumed_shutdown_epochs_accumulate_in_public_usage_and_facts() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event("session.model_change", json!({ "newModel": "model-a" })),
                event_at(
                    "session.shutdown",
                    json!({
                        "modelMetrics": {
                            "model-a": {
                                "usage": { "inputTokens": 100, "outputTokens": 20 }
                            }
                        }
                    }),
                    "2026-07-12T00:00:01Z",
                ),
                event_at("session.resume", json!({}), "2026-07-12T00:00:02Z"),
                event_at(
                    "session.shutdown",
                    json!({
                        "modelMetrics": {
                            "model-a": {
                                "usage": { "inputTokens": 40, "outputTokens": 10 }
                            }
                        }
                    }),
                    "2026-07-12T00:00:03Z",
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();

        assert_eq!(parsed.usage_facts.len(), 2);
        let fact_total: i64 = parsed
            .usage_facts
            .iter()
            .flat_map(|fact| &fact.units)
            .map(|unit| unit.counts.total)
            .sum();
        assert_eq!(fact_total, 170);
        assert_eq!(
            parsed.analysis.records[0].conversation_usage["model-a"],
            json!({
                "input_tokens": 140,
                "output_tokens": 30,
                "reasoning_output_tokens": 0,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0,
                "total_tokens": 170
            })
        );
    }

    #[test]
    fn resumed_open_epoch_keeps_post_shutdown_output_fallback() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event("session.model_change", json!({ "newModel": "model-a" })),
                event("assistant.message", json!({ "outputTokens": 5 })),
                event_at(
                    "session.shutdown",
                    json!({
                        "modelMetrics": {
                            "model-a": {
                                "usage": { "inputTokens": 100, "outputTokens": 20 }
                            }
                        }
                    }),
                    "2026-07-12T00:00:01Z",
                ),
                event_at("session.resume", json!({}), "2026-07-12T00:00:02Z"),
                event_at(
                    "assistant.message",
                    json!({ "outputTokens": 7 }),
                    "2026-07-12T00:00:03Z",
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();

        assert_eq!(parsed.usage_facts.len(), 2);
        let usage = &parsed.analysis.records[0].conversation_usage["model-a"];
        assert_eq!(usage["input_tokens"], 100);
        assert_eq!(usage["output_tokens"], 27);
        assert_eq!(usage["total_tokens"], 127);
        let fact_total: i64 = parsed
            .usage_facts
            .iter()
            .flat_map(|fact| &fact.units)
            .map(|unit| unit.counts.total)
            .sum();
        assert_eq!(fact_total, 127);
    }

    #[test]
    fn missing_tool_success_and_explicit_failure_keep_the_invocation() {
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
        assert!(!missing.diagnostics.is_complete_failure());
        assert_eq!(missing.diagnostics.partial_failure_count(), 1);
        assert_eq!(missing.analysis.records[0].tool_call_counts.read, 1);
        assert_eq!(missing.analysis.records[0].total_read_lines, 0);
        assert_eq!(missing.analysis.records[0].total_unique_files, 0);
        assert_eq!(missing.analysis_facts.len(), 1);
        assert_eq!(missing.analysis_facts[0].status, ToolFactStatus::Pending);
        assert_eq!(missing.analysis_facts[0].metrics.read_count, 1);
        assert_eq!(missing.analysis_facts[0].metrics.read_lines, 0);

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
        assert_eq!(failed.analysis.records[0].tool_call_counts.read, 1);
        assert_eq!(failed.analysis.records[0].total_read_lines, 0);
        assert_eq!(failed.analysis.records[0].total_unique_files, 0);
        assert_eq!(failed.analysis_facts.len(), 1);
        assert_eq!(failed.analysis_facts[0].status, ToolFactStatus::Failed);
        assert_eq!(failed.analysis_facts[0].metrics.read_count, 1);
        assert_eq!(failed.analysis_facts[0].metrics.read_lines, 0);
    }

    #[test]
    fn tracked_tool_without_correlation_id_is_anonymous_without_effects() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event("session.model_change", json!({ "newModel": "model-a" })),
                event(
                    "tool.execution_start",
                    json!({
                        "toolName": "show_file",
                        "arguments": { "path": "/tmp/a.txt" }
                    }),
                ),
                event(
                    "tool.execution_complete",
                    json!({
                        "success": true,
                        "result": { "content": "one\ntwo" }
                    }),
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();
        let record = &parsed.analysis.records[0];

        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 0);
        assert_eq!(record.total_unique_files, 0);
        assert!(record.read_file_details.is_empty());
        assert_eq!(parsed.analysis_facts.len(), 1);
        let fact = &parsed.analysis_facts[0];
        assert!(fact.stable_id.is_none());
        assert_eq!(fact.model, "model-a");
        assert_eq!(fact.status, ToolFactStatus::Pending);
        assert_eq!(fact.metrics.read_count, 1);
        assert_eq!(fact.metrics.read_lines, 0);
        assert!(fact.effect.is_none());
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 2);
    }

    #[test]
    fn null_tool_correlation_id_is_anonymous_without_effects() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event("session.model_change", json!({ "newModel": "model-a" })),
                event(
                    "tool.execution_start",
                    json!({
                        "toolCallId": null,
                        "toolName": "show_file",
                        "arguments": { "path": "/tmp/a.txt" }
                    }),
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();
        let record = &parsed.analysis.records[0];

        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 0);
        assert_eq!(record.total_unique_files, 0);
        assert_eq!(parsed.analysis_facts.len(), 1);
        assert!(parsed.analysis_facts[0].stable_id.is_none());
        assert_eq!(parsed.analysis_facts[0].status, ToolFactStatus::Pending);
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
    }

    #[test]
    fn tool_lifecycle_counts_every_invocation_but_only_successful_effects() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event("session.model_change", json!({ "newModel": "model-a" })),
                event(
                    "tool.execution_start",
                    json!({
                        "toolCallId": "pending-read",
                        "toolName": "show_file",
                        "arguments": { "path": "/tmp/pending.txt" }
                    }),
                ),
                event("abort", json!({ "reason": "cancelled" })),
                event(
                    "tool.execution_start",
                    json!({
                        "toolCallId": "failed-write",
                        "toolName": "create",
                        "arguments": { "path": "/tmp/failed.txt", "file_text": "ignored" }
                    }),
                ),
                event(
                    "tool.execution_complete",
                    json!({
                        "toolCallId": "failed-write",
                        "success": false,
                        "error": { "message": "rejected" }
                    }),
                ),
                event(
                    "tool.execution_start",
                    json!({
                        "toolCallId": "successful-edit",
                        "toolName": "str_replace",
                        "arguments": {
                            "path": "/tmp/success.txt",
                            "old_string": "old",
                            "new_string": "new\nnext"
                        }
                    }),
                ),
                event(
                    "tool.execution_complete",
                    json!({
                        "toolCallId": "successful-edit",
                        "success": true,
                        "result": { "content": "updated" },
                        "model": "model-a"
                    }),
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();

        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.total_read_lines, 0);
        assert_eq!(record.total_write_lines, 0);
        assert_eq!(record.total_edit_lines, 2);
        assert_eq!(record.total_unique_files, 1);
        assert!(record.read_file_details.is_empty());
        assert!(record.write_file_details.is_empty());
        assert_eq!(record.edit_file_details.len(), 1);

        assert_eq!(parsed.analysis_facts.len(), 3);
        let pending = &parsed.analysis_facts[0];
        assert_eq!(
            pending.stable_id.as_deref(),
            Some("copilot-tool:pending-read")
        );
        assert_eq!(pending.status, ToolFactStatus::Pending);
        assert_eq!(pending.model, "model-a");
        assert_eq!(pending.metrics.read_count, 1);
        assert_eq!(pending.metrics.read_lines, 0);
        assert!(pending.effect.is_none());

        let failed = &parsed.analysis_facts[1];
        assert_eq!(
            failed.stable_id.as_deref(),
            Some("copilot-tool:failed-write")
        );
        assert_eq!(failed.status, ToolFactStatus::Failed);
        assert_eq!(failed.model, "model-a");
        assert_eq!(failed.metrics.write_count, 1);
        assert_eq!(failed.metrics.write_lines, 0);
        assert!(failed.effect.is_none());

        let succeeded = &parsed.analysis_facts[2];
        assert_eq!(
            succeeded.stable_id.as_deref(),
            Some("copilot-tool:successful-edit")
        );
        assert_eq!(succeeded.status, ToolFactStatus::Succeeded);
        assert_eq!(succeeded.model, "model-a");
        assert_eq!(succeeded.metrics.edit_count, 1);
        assert_eq!(succeeded.metrics.edit_lines, 2);
        let effect = succeeded.effect.as_ref().expect("successful effect");
        assert_eq!(effect.unique_files, vec!["/tmp/success.txt"]);
        assert_eq!(effect.edit_file_details.len(), 1);
        assert_eq!(
            effect.edit_file_details[0].base.file_path,
            "/tmp/success.txt"
        );
    }

    #[test]
    fn tool_facts_use_the_model_active_at_invocation() {
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event("session.model_change", json!({ "newModel": "model-a" })),
                event_at(
                    "tool.execution_start",
                    json!({
                        "toolCallId": "read-a",
                        "toolName": "show_file",
                        "arguments": { "path": "/tmp/a.txt" }
                    }),
                    "2026-07-12T00:00:01Z",
                ),
                event("session.model_change", json!({ "newModel": "model-b" })),
                event_at(
                    "tool.execution_complete",
                    json!({
                        "toolCallId": "read-a",
                        "success": true,
                        "result": { "content": "one\ntwo" },
                        "model": "model-b"
                    }),
                    "2026-07-12T00:00:03Z",
                ),
                event_at(
                    "tool.execution_start",
                    json!({
                        "toolCallId": "write-b",
                        "toolName": "create",
                        "arguments": { "path": "/tmp/b.txt", "file_text": "three" }
                    }),
                    "2026-07-12T00:00:04Z",
                ),
                event_at(
                    "tool.execution_complete",
                    json!({
                        "toolCallId": "write-b",
                        "success": true,
                        "result": { "content": "created" },
                        "model": "model-a"
                    }),
                    "2026-07-12T00:00:05Z",
                ),
            ],
            ParseMode::Full,
        )
        .unwrap();

        assert_eq!(parsed.analysis_facts.len(), 2);
        assert_eq!(parsed.analysis_facts[0].model, "model-a");
        assert_eq!(
            parsed.analysis_facts[0].timestamp_ms,
            Some(crate::utils::parse_iso_timestamp("2026-07-12T00:00:01Z"))
        );
        assert_eq!(
            parsed.analysis_facts[0].observed_at_ms,
            Some(crate::utils::parse_iso_timestamp("2026-07-12T00:00:03Z"))
        );
        assert_eq!(parsed.analysis_facts[0].metrics.read_count, 1);
        assert_eq!(parsed.analysis_facts[0].metrics.read_lines, 2);
        assert_eq!(parsed.analysis_facts[1].model, "model-b");
        assert_eq!(
            parsed.analysis_facts[1].timestamp_ms,
            Some(crate::utils::parse_iso_timestamp("2026-07-12T00:00:04Z"))
        );
        assert_eq!(parsed.analysis_facts[1].metrics.write_count, 1);
        assert_eq!(parsed.analysis_facts[1].metrics.write_lines, 1);
    }

    #[test]
    fn replayed_tool_lifecycle_is_counted_once() {
        let start = || {
            event(
                "tool.execution_start",
                json!({
                    "toolCallId": "replayed-read",
                    "toolName": "show_file",
                    "arguments": { "path": "/tmp/a.txt" }
                }),
            )
        };
        let complete = || {
            event(
                "tool.execution_complete",
                json!({
                    "toolCallId": "replayed-read",
                    "success": true,
                    "result": { "content": "one\ntwo" },
                    "model": "model-a"
                }),
            )
        };
        let parsed = parse_copilot_events_with_diagnostics(
            vec![
                event("session.model_change", json!({ "newModel": "model-a" })),
                start(),
                complete(),
                start(),
                complete(),
            ],
            ParseMode::Full,
        )
        .unwrap();

        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 2);
        assert_eq!(record.read_file_details.len(), 1);
        assert_eq!(parsed.analysis_facts.len(), 1);
        assert_eq!(parsed.analysis_facts[0].metrics.read_count, 1);
        assert_eq!(parsed.analysis_facts[0].metrics.read_lines, 2);
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
