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
use crate::session::diagnostics::{
    AnalysisFact, AnalysisFactEffect, AnalysisMetrics, AnalysisStateSnapshot, ParseDiagnostics,
    ParsedAnalysis, PricingGranularity, ToolFactStatus, UsageFact, UsageFactUnit,
};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{
    TokenCounts, accumulate_i64_fields, accumulate_nested_object, extract_token_counts,
    get_git_remote_url, parse_iso_timestamp, process_claude_usage,
};
use ahash::AHashSet;
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
/// time (see [`crate::session::parser::parse_session_file`]) feed records
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
    Ok(parse_claude_logs_with_diagnostics(logs, mode)?.analysis)
}

/// Streaming Claude parser with parser-only schema diagnostics.
pub(crate) fn parse_claude_logs_with_diagnostics<I>(
    logs: I,
    mode: ParseMode,
) -> Result<ParsedAnalysis>
where
    I: IntoIterator<Item = ClaudeCodeLog>,
{
    let mut state = SessionParseState::with_mode(mode);
    let mut conversation_usage: FastHashMap<String, Value> =
        FastHashMap::with_capacity(capacity::MODELS_PER_SESSION);
    // Advisor-message token usage is kept separate from `conversation_usage`
    // so the `analysis` aggregator never attributes the main model's file-op
    // counts to an advisor model. The `usage` path merges this in.
    let mut advisor_usage: FastHashMap<String, Value> = FastHashMap::default();
    // Claude Code writes several streamed snapshots for one assistant
    // inference. Every snapshot repeats the complete usage rather than a
    // delta, so retain only the final usage for each provider message id.
    let mut usage_by_message: FastHashMap<String, DeferredClaudeUsage> = FastHashMap::default();
    let mut anonymous_usage = Vec::new();
    // Keep the originating tool name so polymorphic top-level results can be
    // interpreted by lifecycle rather than by ambiguous fields such as
    // `filePath` (which ExitPlanMode also carries).
    let mut pending_tool_uses: FastHashMap<String, PendingClaudeTool> =
        FastHashMap::with_capacity(64);
    let mut seen_tool_uses = AHashSet::with_capacity(64);
    let mut diagnostics = ParseDiagnostics::default();
    let mut analysis_facts = Vec::new();

    for (source_index, log) in logs.into_iter().enumerate() {
        let source_order = source_index + 1;
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
                let deferred = DeferredClaudeUsage {
                    source_order,
                    stable_id: (!message.id.is_empty())
                        .then(|| format!("claude-message:{}", message.id)),
                    timestamp_ms: ts,
                    model: message.model.clone(),
                    usage: usage.clone(),
                };
                if message.id.is_empty() {
                    anonymous_usage.push(deferred);
                } else {
                    usage_by_message.insert(message.id.clone(), deferred);
                }
            }

            for item in &message.content {
                let ClaudeContentItem::ToolUse { id, name, input } = item else {
                    continue;
                };

                let input_supported = tracked_tool_input_supported(name, input.as_ref());
                let new_invocation = id.is_empty() || seen_tool_uses.insert(id.clone());
                if !new_invocation {
                    if let Some(pending) = pending_tool_uses.get_mut(id)
                        && !pending.input_supported
                        && input_supported
                    {
                        pending.name.clone_from(name);
                        pending.input.clone_from(input);
                        pending.input_supported = true;
                    }
                    continue;
                }

                let effect_before = AnalysisStateSnapshot::capture(&state);
                let before = AnalysisMetrics::from_state(&state);
                let tracked = record_claude_invocation(&mut state, name, input.as_ref(), ts);
                if tracked {
                    if input_supported
                        || matches!(name.as_str(), "TodoWrite" | "TaskCreate" | "TaskUpdate")
                    {
                        diagnostics.record_relevant(true);
                    } else if id.is_empty() {
                        diagnostics.record_relevant(false);
                    }
                    let fact_index = analysis_facts.len();
                    analysis_facts.push(AnalysisFact {
                        stable_id: (!id.is_empty()).then(|| format!("claude-tool:{id}")),
                        timestamp_ms: (ts > 0).then_some(ts),
                        observed_at_ms: (ts > 0).then_some(ts),
                        source_order,
                        model: message.model.clone().unwrap_or_default(),
                        status: ToolFactStatus::Pending,
                        metrics: AnalysisMetrics::from_state(&state).saturating_sub(before),
                        effect: None,
                    });
                    if !id.is_empty() {
                        let invocation_effect = effect_before.effect_since(&state, Vec::new());
                        pending_tool_uses.insert(
                            id.clone(),
                            PendingClaudeTool {
                                name: name.clone(),
                                input: input.clone(),
                                input_supported,
                                fact_index: Some(fact_index),
                                invocation_effect,
                            },
                        );
                    }
                }
            }
        }

        if let Some(tur) = &log.tool_use_result {
            let result_id =
                first_tool_result(log.message.as_ref()).map(|(tool_use_id, _, _)| tool_use_id);
            if result_id.is_some_and(|tool_use_id| {
                seen_tool_uses.contains(tool_use_id) && !pending_tool_uses.contains_key(tool_use_id)
            }) {
                continue;
            }
            let correlated =
                first_tool_result(log.message.as_ref()).and_then(|(tool_use_id, _, is_error)| {
                    pending_tool_uses
                        .remove(tool_use_id)
                        .map(|pending| (pending, is_error))
                });
            let effect_before = AnalysisStateSnapshot::capture(&state);
            let before = AnalysisMetrics::from_state(&state);
            let mut fact_status = correlated.as_ref().map(|(_, is_error)| {
                if *is_error {
                    ToolFactStatus::Failed
                } else {
                    ToolFactStatus::Succeeded
                }
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
                    TopLevelToolResult::Unsupported => {
                        diagnostics.record_relevant(false);
                        fact_status = Some(ToolFactStatus::Failed);
                    }
                    TopLevelToolResult::NonTextRead => {
                        diagnostics.record_relevant(true);
                        fact_status = Some(ToolFactStatus::Succeeded);
                        if let Some(path) = correlated
                            .as_ref()
                            .and_then(|(pending, _)| pending.input.as_ref())
                            .and_then(|input| input.file_path.as_deref())
                            .or_else(|| {
                                tur.file.as_ref().and_then(|file| file.file_path.as_deref())
                            })
                        {
                            state.add_non_text_read_path(path);
                        }
                    }
                    TopLevelToolResult::Supported(kind) => {
                        diagnostics.record_relevant(true);
                        dispatch_top_level_tool_result(&mut state, tur, kind, ts);
                        fact_status = Some(ToolFactStatus::Succeeded);
                    }
                }
            }
            if let (Some((pending, _)), Some(status)) = (correlated.as_ref(), fact_status) {
                if status == ToolFactStatus::Succeeded {
                    add_successful_claude_invocation_detail(&mut state, pending, ts);
                }
                let paths = if status == ToolFactStatus::Succeeded {
                    claude_effect_paths(&state, pending)
                } else {
                    Vec::new()
                };
                update_claude_fact(
                    &mut analysis_facts,
                    pending,
                    status,
                    ts,
                    AnalysisMetrics::from_state(&state).saturating_sub(before),
                    effect_before.effect_since(&state, paths),
                );
            } else if let (Some(tool_use_id), Some(status)) = (result_id, fact_status) {
                let effect = (status == ToolFactStatus::Succeeded).then(|| {
                    effect_before.effect_since(&state, standalone_claude_effect_paths(&state, tur))
                });
                analysis_facts.push(AnalysisFact {
                    stable_id: Some(format!("claude-tool:{tool_use_id}")),
                    timestamp_ms: None,
                    observed_at_ms: (ts > 0).then_some(ts),
                    source_order,
                    model: String::new(),
                    status,
                    metrics: AnalysisMetrics::from_state(&state).saturating_sub(before),
                    effect,
                });
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
                let effect_before = AnalysisStateSnapshot::capture(&state);
                let before = AnalysisMetrics::from_state(&state);

                if *is_error {
                    if is_tracked_file_tool(&pending.name) && !pending.input_supported {
                        diagnostics.record_relevant(true);
                    }
                    update_claude_fact(
                        &mut analysis_facts,
                        &pending,
                        ToolFactStatus::Failed,
                        ts,
                        AnalysisMetrics::default(),
                        AnalysisFactEffect::default(),
                    );
                    continue;
                }
                if !log.is_sidechain || !is_tracked_file_tool(&pending.name) {
                    add_successful_claude_invocation_detail(&mut state, &pending, ts);
                    update_claude_fact(
                        &mut analysis_facts,
                        &pending,
                        ToolFactStatus::Succeeded,
                        ts,
                        AnalysisMetrics::default(),
                        effect_before.effect_since(&state, Vec::new()),
                    );
                    continue;
                }

                let normalized = pending.input_supported;
                diagnostics.record_relevant(normalized);
                if normalized && let Some(input) = pending.input.as_ref() {
                    dispatch_subagent_tool_result(&mut state, &pending.name, input, content, ts);
                }
                update_claude_fact(
                    &mut analysis_facts,
                    &pending,
                    if normalized {
                        ToolFactStatus::Succeeded
                    } else {
                        ToolFactStatus::Failed
                    },
                    ts,
                    AnalysisMetrics::from_state(&state).saturating_sub(before),
                    effect_before.effect_since(
                        &state,
                        if normalized {
                            claude_effect_paths(&state, &pending)
                        } else {
                            Vec::new()
                        },
                    ),
                );
            }
        }
    }

    for pending in pending_tool_uses.into_values() {
        if is_tracked_file_tool(&pending.name) && !pending.input_supported {
            diagnostics.record_relevant(false);
        }
    }

    let mut deferred_usage: Vec<_> = usage_by_message
        .into_values()
        .chain(anonymous_usage)
        .collect();
    deferred_usage.sort_unstable_by_key(|usage| usage.source_order);
    let mut usage_facts = Vec::with_capacity(deferred_usage.len());
    for deferred in deferred_usage {
        if let Some(fact) = process_deferred_usage(
            deferred,
            &mut conversation_usage,
            &mut advisor_usage,
            &mut diagnostics,
        ) {
            usage_facts.push(fact);
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
    let mut parsed = ParsedAnalysis::new(analysis, diagnostics);
    parsed.usage_facts = usage_facts;
    parsed.analysis_facts = analysis_facts;
    Ok(parsed)
}

#[derive(Clone)]
struct PendingClaudeTool {
    name: String,
    input: Option<ClaudeToolInput>,
    input_supported: bool,
    fact_index: Option<usize>,
    invocation_effect: AnalysisFactEffect,
}

struct DeferredClaudeUsage {
    source_order: usize,
    stable_id: Option<String>,
    timestamp_ms: i64,
    model: Option<String>,
    usage: Value,
}

fn is_tracked_file_tool(name: &str) -> bool {
    matches!(name, "Read" | "Write" | "Edit")
}

fn record_claude_invocation(
    state: &mut SessionParseState,
    name: &str,
    _input: Option<&ClaudeToolInput>,
    _timestamp: i64,
) -> bool {
    match name {
        "Read" => state.tool_counts.read += 1,
        "Write" => state.tool_counts.write += 1,
        "Edit" => state.tool_counts.edit += 1,
        "TodoWrite" | "TaskCreate" | "TaskUpdate" => state.tool_counts.todo_write += 1,
        "Bash" | "bash" => {
            state.tool_counts.bash += 1;
        }
        _ => return false,
    }
    true
}

fn add_successful_claude_invocation_detail(
    state: &mut SessionParseState,
    pending: &PendingClaudeTool,
    timestamp: i64,
) {
    if !matches!(pending.name.as_str(), "Bash" | "bash") {
        return;
    }
    let command = pending
        .input
        .as_ref()
        .and_then(|input| input.command.as_deref())
        .unwrap_or("");
    let description = pending
        .input
        .as_ref()
        .and_then(|input| input.description.as_deref())
        .unwrap_or("");
    state.add_run_command_detail(command, description, timestamp);
}

fn update_claude_fact(
    facts: &mut [AnalysisFact],
    pending: &PendingClaudeTool,
    status: ToolFactStatus,
    observed_at_ms: i64,
    effects: AnalysisMetrics,
    effect: AnalysisFactEffect,
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
        let mut combined = pending.invocation_effect.clone();
        combined.add_assign(effect);
        fact.effect = Some(combined);
    }
}

fn claude_effect_paths(state: &SessionParseState, pending: &PendingClaudeTool) -> Vec<String> {
    pending
        .input
        .as_ref()
        .and_then(|input| input.file_path.as_deref())
        .map(|path| state.normalize_path(path))
        .filter(|path| !path.is_empty())
        .into_iter()
        .collect()
}

fn standalone_claude_effect_paths(
    state: &SessionParseState,
    result: &ClaudeToolUseResult,
) -> Vec<String> {
    result
        .file_path
        .as_ref()
        .or_else(|| {
            result
                .file
                .as_ref()
                .and_then(|file| file.file_path.as_ref())
        })
        .map(|path| state.normalize_path(path))
        .filter(|path| !path.is_empty())
        .into_iter()
        .collect()
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
        "Bash" | "bash" => input
            .command
            .as_deref()
            .is_some_and(|command| !command.trim().is_empty()),
        _ => false,
    }
}

fn process_deferred_usage(
    deferred: DeferredClaudeUsage,
    conversation_usage: &mut FastHashMap<String, Value>,
    advisor_usage: &mut FastHashMap<String, Value>,
    diagnostics: &mut ParseDiagnostics,
) -> Option<UsageFact> {
    let DeferredClaudeUsage {
        source_order,
        stable_id,
        timestamp_ms,
        model,
        usage,
    } = deferred;
    let model = model.as_deref().filter(|model| !model.is_empty());
    let normalized = normalize_claude_usage(&usage);
    let main_supported = model.is_some() && is_supported_claude_usage(&normalized.main);
    let mut units = Vec::new();
    diagnostics.record_relevant(main_supported);
    if main_supported && let Some(model) = model {
        for field in ["speed", "inference_geo"] {
            if normalized
                .main
                .get(field)
                .is_some_and(|value| !value.is_null() && !value.is_string())
            {
                diagnostics.record_relevant(false);
            }
        }
        process_claude_usage(conversation_usage, model, &normalized.main);
        let mut request_sum = TokenCounts::default();
        let mut request_units = Vec::with_capacity(normalized.main_requests.len());
        for request in &normalized.main_requests {
            if !is_supported_claude_usage(request) {
                diagnostics.record_relevant(false);
                continue;
            }
            let mut unit =
                UsageFactUnit::from_value(model.to_string(), request, PricingGranularity::Request);
            unit.inherit_provider_pricing_modifiers(&normalized.main);
            add_token_counts(&mut request_sum, &unit.counts);
            request_units.push(unit);
        }
        let aggregate = extract_token_counts(&normalized.main);
        let (residual, inconsistent) = positive_token_residual(&aggregate, &request_sum);
        if inconsistent {
            diagnostics.record_relevant(false);
            let mut unit = UsageFactUnit::from_counts(
                model.to_string(),
                aggregate,
                PricingGranularity::Aggregate,
            );
            unit.inherit_provider_pricing_modifiers(&normalized.main);
            units.push(unit);
        } else {
            units.extend(request_units);
        }
        if !inconsistent && token_counts_have_activity(&residual) {
            let mut unit = UsageFactUnit::from_counts(
                model.to_string(),
                residual,
                PricingGranularity::Aggregate,
            );
            unit.inherit_provider_pricing_modifiers(&normalized.main);
            units.push(unit);
        }
    }

    for advisor in &normalized.advisors {
        let advisor_model = advisor
            .model
            .as_deref()
            .filter(|model| !model.is_empty())
            .or(model);
        let supported = advisor_model.is_some() && is_supported_claude_usage(&advisor.usage);
        diagnostics.record_relevant(supported);
        if supported && let Some(advisor_model) = advisor_model {
            process_claude_usage(advisor_usage, advisor_model, &advisor.usage);
            let mut unit = UsageFactUnit::from_value(
                advisor_model.to_string(),
                &advisor.usage,
                PricingGranularity::Request,
            );
            unit.analysis_presence = false;
            units.push(unit);
        }
    }

    if normalized.repaired_cache_split {
        // The scalar cache-write count is authoritative for total tokens, but
        // an incomplete TTL split cannot be priced exactly. Keep the tokens in
        // the default 5-minute bucket and surface the schema mismatch.
        diagnostics.record_relevant(false);
    }

    (!units.is_empty()).then_some(UsageFact {
        stable_id,
        timestamp_ms: (timestamp_ms > 0).then_some(timestamp_ms),
        observed_at_ms: (timestamp_ms > 0).then_some(timestamp_ms),
        source_order,
        units,
    })
}

fn add_token_counts(total: &mut TokenCounts, value: &TokenCounts) {
    total.input_tokens += value.input_tokens;
    total.output_tokens += value.output_tokens;
    total.reasoning_tokens += value.reasoning_tokens;
    total.cache_read += value.cache_read;
    total.cache_creation += value.cache_creation;
    total.cache_creation_5m += value.cache_creation_5m;
    total.cache_creation_1h += value.cache_creation_1h;
    total.web_search_requests += value.web_search_requests;
    total.total += value.total;
}

fn positive_token_residual(total: &TokenCounts, parts: &TokenCounts) -> (TokenCounts, bool) {
    let inconsistent = parts.input_tokens > total.input_tokens
        || parts.output_tokens > total.output_tokens
        || parts.reasoning_tokens > total.reasoning_tokens
        || parts.cache_read > total.cache_read
        || parts.cache_creation > total.cache_creation
        || parts.cache_creation_5m > total.cache_creation_5m
        || parts.cache_creation_1h > total.cache_creation_1h
        || parts.web_search_requests > total.web_search_requests
        || parts.total > total.total;
    (
        TokenCounts {
            input_tokens: (total.input_tokens - parts.input_tokens).max(0),
            output_tokens: (total.output_tokens - parts.output_tokens).max(0),
            reasoning_tokens: (total.reasoning_tokens - parts.reasoning_tokens).max(0),
            cache_read: (total.cache_read - parts.cache_read).max(0),
            cache_creation: (total.cache_creation - parts.cache_creation).max(0),
            cache_creation_5m: (total.cache_creation_5m - parts.cache_creation_5m).max(0),
            cache_creation_1h: (total.cache_creation_1h - parts.cache_creation_1h).max(0),
            web_search_requests: (total.web_search_requests - parts.web_search_requests).max(0),
            total: (total.total - parts.total).max(0),
        },
        inconsistent,
    )
}

fn token_counts_have_activity(counts: &TokenCounts) -> bool {
    counts.total != 0
        || counts.input_tokens != 0
        || counts.output_tokens != 0
        || counts.reasoning_tokens != 0
        || counts.cache_read != 0
        || counts.cache_creation != 0
        || counts.web_search_requests != 0
}

struct NormalizedClaudeUsage {
    main: Value,
    main_requests: Vec<Value>,
    advisors: Vec<NormalizedAdvisorUsage>,
    repaired_cache_split: bool,
}

struct NormalizedAdvisorUsage {
    model: Option<String>,
    usage: Value,
}

/// Splits Claude's iteration ledger into main-model and advisor usage.
///
/// The top-level scalar fields describe only `message` iterations, while an
/// `advisor_message` is a separately billed inference. Newer Claude Code
/// versions can also leave the top-level TTL split at the first iteration,
/// so the main split is rebuilt from every `message` iteration.
fn normalize_claude_usage(usage: &Value) -> NormalizedClaudeUsage {
    let Some(usage_obj) = usage.as_object() else {
        return NormalizedClaudeUsage {
            main: usage.clone(),
            main_requests: vec![usage.clone()],
            advisors: Vec::new(),
            repaired_cache_split: false,
        };
    };
    let Some(iterations) = usage_obj.get("iterations").and_then(Value::as_array) else {
        let (main, repaired_cache_split) = repair_cache_creation_split(usage);
        return NormalizedClaudeUsage {
            main_requests: vec![main.clone()],
            main,
            advisors: Vec::new(),
            repaired_cache_split,
        };
    };

    let mut main = serde_json::Map::new();
    let mut main_requests = Vec::new();
    let mut has_main_iteration = false;
    let mut advisors = Vec::new();
    let mut repaired_cache_split = false;

    for iteration in iterations {
        match iteration.get("type").and_then(Value::as_str) {
            Some("message") => {
                has_main_iteration = true;
                let (iteration, repaired) = repair_cache_creation_split(iteration);
                repaired_cache_split |= repaired;
                main_requests.push(iteration.clone());
                if let Some(iteration) = iteration.as_object() {
                    accumulate_claude_usage_object(&mut main, iteration);
                }
            }
            Some("advisor_message") => {
                let (iteration, repaired) = repair_cache_creation_split(iteration);
                repaired_cache_split |= repaired;
                advisors.push(NormalizedAdvisorUsage {
                    model: iteration
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    usage: iteration,
                });
            }
            _ => {}
        }
    }

    let main = if has_main_iteration {
        // Top-level scalars are the authoritative sum of the main-message
        // iterations. Keep them, but replace the occasionally-truncated
        // top-level TTL split with the complete iteration ledger.
        let mut normalized = usage_obj.clone();
        normalized.remove("iterations");
        for field in [
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
            "output_tokens",
        ] {
            if !normalized.contains_key(field)
                && let Some(value) = main.get(field)
            {
                normalized.insert(field.to_string(), value.clone());
            }
        }
        if main
            .get("cache_creation")
            .and_then(Value::as_object)
            .is_some_and(|split| split.values().any(|value| value.as_i64().is_some()))
        {
            normalized.insert("cache_creation".to_string(), main["cache_creation"].clone());
        }
        if !normalized.contains_key("server_tool_use")
            && let Some(server_tool_use) = main.get("server_tool_use")
        {
            normalized.insert("server_tool_use".to_string(), server_tool_use.clone());
        }
        let (main, repaired) = repair_cache_creation_split(&Value::Object(normalized));
        repaired_cache_split |= repaired;
        main
    } else {
        let (main, repaired) = repair_cache_creation_split(usage);
        repaired_cache_split |= repaired;
        main
    };

    NormalizedClaudeUsage {
        main,
        main_requests,
        advisors,
        repaired_cache_split,
    }
}

fn accumulate_claude_usage_object(
    target: &mut serde_json::Map<String, Value>,
    source: &serde_json::Map<String, Value>,
) {
    accumulate_i64_fields(
        target,
        source,
        &[
            "input_tokens",
            "cache_creation_input_tokens",
            "cache_read_input_tokens",
            "output_tokens",
        ],
    );
    for nested in ["cache_creation", "server_tool_use"] {
        if let Some(source_nested) = source.get(nested).and_then(Value::as_object) {
            accumulate_nested_object(target, nested, source_nested);
        }
    }
}

fn repair_cache_creation_split(usage: &Value) -> (Value, bool) {
    let Some(usage_obj) = usage.as_object() else {
        return (usage.clone(), false);
    };
    let Some(total) = usage_obj
        .get("cache_creation_input_tokens")
        .and_then(Value::as_i64)
    else {
        return (usage.clone(), false);
    };
    let Some(cache_creation) = usage_obj.get("cache_creation").and_then(Value::as_object) else {
        return (usage.clone(), false);
    };

    let five_minutes = match cache_creation.get("ephemeral_5m_input_tokens") {
        Some(value) => match value.as_i64() {
            Some(value) => value,
            None => return (usage.clone(), false),
        },
        None => 0,
    };
    let one_hour = match cache_creation.get("ephemeral_1h_input_tokens") {
        Some(value) => match value.as_i64() {
            Some(value) => value,
            None => return (usage.clone(), false),
        },
        None => 0,
    };
    let residual = total - five_minutes - one_hour;
    if residual <= 0 {
        return (usage.clone(), residual < 0);
    }

    let mut repaired = usage.clone();
    repaired["cache_creation"]["ephemeral_5m_input_tokens"] = Value::from(five_minutes + residual);
    (repaired, true)
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
                { "type": "tool_use", "id": "task-2", "name": "TaskUpdate", "input": {} }
            ]),
        );

        let analysis = parse_claude_logs(vec![log], ParseMode::Full).unwrap();
        assert_eq!(analysis.records[0].tool_call_counts.todo_write, 2);
    }

    #[test]
    fn streamed_message_snapshots_use_final_usage_and_union_tools_by_id() {
        let snapshot = |output_tokens, complete_input| {
            let input = if complete_input {
                serde_json::json!({ "file_path": "/tmp/streamed.txt" })
            } else {
                serde_json::json!({})
            };
            serde_json::from_value::<ClaudeCodeLog>(serde_json::json!({
                "type": "assistant",
                "timestamp": "2026-07-12T00:00:00Z",
                "isSidechain": true,
                "message": {
                    "id": "msg-streamed",
                    "model": "claude-opus-4-7",
                    "usage": { "input_tokens": 2, "output_tokens": output_tokens },
                    "content": [{
                        "type": "tool_use",
                        "id": "read-streamed",
                        "name": "Read",
                        "input": input
                    }]
                }
            }))
            .unwrap()
        };
        let result = user_log(
            "2026-07-12T00:00:01Z",
            serde_json::json!([{
                "type": "tool_result",
                "tool_use_id": "read-streamed",
                "content": "one\ntwo"
            }]),
        );

        let parsed = parse_claude_logs_with_diagnostics(
            [
                snapshot(1, false),
                snapshot(5, true),
                result.clone(),
                result,
            ],
            ParseMode::Full,
        )
        .unwrap();
        let record = &parsed.analysis.records[0];
        let usage = record.conversation_usage.get("claude-opus-4-7").unwrap();
        assert_eq!(usage["input_tokens"], 2);
        assert_eq!(usage["output_tokens"], 5);
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 2);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
    }

    #[test]
    fn replayed_top_level_tool_results_do_not_duplicate_effects() {
        let assistant = assistant_log(
            "2026-07-12T00:00:00Z",
            "claude-opus-4-7",
            serde_json::json!([{
                "type": "tool_use",
                "id": "read-replayed",
                "name": "Read",
                "input": { "file_path": "/tmp/replayed.txt" }
            }]),
        );
        let result: ClaudeCodeLog = serde_json::from_value(serde_json::json!({
            "type": "user",
            "timestamp": "2026-07-12T00:00:01Z",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "read-replayed",
                    "content": "one\ntwo"
                }]
            },
            "toolUseResult": {
                "type": "text",
                "file": {
                    "filePath": "/tmp/replayed.txt",
                    "content": "one\ntwo"
                }
            }
        }))
        .unwrap();

        let parsed = parse_claude_logs_with_diagnostics(
            [assistant, result.clone(), result],
            ParseMode::Full,
        )
        .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 2);
        assert_eq!(record.read_file_details.len(), 1);
    }

    #[test]
    fn iterations_rebuild_main_and_advisor_cache_ttl_splits() {
        let log: ClaudeCodeLog = serde_json::from_value(serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-07-12T00:00:00Z",
            "message": {
                "id": "msg-iterations",
                "model": "claude-haiku-4-5",
                "content": [],
                "usage": {
                    "input_tokens": 5,
                    "output_tokens": 9,
                    "speed": "fast",
                    "inference_geo": "us",
                    "cache_creation_input_tokens": 30,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 10,
                        "ephemeral_1h_input_tokens": 0
                    },
                    "server_tool_use": { "web_search_requests": 2 },
                    "iterations": [
                        {
                            "type": "message",
                            "input_tokens": 2,
                            "output_tokens": 4,
                            "cache_creation_input_tokens": 10,
                            "cache_creation": {
                                "ephemeral_5m_input_tokens": 10,
                                "ephemeral_1h_input_tokens": 0
                            }
                        },
                        {
                            "type": "advisor_message",
                            "model": "claude-opus-4-8",
                            "input_tokens": 7,
                            "output_tokens": 11,
                            "cache_creation_input_tokens": 3,
                            "cache_creation": {
                                "ephemeral_5m_input_tokens": 0,
                                "ephemeral_1h_input_tokens": 3
                            }
                        },
                        {
                            "type": "message",
                            "input_tokens": 3,
                            "output_tokens": 5,
                            "cache_creation_input_tokens": 20,
                            "cache_creation": {
                                "ephemeral_5m_input_tokens": 0,
                                "ephemeral_1h_input_tokens": 20
                            }
                        }
                    ]
                }
            }
        }))
        .unwrap();

        let parsed = parse_claude_logs_with_diagnostics([log], ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        let main = record.conversation_usage.get("claude-haiku-4-5").unwrap();
        assert_eq!(main["input_tokens"], 5);
        assert_eq!(main["output_tokens"], 9);
        assert_eq!(main["cache_creation_input_tokens"], 30);
        assert_eq!(main["cache_creation"]["ephemeral_5m_input_tokens"], 10);
        assert_eq!(main["cache_creation"]["ephemeral_1h_input_tokens"], 20);
        assert_eq!(main["server_tool_use"]["web_search_requests"], 2);
        let advisor = record.advisor_usage.get("claude-opus-4-8").unwrap();
        assert_eq!(advisor["cache_creation_input_tokens"], 3);
        assert_eq!(advisor["cache_creation"]["ephemeral_1h_input_tokens"], 3);
        assert_eq!(parsed.usage_facts[0].units.len(), 4);
        for unit in &parsed.usage_facts[0].units[..3] {
            assert_eq!(unit.provider_pricing_modifiers, ["fast", "us"]);
        }
        assert!(
            parsed.usage_facts[0].units[3]
                .provider_pricing_modifiers
                .is_empty()
        );
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
    }

    #[test]
    fn null_provider_modifiers_are_absent_without_diagnostics() {
        let log: ClaudeCodeLog = serde_json::from_value(serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-07-12T00:00:00Z",
            "message": {
                "id": "msg-null-modifiers",
                "model": "claude-opus-4-8",
                "content": [],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "speed": null,
                    "inference_geo": null
                }
            }
        }))
        .unwrap();

        let parsed = parse_claude_logs_with_diagnostics([log], ParseMode::Full).unwrap();

        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.usage_facts.len(), 1);
        assert!(
            parsed.usage_facts[0].units[0]
                .provider_pricing_modifiers
                .is_empty()
        );
    }

    #[test]
    fn inconsistent_iteration_sum_falls_back_to_authoritative_aggregate() {
        let log: ClaudeCodeLog = serde_json::from_value(serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-07-12T00:00:00Z",
            "message": {
                "id": "msg-inconsistent-iterations",
                "model": "claude-opus-4-7",
                "content": [],
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 10,
                    "speed": "fast",
                    "iterations": [
                        { "type": "message", "input_tokens": 60, "output_tokens": 5 },
                        { "type": "message", "input_tokens": 60, "output_tokens": 5 }
                    ]
                }
            }
        }))
        .unwrap();

        let parsed = parse_claude_logs_with_diagnostics([log], ParseMode::Full).unwrap();
        let fact = &parsed.usage_facts[0];
        assert_eq!(fact.units.len(), 1);
        assert_eq!(fact.units[0].counts.input_tokens, 100);
        assert_eq!(fact.units[0].counts.output_tokens, 10);
        assert_eq!(fact.units[0].granularity, PricingGranularity::Aggregate);
        assert_eq!(fact.units[0].provider_pricing_modifiers, ["fast"]);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        let main = parsed.analysis.records[0]
            .conversation_usage
            .get("claude-opus-4-7")
            .unwrap();
        assert_eq!(main["input_tokens"], 100);
        assert_eq!(main["output_tokens"], 10);
    }

    #[test]
    fn incomplete_cache_ttl_split_defaults_residual_to_five_minutes() {
        let log: ClaudeCodeLog = serde_json::from_value(serde_json::json!({
            "type": "assistant",
            "timestamp": "2026-07-12T00:00:00Z",
            "message": {
                "id": "msg-residual",
                "model": "claude-opus-4-7",
                "content": [],
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 1,
                    "cache_creation_input_tokens": 10,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 2,
                        "ephemeral_1h_input_tokens": 3
                    }
                }
            }
        }))
        .unwrap();

        let parsed = parse_claude_logs_with_diagnostics([log], ParseMode::Full).unwrap();
        let usage = parsed.analysis.records[0]
            .conversation_usage
            .get("claude-opus-4-7")
            .unwrap();
        assert_eq!(usage["cache_creation"]["ephemeral_5m_input_tokens"], 7);
        assert_eq!(usage["cache_creation"]["ephemeral_1h_input_tokens"], 3);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
    }

    #[test]
    fn lowercase_bash_and_file_history_delta_are_supported() {
        let bash = assistant_log(
            "2026-07-12T00:00:00Z",
            "claude-opus-4-7",
            serde_json::json!([{
                "type": "tool_use",
                "id": "bash-lowercase",
                "name": "bash",
                "input": { "command": "true", "description": "succeeds" }
            }]),
        );
        let metadata: ClaudeCodeLog = serde_json::from_value(serde_json::json!({
            "type": "file-history-delta",
            "timestamp": "2026-07-12T00:00:01Z"
        }))
        .unwrap();

        let parsed = parse_claude_logs_with_diagnostics([bash, metadata], ParseMode::Full).unwrap();
        assert_eq!(parsed.analysis.records[0].tool_call_counts.bash, 1);
        assert_eq!(parsed.diagnostics.unrecognized_records, 0);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
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
                parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full).unwrap();
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
            parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full).unwrap();
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
            parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full).unwrap();
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
    fn explicitly_errored_invalid_reads_count_invocations_without_effects() {
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
                parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full).unwrap();
            assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
            assert!(!parsed.diagnostics.is_complete_failure());
            let record = &parsed.analysis.records[0];
            assert_eq!(record.tool_call_counts.read, 1);
            assert_eq!(record.total_read_lines, 0);
            assert_eq!(record.total_unique_files, 0);
        }
    }

    #[test]
    fn bash_details_are_retained_only_after_confirmed_success() {
        let assistant = assistant_log(
            "2026-07-12T00:00:00Z",
            "claude-opus-4-7",
            serde_json::json!([
                {
                    "type": "tool_use",
                    "id": "bash-success",
                    "name": "Bash",
                    "input": { "command": "pwd" }
                },
                {
                    "type": "tool_use",
                    "id": "bash-failed",
                    "name": "Bash",
                    "input": { "command": "false" }
                },
                {
                    "type": "tool_use",
                    "id": "bash-pending",
                    "name": "Bash",
                    "input": { "command": "sleep 1" }
                }
            ]),
        );
        let results = user_log(
            "2026-07-12T00:00:01Z",
            serde_json::json!([
                {
                    "type": "tool_result",
                    "tool_use_id": "bash-success",
                    "content": "/tmp"
                },
                {
                    "type": "tool_result",
                    "tool_use_id": "bash-failed",
                    "content": "exit 1",
                    "is_error": true
                }
            ]),
        );

        let parsed =
            parse_claude_logs_with_diagnostics([assistant, results], ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 3);
        assert_eq!(record.run_command_details.len(), 1);
        assert_eq!(record.run_command_details[0].command, "pwd");
        assert_eq!(parsed.analysis_facts.len(), 3);
        assert_eq!(parsed.analysis_facts[0].status, ToolFactStatus::Succeeded);
        assert_eq!(parsed.analysis_facts[1].status, ToolFactStatus::Failed);
        assert_eq!(parsed.analysis_facts[2].status, ToolFactStatus::Pending);
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

        let parsed = parse_claude_logs_with_diagnostics([assistant], ParseMode::Full).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 0);
        assert_eq!(record.total_unique_files, 0);
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

        let parsed = parse_claude_logs_with_diagnostics([log], ParseMode::Full).unwrap();
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
        assert_eq!(main["cache_read_input_tokens"].as_i64().unwrap(), 68709);
        assert_eq!(main["cache_creation_input_tokens"].as_i64().unwrap(), 18687);
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
        let parsed = parse_claude_logs_with_diagnostics([log], ParseMode::Full).unwrap();

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
