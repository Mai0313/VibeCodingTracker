//! Parser for OpenAI Codex rollout logs (`~/.codex/sessions/**/*.jsonl`).
//!
//! Codex records carry a `type` discriminator (`session_meta`,
//! `turn_context`, `event_msg`, `response_item`). File operations are not
//! first-class: Codex records them as legacy `function_call` pairs or current
//! `custom_tool_call` pairs. This parser joins each call to its output by
//! `call_id`. Legacy shell calls retain command-level analysis. A top-level
//! custom `exec` is counted as one run command because its JavaScript source
//! does not prove which nested tools actually ran. Direct custom patches are
//! applied only when their paired output explicitly reports success.
use crate::constants::FastHashMap;
use crate::models::*;
use crate::session::diagnostics::{
    AnalysisFact, AnalysisFactEffect, AnalysisMetrics, AnalysisStateSnapshot, ParseDiagnostics,
    ParsedAnalysis, PricingGranularity, ToolFactStatus, UsageFact, UsageFactUnit,
};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{get_git_remote_url, parse_iso_timestamp, process_codex_usage};
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::borrow::Borrow;
use std::collections::HashSet;

const CODEX_TOKEN_FIELDS: [&str; 5] = [
    "input_tokens",
    "cached_input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "total_tokens",
];

/// Parse Codex session records from a slice of pre-typed logs.
///
/// Walks the records in order, threading function and custom-tool calls to
/// their outputs and folding token usage from `token_count` events. Returns a
/// single-record [`CodeAnalysis`] with the runtime metadata fields blank
/// (the caller's `finalize` step fills them).
///
/// # Errors
///
/// Returns `anyhow::Result` for parity with the other provider parsers, but
/// has no fallible step: malformed arguments and outputs are tolerated
/// (skipped or substituted), so it returns `Ok` for any slice.
pub fn parse_codex_logs(logs: &[CodexLog], mode: ParseMode) -> Result<CodeAnalysis> {
    parse_codex_log_iter(logs, mode)
}

/// Streaming Codex parser used by JSONL readers that already own an iterator.
pub(crate) fn parse_codex_log_iter<I>(logs: I, mode: ParseMode) -> Result<CodeAnalysis>
where
    I: IntoIterator,
    I::Item: Borrow<CodexLog>,
{
    Ok(parse_codex_log_iter_with_diagnostics(logs, mode)?.analysis)
}

/// Streaming Codex parser with parser-only schema diagnostics.
pub(crate) fn parse_codex_log_iter_with_diagnostics<I>(
    logs: I,
    mode: ParseMode,
) -> Result<ParsedAnalysis>
where
    I: IntoIterator,
    I::Item: Borrow<CodexLog>,
{
    let mut state = SessionParseState::with_mode(mode);
    let mut conversation_usage: FastHashMap<String, Value> = FastHashMap::with_capacity(5);
    let mut current_model = String::new();
    let mut previous_total_usage = None;
    let mut shell_calls: FastHashMap<String, PendingCodexShellInvocation> =
        FastHashMap::with_capacity(50);
    let mut running_shell_calls: FastHashMap<u64, RunningCodexShellInvocation> =
        FastHashMap::with_capacity(16);
    let mut shell_poll_sessions: FastHashMap<String, u64> = FastHashMap::with_capacity(16);
    let mut custom_calls: FastHashMap<String, PendingCodexCustomInvocation> =
        FastHashMap::with_capacity(32);
    let mut processed_patch_calls: HashSet<String> = HashSet::with_capacity(32);
    let mut diagnostics = ParseDiagnostics::default();
    let mut usage_facts = Vec::new();
    let mut analysis_facts = Vec::new();
    let mut analysis_fact_indices: FastHashMap<String, usize> = FastHashMap::with_capacity(50);
    let mut source_order = 0usize;

    for entry in logs {
        source_order += 1;
        let entry = entry.borrow();
        let recognized = matches!(
            entry.log_type.as_str(),
            "session_meta"
                | "turn_context"
                | "event_msg"
                | "response_item"
                | "inter_agent_communication_metadata"
                | "world_state"
                | "compacted"
        );
        if recognized {
            diagnostics.record_recognized_source();
        } else {
            diagnostics.record_unrecognized();
        }
        let ts = parse_iso_timestamp(&entry.timestamp);
        if ts > state.last_ts {
            state.last_ts = ts;
        }

        match entry.log_type.as_str() {
            "session_meta" => {
                if state.folder_path.is_empty()
                    && let Some(cwd) = &entry.payload.cwd
                {
                    state.folder_path.clone_from(cwd); // More efficient than clone()
                }
                if state.task_id.is_empty()
                    && let Some(id) = &entry.payload.id
                {
                    state.task_id.clone_from(id);
                }
                if state.git_remote.is_empty()
                    && let Some(git) = &entry.payload.git
                    && let Some(url) = &git.repository_url
                {
                    state.git_remote.clone_from(url);
                }
            }
            "turn_context" => {
                if state.folder_path.is_empty()
                    && let Some(cwd) = &entry.payload.cwd
                {
                    state.folder_path.clone_from(cwd);
                }
                if let Some(model) = entry
                    .payload
                    .model
                    .as_ref()
                    .filter(|model| !model.is_empty())
                {
                    current_model.clone_from(model); // Reuse existing allocation
                }
            }
            "event_msg" => {
                if let Some(payload_type) = &entry.payload.payload_type {
                    if payload_type == "token_count"
                        && let Some(info) =
                            entry.payload.info.as_ref().filter(|info| !info.is_null())
                    {
                        if !is_supported_codex_usage(info) {
                            diagnostics.record_relevant(false);
                        } else {
                            diagnostics.record_relevant(true);
                            if let Some(usage) = accumulate_codex_usage_event(
                                &mut conversation_usage,
                                &current_model,
                                info,
                                &mut previous_total_usage,
                            ) {
                                usage_facts.push(UsageFact::anonymous(
                                    ts,
                                    source_order,
                                    vec![UsageFactUnit::from_value(
                                        current_model.clone(),
                                        &usage,
                                        PricingGranularity::Request,
                                    )],
                                ));
                            }
                        }
                    } else if payload_type == "patch_apply_end" {
                        let Some(call_id) = entry.payload.call_id.as_deref() else {
                            diagnostics.record_relevant(false);
                            continue;
                        };
                        if processed_patch_calls.contains(call_id) {
                            diagnostics.record_relevant(true);
                            continue;
                        }

                        let pending_custom = custom_calls.remove(call_id);
                        let pending_shell = shell_calls.remove(call_id);
                        let invocation_ts = pending_custom
                            .as_ref()
                            .map(PendingCodexCustomInvocation::timestamp)
                            .or_else(|| {
                                pending_shell
                                    .as_ref()
                                    .map(PendingCodexShellInvocation::timestamp)
                            })
                            .unwrap_or(ts);
                        let kind = entry
                            .payload
                            .info
                            .as_ref()
                            .and_then(Value::as_object)
                            .map(structured_patch_kind)
                            .unwrap_or(PatchInvocationKind::Edit);
                        let fact_index = pending_custom
                            .as_ref()
                            .map(PendingCodexCustomInvocation::fact_index)
                            .or_else(|| {
                                pending_shell
                                    .as_ref()
                                    .map(PendingCodexShellInvocation::fact_index)
                            })
                            .unwrap_or_else(|| {
                                ensure_codex_analysis_fact(
                                    &mut analysis_facts,
                                    &mut analysis_fact_indices,
                                    call_id,
                                    &current_model,
                                    invocation_ts,
                                    source_order,
                                    patch_invocation_metrics(kind),
                                )
                            });
                        set_codex_fact_invocation_metrics(
                            &mut analysis_facts,
                            fact_index,
                            patch_invocation_metrics(kind),
                        );
                        let outcome = structured_patch_result(&entry.payload);
                        let effect_paths = structured_patch_effect_paths(&state, &entry.payload);
                        let effect_before = AnalysisStateSnapshot::capture(&state);
                        let before = AnalysisMetrics::from_state(&state);
                        let normalized =
                            dispatch_structured_patch(&mut state, &entry.payload, invocation_ts);
                        diagnostics.record_relevant(normalized);
                        update_codex_analysis_fact(
                            &mut analysis_facts,
                            fact_index,
                            outcome.fact_status(),
                            ts,
                            successful_effect_metrics(before, &state, outcome.is_success()),
                            (outcome.is_success() && normalized)
                                .then(|| effect_before.effect_since(&state, effect_paths)),
                        );
                        processed_patch_calls.insert(call_id.to_string());
                    }
                }
            }
            "response_item" => {
                if let Some(payload_type) = &entry.payload.payload_type {
                    match payload_type.as_str() {
                        "function_call" => {
                            if let Some(name) = entry.payload.name.as_deref()
                                && name == "write_stdin"
                            {
                                if let Some(call_id) = entry
                                    .payload
                                    .call_id
                                    .as_deref()
                                    .filter(|call_id| !call_id.is_empty())
                                    && let Some(session_id) = entry
                                        .payload
                                        .arguments
                                        .as_deref()
                                        .and_then(parse_write_stdin_session_id)
                                {
                                    shell_poll_sessions.insert(call_id.to_string(), session_id);
                                }
                            } else if let Some(name) = entry.payload.name.as_deref()
                                && matches!(name, "shell" | "exec_command")
                            {
                                let call = entry
                                    .payload
                                    .arguments
                                    .as_deref()
                                    .and_then(|args| parse_function_call(name, args, ts));
                                let metrics = call
                                    .as_ref()
                                    .map(shell_invocation_metrics)
                                    .unwrap_or_else(bash_invocation_metrics);
                                if let Some(call_id) = entry
                                    .payload
                                    .call_id
                                    .as_deref()
                                    .filter(|call_id| !call_id.is_empty())
                                {
                                    let fact_index = ensure_codex_analysis_fact(
                                        &mut analysis_facts,
                                        &mut analysis_fact_indices,
                                        call_id,
                                        &current_model,
                                        ts,
                                        source_order,
                                        metrics,
                                    );
                                    if let Some(call) = call {
                                        diagnostics.record_relevant(true);
                                        shell_calls.insert(
                                            call_id.to_string(),
                                            PendingCodexShellInvocation {
                                                call: Some(call),
                                                timestamp: ts,
                                                fact_index,
                                                accumulated_output: String::new(),
                                            },
                                        );
                                    } else {
                                        // Defer the schema verdict until the paired output. Codex
                                        // persists model-generated argument errors as ordinary
                                        // lifecycle records even though no command ran.
                                        shell_calls.insert(
                                            call_id.to_string(),
                                            PendingCodexShellInvocation {
                                                call: None,
                                                timestamp: ts,
                                                fact_index,
                                                accumulated_output: String::new(),
                                            },
                                        );
                                    }
                                } else {
                                    diagnostics.record_relevant(true);
                                    if call.is_none() {
                                        diagnostics.record_relevant(false);
                                    }
                                    diagnostics.record_relevant(false);
                                    record_invocation_metrics(&mut state, metrics);
                                    push_anonymous_codex_analysis_fact(
                                        &mut analysis_facts,
                                        &current_model,
                                        ts,
                                        source_order,
                                        metrics,
                                    );
                                }
                            }
                        }
                        "function_call_output" => {
                            if entry.payload.call_id.as_deref().is_none_or(str::is_empty) {
                                diagnostics.record_relevant(false);
                            } else if let Some(call_id) = &entry.payload.call_id
                                && let Some(call) = shell_calls.remove(call_id)
                            {
                                if processed_patch_calls.contains(call_id) {
                                    diagnostics.record_relevant(true);
                                    continue;
                                }
                                let output = shell_output(entry.payload.output.as_deref());
                                if call.call.is_some()
                                    && let Some(session_id) = running_shell_session_id(&output)
                                {
                                    let mut running = RunningCodexShellInvocation {
                                        original_call_id: call_id.to_string(),
                                        invocation: call,
                                    };
                                    append_shell_output_chunk(&mut running.invocation, &output);
                                    touch_codex_analysis_fact(
                                        &mut analysis_facts,
                                        running.invocation.fact_index,
                                        ts,
                                    );
                                    diagnostics.record_relevant(true);
                                    if let Some(displaced) =
                                        running_shell_calls.insert(session_id, running)
                                        && let Some(displaced_call) = displaced.invocation.call
                                    {
                                        record_pending_shell_invocation(
                                            &mut state,
                                            &displaced_call,
                                        );
                                    }
                                } else {
                                    if finalize_codex_shell_invocation(
                                        &mut state,
                                        &mut diagnostics,
                                        &mut analysis_facts,
                                        call,
                                        output,
                                        ts,
                                    ) {
                                        processed_patch_calls.insert(call_id.to_string());
                                    }
                                }
                            } else if let Some(call_id) = &entry.payload.call_id
                                && let Some(session_id) = shell_poll_sessions.remove(call_id)
                                && let Some(mut running) = running_shell_calls.remove(&session_id)
                            {
                                let output = shell_output(entry.payload.output.as_deref());
                                if let Some(next_session_id) = running_shell_session_id(&output) {
                                    append_shell_output_chunk(&mut running.invocation, &output);
                                    touch_codex_analysis_fact(
                                        &mut analysis_facts,
                                        running.invocation.fact_index,
                                        ts,
                                    );
                                    diagnostics.record_relevant(true);
                                    if let Some(displaced) =
                                        running_shell_calls.insert(next_session_id, running)
                                        && let Some(displaced_call) = displaced.invocation.call
                                    {
                                        record_pending_shell_invocation(
                                            &mut state,
                                            &displaced_call,
                                        );
                                    }
                                } else if running.invocation.call.as_ref().is_some_and(|call| {
                                    shell_file_effect_outcome(&output, call.is_patch()).is_some()
                                }) {
                                    let original_call_id = running.original_call_id;
                                    if finalize_codex_shell_invocation(
                                        &mut state,
                                        &mut diagnostics,
                                        &mut analysis_facts,
                                        running.invocation,
                                        output,
                                        ts,
                                    ) {
                                        processed_patch_calls.insert(original_call_id);
                                    }
                                } else {
                                    touch_codex_analysis_fact(
                                        &mut analysis_facts,
                                        running.invocation.fact_index,
                                        ts,
                                    );
                                    diagnostics.record_relevant(true);
                                    running_shell_calls.insert(session_id, running);
                                }
                            }
                        }
                        "custom_tool_call" => {
                            if let Some(name) = entry.payload.name.as_deref()
                                && matches!(name, "exec" | "apply_patch")
                            {
                                let call = entry
                                    .payload
                                    .arguments
                                    .as_deref()
                                    .and_then(|input| parse_custom_call(name, input, ts, mode));
                                let metrics = call
                                    .as_ref()
                                    .map(custom_invocation_metrics)
                                    .unwrap_or_else(|| invalid_custom_invocation_metrics(name));
                                if let Some(call_id) = entry
                                    .payload
                                    .call_id
                                    .as_deref()
                                    .filter(|call_id| !call_id.is_empty())
                                {
                                    diagnostics.record_relevant(call.is_some());
                                    let fact_index = ensure_codex_analysis_fact(
                                        &mut analysis_facts,
                                        &mut analysis_fact_indices,
                                        call_id,
                                        &current_model,
                                        ts,
                                        source_order,
                                        metrics,
                                    );
                                    custom_calls.insert(
                                        call_id.to_string(),
                                        PendingCodexCustomInvocation {
                                            call,
                                            kind: metrics,
                                            timestamp: ts,
                                            fact_index,
                                        },
                                    );
                                } else {
                                    diagnostics.record_relevant(true);
                                    if call.is_none() {
                                        diagnostics.record_relevant(false);
                                    }
                                    diagnostics.record_relevant(false);
                                    record_invocation_metrics(&mut state, metrics);
                                    push_anonymous_codex_analysis_fact(
                                        &mut analysis_facts,
                                        &current_model,
                                        ts,
                                        source_order,
                                        metrics,
                                    );
                                }
                            }
                        }
                        "custom_tool_call_output" => {
                            if entry.payload.call_id.as_deref().is_none_or(str::is_empty) {
                                diagnostics.record_relevant(false);
                            } else if let Some(call_id) = entry.payload.call_id.as_deref()
                                && let Some(call) = custom_calls.remove(call_id)
                            {
                                if processed_patch_calls.contains(call_id) {
                                    diagnostics.record_relevant(true);
                                    continue;
                                }
                                if let Some(parsed_call) = call.call {
                                    let patch_call = parsed_call.is_patch();
                                    let outcome = custom_call_result(
                                        &parsed_call,
                                        entry.payload.output.as_deref(),
                                    );
                                    let effect_paths =
                                        custom_call_effect_paths(&state, &parsed_call);
                                    let effect_before = AnalysisStateSnapshot::capture(&state);
                                    let before = AnalysisMetrics::from_state(&state);
                                    let normalized = dispatch_custom_call(
                                        &mut state,
                                        parsed_call,
                                        entry.payload.output.as_deref(),
                                    );
                                    diagnostics.record_relevant(normalized);
                                    update_codex_analysis_fact(
                                        &mut analysis_facts,
                                        call.fact_index,
                                        outcome.fact_status(),
                                        ts,
                                        successful_effect_metrics(
                                            before,
                                            &state,
                                            outcome.is_success(),
                                        ),
                                        outcome.is_success().then(|| {
                                            effect_before.effect_since(&state, effect_paths)
                                        }),
                                    );
                                    if patch_call {
                                        processed_patch_calls.insert(call_id.to_string());
                                    }
                                } else {
                                    record_invocation_metrics(&mut state, call.kind);
                                    update_codex_analysis_fact(
                                        &mut analysis_facts,
                                        call.fact_index,
                                        ToolFactStatus::Failed,
                                        ts,
                                        AnalysisMetrics::default(),
                                        None,
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    for call in shell_calls.into_values() {
        if let Some(call) = call.call {
            record_pending_shell_invocation(&mut state, &call);
        } else {
            state.tool_counts.bash += 1;
            diagnostics.record_relevant(false);
        }
    }
    for running in running_shell_calls.into_values() {
        if let Some(call) = running.invocation.call {
            record_pending_shell_invocation(&mut state, &call);
        }
    }
    for call in custom_calls.into_values() {
        record_invocation_metrics(&mut state, call.kind);
    }
    if state.git_remote.is_empty() {
        state.git_remote = get_git_remote_url(&state.folder_path);
    }

    let record = state.into_record(conversation_usage);

    let analysis = CodeAnalysis {
        user: String::new(),
        extension_name: String::new(),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    };
    Ok(ParsedAnalysis::new(analysis, diagnostics).with_facts(usage_facts, analysis_facts))
}

fn is_supported_codex_usage(info: &Value) -> bool {
    let Some(info) = info.as_object() else {
        return false;
    };
    let mut recognized = false;
    for key in ["total_token_usage", "last_token_usage"] {
        let Some(usage) = info.get(key) else {
            continue;
        };
        if !is_supported_codex_token_usage(usage) {
            return false;
        }
        recognized = true;
    }
    if let Some(context_window) = info.get("model_context_window") {
        if context_window.is_null() {
            // Older Codex records retain the field with a null value while
            // still carrying valid token objects.
        } else if context_window.as_i64().is_none() {
            return false;
        } else {
            recognized = true;
        }
    }
    recognized
}

fn accumulate_codex_usage_event(
    conversation_usage: &mut FastHashMap<String, Value>,
    model: &str,
    info: &Value,
    previous_total_usage: &mut Option<Value>,
) -> Option<Value> {
    let total = info.get("total_token_usage").and_then(Value::as_object);
    let last = info.get("last_token_usage").and_then(Value::as_object);
    let previous = previous_total_usage.as_ref().and_then(Value::as_object);

    let replay = total
        .zip(previous)
        .is_some_and(|(current, previous)| codex_usage_equal(current, previous));
    let reset = !replay
        && total
            .zip(previous)
            .is_some_and(|(current, previous)| codex_usage_rolled_back(current, previous));

    let cumulative_delta = total.map(|current| {
        if replay {
            serde_json::Map::new()
        } else if reset {
            clone_codex_usage_fields(current)
        } else if let Some(previous) = previous {
            subtract_codex_usage(current, previous)
        } else {
            clone_codex_usage_fields(current)
        }
    });

    if let Some(total) = info.get("total_token_usage") {
        *previous_total_usage = Some(total.clone());
    }
    if model.is_empty() {
        return None;
    }

    let mut contribution = serde_json::Map::new();
    if !replay {
        for key in CODEX_TOKEN_FIELDS {
            let value = last
                .and_then(|usage| usage.get(key))
                .and_then(Value::as_u64)
                .or_else(|| {
                    cumulative_delta
                        .as_ref()
                        .and_then(|usage| usage.get(key))
                        .and_then(Value::as_u64)
                });
            if let Some(value) = value {
                contribution.insert(key.to_string(), Value::from(value));
            }
        }
    }

    let mut accumulated = conversation_usage
        .get(model)
        .and_then(|usage| usage.get("total_token_usage"))
        .and_then(Value::as_object)
        .map(clone_codex_usage_fields)
        .unwrap_or_default();
    for key in CODEX_TOKEN_FIELDS {
        let Some(value) = contribution.get(key).and_then(Value::as_u64) else {
            continue;
        };
        let existing = accumulated.get(key).and_then(Value::as_u64).unwrap_or(0);
        accumulated.insert(key.to_string(), Value::from(existing.saturating_add(value)));
    }

    let fact_usage = (!contribution.is_empty()).then(|| {
        let mut fact = serde_json::Map::new();
        fact.insert(
            "total_token_usage".to_string(),
            Value::Object(contribution.clone()),
        );
        if let Some(context_window) = info.get("model_context_window") {
            fact.insert("model_context_window".to_string(), context_window.clone());
        }
        Value::Object(fact)
    });

    let mut aggregated_info = serde_json::Map::new();
    aggregated_info.insert("total_token_usage".to_string(), Value::Object(accumulated));
    if !contribution.is_empty() {
        aggregated_info.insert("last_token_usage".to_string(), Value::Object(contribution));
    }
    if let Some(context_window) = info.get("model_context_window") {
        aggregated_info.insert("model_context_window".to_string(), context_window.clone());
    }
    process_codex_usage(conversation_usage, model, &Value::Object(aggregated_info));
    fact_usage
}

fn clone_codex_usage_fields(
    usage: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    CODEX_TOKEN_FIELDS
        .into_iter()
        .filter_map(|key| {
            usage
                .get(key)
                .and_then(Value::as_u64)
                .map(|value| (key.to_string(), Value::from(value)))
        })
        .collect()
}

fn subtract_codex_usage(
    current: &serde_json::Map<String, Value>,
    previous: &serde_json::Map<String, Value>,
) -> serde_json::Map<String, Value> {
    CODEX_TOKEN_FIELDS
        .into_iter()
        .filter_map(|key| {
            current.get(key).and_then(Value::as_u64).map(|value| {
                let previous = previous.get(key).and_then(Value::as_u64).unwrap_or(0);
                (key.to_string(), Value::from(value.saturating_sub(previous)))
            })
        })
        .collect()
}

fn codex_usage_equal(
    current: &serde_json::Map<String, Value>,
    previous: &serde_json::Map<String, Value>,
) -> bool {
    CODEX_TOKEN_FIELDS
        .into_iter()
        .any(|key| current.contains_key(key) || previous.contains_key(key))
        && CODEX_TOKEN_FIELDS.into_iter().all(|key| {
            current.get(key).and_then(Value::as_u64) == previous.get(key).and_then(Value::as_u64)
        })
}

fn codex_usage_rolled_back(
    current: &serde_json::Map<String, Value>,
    previous: &serde_json::Map<String, Value>,
) -> bool {
    CODEX_TOKEN_FIELDS.into_iter().any(|key| {
        current
            .get(key)
            .and_then(Value::as_u64)
            .zip(previous.get(key).and_then(Value::as_u64))
            .is_some_and(|(current, previous)| current < previous)
    })
}

fn is_supported_codex_token_usage(usage: &Value) -> bool {
    let Some(usage) = usage.as_object() else {
        return false;
    };
    if usage.is_empty() {
        return true;
    }

    let mut recognized = false;
    for key in [
        "input_tokens",
        "cached_input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
        "total_tokens",
    ] {
        if let Some(value) = usage.get(key) {
            if value.as_u64().is_none() {
                return false;
            }
            recognized = true;
        }
    }
    recognized
}

struct PendingCodexShellInvocation {
    call: Option<CodexShellCall>,
    timestamp: i64,
    fact_index: usize,
    accumulated_output: String,
}

impl PendingCodexShellInvocation {
    fn timestamp(&self) -> i64 {
        self.timestamp
    }

    fn fact_index(&self) -> usize {
        self.fact_index
    }
}

struct RunningCodexShellInvocation {
    original_call_id: String,
    invocation: PendingCodexShellInvocation,
}

struct PendingCodexCustomInvocation {
    call: Option<CodexCustomCall>,
    kind: AnalysisMetrics,
    timestamp: i64,
    fact_index: usize,
}

impl PendingCodexCustomInvocation {
    fn timestamp(&self) -> i64 {
        self.timestamp
    }

    fn fact_index(&self) -> usize {
        self.fact_index
    }
}

fn ensure_codex_analysis_fact(
    facts: &mut Vec<AnalysisFact>,
    indices: &mut FastHashMap<String, usize>,
    call_id: &str,
    model: &str,
    timestamp: i64,
    source_order: usize,
    metrics: AnalysisMetrics,
) -> usize {
    if let Some(index) = indices.get(call_id).copied() {
        if let Some(fact) = facts.get_mut(index)
            && timestamp > 0
        {
            fact.timestamp_ms = Some(
                fact.timestamp_ms
                    .map_or(timestamp, |existing| existing.min(timestamp)),
            );
        }
        return index;
    }

    let index = facts.len();
    facts.push(AnalysisFact {
        stable_id: Some(format!("codex-tool:{call_id}")),
        timestamp_ms: (timestamp > 0).then_some(timestamp),
        observed_at_ms: (timestamp > 0).then_some(timestamp),
        source_order,
        model: model.to_string(),
        status: ToolFactStatus::Pending,
        metrics,
        effect: None,
    });
    indices.insert(call_id.to_string(), index);
    index
}

fn push_anonymous_codex_analysis_fact(
    facts: &mut Vec<AnalysisFact>,
    model: &str,
    timestamp: i64,
    source_order: usize,
    metrics: AnalysisMetrics,
) {
    facts.push(AnalysisFact {
        stable_id: None,
        timestamp_ms: (timestamp > 0).then_some(timestamp),
        observed_at_ms: (timestamp > 0).then_some(timestamp),
        source_order,
        model: model.to_string(),
        status: ToolFactStatus::Pending,
        metrics,
        effect: None,
    });
}

fn set_codex_fact_invocation_metrics(
    facts: &mut [AnalysisFact],
    index: usize,
    metrics: AnalysisMetrics,
) {
    if let Some(fact) = facts.get_mut(index) {
        fact.metrics = metrics;
    }
}

fn update_codex_analysis_fact(
    facts: &mut [AnalysisFact],
    index: usize,
    status: ToolFactStatus,
    observed_at_ms: i64,
    effects: AnalysisMetrics,
    effect: Option<AnalysisFactEffect>,
) {
    let Some(fact) = facts.get_mut(index) else {
        return;
    };
    fact.status = status;
    fact.observed_at_ms = (observed_at_ms > 0).then_some(observed_at_ms);
    if status == ToolFactStatus::Succeeded {
        fact.metrics.add_assign(effects);
        fact.effect = effect;
    } else {
        fact.effect = None;
    }
}

fn touch_codex_analysis_fact(facts: &mut [AnalysisFact], index: usize, observed_at_ms: i64) {
    if observed_at_ms > 0
        && let Some(fact) = facts.get_mut(index)
    {
        fact.observed_at_ms = Some(observed_at_ms);
    }
}

fn finalize_codex_shell_invocation(
    state: &mut SessionParseState,
    diagnostics: &mut ParseDiagnostics,
    analysis_facts: &mut [AnalysisFact],
    mut invocation: PendingCodexShellInvocation,
    output: CodexShellOutput,
    observed_at_ms: i64,
) -> bool {
    append_shell_output_chunk(&mut invocation, &output);
    let Some(parsed_call) = invocation.call else {
        state.tool_counts.bash += 1;
        diagnostics.record_relevant(output_reports_argument_error(Some(&output.output)));
        update_codex_analysis_fact(
            analysis_facts,
            invocation.fact_index,
            ToolFactStatus::Failed,
            observed_at_ms,
            AnalysisMetrics::default(),
            None,
        );
        return false;
    };

    let outcome = shell_file_effect_outcome(&output, parsed_call.is_patch());
    let patch_call = parsed_call.is_patch();
    let effect_paths = shell_call_effect_paths(state, &parsed_call);
    let effect_before = AnalysisStateSnapshot::capture(state);
    let before = AnalysisMetrics::from_state(state);
    let normalized = state.handle_shell_call(
        parsed_call,
        CodexShellOutput {
            output: invocation.accumulated_output,
            metadata: outcome.map(|success| CodexShellMetadata {
                exit_code: if success { 0 } else { 1 },
                duration_seconds: 0.0,
            }),
        },
    );
    diagnostics.record_relevant(normalized);
    update_codex_analysis_fact(
        analysis_facts,
        invocation.fact_index,
        shell_fact_status(outcome),
        observed_at_ms,
        successful_effect_metrics(before, state, outcome == Some(true)),
        (outcome == Some(true)).then(|| effect_before.effect_since(state, effect_paths)),
    );
    patch_call
}

fn append_shell_output_chunk(
    invocation: &mut PendingCodexShellInvocation,
    output: &CodexShellOutput,
) {
    let retains_output = invocation.call.as_ref().is_some_and(|call| {
        extract_sed_file_path(&call.script).is_some()
            || extract_cat_read(&call.script, "").is_some()
    });
    if retains_output {
        invocation
            .accumulated_output
            .push_str(strip_exec_command_metadata_prefix(&output.output));
    }
}

fn successful_effect_metrics(
    before: AnalysisMetrics,
    state: &SessionParseState,
    succeeded: bool,
) -> AnalysisMetrics {
    if !succeeded {
        return AnalysisMetrics::default();
    }
    let mut effects = AnalysisMetrics::from_state(state).saturating_sub(before);
    effects.bash_count = 0;
    effects.edit_count = 0;
    effects.read_count = 0;
    effects.todo_write_count = 0;
    effects.write_count = 0;
    effects
}

fn push_effect_path(paths: &mut Vec<String>, state: &SessionParseState, path: &str) {
    let path = state.normalize_path(path);
    if !path.is_empty() && !paths.contains(&path) {
        paths.push(path);
    }
}

fn shell_call_effect_paths(state: &SessionParseState, call: &CodexShellCall) -> Vec<String> {
    let mut paths = Vec::new();
    if call.is_patch() {
        for patch in parse_apply_patch_script(&call.script) {
            push_effect_path(&mut paths, state, &patch.file_path);
        }
    } else if let Some(path) = extract_sed_file_path(&call.script) {
        push_effect_path(&mut paths, state, &path);
    } else if let Some((path, _)) = extract_cat_read(&call.script, "") {
        push_effect_path(&mut paths, state, &path);
    }
    paths
}

fn custom_call_effect_paths(state: &SessionParseState, call: &CodexCustomCall) -> Vec<String> {
    let mut paths = Vec::new();
    if let CodexCustomCall::ApplyPatch { patches, .. } = call {
        for patch in patches {
            push_effect_path(&mut paths, state, &patch.file_path);
        }
    }
    paths
}

fn structured_patch_effect_paths(state: &SessionParseState, payload: &CodexPayload) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(changes) = payload.info.as_ref().and_then(Value::as_object) {
        for (path, change) in changes {
            push_effect_path(&mut paths, state, path);
            if let Some(destination) = change
                .get("move_path")
                .and_then(Value::as_str)
                .filter(|path| !path.is_empty())
            {
                push_effect_path(&mut paths, state, destination);
            }
        }
    }
    paths
}

fn bash_invocation_metrics() -> AnalysisMetrics {
    AnalysisMetrics {
        bash_count: 1,
        ..AnalysisMetrics::default()
    }
}

fn patch_invocation_metrics(kind: PatchInvocationKind) -> AnalysisMetrics {
    match kind {
        PatchInvocationKind::Write => AnalysisMetrics {
            write_count: 1,
            ..AnalysisMetrics::default()
        },
        PatchInvocationKind::Edit => AnalysisMetrics {
            edit_count: 1,
            ..AnalysisMetrics::default()
        },
    }
}

fn shell_invocation_metrics(call: &CodexShellCall) -> AnalysisMetrics {
    if call.is_patch() {
        return patch_invocation_metrics(patch_kind(&parse_apply_patch_script(&call.script)));
    }
    if extract_sed_file_path(&call.script).is_some() || extract_cat_read(&call.script, "").is_some()
    {
        return AnalysisMetrics {
            read_count: 1,
            ..AnalysisMetrics::default()
        };
    }
    bash_invocation_metrics()
}

fn custom_invocation_metrics(call: &CodexCustomCall) -> AnalysisMetrics {
    match call {
        CodexCustomCall::Exec { .. } => bash_invocation_metrics(),
        CodexCustomCall::ApplyPatch { patches, .. } => {
            patch_invocation_metrics(patch_kind(patches))
        }
    }
}

fn invalid_custom_invocation_metrics(name: &str) -> AnalysisMetrics {
    match name {
        "exec" => bash_invocation_metrics(),
        "apply_patch" => patch_invocation_metrics(PatchInvocationKind::Edit),
        _ => AnalysisMetrics::default(),
    }
}

fn record_invocation_metrics(state: &mut SessionParseState, metrics: AnalysisMetrics) {
    state.tool_counts.bash += metrics.bash_count;
    state.tool_counts.edit += metrics.edit_count;
    state.tool_counts.read += metrics.read_count;
    state.tool_counts.todo_write += metrics.todo_write_count;
    state.tool_counts.write += metrics.write_count;
}

fn shell_fact_status(outcome: Option<bool>) -> ToolFactStatus {
    if outcome == Some(true) {
        ToolFactStatus::Succeeded
    } else {
        ToolFactStatus::Failed
    }
}

fn output_reports_argument_error(output: Option<&str>) -> bool {
    let output = shell_output(output).output;
    let output = strip_exec_command_metadata_prefix(&output)
        .trim_start()
        .to_ascii_lowercase();
    (output.starts_with("error") || output.starts_with("failed"))
        && (output.contains("failed to parse")
            || output.contains("missing field")
            || output.contains("missing required")
            || output.contains("invalid argument"))
}

/// Strip the metadata header that current Codex (`exec_command`) prepends
/// to every shell output. Format observed in the wild:
///
/// ```text
/// Chunk ID: <hex>
/// Wall time: <num> seconds
/// Process exited with code <num>
/// Original token count: <num>
/// Output:
/// <actual stdout>
/// ```
///
/// Without stripping, sed/cat read-detail extraction over-counts the file
/// by 5 lines (the prefix). Legacy `shell` outputs do not include the
/// header, so the search returns the original slice unchanged when no
/// `\nOutput:\n` marker is found.
fn strip_exec_command_metadata_prefix(output: &str) -> &str {
    const MARKER: &str = "\nOutput:\n";
    if let Some(idx) = output.find(MARKER) {
        &output[idx + MARKER.len()..]
    } else if let Some(rest) = output.strip_prefix("Output:\n") {
        rest
    } else {
        output
    }
}

/// Normalizes legacy strings and structured outputs serialized at the model boundary.
fn shell_output(output: Option<&str>) -> CodexShellOutput {
    let Some(output) = output else {
        return CodexShellOutput {
            output: String::new(),
            metadata: None,
        };
    };

    serde_json::from_str::<CodexShellOutput>(output).unwrap_or_else(|_| CodexShellOutput {
        output: serde_json::from_str::<Value>(output)
            .ok()
            .and_then(|value| value.get("output")?.as_str().map(str::to_string))
            .unwrap_or_else(|| output.to_string()),
        metadata: None,
    })
}

/// Build a `CodexShellCall` from either the legacy `shell` function or the
/// current `exec_command` function.
///
/// Returns `None` when the function name is unrelated (e.g. `update_plan`,
/// MCP tool calls) or when the arguments fail to deserialize. Both shapes
/// collapse to the same downstream representation so the patch / sed / cat
/// dispatch in `handle_shell_call` does not need to branch on the source
/// function name.
fn parse_function_call(name: &str, args_str: &str, ts: i64) -> Option<CodexShellCall> {
    match name {
        "shell" => {
            let args = serde_json::from_str::<CodexShellArguments>(args_str).ok()?;
            let script = args.command.last().cloned().unwrap_or_default();
            Some(CodexShellCall {
                timestamp: ts,
                script,
                full_command: args.command,
            })
        }
        "exec_command" => {
            let args = serde_json::from_str::<CodexExecCommandArguments>(args_str).ok()?;
            let cmd = args.cmd;
            Some(CodexShellCall {
                timestamp: ts,
                script: cmd.clone(),
                full_command: vec![cmd],
            })
        }
        _ => None,
    }
}

fn parse_write_stdin_session_id(args: &str) -> Option<u64> {
    let args = serde_json::from_str::<Value>(args).ok()?;
    let session_id = args.get("session_id")?;
    session_id
        .as_u64()
        .or_else(|| session_id.as_str()?.parse().ok())
}

fn running_shell_session_id(output: &CodexShellOutput) -> Option<u64> {
    let (header, _) = output.output.split_once("\nOutput:\n")?;
    header.lines().find_map(|line| {
        let line = line.trim();
        [
            "Process running with session ID ",
            "Process still running with session ID ",
        ]
        .into_iter()
        .find_map(|prefix| line.strip_prefix(prefix)?.trim().parse().ok())
    })
}

enum CodexCustomCall {
    Exec {
        source: Option<String>,
        timestamp: i64,
    },
    ApplyPatch {
        patches: Vec<CodexPatch>,
        timestamp: i64,
    },
}

impl CodexCustomCall {
    fn is_patch(&self) -> bool {
        matches!(self, Self::ApplyPatch { .. })
    }
}

fn parse_custom_call(
    name: &str,
    input: &str,
    timestamp: i64,
    mode: ParseMode,
) -> Option<CodexCustomCall> {
    match name {
        "exec" if !input.trim().is_empty() => Some(CodexCustomCall::Exec {
            source: matches!(mode, ParseMode::Full).then(|| input.to_string()),
            timestamp,
        }),
        "apply_patch" => {
            let patches: Vec<_> = parse_apply_patch_script(input)
                .into_iter()
                .filter(|patch| !patch.file_path.is_empty())
                .collect();
            (!patches.is_empty()).then_some(CodexCustomCall::ApplyPatch { patches, timestamp })
        }
        _ => None,
    }
}

fn dispatch_custom_call(
    state: &mut SessionParseState,
    call: CodexCustomCall,
    output: Option<&str>,
) -> bool {
    match call {
        CodexCustomCall::Exec { source, timestamp } => {
            // The JavaScript wrapper can contain branches, loops, retries, or
            // tool calls whose execution cannot be proven from source text.
            // Treat the completed cell itself as the only observable action.
            if let Some(source) = source {
                state.add_run_command(&source, "", timestamp);
            } else {
                state.tool_counts.bash += 1;
            }
            true
        }
        CodexCustomCall::ApplyPatch { patches, timestamp } => {
            match custom_apply_patch_result(output) {
                CustomApplyPatchResult::Success => {
                    apply_patch_invocation(state, patches, timestamp, true);
                    true
                }
                CustomApplyPatchResult::Failure => {
                    apply_patch_invocation(state, patches, timestamp, false);
                    true
                }
                CustomApplyPatchResult::Unknown => {
                    apply_patch_invocation(state, patches, timestamp, false);
                    false
                }
            }
        }
    }
}

fn dispatch_structured_patch(
    state: &mut SessionParseState,
    payload: &CodexPayload,
    timestamp: i64,
) -> bool {
    let outcome = structured_patch_result(payload);
    let changes = payload.info.as_ref().and_then(Value::as_object);
    let kind = changes
        .map(structured_patch_kind)
        .unwrap_or(PatchInvocationKind::Edit);

    match outcome {
        CustomApplyPatchResult::Success => {
            let Some(changes) = changes.filter(|changes| !changes.is_empty()) else {
                record_patch_invocation(state, kind);
                return false;
            };
            if !changes.iter().all(|(path, change)| {
                !path.is_empty()
                    && matches!(
                        structured_change_type(change),
                        Some("add" | "update" | "delete")
                    )
            }) {
                record_patch_invocation(state, kind);
                return false;
            }
            apply_structured_patch_invocation(state, changes, timestamp, kind);
            true
        }
        CustomApplyPatchResult::Failure => {
            record_patch_invocation(state, kind);
            true
        }
        CustomApplyPatchResult::Unknown => {
            record_patch_invocation(state, kind);
            false
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatchInvocationKind {
    Write,
    Edit,
}

fn patch_kind(patches: &[CodexPatch]) -> PatchInvocationKind {
    if !patches.is_empty() && patches.iter().all(|patch| patch.action == "add") {
        PatchInvocationKind::Write
    } else {
        PatchInvocationKind::Edit
    }
}

fn structured_patch_kind(changes: &serde_json::Map<String, Value>) -> PatchInvocationKind {
    if !changes.is_empty()
        && changes.values().all(|change| {
            structured_change_type(change) == Some("add")
                && change.get("move_path").is_none_or(Value::is_null)
        })
    {
        PatchInvocationKind::Write
    } else {
        PatchInvocationKind::Edit
    }
}

fn record_patch_invocation(state: &mut SessionParseState, kind: PatchInvocationKind) {
    match kind {
        PatchInvocationKind::Write => state.tool_counts.write += 1,
        PatchInvocationKind::Edit => state.tool_counts.edit += 1,
    }
}

fn apply_patch_invocation(
    state: &mut SessionParseState,
    patches: Vec<CodexPatch>,
    timestamp: i64,
    apply_effects: bool,
) {
    let kind = patch_kind(&patches);
    let write_count = state.tool_counts.write;
    let edit_count = state.tool_counts.edit;
    if apply_effects {
        for patch in patches {
            state.handle_patch(patch, timestamp);
        }
    }
    state.tool_counts.write = write_count;
    state.tool_counts.edit = edit_count;
    record_patch_invocation(state, kind);
}

fn apply_structured_patch_invocation(
    state: &mut SessionParseState,
    changes: &serde_json::Map<String, Value>,
    timestamp: i64,
    kind: PatchInvocationKind,
) {
    let write_count = state.tool_counts.write;
    let edit_count = state.tool_counts.edit;
    for (path, change) in changes {
        state.handle_structured_patch(path, change, timestamp);
    }
    state.tool_counts.write = write_count;
    state.tool_counts.edit = edit_count;
    record_patch_invocation(state, kind);
}

fn structured_change_type(change: &Value) -> Option<&str> {
    change.get("type").and_then(Value::as_str)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomApplyPatchResult {
    Success,
    Failure,
    Unknown,
}

impl CustomApplyPatchResult {
    fn is_success(self) -> bool {
        self == Self::Success
    }

    fn fact_status(self) -> ToolFactStatus {
        if self.is_success() {
            ToolFactStatus::Succeeded
        } else {
            ToolFactStatus::Failed
        }
    }
}

fn custom_call_result(call: &CodexCustomCall, output: Option<&str>) -> CustomApplyPatchResult {
    match call {
        CodexCustomCall::Exec { .. } => custom_exec_result(output),
        CodexCustomCall::ApplyPatch { .. } => custom_apply_patch_result(output),
    }
}

fn custom_exec_result(output: Option<&str>) -> CustomApplyPatchResult {
    let output = normalize_custom_output(output);
    let output = output.trim_start().to_ascii_lowercase();
    if output.starts_with("script completed") || output.starts_with("completed") {
        CustomApplyPatchResult::Success
    } else if output.starts_with("script failed")
        || output.starts_with("failed")
        || output.starts_with("error")
        || output.starts_with("cancelled")
        || output.starts_with("canceled")
        || output.starts_with("rejected")
    {
        CustomApplyPatchResult::Failure
    } else {
        CustomApplyPatchResult::Unknown
    }
}

fn structured_patch_result(payload: &CodexPayload) -> CustomApplyPatchResult {
    match payload.sandbox_policy.as_ref().and_then(Value::as_bool) {
        Some(true) => CustomApplyPatchResult::Success,
        Some(false) => CustomApplyPatchResult::Failure,
        None => custom_apply_patch_result(payload.output.as_deref()),
    }
}

fn custom_apply_patch_result(output: Option<&str>) -> CustomApplyPatchResult {
    let output = normalize_custom_output(output);
    let output = output.trim();
    if output == "Done!" || output.starts_with("Success. Updated the following files:") {
        CustomApplyPatchResult::Success
    } else if output.starts_with("Failed")
        || output.starts_with("Error")
        || output.starts_with("apply_patch verification failed:")
        || output.starts_with("Invalid patch")
        || output.starts_with("patch rejected")
        || output.starts_with("apply_patch handler received")
        || output.starts_with("apply_patch is unavailable")
    {
        CustomApplyPatchResult::Failure
    } else {
        CustomApplyPatchResult::Unknown
    }
}

/// Decodes a custom tool's normalized string or `JSON.stringify` result.
fn normalize_custom_output(output: Option<&str>) -> String {
    let body = strip_exec_command_metadata_prefix(output.unwrap_or_default()).trim();
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(object)) => object
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or(body)
            .to_string(),
        Ok(Value::String(text)) => text,
        _ => body.to_string(),
    }
}

/// Codex-specific dispatch helpers grafted onto [`SessionParseState`].
///
/// Kept as a private extension trait so the generic state type stays free of
/// Codex's `apply_patch`/`sed`/`cat` heuristics.
trait CodexAnalysisExt {
    /// Routes a completed shell call to the read / patch / run-command tally
    /// based on what its `script` did.
    fn handle_shell_call(&mut self, call: CodexShellCall, output: CodexShellOutput) -> bool;
    /// Applies one parsed `apply_patch` hunk as a write, delete, or edit.
    fn handle_patch(&mut self, patch: CodexPatch, ts: i64);
    /// Applies one structured `patch_apply_end` file effect.
    fn handle_structured_patch(&mut self, path: &str, change: &Value, ts: i64);
    /// Records a shell call that was not a file operation as a run command.
    fn record_run_command(&mut self, call: CodexShellCall);
}

impl CodexAnalysisExt for SessionParseState {
    fn handle_shell_call(&mut self, call: CodexShellCall, output: CodexShellOutput) -> bool {
        // Patch payloads carry a stable envelope regardless of the launcher name.
        if call.is_patch() {
            let patches = parse_apply_patch_script(&call.script);
            let outcome = shell_file_effect_outcome(&output, true);
            apply_patch_invocation(self, patches, call.timestamp, outcome == Some(true));
            return outcome.is_some();
        }

        // The legacy `shell` function returned just the raw command output
        // in `output`. The current `exec_command` function wraps that
        // output with a metadata header — strip it so line counting sees
        // only what the model actually saw as the file body.
        let output_body = strip_exec_command_metadata_prefix(&output.output);

        // Check for sed command
        if let Some(path) = extract_sed_file_path(&call.script) {
            let outcome = shell_file_effect_outcome(&output, false);
            if outcome == Some(true) {
                self.add_read_detail(&path, output_body, call.timestamp);
            } else {
                self.tool_counts.read += 1;
            }
            return outcome.is_some();
        }

        // Check for cat command
        if let Some((path, content)) = extract_cat_read(&call.script, output_body) {
            let outcome = shell_file_effect_outcome(&output, false);
            if outcome == Some(true) {
                self.add_read_detail(&path, &content, call.timestamp);
            } else {
                self.tool_counts.read += 1;
            }
            return outcome.is_some();
        }

        // Record command details only after a confirmed successful exit.
        let outcome = shell_file_effect_outcome(&output, false);
        if outcome == Some(true) {
            self.record_run_command(call);
        } else {
            self.tool_counts.bash += 1;
        }
        outcome.is_some()
    }

    fn handle_patch(&mut self, patch: CodexPatch, ts: i64) {
        if patch.file_path.is_empty() {
            return;
        }

        let resolved = self.normalize_path(&patch.file_path);
        if resolved.is_empty() {
            return;
        }

        let (old_str, new_str) = extract_patch_strings(&patch.lines);

        match patch.action.as_str() {
            "add" => {
                self.add_write_detail(&resolved, &new_str, ts);
            }
            "delete" => {
                let content = old_str.trim_end_matches('\n');
                self.add_edit_detail_raw(&resolved, content, "", ts);
            }
            _ => {
                self.add_edit_detail_raw(&resolved, &old_str, &new_str, ts);
            }
        }
    }

    fn handle_structured_patch(&mut self, path: &str, change: &Value, ts: i64) {
        match structured_change_type(change) {
            Some("add") => self.add_write_detail(
                path,
                change
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                ts,
            ),
            Some("delete") => self.add_edit_detail_raw(
                path,
                change
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                "",
                ts,
            ),
            Some("update") => {
                let (old, new) = change
                    .get("unified_diff")
                    .and_then(Value::as_str)
                    .map(diff_strings)
                    .unwrap_or_default();
                let destination = change
                    .get("move_path")
                    .and_then(Value::as_str)
                    .filter(|path| !path.is_empty())
                    .unwrap_or(path);
                self.add_edit_detail_raw(destination, &old, &new, ts);
                if destination != path {
                    let source = self.normalize_path(path);
                    if !source.is_empty() {
                        self.add_unique_file(source);
                    }
                }
            }
            _ => {}
        }
    }

    fn record_run_command(&mut self, call: CodexShellCall) {
        let command_str = if call.full_command.is_empty() {
            call.script.trim()
        } else {
            &call.full_command.join(" ")
        };

        self.add_run_command(command_str, "", call.timestamp);
    }
}

/// A Codex shell invocation awaiting its output, normalized across the
/// legacy `shell` and current `exec_command` function shapes.
struct CodexShellCall {
    /// Event timestamp in epoch milliseconds.
    timestamp: i64,
    /// The command line actually executed (used for `sed`/`cat`/patch sniffing).
    script: String,
    /// The full argv as written by the model, for verbatim run-command display.
    full_command: Vec<String>,
}

impl CodexShellCall {
    fn is_patch(&self) -> bool {
        self.script.contains("*** Begin Patch")
    }
}

fn shell_file_effect_outcome(output: &CodexShellOutput, patch: bool) -> Option<bool> {
    if let Some(metadata) = &output.metadata {
        return Some(metadata.exit_code == 0);
    }
    if let Some(exit_code) = output
        .output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Process exited with code "))
        .and_then(|code| code.trim().parse::<i32>().ok())
    {
        return Some(exit_code == 0);
    }
    if is_explicit_shell_cancellation(&output.output) {
        return Some(false);
    }
    if patch {
        return match custom_apply_patch_result(Some(&output.output)) {
            CustomApplyPatchResult::Success => Some(true),
            CustomApplyPatchResult::Failure => Some(false),
            CustomApplyPatchResult::Unknown => None,
        };
    }
    None
}

fn is_explicit_shell_cancellation(output: &str) -> bool {
    let Some(duration) = output
        .trim()
        .strip_prefix("aborted by user after ")
        .and_then(|duration| duration.strip_suffix('s'))
        .and_then(|duration| duration.parse::<f64>().ok())
    else {
        return false;
    };
    duration.is_finite() && duration >= 0.0
}

fn record_pending_shell_invocation(state: &mut SessionParseState, call: &CodexShellCall) {
    if call.is_patch() {
        let patches = parse_apply_patch_script(&call.script);
        record_patch_invocation(state, patch_kind(&patches));
    } else if extract_sed_file_path(&call.script).is_some()
        || extract_cat_read(&call.script, "").is_some()
    {
        state.tool_counts.read += 1;
    } else {
        state.tool_counts.bash += 1;
    }
}

/// One file hunk extracted from an `apply_patch` script.
struct CodexPatch {
    /// `"add"`, `"delete"`, or `"update"`.
    action: String,
    /// Target file path as written in the patch header.
    file_path: String,
    /// Raw diff body lines (with their leading `+`/`-`/context markers).
    lines: Vec<String>,
}

/// Parses an `apply_patch` script into its constituent per-file hunks.
///
/// Returns an empty `Vec` when no `*** Begin Patch` marker is present.
fn parse_apply_patch_script(script: &str) -> Vec<CodexPatch> {
    let start = match script.find("*** Begin Patch") {
        Some(idx) => idx,
        None => return Vec::new(),
    };

    let segment = &script[start..];
    let lines: Vec<&str> = segment.lines().collect();
    // Pre-allocate capacity based on typical patch count (1-5 patches)
    let mut patches = Vec::with_capacity(3);
    let mut current: Option<CodexPatch> = None;

    for line in lines {
        let line = line.trim_end_matches('\r');

        if line.starts_with("*** End Patch") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            break;
        } else if line.starts_with("*** Begin Patch") {
            continue;
        } else if line.starts_with("*** Update File:") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            let file_path = line
                .trim_start_matches("*** Update File:")
                .trim()
                .to_string();
            current = Some(CodexPatch {
                action: "update".to_string(),
                file_path,
                lines: Vec::with_capacity(20), // typical: 10-30 lines per patch
            });
        } else if line.starts_with("*** Add File:") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            let file_path = line.trim_start_matches("*** Add File:").trim().to_string();
            current = Some(CodexPatch {
                action: "add".to_string(),
                file_path,
                lines: Vec::with_capacity(20),
            });
        } else if line.starts_with("*** Delete File:") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            let file_path = line
                .trim_start_matches("*** Delete File:")
                .trim()
                .to_string();
            current = Some(CodexPatch {
                action: "delete".to_string(),
                file_path,
                lines: Vec::with_capacity(20),
            });
        } else if let Some(ref mut patch) = current {
            patch.lines.push(line.to_string());
        }
    }

    if let Some(patch) = current {
        patches.push(patch);
    }

    patches
}

/// Splits diff `lines` into the joined `(old, new)` text.
///
/// `+`-prefixed lines build the new content, `-`-prefixed lines the old;
/// `@@` hunk headers and `\` no-newline markers are skipped. Both results
/// have their trailing newline trimmed.
fn extract_patch_strings(lines: &[String]) -> (String, String) {
    // Pre-allocate with estimated capacity
    let estimated_size = lines.iter().map(|l| l.len()).sum::<usize>();
    let mut old_str = String::with_capacity(estimated_size / 2);
    let mut new_str = String::with_capacity(estimated_size / 2);

    for line in lines {
        if line.is_empty() {
            continue;
        }

        if line.len() > 1 && line.starts_with("@@") {
            continue;
        }

        let Some(first_char) = line.chars().next() else {
            continue;
        };
        match first_char {
            '+' => {
                new_str.push_str(&line[1..]);
                new_str.push('\n');
            }
            '-' => {
                old_str.push_str(&line[1..]);
                old_str.push('\n');
            }
            '\\' => continue,
            _ => {}
        }
    }

    // Trim in-place instead of allocating new strings
    let old_len = old_str.trim_end_matches('\n').len();
    old_str.truncate(old_len);
    let new_len = new_str.trim_end_matches('\n').len();
    new_str.truncate(new_len);

    (old_str, new_str)
}

fn diff_strings(diff: &str) -> (String, String) {
    let lines: Vec<String> = diff.lines().map(str::to_string).collect();
    extract_patch_strings(&lines)
}

/// Extracts the file path read by a `sed -n '<range>' <path>` command.
///
/// Returns `None` when the script is not a recognised `sed -n` read.
fn extract_sed_file_path(script: &str) -> Option<String> {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"sed\s+-n\s+'[^']*'\s+([^\s]+)").unwrap());
    let caps = re.captures(script)?;
    Some(
        caps.get(1)?
            .as_str()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string(),
    )
}

/// Extracts the `(path, content)` read by a `cat <path>` command.
///
/// `output` is the captured stdout (already metadata-stripped); content after
/// a `\n---` separator is dropped. Returns `None` when no `cat` line is found.
fn extract_cat_read(script: &str, output: &str) -> Option<(String, String)> {
    for line in script.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("cat ") {
            continue;
        }

        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }

        let path = fields[1].trim_matches(|c| c == '"' || c == '\'');

        // Optimize: avoid multiple allocations
        let clean_output = if let Some(idx) = output.find("\n---") {
            output[..idx].trim_end_matches('\n').to_string()
        } else {
            output.trim_end_matches('\n').to_string()
        };

        return Some((path.to_string(), clean_output));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log_at(timestamp: &str, log_type: &str, payload: Value) -> CodexLog {
        serde_json::from_value(serde_json::json!({
            "timestamp": timestamp,
            "type": log_type,
            "payload": payload
        }))
        .unwrap()
    }

    fn response_item(payload: Value) -> CodexLog {
        log_at("2026-07-12T00:00:00Z", "response_item", payload)
    }

    fn custom_output(call_id: &str, blocks: Value) -> CodexLog {
        response_item(serde_json::json!({
            "type": "custom_tool_call_output",
            "call_id": call_id,
            "output": blocks
        }))
    }

    fn event_msg(payload: Value) -> CodexLog {
        log_at("2026-07-12T00:00:01Z", "event_msg", payload)
    }

    fn function_call_at(timestamp: &str, name: &str, call_id: &str, arguments: Value) -> CodexLog {
        log_at(
            timestamp,
            "response_item",
            serde_json::json!({
                "type": "function_call",
                "name": name,
                "arguments": arguments,
                "call_id": call_id
            }),
        )
    }

    fn function_output_at(timestamp: &str, call_id: &str, output: String) -> CodexLog {
        log_at(
            timestamp,
            "response_item",
            serde_json::json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": output
            }),
        )
    }

    fn running_output(session_id: u64, output: &str) -> String {
        format!(
            "Chunk ID: running\nWall time: 1.0 seconds\nProcess running with session ID {session_id}\nOriginal token count: 1\nOutput:\n{output}"
        )
    }

    fn exited_output(exit_code: i32, output: &str) -> String {
        format!(
            "Chunk ID: exited\nWall time: 1.0 seconds\nProcess exited with code {exit_code}\nOriginal token count: 1\nOutput:\n{output}"
        )
    }

    fn exec_command_at(timestamp: &str, call_id: &str, command: &str) -> CodexLog {
        function_call_at(
            timestamp,
            "exec_command",
            call_id,
            serde_json::json!({ "cmd": command, "yield_time_ms": 1_000 }),
        )
    }

    fn write_stdin_at(timestamp: &str, call_id: &str, session_id: u64, chars: &str) -> CodexLog {
        function_call_at(
            timestamp,
            "write_stdin",
            call_id,
            serde_json::json!({
                "session_id": session_id,
                "chars": chars,
                "yield_time_ms": 30_000
            }),
        )
    }

    #[test]
    fn legacy_shell_function_parses_into_call() {
        // Old schema: arguments = {"command": ["bash", "-lc", "<script>"]}
        let args = r#"{"command":["bash","-lc","ls -la"]}"#;
        let call = parse_function_call("shell", args, 42).expect("shell call should parse");
        assert_eq!(call.timestamp, 42);
        assert_eq!(call.script, "ls -la");
        assert_eq!(call.full_command, vec!["bash", "-lc", "ls -la"]);
    }

    #[test]
    fn current_exec_command_function_parses_into_call() {
        // Current schema: arguments = {"cmd":"...","workdir":"...","yield_time_ms":...}
        let args =
            r#"{"cmd":"sed -n '1,260p' src/main.rs","workdir":"/repo","yield_time_ms":1000}"#;
        let call =
            parse_function_call("exec_command", args, 99).expect("exec_command should parse");
        assert_eq!(call.timestamp, 99);
        assert_eq!(call.script, "sed -n '1,260p' src/main.rs");
        // `full_command` collapses to the single cmd string so
        // `record_run_command`'s `join(" ")` produces the verbatim command.
        assert_eq!(call.full_command, vec!["sed -n '1,260p' src/main.rs"]);
    }

    #[test]
    fn structured_function_arguments_are_normalized_end_to_end() {
        let logs = vec![
            response_item(serde_json::json!({
                "type": "function_call",
                "name": "exec_command",
                "arguments": { "cmd": "pwd", "workdir": "/repo" },
                "call_id": "exec-object"
            })),
            response_item(serde_json::json!({
                "type": "function_call_output",
                "call_id": "exec-object",
                "output": {
                    "output": "done",
                    "metadata": { "exit_code": 0, "duration_seconds": 0.1 }
                }
            })),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.run_command_details[0].command, "pwd");
    }

    #[test]
    fn async_exec_command_completes_through_write_stdin() {
        let logs = vec![
            log_at(
                "2026-07-12T00:00:00Z",
                "turn_context",
                serde_json::json!({ "model": "gpt-5-codex" }),
            ),
            exec_command_at("2026-07-12T00:00:01Z", "exec-1", "sleep 1"),
            function_output_at(
                "2026-07-12T00:00:02Z",
                "exec-1",
                running_output(42, "partial output\n"),
            ),
            write_stdin_at("2026-07-12T00:00:03Z", "poll-1", 42, ""),
            function_output_at("2026-07-12T00:00:04Z", "poll-1", exited_output(0, "done\n")),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.run_command_details.len(), 1);
        assert_eq!(record.run_command_details[0].command, "sleep 1");
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.analysis_facts.len(), 1);
        let fact = &parsed.analysis_facts[0];
        assert_eq!(fact.stable_id.as_deref(), Some("codex-tool:exec-1"));
        assert_eq!(fact.status, ToolFactStatus::Succeeded);
        assert_eq!(fact.metrics.bash_count, 1);
        assert_eq!(
            fact.observed_at_ms,
            Some(parse_iso_timestamp("2026-07-12T00:00:04Z"))
        );
    }

    #[test]
    fn async_shell_read_accumulates_multiple_poll_chunks() {
        let logs = vec![
            exec_command_at(
                "2026-07-12T00:00:01Z",
                "read-1",
                "sed -n '1,3p' /repo/file.rs",
            ),
            function_output_at(
                "2026-07-12T00:00:02Z",
                "read-1",
                running_output(51, "one\n"),
            ),
            write_stdin_at("2026-07-12T00:00:03Z", "poll-1", 51, ""),
            function_output_at(
                "2026-07-12T00:00:04Z",
                "poll-1",
                running_output(51, "two\n"),
            ),
            write_stdin_at("2026-07-12T00:00:05Z", "poll-2", 51, ""),
            function_output_at(
                "2026-07-12T00:00:06Z",
                "poll-2",
                exited_output(0, "three\n"),
            ),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.tool_call_counts.bash, 0);
        assert_eq!(record.total_read_lines, 3);
        assert_eq!(record.total_read_characters, 13);
        assert_eq!(record.total_unique_files, 1);
        assert_eq!(record.read_file_details.len(), 1);
        assert_eq!(record.read_file_details[0].base.file_path, "/repo/file.rs");
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.analysis_facts[0].metrics.read_count, 1);
        assert_eq!(parsed.analysis_facts[0].metrics.read_lines, 3);
    }

    #[test]
    fn async_exec_command_nonzero_exit_has_no_success_detail() {
        let logs = vec![
            exec_command_at("2026-07-12T00:00:01Z", "exec-failed", "false"),
            function_output_at(
                "2026-07-12T00:00:02Z",
                "exec-failed",
                running_output(61, ""),
            ),
            write_stdin_at("2026-07-12T00:00:03Z", "poll-failed", 61, ""),
            function_output_at(
                "2026-07-12T00:00:04Z",
                "poll-failed",
                exited_output(1, "failed\n"),
            ),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert!(record.run_command_details.is_empty());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.analysis_facts.len(), 1);
        assert_eq!(parsed.analysis_facts[0].status, ToolFactStatus::Failed);
        assert!(parsed.analysis_facts[0].effect.is_none());
    }

    #[test]
    fn user_aborted_exec_command_is_a_supported_failed_lifecycle() {
        let logs = vec![
            exec_command_at("2026-07-12T00:00:01Z", "exec-aborted", "long-command"),
            function_output_at(
                "2026-07-12T00:00:02Z",
                "exec-aborted",
                "aborted by user after 0.1s".to_string(),
            ),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert!(record.run_command_details.is_empty());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.analysis_facts.len(), 1);
        assert_eq!(parsed.analysis_facts[0].status, ToolFactStatus::Failed);
        assert_eq!(parsed.analysis_facts[0].metrics.bash_count, 1);
        assert!(parsed.analysis_facts[0].effect.is_none());
    }

    #[test]
    fn async_exec_command_at_eof_stays_pending_and_counts_once() {
        let logs = vec![
            exec_command_at("2026-07-12T00:00:01Z", "exec-pending", "sleep 30"),
            function_output_at(
                "2026-07-12T00:00:02Z",
                "exec-pending",
                running_output(71, "waiting\n"),
            ),
            write_stdin_at("2026-07-12T00:00:03Z", "poll-pending", 71, ""),
            function_output_at(
                "2026-07-12T00:00:04Z",
                "poll-pending",
                running_output(71, "still waiting\n"),
            ),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert!(record.run_command_details.is_empty());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.analysis_facts.len(), 1);
        assert_eq!(parsed.analysis_facts[0].status, ToolFactStatus::Pending);
        assert_eq!(parsed.analysis_facts[0].metrics.bash_count, 1);
        assert_eq!(
            parsed.analysis_facts[0].observed_at_ms,
            Some(parse_iso_timestamp("2026-07-12T00:00:04Z"))
        );
    }

    #[test]
    fn async_stdin_write_error_does_not_terminate_original_command() {
        let logs = vec![
            exec_command_at("2026-07-12T00:00:01Z", "exec-input", "long-command"),
            function_output_at("2026-07-12T00:00:02Z", "exec-input", running_output(81, "")),
            write_stdin_at("2026-07-12T00:00:03Z", "write-input", 81, "yes\n"),
            function_output_at(
                "2026-07-12T00:00:04Z",
                "write-input",
                "write_stdin failed: session input stream is closed".to_string(),
            ),
            write_stdin_at("2026-07-12T00:00:05Z", "poll-after-error", 81, ""),
            function_output_at(
                "2026-07-12T00:00:06Z",
                "poll-after-error",
                exited_output(0, "done\n"),
            ),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.run_command_details.len(), 1);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.analysis_facts.len(), 1);
        assert_eq!(parsed.analysis_facts[0].status, ToolFactStatus::Succeeded);
    }

    #[test]
    fn async_exec_commands_can_complete_out_of_order() {
        let logs = vec![
            log_at(
                "2026-07-12T00:00:00Z",
                "turn_context",
                serde_json::json!({ "model": "model-a" }),
            ),
            exec_command_at("2026-07-12T00:00:01Z", "exec-a", "command-a"),
            function_output_at("2026-07-12T00:00:02Z", "exec-a", running_output(91, "")),
            log_at(
                "2026-07-12T00:00:03Z",
                "turn_context",
                serde_json::json!({ "model": "model-b" }),
            ),
            exec_command_at("2026-07-12T00:00:03Z", "exec-b", "command-b"),
            function_output_at("2026-07-12T00:00:04Z", "exec-b", running_output(92, "")),
            write_stdin_at("2026-07-12T00:00:05Z", "poll-b", 92, ""),
            function_output_at("2026-07-12T00:00:06Z", "poll-b", exited_output(0, "b\n")),
            write_stdin_at("2026-07-12T00:00:07Z", "poll-a", 91, ""),
            function_output_at("2026-07-12T00:00:08Z", "poll-a", exited_output(0, "a\n")),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 2);
        assert_eq!(record.run_command_details.len(), 2);
        assert_eq!(record.run_command_details[0].command, "command-b");
        assert_eq!(record.run_command_details[1].command, "command-a");
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.analysis_facts.len(), 2);
        assert!(
            parsed
                .analysis_facts
                .iter()
                .all(|fact| fact.status == ToolFactStatus::Succeeded)
        );
        let fact_a = parsed
            .analysis_facts
            .iter()
            .find(|fact| fact.stable_id.as_deref() == Some("codex-tool:exec-a"))
            .unwrap();
        let fact_b = parsed
            .analysis_facts
            .iter()
            .find(|fact| fact.stable_id.as_deref() == Some("codex-tool:exec-b"))
            .unwrap();
        assert_eq!(fact_a.model, "model-a");
        assert_eq!(fact_b.model, "model-b");
    }

    #[test]
    fn orphan_and_duplicate_write_stdin_polls_do_not_invent_invocations() {
        let logs = vec![
            write_stdin_at("2026-07-12T00:00:00Z", "orphan-poll", 999, ""),
            function_output_at(
                "2026-07-12T00:00:01Z",
                "orphan-poll",
                exited_output(0, "orphan\n"),
            ),
            exec_command_at("2026-07-12T00:00:02Z", "exec-1", "true"),
            function_output_at("2026-07-12T00:00:03Z", "exec-1", running_output(100, "")),
            write_stdin_at("2026-07-12T00:00:04Z", "terminal-poll", 100, ""),
            function_output_at(
                "2026-07-12T00:00:05Z",
                "terminal-poll",
                exited_output(0, "done\n"),
            ),
            write_stdin_at("2026-07-12T00:00:06Z", "duplicate-poll", 100, ""),
            function_output_at(
                "2026-07-12T00:00:07Z",
                "duplicate-poll",
                exited_output(0, "duplicate\n"),
            ),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.run_command_details.len(), 1);
        assert_eq!(parsed.analysis_facts.len(), 1);
        assert_eq!(parsed.analysis_facts[0].status, ToolFactStatus::Succeeded);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
    }

    #[test]
    fn unrelated_function_names_are_ignored() {
        // MCP tool calls, `update_plan`, etc. must not be treated as shell.
        assert!(parse_function_call("update_plan", "{}", 0).is_none());
        assert!(parse_function_call("_fetch_pr", "{}", 0).is_none());
    }

    #[test]
    fn malformed_arguments_yield_none_instead_of_panicking() {
        assert!(parse_function_call("shell", "not json", 0).is_none());
        assert!(parse_function_call("exec_command", "not json", 0).is_none());
    }

    #[test]
    fn paired_exec_argument_errors_are_supported_metric_free_lifecycles() {
        for arguments in ["{not json", r#"{"command_as_key":""}"#] {
            let logs = vec![
                response_item(serde_json::json!({
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": arguments,
                    "call_id": "bad-exec"
                })),
                response_item(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": "bad-exec",
                    "output": "Error: failed to parse function arguments"
                })),
            ];

            let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
            assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
            assert!(!parsed.diagnostics.is_complete_failure());
            assert_eq!(parsed.analysis.records[0].tool_call_counts.bash, 1);
            assert!(parsed.analysis.records[0].run_command_details.is_empty());
            assert_eq!(parsed.analysis_facts.len(), 1);
            let fact = &parsed.analysis_facts[0];
            assert_eq!(fact.stable_id.as_deref(), Some("codex-tool:bad-exec"));
            assert_eq!(fact.status, ToolFactStatus::Failed);
            assert_eq!(fact.metrics.bash_count, 1);
        }
    }

    #[test]
    fn invalid_exec_arguments_without_an_explicit_error_remain_drift() {
        let logs = vec![
            response_item(serde_json::json!({
                "type": "function_call",
                "name": "exec_command",
                "arguments": { "future_command": "pwd" },
                "call_id": "future-exec"
            })),
            response_item(serde_json::json!({
                "type": "function_call_output",
                "call_id": "future-exec",
                "output": "completed"
            })),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.analysis.records[0].tool_call_counts.bash, 1);
    }

    #[test]
    fn analysis_facts_use_the_model_and_timestamp_at_invocation() {
        let logs = vec![
            log_at(
                "2026-07-12T00:00:00Z",
                "turn_context",
                serde_json::json!({ "model": "gpt-model-a" }),
            ),
            log_at(
                "2026-07-12T00:00:01Z",
                "response_item",
                serde_json::json!({
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": { "cmd": "sed -n '1,2p' /repo/file.rs" },
                    "call_id": "read-model-a"
                }),
            ),
            log_at(
                "2026-07-12T00:00:02Z",
                "turn_context",
                serde_json::json!({ "model": "gpt-model-b" }),
            ),
            log_at(
                "2026-07-12T00:00:03Z",
                "response_item",
                serde_json::json!({
                    "type": "function_call_output",
                    "call_id": "read-model-a",
                    "output": {
                        "output": "line one\nline two\n",
                        "metadata": { "exit_code": 0, "duration_seconds": 0.1 }
                    }
                }),
            ),
            log_at(
                "2026-07-12T00:00:04Z",
                "response_item",
                serde_json::json!({
                    "type": "custom_tool_call",
                    "name": "exec",
                    "input": "await tools.exec_command({cmd: 'pwd'});",
                    "call_id": "exec-model-b"
                }),
            ),
            log_at(
                "2026-07-12T00:00:05Z",
                "response_item",
                serde_json::json!({
                    "type": "custom_tool_call_output",
                    "call_id": "exec-model-b",
                    "output": "Script completed"
                }),
            ),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        assert_eq!(parsed.analysis_facts.len(), 2);

        let read = parsed
            .analysis_facts
            .iter()
            .find(|fact| fact.stable_id.as_deref() == Some("codex-tool:read-model-a"))
            .unwrap();
        assert_eq!(read.model, "gpt-model-a");
        assert_eq!(
            read.timestamp_ms,
            Some(parse_iso_timestamp("2026-07-12T00:00:01Z"))
        );
        assert_eq!(
            read.observed_at_ms,
            Some(parse_iso_timestamp("2026-07-12T00:00:03Z"))
        );
        assert_eq!(read.status, ToolFactStatus::Succeeded);
        assert_eq!(read.metrics.read_count, 1);
        assert_eq!(read.metrics.read_lines, 2);
        let read_effect = read.effect.as_ref().unwrap();
        assert_eq!(read_effect.read_characters, 17);
        assert_eq!(read_effect.unique_files, ["/repo/file.rs"]);
        assert_eq!(read_effect.read_file_details.len(), 1);

        let exec = parsed
            .analysis_facts
            .iter()
            .find(|fact| fact.stable_id.as_deref() == Some("codex-tool:exec-model-b"))
            .unwrap();
        assert_eq!(exec.model, "gpt-model-b");
        assert_eq!(
            exec.timestamp_ms,
            Some(parse_iso_timestamp("2026-07-12T00:00:04Z"))
        );
        assert_eq!(exec.status, ToolFactStatus::Succeeded);
        assert_eq!(exec.metrics.bash_count, 1);
        let exec_effect = exec.effect.as_ref().unwrap();
        assert_eq!(exec_effect.run_command_details.len(), 1);
        assert_eq!(
            exec_effect.run_command_details[0].command,
            "await tools.exec_command({cmd: 'pwd'});"
        );
    }

    #[test]
    fn valid_token_count_without_model_context_is_not_schema_failure() {
        let logs: Vec<CodexLog> = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "session_meta",
                "payload": { "id": "compacted-session", "model_provider": "openai" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": { "input_tokens": 1, "output_tokens": 1 },
                        "last_token_usage": { "input_tokens": 1, "output_tokens": 1 },
                        "model_context_window": 200000
                    }
                }
            }),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.diagnostics.relevant_records, 1);
        assert_eq!(parsed.diagnostics.normalized_records, 1);
        assert_eq!(parsed.diagnostics.failed_relevant_records, 0);
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }

    #[test]
    fn resumed_usage_subtracts_the_pre_context_replay_baseline() {
        let logs: Vec<CodexLog> = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "session_meta",
                "payload": { "id": "resumed-session", "model_provider": "openai" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 100,
                            "cached_input_tokens": 20,
                            "output_tokens": 30,
                            "reasoning_output_tokens": 10,
                            "total_tokens": 130
                        },
                        "model_context_window": 200000
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:02Z",
                "type": "turn_context",
                "payload": { "model": "gpt-5.3-codex" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:03Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 160,
                            "cached_input_tokens": 25,
                            "output_tokens": 50,
                            "reasoning_output_tokens": 15,
                            "total_tokens": 210
                        },
                        "last_token_usage": { "input_tokens": 60, "output_tokens": 20 },
                        "model_context_window": 200000
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:04Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 190,
                            "cached_input_tokens": 30,
                            "output_tokens": 60,
                            "reasoning_output_tokens": 20,
                            "total_tokens": 250
                        },
                        "last_token_usage": { "input_tokens": 30, "output_tokens": 10 },
                        "model_context_window": 200000
                    }
                }
            }),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);

        let usage = &parsed.analysis.records[0].conversation_usage["gpt-5.3-codex"];
        assert_eq!(usage["total_token_usage"]["input_tokens"], 90);
        assert_eq!(usage["total_token_usage"]["cached_input_tokens"], 10);
        assert_eq!(usage["total_token_usage"]["output_tokens"], 30);
        assert_eq!(usage["total_token_usage"]["reasoning_output_tokens"], 10);
        assert_eq!(usage["total_token_usage"]["total_tokens"], 120);
        assert_eq!(usage["last_token_usage"]["output_tokens"], 10);
        assert_eq!(usage["model_context_window"], 200000);
    }

    #[test]
    fn cumulative_usage_is_deltaed_across_model_switches() {
        let logs: Vec<CodexLog> = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "turn_context",
                "payload": { "model": "gpt-a" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 100,
                            "output_tokens": 20,
                            "total_tokens": 120
                        }
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:02Z",
                "type": "turn_context",
                "payload": { "model": "gpt-b" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:03Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 140,
                            "output_tokens": 30,
                            "total_tokens": 170
                        }
                    }
                }
            }),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let usage = &analysis.records[0].conversation_usage;
        assert_eq!(usage["gpt-a"]["total_token_usage"]["total_tokens"], 120);
        assert_eq!(usage["gpt-b"]["total_token_usage"]["input_tokens"], 40);
        assert_eq!(usage["gpt-b"]["total_token_usage"]["output_tokens"], 10);
        assert_eq!(usage["gpt-b"]["total_token_usage"]["total_tokens"], 50);
    }

    #[test]
    fn last_usage_wins_while_missing_components_use_cumulative_delta() {
        let logs: Vec<CodexLog> = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "turn_context",
                "payload": { "model": "gpt-a" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 100,
                            "cached_input_tokens": 20,
                            "output_tokens": 20,
                            "total_tokens": 120
                        },
                        "last_token_usage": {
                            "input_tokens": 100,
                            "cached_input_tokens": 20,
                            "output_tokens": 20,
                            "total_tokens": 120
                        }
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:02Z",
                "type": "turn_context",
                "payload": { "model": "gpt-b" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:03Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 160,
                            "cached_input_tokens": 35,
                            "output_tokens": 30,
                            "total_tokens": 190
                        },
                        "last_token_usage": {
                            "input_tokens": 25,
                            "output_tokens": 4
                        }
                    }
                }
            }),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let usage = &analysis.records[0].conversation_usage["gpt-b"]["total_token_usage"];
        assert_eq!(usage["input_tokens"], 25);
        assert_eq!(usage["cached_input_tokens"], 15);
        assert_eq!(usage["output_tokens"], 4);
        assert_eq!(usage["total_tokens"], 70);
    }

    #[test]
    fn cumulative_replay_is_zero_and_rollback_starts_a_new_epoch() {
        let logs: Vec<CodexLog> = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "turn_context",
                "payload": { "model": "gpt-a" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": { "input_tokens": 100, "total_tokens": 100 },
                        "last_token_usage": { "input_tokens": 100, "total_tokens": 100 }
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:02Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": { "input_tokens": 100, "total_tokens": 100 },
                        "last_token_usage": { "input_tokens": 100, "total_tokens": 100 }
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:03Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": { "input_tokens": 20, "total_tokens": 20 }
                    }
                }
            }),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let usage = &analysis.records[0].conversation_usage["gpt-a"]["total_token_usage"];
        assert_eq!(usage["input_tokens"], 120);
        assert_eq!(usage["total_tokens"], 120);
    }

    #[test]
    fn pre_context_usage_is_not_guessed_from_a_later_model() {
        let logs: Vec<CodexLog> = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": { "total_token_usage": { "input_tokens": 10 } }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "turn_context",
                "payload": { "model": "gpt-5.3-codex" }
            }),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }

    #[test]
    fn malformed_pre_context_usage_remains_schema_failure() {
        let logs: Vec<CodexLog> = [serde_json::json!({
            "timestamp": "2026-07-12T00:00:00Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": { "total_token_usage": { "input_tokens": "10" } }
            }
        })]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }

    #[test]
    fn usage_validation_rejects_unknown_only_and_wrong_typed_token_payloads() {
        assert!(is_supported_codex_usage(&serde_json::json!({
            "total_token_usage": {}
        })));
        assert!(is_supported_codex_usage(&serde_json::json!({
            "last_token_usage": { "input_tokens": 0, "future_metric": 5 }
        })));
        assert!(is_supported_codex_usage(&serde_json::json!({
            "total_token_usage": { "output_tokens": 0 },
            "model_context_window": null
        })));
        assert!(is_supported_codex_usage(&serde_json::json!({
            "model_context_window": 200000
        })));

        assert!(!is_supported_codex_usage(&serde_json::json!({
            "total_token_usage": { "prompt_tokens": 3 }
        })));
        assert!(!is_supported_codex_usage(&serde_json::json!({
            "last_token_usage": { "output_tokens": "3" }
        })));
        assert!(!is_supported_codex_usage(&serde_json::json!({
            "total_token_usage": { "input_tokens": 3 },
            "last_token_usage": { "completion_tokens": 1 }
        })));
        assert!(!is_supported_codex_usage(&serde_json::json!({
            "model_context_window": "200000"
        })));
    }

    #[test]
    fn exec_command_metadata_prefix_is_stripped() {
        // Real-world Codex `exec_command` output wraps actual stdout with
        // a 5-line header; without stripping, the analyzer over-counts
        // file reads by 5 lines per sed/cat invocation.
        let raw = "Chunk ID: deadbeef\n\
                   Wall time: 0.0000 seconds\n\
                   Process exited with code 0\n\
                   Original token count: 100\n\
                   Output:\n\
                   line one\n\
                   line two\n";
        assert_eq!(
            strip_exec_command_metadata_prefix(raw),
            "line one\nline two\n"
        );
    }

    #[test]
    fn running_session_marker_is_read_only_from_the_metadata_header() {
        let raw = exited_output(0, "Process running with session ID 999\n");
        assert_eq!(running_shell_session_id(&shell_output(Some(&raw))), None);

        let raw = running_output(42, "Process running with session ID 999\n");
        assert_eq!(
            running_shell_session_id(&shell_output(Some(&raw))),
            Some(42)
        );
    }

    #[test]
    fn legacy_shell_output_passes_through_unchanged() {
        // Legacy `shell` function output has no metadata header; the
        // helper must leave non-prefixed strings exactly as-is so the
        // existing fixture-based tests keep matching.
        let raw = "line one\nline two\n";
        assert_eq!(strip_exec_command_metadata_prefix(raw), raw);
    }

    #[test]
    fn output_starting_with_marker_handles_no_leading_newline() {
        // Defensive: if a future Codex variant drops the leading newline
        // before the `Output:` marker, still strip it cleanly.
        let raw = "Output:\nthe content";
        assert_eq!(strip_exec_command_metadata_prefix(raw), "the content");
    }

    #[test]
    fn custom_apply_patch_requires_supported_nonempty_file_headers() {
        let drifted = "*** Begin Patch\n*** Future File: src/lib.rs\n+new\n*** End Patch";
        assert!(
            parse_custom_call("apply_patch", drifted, 1, ParseMode::Full).is_none(),
            "an unknown file header must not normalize as a successful patch"
        );

        for patch in [
            "*** Begin Patch\n*** Add File: empty.txt\n*** End Patch",
            "*** Begin Patch\n*** Delete File: empty.txt\n*** End Patch",
        ] {
            let Some(CodexCustomCall::ApplyPatch { patches, .. }) =
                parse_custom_call("apply_patch", patch, 1, ParseMode::Full)
            else {
                panic!("supported empty-body patch was rejected");
            };
            assert_eq!(patches.len(), 1);
            assert_eq!(patches[0].file_path, "empty.txt");
        }

        let empty_path = "*** Begin Patch\n*** Add File:\n+new\n*** End Patch";
        assert!(parse_custom_call("apply_patch", empty_path, 1, ParseMode::Full).is_none());
    }

    #[test]
    fn malformed_known_custom_calls_still_count_as_pending_invocations() {
        let logs = vec![
            log_at(
                "2026-07-12T00:00:00Z",
                "turn_context",
                serde_json::json!({ "model": "gpt-5-codex" }),
            ),
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "exec",
                "input": "",
                "call_id": "invalid-exec"
            })),
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": "*** Begin Patch\n*** Future File: src/lib.rs\n+new\n*** End Patch",
                "call_id": "invalid-patch"
            })),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(parsed.analysis_facts.len(), 2);

        let exec = parsed
            .analysis_facts
            .iter()
            .find(|fact| fact.stable_id.as_deref() == Some("codex-tool:invalid-exec"))
            .unwrap();
        assert_eq!(exec.model, "gpt-5-codex");
        assert_eq!(exec.status, ToolFactStatus::Pending);
        assert_eq!(exec.metrics.bash_count, 1);

        let patch = parsed
            .analysis_facts
            .iter()
            .find(|fact| fact.stable_id.as_deref() == Some("codex-tool:invalid-patch"))
            .unwrap();
        assert_eq!(patch.model, "gpt-5-codex");
        assert_eq!(patch.status, ToolFactStatus::Pending);
        assert_eq!(patch.metrics.edit_count, 1);
    }

    #[test]
    fn tracked_calls_without_correlation_ids_are_anonymous_without_effects() {
        let logs = vec![
            log_at(
                "2026-07-12T00:00:00Z",
                "turn_context",
                serde_json::json!({ "model": "gpt-5-codex" }),
            ),
            response_item(serde_json::json!({
                "type": "function_call",
                "name": "exec_command",
                "arguments": { "cmd": "printf test" }
            })),
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch"
            })),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];

        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.total_unique_files, 0);
        assert_eq!(record.total_edit_lines, 0);
        assert!(record.edit_file_details.is_empty());
        assert!(record.run_command_details.is_empty());
        assert_eq!(parsed.analysis_facts.len(), 2);
        assert!(parsed.analysis_facts.iter().all(|fact| {
            fact.stable_id.is_none()
                && fact.status == ToolFactStatus::Pending
                && fact.effect.is_none()
        }));
        assert_eq!(parsed.analysis_facts[0].metrics.bash_count, 1);
        assert_eq!(parsed.analysis_facts[1].metrics.edit_count, 1);
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 2);
    }

    #[test]
    fn malformed_tracked_calls_without_ids_still_emit_invocations() {
        let logs = vec![
            log_at(
                "2026-07-12T00:00:00Z",
                "turn_context",
                serde_json::json!({ "model": "gpt-5-codex" }),
            ),
            response_item(serde_json::json!({
                "type": "function_call",
                "name": "exec_command",
                "arguments": { "unexpected": true }
            })),
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": "*** Begin Patch\n*** Future File: src/lib.rs\n+new\n*** End Patch"
            })),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];

        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.total_unique_files, 0);
        assert_eq!(record.total_edit_lines, 0);
        assert_eq!(parsed.analysis_facts.len(), 2);
        assert!(parsed.analysis_facts.iter().all(|fact| {
            fact.stable_id.is_none()
                && fact.status == ToolFactStatus::Pending
                && fact.effect.is_none()
        }));
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 4);
    }

    #[test]
    fn direct_custom_apply_patch_success_is_paired_and_parsed() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
        let wire_result = serde_json::json!({
            "output": "Success. Updated the following files:\nM src/lib.rs",
            "metadata": { "exit_code": 0, "duration_seconds": 0.01 }
        })
        .to_string();
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "patch-1"
            })),
            custom_output("patch-1", Value::String(wire_result)),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.edit_file_details.len(), 1);
        assert_eq!(record.edit_file_details[0].base.file_path, "src/lib.rs");
    }

    #[test]
    fn direct_object_custom_apply_patch_output_is_paired_and_parsed() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "patch-object"
            })),
            custom_output(
                "patch-object",
                serde_json::json!({
                    "output": "Success. Updated the following files:\nM src/lib.rs",
                    "metadata": { "exit_code": 0, "duration_seconds": 0.01 }
                }),
            ),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.edit_file_details.len(), 1);
    }

    #[test]
    fn structured_patch_is_authoritative_and_deduplicates_direct_lifecycle() {
        let direct =
            "*** Begin Patch\n*** Update File: /repo/update.rs\n@@\n-old\n+new\n*** End Patch";
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": direct,
                "call_id": "patch-structured"
            })),
            event_msg(serde_json::json!({
                "type": "patch_apply_end",
                "call_id": "patch-structured",
                "success": true,
                "status": "completed",
                "stdout": "Success. Updated the following files:\nM /repo/update.rs\n",
                "stderr": "",
                "changes": {
                    "/repo/add.rs": {
                        "type": "add",
                        "content": "one\ntwo\n"
                    },
                    "/repo/update.rs": {
                        "type": "update",
                        "unified_diff": "@@ -1 +1 @@\n-old\n+new\n",
                        "move_path": null
                    },
                    "/repo/delete.rs": {
                        "type": "delete",
                        "content": "removed\n"
                    },
                    "/repo/old.rs": {
                        "type": "update",
                        "unified_diff": "",
                        "move_path": "/repo/new.rs"
                    }
                }
            })),
            custom_output(
                "patch-structured",
                serde_json::json!("Success. Updated the following files:\nM /repo/update.rs"),
            ),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.write_file_details.len(), 1);
        assert_eq!(record.edit_file_details.len(), 3);
        assert_eq!(record.total_unique_files, 5);
        assert!(
            record
                .edit_file_details
                .iter()
                .any(|detail| detail.base.file_path == "/repo/new.rs")
        );
        assert_eq!(parsed.analysis_facts.len(), 1);
        let fact = &parsed.analysis_facts[0];
        assert_eq!(
            fact.stable_id.as_deref(),
            Some("codex-tool:patch-structured")
        );
        assert_eq!(fact.status, ToolFactStatus::Succeeded);
        assert_eq!(fact.metrics.edit_count, 1);
        assert_eq!(fact.metrics.write_count, 0);
        let effect = fact.effect.as_ref().unwrap();
        assert_eq!(effect.unique_files.len(), 5);
        assert_eq!(effect.write_file_details.len(), 1);
        assert_eq!(effect.edit_file_details.len(), 3);
    }

    #[test]
    fn structured_failed_patch_counts_invocation_without_effects() {
        let logs = vec![event_msg(serde_json::json!({
            "type": "patch_apply_end",
            "call_id": "patch-failed-event",
            "success": false,
            "status": "completed",
            "stdout": "",
            "stderr": "failed",
            "changes": {
                "/repo/file.rs": {
                    "type": "update",
                    "unified_diff": "@@ -1 +1 @@\n-old\n+new\n",
                    "move_path": null
                }
            }
        }))];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.total_unique_files, 0);
        assert!(record.edit_file_details.is_empty());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert!(parsed.analysis_facts[0].effect.is_none());
    }

    #[test]
    fn direct_delete_without_body_is_a_successful_edit() {
        let patch = "*** Begin Patch\n*** Delete File: src/obsolete.rs\n*** End Patch";
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "delete-empty"
            })),
            custom_output("delete-empty", serde_json::json!("Done!")),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.edit_file_details.len(), 1);
        assert_eq!(record.total_unique_files, 1);
        assert_eq!(record.edit_file_details[0].base.line_count, 0);
    }

    #[test]
    fn failed_direct_custom_apply_patch_is_skipped() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
        let wire_result = serde_json::json!({
            "output": "Failed to find expected lines in src/lib.rs",
            "metadata": { "exit_code": 1, "duration_seconds": 0.01 }
        })
        .to_string();
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "patch-failed"
            })),
            custom_output("patch-failed", Value::String(wire_result)),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert!(record.edit_file_details.is_empty());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
    }

    #[test]
    fn patch_facts_count_every_lifecycle_but_only_success_keeps_effects() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
        let mut logs = vec![log_at(
            "2026-07-12T00:00:00Z",
            "turn_context",
            serde_json::json!({ "model": "gpt-5-codex" }),
        )];
        for (second, call_id) in [
            (1, "patch-success"),
            (2, "patch-failed"),
            (3, "patch-rejected"),
            (4, "patch-unknown"),
            (5, "patch-pending"),
        ] {
            logs.push(log_at(
                &format!("2026-07-12T00:00:{second:02}Z"),
                "response_item",
                serde_json::json!({
                    "type": "custom_tool_call",
                    "name": "apply_patch",
                    "input": patch,
                    "call_id": call_id
                }),
            ));
        }
        for (second, call_id, output) in [
            (6, "patch-success", "Done!"),
            (7, "patch-failed", "Failed to find expected lines"),
            (8, "patch-rejected", "patch rejected by user"),
            (9, "patch-unknown", "Patch command finished"),
        ] {
            logs.push(log_at(
                &format!("2026-07-12T00:00:{second:02}Z"),
                "response_item",
                serde_json::json!({
                    "type": "custom_tool_call_output",
                    "call_id": call_id,
                    "output": output
                }),
            ));
        }

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 5);
        assert_eq!(record.edit_file_details.len(), 1);
        assert_eq!(parsed.analysis_facts.len(), 5);

        for (call_id, status, edit_lines) in [
            ("patch-success", ToolFactStatus::Succeeded, 1),
            ("patch-failed", ToolFactStatus::Failed, 0),
            ("patch-rejected", ToolFactStatus::Failed, 0),
            ("patch-unknown", ToolFactStatus::Failed, 0),
            ("patch-pending", ToolFactStatus::Pending, 0),
        ] {
            let stable_id = format!("codex-tool:{call_id}");
            let fact = parsed
                .analysis_facts
                .iter()
                .find(|fact| fact.stable_id.as_deref() == Some(stable_id.as_str()))
                .unwrap();
            assert_eq!(fact.model, "gpt-5-codex");
            assert_eq!(fact.status, status);
            assert_eq!(fact.metrics.edit_count, 1);
            assert_eq!(fact.metrics.edit_lines, edit_lines);
            assert_eq!(fact.effect.is_some(), status == ToolFactStatus::Succeeded);
        }
    }

    #[test]
    fn unknown_direct_custom_apply_patch_result_is_skipped() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
        for output in [
            Value::Null,
            serde_json::json!("Patch command finished"),
            serde_json::json!({
                "success": true,
                "updated_files": ["src/lib.rs"]
            }),
        ] {
            let logs = vec![
                response_item(serde_json::json!({
                    "type": "custom_tool_call",
                    "name": "apply_patch",
                    "input": patch,
                    "call_id": "patch-unknown"
                })),
                custom_output("patch-unknown", output),
            ];

            let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
            let record = &parsed.analysis.records[0];
            assert_eq!(record.tool_call_counts.edit, 1);
            assert!(record.edit_file_details.is_empty());
            assert_eq!(parsed.diagnostics.relevant_records, 2);
            assert_eq!(parsed.diagnostics.normalized_records, 1);
            assert_eq!(parsed.diagnostics.failed_relevant_records, 1);
            assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
            assert!(!parsed.diagnostics.is_complete_failure());
        }
    }

    #[test]
    fn custom_output_envelope_is_stripped_before_json_result() {
        let result = serde_json::json!({
            "output": "Success. Updated the following files:\nM src/lib.rs",
            "metadata": { "exit_code": 0, "duration_seconds": 0.01 }
        })
        .to_string();
        let output = custom_output(
            "patch-array",
            serde_json::json!([
                { "type": "input_text", "text": "Script completed\nWall time: 0.1 seconds\nOutput:\n" },
                { "type": "input_text", "text": result }
            ]),
        )
        .payload
        .output;

        assert_eq!(
            normalize_custom_output(output.as_deref()),
            "Success. Updated the following files:\nM src/lib.rs"
        );
        assert_eq!(
            custom_apply_patch_result(output.as_deref()),
            CustomApplyPatchResult::Success
        );
    }

    #[test]
    fn custom_exec_is_one_bash_cell_not_nested_operations() {
        let input = "const results = await Promise.all([tools.exec_command({cmd: \"sed -n '1,2p' src/lib.rs\"}), tools.apply_patch(`*** Begin Patch\n*** Add File: notes.txt\n+hello\n*** End Patch`)]); text(JSON.stringify(results));";
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "exec",
                "input": input,
                "call_id": "exec-1"
            })),
            custom_output("exec-1", serde_json::json!("Script completed")),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.tool_call_counts.read, 0);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.tool_call_counts.edit, 0);
        assert_eq!(record.run_command_details.len(), 1);
        assert_eq!(record.run_command_details[0].command, input);
    }

    #[test]
    fn custom_exec_usage_only_keeps_count_without_source_detail() {
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "exec",
                "input": "await tools.exec_command({cmd: dynamicCommand});",
                "call_id": "exec-dynamic"
            })),
            custom_output("exec-dynamic", serde_json::json!("Script completed")),
        ];

        let analysis = parse_codex_log_iter(logs, ParseMode::UsageOnly).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert!(record.run_command_details.is_empty());
    }

    #[test]
    fn current_exec_command_apply_patch_marker_is_parsed() {
        let args = serde_json::json!({
            "cmd": "apply_patch <<'PATCH'\n*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch\nPATCH"
        })
        .to_string();
        let call = parse_function_call("exec_command", &args, 1).unwrap();
        let mut state = SessionParseState::new();
        state.handle_shell_call(
            call,
            CodexShellOutput {
                output: String::new(),
                metadata: Some(CodexShellMetadata {
                    exit_code: 0,
                    duration_seconds: 0.0,
                }),
            },
        );
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.edit_details.len(), 1);
    }

    #[test]
    fn failed_and_unknown_shell_reads_count_without_file_effects() {
        for (call_id, output, partial_failures) in [
            (
                "failed-read",
                serde_json::json!({
                    "output": "permission denied",
                    "metadata": { "exit_code": 1, "duration_seconds": 0.1 }
                }),
                0,
            ),
            ("unknown-read", serde_json::json!("file contents"), 1),
        ] {
            let logs = vec![
                response_item(serde_json::json!({
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": { "cmd": "sed -n '1,10p' /repo/file.rs" },
                    "call_id": call_id
                })),
                response_item(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output
                })),
            ];

            let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full).unwrap();
            let record = &parsed.analysis.records[0];
            assert_eq!(record.tool_call_counts.read, 1);
            assert_eq!(record.total_unique_files, 0);
            assert!(record.read_file_details.is_empty());
            assert_eq!(parsed.diagnostics.partial_failure_count(), partial_failures);
        }
    }
}
