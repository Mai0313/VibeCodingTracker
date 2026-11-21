//! Parser for GitHub Copilot CLI session events (`~/.copilot/session-state/<sessionId>/events.jsonl`).
//!
//! Token usage comes from `session.shutdown` when present, falling back to streamed `assistant.message.outputTokens` (with `input_tokens: 0`) for sessions that did not shut down cleanly. File operations are paired across `tool.execution_start` and `tool.execution_complete` by `toolCallId` and counted only on success. Legacy single-object dumps under `~/.copilot/history-session-state/` are not supported.
use crate::constants::{FastHashMap, capacity};
use crate::models::*;
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::parser::stream_jsonl_borrowed;
use crate::session::state::{ParseMode, SessionParseState, TextMetrics, count_json_string_metrics};
use crate::utils::{get_git_remote_url, parse_iso_timestamp};
use anyhow::Result;
use serde::{
    Deserialize, Deserializer,
    de::{Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, json, value::RawValue};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

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
    let mut fold = CopilotFold::new(mode);
    for event in events {
        fold.observe_owned_event(event);
    }
    Ok(fold.finish())
}

/// Streams and parses a Copilot CLI `events.jsonl` file at `path` using borrowed deserialization without per-line `Value` allocation.
pub(crate) fn parse_copilot_jsonl_path(
    path: &Path,
    mode: ParseMode,
) -> Result<Option<ParsedAnalysis>> {
    stream_jsonl_borrowed(
        path,
        ExtensionType::Copilot,
        CopilotFold::new(mode),
        |fold, bytes| {
            if !bytes.starts_with(b"{") {
                return Err(serde::de::Error::custom("expected Copilot event object"));
            }
            let event = serde_json::from_slice::<BorrowedCopilotEvent<'_>>(bytes)?;
            fold.observe_borrowed_event(event);
            Ok(())
        },
        |fold| Ok(fold.finish()),
    )
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct CopilotDisjointTokens {
    input_tokens: i64,
    output_tokens: i64,
    reasoning_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
}

impl CopilotDisjointTokens {
    fn from_shutdown(usage: &CopilotModelUsage, token_details_input: Option<i64>) -> Self {
        // Copilot's `usage.inputTokens` includes `cacheReadTokens + cacheWriteTokens`, whereas `tokenDetails.input.tokenCount` (when present) reports uncached input directly.
        let input_tokens = token_details_input
            .unwrap_or_else(|| {
                usage
                    .input_tokens
                    .saturating_sub(usage.cache_read_tokens)
                    .saturating_sub(usage.cache_write_tokens)
            })
            .max(0);
        // Copilot's `outputTokens` includes `reasoningTokens`, whereas the flat usage shape keeps output and reasoning disjoint.
        let output_tokens = usage
            .output_tokens
            .saturating_sub(usage.reasoning_tokens)
            .max(0);
        Self {
            input_tokens,
            output_tokens,
            reasoning_tokens: usage.reasoning_tokens.max(0),
            cache_read_tokens: usage.cache_read_tokens.max(0),
            cache_write_tokens: usage.cache_write_tokens.max(0),
        }
    }

    fn is_monotonic_from(self, prev: Self) -> bool {
        self.input_tokens >= prev.input_tokens
            && self.output_tokens >= prev.output_tokens
            && self.reasoning_tokens >= prev.reasoning_tokens
            && self.cache_read_tokens >= prev.cache_read_tokens
            && self.cache_write_tokens >= prev.cache_write_tokens
    }

    fn add_assign(&mut self, delta: Self) {
        self.input_tokens += delta.input_tokens;
        self.output_tokens += delta.output_tokens;
        self.reasoning_tokens += delta.reasoning_tokens;
        self.cache_read_tokens += delta.cache_read_tokens;
        self.cache_write_tokens += delta.cache_write_tokens;
    }

    fn delta_from(self, prev: Self) -> Self {
        Self {
            input_tokens: (self.input_tokens - prev.input_tokens).max(0),
            output_tokens: (self.output_tokens - prev.output_tokens).max(0),
            reasoning_tokens: (self.reasoning_tokens - prev.reasoning_tokens).max(0),
            cache_read_tokens: (self.cache_read_tokens - prev.cache_read_tokens).max(0),
            cache_write_tokens: (self.cache_write_tokens - prev.cache_write_tokens).max(0),
        }
    }

    fn to_json(self) -> Value {
        json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "reasoning_output_tokens": self.reasoning_tokens,
            "cache_read_input_tokens": self.cache_read_tokens,
            "cache_creation_input_tokens": self.cache_write_tokens,
        })
    }
}

struct CopilotShutdownMetric {
    usage: Option<CopilotModelUsage>,
    token_details_input: Option<i64>,
}

struct CopilotShutdownAccumulator {
    accumulated: CopilotDisjointTokens,
    last_snapshot: CopilotDisjointTokens,
}

struct CopilotFold {
    state: SessionParseState,
    conversation_usage: FastHashMap<String, Value>,
    // Per-model running total and previous `session.shutdown` snapshot so resumed sessions handle both cumulative and per-run reset counters.
    shutdown_totals: FastHashMap<String, CopilotShutdownAccumulator>,
    // Latches true once any model drops across `session.shutdown`, indicating older per-run reset counters for the rest of the session.
    per_run_resets: bool,
    // Pending tool calls indexed by `toolCallId`.
    pending_tools: FastHashMap<String, PendingTool>,
    // Active model for fallback `assistant.message.outputTokens` attribution when `session.shutdown` never arrives.
    current_model: String,
    // Guards against the fallback overwriting authoritative `session.shutdown` rows on the same canonicalized model key.
    shutdown_seen: bool,
    pending_output_tokens: FastHashMap<String, i64>,
    diagnostics: ParseDiagnostics,
}

impl CopilotFold {
    fn new(mode: ParseMode) -> Self {
        Self {
            state: SessionParseState::with_mode(mode),
            conversation_usage: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            shutdown_totals: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            per_run_resets: false,
            pending_tools: FastHashMap::with_capacity(32),
            current_model: String::new(),
            shutdown_seen: false,
            pending_output_tokens: FastHashMap::with_capacity(3),
            diagnostics: ParseDiagnostics::default(),
        }
    }

    fn observe_envelope(&mut self, event_type: &str, timestamp: &str) -> i64 {
        let recognized = matches!(
            event_type,
            "session.start"
                | "session.model_change"
                | "session.task_complete"
                | "session.shutdown"
                | "session.info"
                | "session.mode_changed"
                | "session.usage_checkpoint"
                | "session.permissions_changed"
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
            self.diagnostics.record_recognized_source();
        } else {
            self.diagnostics.record_unrecognized();
        }
        let ts = parse_iso_timestamp(timestamp);
        if ts > self.state.last_ts {
            self.state.last_ts = ts;
        }
        ts
    }

    fn apply_shutdown_metrics(&mut self, metrics: BTreeMap<String, CopilotShutdownMetric>) {
        self.diagnostics.record_relevant(true);
        let parsed: Vec<(String, CopilotDisjointTokens)> = metrics
            .into_iter()
            .filter_map(|(model, metric)| {
                if model.is_empty() {
                    return None;
                }
                let usage = metric.usage?;
                let current =
                    CopilotDisjointTokens::from_shutdown(&usage, metric.token_details_input);
                Some((canonicalize_model_name(&model), current))
            })
            .collect();

        // Detect a per-run reset across any model before applying deltas so sibling models and later runs accumulate in full.
        if !self.per_run_resets
            && parsed.iter().any(|(canonical, current)| {
                self.shutdown_totals
                    .get(canonical)
                    .is_some_and(|entry| !current.is_monotonic_from(entry.last_snapshot))
            })
        {
            self.per_run_resets = true;
        }

        for (canonical, current) in parsed {
            let accumulated = match self.shutdown_totals.get_mut(&canonical) {
                Some(entry) => {
                    let delta = if self.per_run_resets {
                        // Older Copilot builds reset per-run counters across `session.resume`; add each reset segment in full.
                        current
                    } else {
                        current.delta_from(entry.last_snapshot)
                    };
                    entry.accumulated.add_assign(delta);
                    entry.last_snapshot = current;
                    entry.accumulated
                }
                None => {
                    self.shutdown_totals.insert(
                        canonical.clone(),
                        CopilotShutdownAccumulator {
                            accumulated: current,
                            last_snapshot: current,
                        },
                    );
                    current
                }
            };
            self.conversation_usage
                .insert(canonical, accumulated.to_json());
        }
        self.shutdown_seen = true;
    }

    fn observe_borrowed_event(&mut self, event: BorrowedCopilotEvent<'_>) {
        let ts = self.observe_envelope(event.event_type.as_ref(), event.timestamp.as_ref());
        let Some(data_raw) = event.data.filter(|raw| raw.get().starts_with('{')) else {
            if matches!(
                event.event_type.as_ref(),
                "session.shutdown" | "tool.execution_start" | "tool.execution_complete"
            ) {
                self.diagnostics.record_relevant(false);
            }
            return;
        };

        match event.event_type.as_ref() {
            "session.start" => {
                if let Ok(data) =
                    serde_json::from_str::<BorrowedSessionStartData<'_>>(data_raw.get())
                {
                    if self.state.task_id.is_empty() && !data.session_id.is_empty() {
                        self.state.task_id = data.session_id.into_owned();
                    }
                    if let Some(ctx) = data.context {
                        if self.state.folder_path.is_empty() {
                            if !ctx.cwd.is_empty() {
                                self.state.folder_path = ctx.cwd.into_owned();
                            } else if !ctx.git_root.is_empty() {
                                self.state.folder_path = ctx.git_root.into_owned();
                            }
                        }
                        if self.state.git_remote.is_empty() {
                            self.state.git_remote =
                                build_remote_url(&ctx.repository_host, &ctx.repository);
                        }
                    }
                }
            }
            "session.model_change" => {
                if let Ok(data) =
                    serde_json::from_str::<BorrowedModelChangeData<'_>>(data_raw.get())
                    && let Some(new_model) =
                        raw_as_str(data.new_model).filter(|model| !model.is_empty())
                {
                    self.current_model = canonicalize_model_name(new_model.as_ref());
                }
            }
            "session.shutdown" => {
                if let Ok(data) = serde_json::from_str::<ValidatedShutdownData>(data_raw.get()) {
                    self.apply_shutdown_metrics(data.model_metrics);
                } else {
                    self.diagnostics.record_relevant(false);
                }
            }
            "assistant.message" => {
                if let Ok(data) =
                    serde_json::from_str::<BorrowedAssistantMessageData<'_>>(data_raw.get())
                    && let Some(raw_tokens) = data.output_tokens
                {
                    let output_tokens = serde_json::from_str::<i64>(raw_tokens.get()).ok();
                    self.diagnostics
                        .record_relevant(output_tokens.is_some() && !self.current_model.is_empty());
                    if let Some(output_tokens) = output_tokens.filter(|&t| t > 0)
                        && !self.current_model.is_empty()
                    {
                        if let Some(total) = self.pending_output_tokens.get_mut(&self.current_model)
                        {
                            *total += output_tokens;
                        } else {
                            self.pending_output_tokens
                                .insert(self.current_model.clone(), output_tokens);
                        }
                    }
                }
            }
            "tool.execution_start" => {
                match serde_json::from_str::<BorrowedToolStartData<'_>>(data_raw.get()) {
                    Ok(data) if !data.tool_call_id.is_empty() && !data.tool_name.is_empty() => {
                        let tool = TrackedTool::from_name(data.tool_name.as_ref());
                        let payload = match tool {
                            Some(tracked) => {
                                parse_borrowed_start_args(tracked, data.arguments, self.state.mode)
                            }
                            None => PendingToolPayload::Untracked,
                        };
                        self.pending_tools.insert(
                            data.tool_call_id.into_owned(),
                            PendingTool {
                                tool,
                                payload,
                                timestamp: ts,
                            },
                        );
                    }
                    Ok(data) => {
                        self.diagnostics.record_relevant(false);
                        // Keep the `toolCallId` registered so its completion pairs cleanly instead of double-counting the drift as an orphan.
                        if !data.tool_call_id.is_empty() {
                            self.pending_tools.insert(
                                data.tool_call_id.into_owned(),
                                PendingTool {
                                    tool: None,
                                    payload: PendingToolPayload::Untracked,
                                    timestamp: ts,
                                },
                            );
                        }
                    }
                    Err(_) => self.diagnostics.record_relevant(false),
                }
            }
            "tool.execution_complete" => {
                let Ok(data) = serde_json::from_str::<BorrowedToolCompleteData<'_>>(data_raw.get())
                else {
                    self.diagnostics.record_relevant(false);
                    return;
                };
                let Some(tool_call_id) = raw_as_str(data.tool_call_id).filter(|id| !id.is_empty())
                else {
                    self.diagnostics.record_relevant(false);
                    return;
                };
                let success = data
                    .success
                    .and_then(|raw| serde_json::from_str::<bool>(raw.get()).ok());
                let Some(pending) = self.pending_tools.remove(tool_call_id.as_ref()) else {
                    // Only successful orphans count as drift because failed tool calls are ignored even when paired.
                    if success == Some(true) {
                        self.diagnostics.record_relevant(false);
                    }
                    return;
                };
                let Some(success) = success else {
                    if pending.tool.is_some() {
                        self.diagnostics.record_relevant(false);
                    }
                    return;
                };
                // Failed tool calls do not apply file operations; count them as valid lifecycle records without dispatching.
                if !success {
                    if pending.tool.is_some() {
                        self.diagnostics.record_relevant(true);
                    }
                    return;
                }
                // Match `CopilotToolCompleteData`'s `model: String` validation.
                if let Some(model_raw) = data.model
                    && !model_raw.get().starts_with('"')
                {
                    if pending.tool.is_some() {
                        self.diagnostics.record_relevant(false);
                    }
                    return;
                }
                if pending.tool.is_none() {
                    return;
                }
                let complete = apply_borrowed_pending_tool(
                    pending.payload,
                    data.result,
                    &mut self.state,
                    pending.timestamp,
                );
                self.diagnostics.record_relevant(complete);
            }
            _ => {}
        }
    }

    fn observe_owned_event(&mut self, event: CopilotEvent) {
        let ts = self.observe_envelope(&event.event_type, &event.timestamp);

        match event.event_type.as_str() {
            "session.start" => {
                if let Ok(data) = serde_json::from_value::<CopilotSessionStartData>(event.data) {
                    if self.state.task_id.is_empty() && !data.session_id.is_empty() {
                        self.state.task_id = data.session_id;
                    }
                    if let Some(ctx) = data.context {
                        if self.state.folder_path.is_empty() {
                            if !ctx.cwd.is_empty() {
                                self.state.folder_path = ctx.cwd;
                            } else if !ctx.git_root.is_empty() {
                                self.state.folder_path = ctx.git_root;
                            }
                        }
                        if self.state.git_remote.is_empty() {
                            self.state.git_remote =
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
                    self.current_model = canonicalize_model_name(new_model);
                }
            }
            "session.shutdown" => {
                let payload_supported = shutdown_payload_supported(&event.data);
                if payload_supported
                    && let Ok(data) =
                        serde_json::from_value::<CopilotShutdownData>(event.data.clone())
                {
                    let raw_metrics = event.data.get("modelMetrics").and_then(Value::as_object);
                    let metrics = data
                        .model_metrics
                        .into_iter()
                        .map(|(model, metric)| {
                            let token_details_input = raw_metrics
                                .and_then(|map| map.get(&model))
                                .and_then(|entry| entry.get("tokenDetails"))
                                .and_then(|details| details.get("input"))
                                .and_then(|input| input.get("tokenCount"))
                                .and_then(Value::as_i64);
                            (
                                model,
                                CopilotShutdownMetric {
                                    usage: metric.usage,
                                    token_details_input,
                                },
                            )
                        })
                        .collect();
                    self.apply_shutdown_metrics(metrics);
                } else {
                    self.diagnostics.record_relevant(false);
                }
            }
            "assistant.message" => {
                // Only used as a fallback when no `session.shutdown` arrives.
                if let Some(output_tokens) = event.data.get("outputTokens") {
                    let output_tokens = output_tokens.as_i64();
                    self.diagnostics
                        .record_relevant(output_tokens.is_some() && !self.current_model.is_empty());
                    if let Some(output_tokens) = output_tokens.filter(|&t| t > 0)
                        && !self.current_model.is_empty()
                    {
                        if let Some(total) = self.pending_output_tokens.get_mut(&self.current_model)
                        {
                            *total += output_tokens;
                        } else {
                            self.pending_output_tokens
                                .insert(self.current_model.clone(), output_tokens);
                        }
                    }
                }
            }
            "tool.execution_start" => {
                match serde_json::from_value::<CopilotToolStartData>(event.data) {
                    Ok(data) if !data.tool_call_id.is_empty() && !data.tool_name.is_empty() => {
                        let tool = TrackedTool::from_name(&data.tool_name);
                        let payload = if tool.is_some() {
                            PendingToolPayload::Owned(data.arguments)
                        } else {
                            PendingToolPayload::Untracked
                        };
                        self.pending_tools.insert(
                            data.tool_call_id,
                            PendingTool {
                                tool,
                                payload,
                                timestamp: ts,
                            },
                        );
                    }
                    Ok(data) => {
                        self.diagnostics.record_relevant(false);
                        // Keep the `toolCallId` registered so its completion pairs cleanly instead of double-counting the drift as an orphan.
                        if !data.tool_call_id.is_empty() {
                            self.pending_tools.insert(
                                data.tool_call_id,
                                PendingTool {
                                    tool: None,
                                    payload: PendingToolPayload::Untracked,
                                    timestamp: ts,
                                },
                            );
                        }
                    }
                    Err(_) => self.diagnostics.record_relevant(false),
                }
            }
            "tool.execution_complete" => {
                let Some(tool_call_id) = event
                    .data
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                else {
                    self.diagnostics.record_relevant(false);
                    return;
                };
                let success_opt = event.data.get("success").and_then(Value::as_bool);
                let Some(pending) = self.pending_tools.remove(tool_call_id) else {
                    // Only successful orphans count as drift because failed tool calls are ignored even when paired.
                    if success_opt == Some(true) {
                        self.diagnostics.record_relevant(false);
                    }
                    return;
                };
                let Some(success) = success_opt else {
                    if pending.tool.is_some() {
                        self.diagnostics.record_relevant(false);
                    }
                    return;
                };
                // Failed tool calls do not apply file operations; count them as valid lifecycle records without dispatching.
                if !success {
                    if pending.tool.is_some() {
                        self.diagnostics.record_relevant(true);
                    }
                    return;
                }
                let data = match serde_json::from_value::<CopilotToolCompleteData>(event.data) {
                    Ok(data) => data,
                    Err(_) => {
                        if pending.tool.is_some() {
                            self.diagnostics.record_relevant(false);
                        }
                        return;
                    }
                };
                let PendingToolPayload::Owned(arguments) = &pending.payload else {
                    return;
                };
                let folded = pending
                    .tool
                    .and_then(|tool| tool.read_args(arguments, &data.result));
                if pending.tool.is_some() {
                    self.diagnostics
                        .record_relevant(folded.as_ref().is_some_and(ToolArgs::is_complete));
                }
                if let Some(folded) = folded {
                    folded.record(&mut self.state, pending.timestamp);
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> ParsedAnalysis {
        if !self.shutdown_seen {
            for (model, output_tokens) in self.pending_output_tokens {
                self.conversation_usage.insert(
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

        if self.state.git_remote.is_empty() {
            self.state.git_remote = get_git_remote_url(&self.state.folder_path);
        }

        let record = self.state.into_record(self.conversation_usage);

        let analysis = CodeAnalysis {
            user: String::new(),
            extension_name: String::from("Copilot-CLI"),
            insights_version: String::new(),
            machine_id: String::new(),
            records: vec![record],
        };
        ParsedAnalysis::new(analysis, self.diagnostics)
    }
}

fn deserialize_present_raw<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<&'de RawValue>, D::Error>
where
    D: Deserializer<'de>,
{
    <&'de RawValue>::deserialize(deserializer).map(Some)
}

fn raw_as_str<'a>(raw: Option<&'a RawValue>) -> Option<Cow<'a, str>> {
    let raw = raw?;
    if !raw.get().starts_with('"') {
        return None;
    }
    serde_json::from_str::<Cow<'a, str>>(raw.get()).ok()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedCopilotEvent<'a> {
    #[serde(rename = "type", default, borrow)]
    event_type: Cow<'a, str>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    data: Option<&'a RawValue>,
    #[serde(default, borrow, rename = "id")]
    _id: Cow<'a, str>,
    #[serde(default, borrow)]
    timestamp: Cow<'a, str>,
    #[serde(default, borrow, rename = "parentId")]
    _parent_id: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedSessionStartData<'a> {
    #[serde(default, borrow)]
    session_id: Cow<'a, str>,
    #[serde(default, borrow)]
    context: Option<BorrowedSessionContext<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedSessionContext<'a> {
    #[serde(default, borrow)]
    cwd: Cow<'a, str>,
    #[serde(default, borrow)]
    git_root: Cow<'a, str>,
    #[serde(default, borrow)]
    repository: Cow<'a, str>,
    #[serde(default, borrow)]
    repository_host: Cow<'a, str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedModelChangeData<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    new_model: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedAssistantMessageData<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    output_tokens: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedToolStartData<'a> {
    #[serde(default, borrow)]
    tool_call_id: Cow<'a, str>,
    #[serde(default, borrow)]
    tool_name: Cow<'a, str>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    arguments: Option<&'a RawValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedToolCompleteData<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    tool_call_id: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    success: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    result: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    model: Option<&'a RawValue>,
}

struct ValidatedShutdownData {
    model_metrics: BTreeMap<String, CopilotShutdownMetric>,
}

impl<'de> Deserialize<'de> for ValidatedShutdownData {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ValidatedShutdownData;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Copilot session.shutdown data object")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut metrics: Option<BTreeMap<String, CopilotShutdownMetric>> = None;
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    if key == "modelMetrics" {
                        metrics = Some(map.next_value::<ValidatedModelMetricsMap>()?.0);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                let model_metrics =
                    metrics.ok_or_else(|| A::Error::custom("missing modelMetrics"))?;
                Ok(ValidatedShutdownData { model_metrics })
            }
        }
        deserializer.deserialize_map(V)
    }
}

struct ValidatedModelMetricsMap(BTreeMap<String, CopilotShutdownMetric>);

impl<'de> Deserialize<'de> for ValidatedModelMetricsMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ValidatedModelMetricsMap;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Copilot modelMetrics map")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut out = BTreeMap::new();
                while let Some(model) = map.next_key::<String>()? {
                    let metric = map.next_value::<ValidatedMetricEntry>()?;
                    out.insert(model, metric.0);
                }
                Ok(ValidatedModelMetricsMap(out))
            }
        }
        deserializer.deserialize_map(V)
    }
}

struct ValidatedMetricEntry(CopilotShutdownMetric);

impl<'de> Deserialize<'de> for ValidatedMetricEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ValidatedMetricEntry;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Copilot model metric object")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut usage: Option<CopilotModelUsage> = None;
                let mut token_details_input: Option<i64> = None;
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    if key == "usage" {
                        usage = map
                            .next_value::<Option<ValidatedCopilotUsage>>()?
                            .map(|u| u.0);
                    } else if key == "tokenDetails" {
                        let raw = map.next_value::<&'de RawValue>()?;
                        token_details_input = parse_token_details_input(raw.get());
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(ValidatedMetricEntry(CopilotShutdownMetric {
                    usage,
                    token_details_input,
                }))
            }
        }
        deserializer.deserialize_map(V)
    }
}

#[derive(Deserialize)]
struct BorrowedTokenDetails<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    input: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedTokenDetailBucket<'a> {
    #[serde(
        default,
        borrow,
        rename = "tokenCount",
        deserialize_with = "deserialize_present_raw"
    )]
    token_count: Option<&'a RawValue>,
}

fn parse_token_details_input(raw: &str) -> Option<i64> {
    let details = serde_json::from_str::<BorrowedTokenDetails<'_>>(raw).ok()?;
    let input_raw = details.input?;
    let bucket = serde_json::from_str::<BorrowedTokenDetailBucket<'_>>(input_raw.get()).ok()?;
    let count_raw = bucket.token_count?;
    serde_json::from_str::<i64>(count_raw.get()).ok()
}

struct ValidatedCopilotUsage(CopilotModelUsage);

impl<'de> Deserialize<'de> for ValidatedCopilotUsage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ValidatedCopilotUsage;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Copilot usage object")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut usage = CopilotModelUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                };
                let mut field_count = 0_usize;
                let mut recognized = false;

                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    field_count += 1;
                    match key.as_ref() {
                        "inputTokens" => {
                            usage.input_tokens = map.next_value::<i64>()?;
                            recognized = true;
                        }
                        "outputTokens" => {
                            usage.output_tokens = map.next_value::<i64>()?;
                            recognized = true;
                        }
                        "cacheReadTokens" => {
                            usage.cache_read_tokens = map.next_value::<i64>()?;
                            recognized = true;
                        }
                        "cacheWriteTokens" => {
                            usage.cache_write_tokens = map.next_value::<i64>()?;
                            recognized = true;
                        }
                        "reasoningTokens" => {
                            usage.reasoning_tokens = map.next_value::<i64>()?;
                            recognized = true;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }

                if field_count > 0 && !recognized {
                    return Err(A::Error::custom("unrecognized Copilot usage keys"));
                }
                Ok(ValidatedCopilotUsage(usage))
            }
        }
        deserializer.deserialize_map(V)
    }
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

#[derive(Default, Deserialize)]
struct BorrowedCopilotToolArgs<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    path: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    view_range: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    file_text: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    content: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    old_string: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    old_str: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    old_text: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    new_string: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    new_str: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    new_text: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    input: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    patch: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "patchText",
        deserialize_with = "deserialize_present_raw"
    )]
    patch_text: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    string: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    command: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    cmd: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    description: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedCopilotReadResult<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    content: Option<&'a RawValue>,
}

#[derive(Clone, Copy)]
enum ViewRangeState {
    Missing,
    Valid(usize),
    Invalid,
}

fn parse_raw_view_range(raw: Option<&RawValue>) -> ViewRangeState {
    struct RangeVisitor;
    impl<'de> Visitor<'de> for RangeVisitor {
        type Value = ViewRangeState;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a Copilot view_range array")
        }

        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let Some(first) = seq.next_element::<&'de RawValue>()? else {
                return Ok(ViewRangeState::Missing);
            };
            let Some(second) = seq.next_element::<&'de RawValue>()? else {
                return Ok(ViewRangeState::Missing);
            };
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            let (Ok(start), Ok(end)) = (
                serde_json::from_str::<i64>(first.get()),
                serde_json::from_str::<i64>(second.get()),
            ) else {
                return Ok(ViewRangeState::Invalid);
            };
            Ok(ViewRangeState::Valid((end - start + 1).max(0) as usize))
        }
    }

    let Some(raw) = raw else {
        return ViewRangeState::Missing;
    };
    if !raw.get().starts_with('[') {
        return ViewRangeState::Missing;
    }
    let mut de = serde_json::Deserializer::from_str(raw.get());
    de.deserialize_seq(RangeVisitor)
        .unwrap_or(ViewRangeState::Missing)
}

enum PendingToolPayload {
    Untracked,
    Invalid,
    Owned(Value),
    Read {
        path: String,
        view_range: ViewRangeState,
    },
    Search,
    WriteFull {
        path: String,
        content: String,
    },
    WriteCounts {
        path: String,
        metrics: TextMetrics,
    },
    EditFull {
        path: String,
        old_string: String,
        new_string: String,
    },
    EditCounts {
        path: String,
        old_is_empty: bool,
        new_metrics: TextMetrics,
    },
    ApplyPatch {
        patches: Vec<CopilotPatch>,
    },
    ShellFull {
        command: String,
        description: String,
    },
    ShellCount,
}

fn first_raw_choice<'a>(choices: &[Option<&'a RawValue>]) -> Option<&'a RawValue> {
    choices.iter().copied().flatten().next()
}

fn parse_borrowed_start_args(
    tool: TrackedTool,
    args_raw: Option<&RawValue>,
    mode: ParseMode,
) -> PendingToolPayload {
    if matches!(tool, TrackedTool::Search) {
        return PendingToolPayload::Search;
    }
    let Some(args_raw) = args_raw else {
        return PendingToolPayload::Invalid;
    };
    let usage_only = matches!(mode, ParseMode::UsageOnly);

    if matches!(tool, TrackedTool::ApplyPatch) {
        let patch_text = if args_raw.get().starts_with('"') {
            raw_as_str(Some(args_raw))
        } else if args_raw.get().starts_with('{') {
            serde_json::from_str::<BorrowedCopilotToolArgs<'_>>(args_raw.get())
                .ok()
                .and_then(|args| {
                    raw_as_str(args.input)
                        .or_else(|| raw_as_str(args.patch))
                        .or_else(|| raw_as_str(args.patch_text))
                        .or_else(|| raw_as_str(args.string))
                })
        } else {
            None
        };
        let Some(patch_text) = patch_text else {
            return PendingToolPayload::Invalid;
        };
        let patches = parse_apply_patch_text(&patch_text);
        if patches.iter().all(|patch| patch.file_path.is_empty()) {
            return PendingToolPayload::Invalid;
        }
        return PendingToolPayload::ApplyPatch { patches };
    }

    if !args_raw.get().starts_with('{') {
        return PendingToolPayload::Invalid;
    }
    let Ok(args) = serde_json::from_str::<BorrowedCopilotToolArgs<'_>>(args_raw.get()) else {
        return PendingToolPayload::Invalid;
    };

    match tool {
        TrackedTool::Read => {
            let Some(path) = raw_as_str(args.path).filter(|p| !p.is_empty()) else {
                return PendingToolPayload::Invalid;
            };
            PendingToolPayload::Read {
                path: path.into_owned(),
                view_range: parse_raw_view_range(args.view_range),
            }
        }
        TrackedTool::Write => {
            let Some(path) = raw_as_str(args.path).filter(|p| !p.is_empty()) else {
                return PendingToolPayload::Invalid;
            };
            let Some(body_raw) = first_raw_choice(&[args.file_text, args.content]) else {
                return PendingToolPayload::Invalid;
            };
            if usage_only {
                let Some(metrics) = count_json_string_metrics(body_raw.get()) else {
                    return PendingToolPayload::Invalid;
                };
                PendingToolPayload::WriteCounts {
                    path: path.into_owned(),
                    metrics,
                }
            } else {
                let Some(content) = raw_as_str(Some(body_raw)) else {
                    return PendingToolPayload::Invalid;
                };
                PendingToolPayload::WriteFull {
                    path: path.into_owned(),
                    content: content.into_owned(),
                }
            }
        }
        TrackedTool::Edit => {
            let Some(path) = raw_as_str(args.path).filter(|p| !p.is_empty()) else {
                return PendingToolPayload::Invalid;
            };
            let Some(old_raw) = first_raw_choice(&[args.old_string, args.old_str, args.old_text])
            else {
                return PendingToolPayload::Invalid;
            };
            let Some(new_raw) = first_raw_choice(&[args.new_string, args.new_str, args.new_text])
            else {
                return PendingToolPayload::Invalid;
            };
            if usage_only {
                let Some(old_metrics) = count_json_string_metrics(old_raw.get()) else {
                    return PendingToolPayload::Invalid;
                };
                let Some(new_metrics) = count_json_string_metrics(new_raw.get()) else {
                    return PendingToolPayload::Invalid;
                };
                PendingToolPayload::EditCounts {
                    path: path.into_owned(),
                    old_is_empty: old_metrics.is_empty(),
                    new_metrics,
                }
            } else {
                let Some(old_string) = raw_as_str(Some(old_raw)) else {
                    return PendingToolPayload::Invalid;
                };
                let Some(new_string) = raw_as_str(Some(new_raw)) else {
                    return PendingToolPayload::Invalid;
                };
                PendingToolPayload::EditFull {
                    path: path.into_owned(),
                    old_string: old_string.into_owned(),
                    new_string: new_string.into_owned(),
                }
            }
        }
        TrackedTool::Shell => {
            let Some(command) = raw_as_str(first_raw_choice(&[args.command, args.cmd]))
                .filter(|command| !command.trim().is_empty())
            else {
                return PendingToolPayload::Invalid;
            };
            if usage_only {
                PendingToolPayload::ShellCount
            } else {
                let description = raw_as_str(args.description).unwrap_or(Cow::Borrowed(""));
                PendingToolPayload::ShellFull {
                    command: command.into_owned(),
                    description: description.into_owned(),
                }
            }
        }
        TrackedTool::WriteBash => {
            let Some(input) = raw_as_str(args.input).filter(|input| !input.trim().is_empty())
            else {
                return PendingToolPayload::Invalid;
            };
            if usage_only {
                PendingToolPayload::ShellCount
            } else {
                PendingToolPayload::ShellFull {
                    command: input.into_owned(),
                    description: String::new(),
                }
            }
        }
        TrackedTool::Search | TrackedTool::ApplyPatch => unreachable!(),
    }
}

fn view_range_metrics(line_count: usize) -> TextMetrics {
    if line_count == 0 {
        TextMetrics::default()
    } else {
        // Matches `vec!["-"; line_count].join("\n")`: `line_count` dashes plus `line_count - 1` newlines.
        TextMetrics {
            lines: line_count,
            chars: line_count.saturating_mul(2).saturating_sub(1),
        }
    }
}

fn apply_borrowed_pending_tool(
    payload: PendingToolPayload,
    result_raw: Option<&RawValue>,
    state: &mut SessionParseState,
    ts: i64,
) -> bool {
    match payload {
        PendingToolPayload::Untracked | PendingToolPayload::Owned(_) => true,
        PendingToolPayload::Invalid => false,
        PendingToolPayload::Read { path, view_range } => {
            state.tool_counts.read += 1;
            let usage_only = matches!(state.mode, ParseMode::UsageOnly);
            match view_range {
                ViewRangeState::Invalid => false,
                ViewRangeState::Valid(line_count) => {
                    if usage_only {
                        let metrics = view_range_metrics(line_count);
                        attach_read_counts(state, &path, metrics.lines, metrics.chars);
                    } else if line_count > 0 {
                        let placeholder = vec!["-"; line_count].join("\n");
                        attach_read_detail(state, &path, &placeholder, ts);
                    }
                    true
                }
                ViewRangeState::Missing => {
                    let content_raw = result_raw
                        .filter(|raw| raw.get().starts_with('{'))
                        .and_then(|raw| {
                            serde_json::from_str::<BorrowedCopilotReadResult<'_>>(raw.get()).ok()
                        })
                        .and_then(|res| res.content);
                    let Some(content_raw) = content_raw else {
                        return false;
                    };
                    if usage_only {
                        let Some(metrics) = count_json_string_metrics(content_raw.get()) else {
                            return false;
                        };
                        attach_read_counts(state, &path, metrics.lines, metrics.chars);
                    } else {
                        let Some(content) = raw_as_str(Some(content_raw)) else {
                            return false;
                        };
                        attach_read_detail(state, &path, &content, ts);
                    }
                    true
                }
            }
        }
        PendingToolPayload::Search => {
            state.tool_counts.read += 1;
            true
        }
        PendingToolPayload::WriteFull { path, content } => {
            state.add_write_detail(&path, &content, ts);
            true
        }
        PendingToolPayload::WriteCounts { path, metrics } => {
            state.add_write_counts(&path, metrics.lines, metrics.chars);
            true
        }
        PendingToolPayload::EditFull {
            path,
            old_string,
            new_string,
        } => {
            state.add_edit_detail(&path, &old_string, &new_string, ts);
            true
        }
        PendingToolPayload::EditCounts {
            path,
            old_is_empty,
            new_metrics,
        } => {
            state.add_edit_counts(&path, old_is_empty, new_metrics.lines, new_metrics.chars);
            true
        }
        PendingToolPayload::ApplyPatch { patches } => {
            record_patches(state, patches, ts);
            true
        }
        PendingToolPayload::ShellFull {
            command,
            description,
        } => {
            state.add_run_command(&command, &description, ts);
            true
        }
        PendingToolPayload::ShellCount => {
            state.tool_counts.bash += 1;
            true
        }
    }
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
    fn read_args<'a>(self, args: &'a Value, result: &'a Value) -> Option<ToolArgs<'a>> {
        Some(match self {
            Self::Read => ToolArgs::Read {
                path: arg_path(args)?,
                content: read_view_content_ref(args, result),
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

enum ViewContent<'a> {
    Range(usize),
    Text(&'a str),
}

/// Validated payload of a successful [`TrackedTool`] call.
enum ToolArgs<'a> {
    /// `content` is `None` when the completion body is unreadable; the read invocation still counts, but records schema drift.
    Read {
        path: &'a str,
        content: Option<ViewContent<'a>>,
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
                match content {
                    Some(ViewContent::Range(line_count)) => {
                        if matches!(state.mode, ParseMode::UsageOnly) {
                            let metrics = view_range_metrics(line_count);
                            attach_read_counts(state, path, metrics.lines, metrics.chars);
                        } else if line_count > 0 {
                            let placeholder = vec!["-"; line_count].join("\n");
                            attach_read_detail(state, path, &placeholder, ts);
                        }
                    }
                    Some(ViewContent::Text(text)) => {
                        attach_read_detail(state, path, text, ts);
                    }
                    None => {}
                }
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
    /// Extracted or raw tool arguments from `tool.execution_start`.
    payload: PendingToolPayload,
    /// Start-event timestamp in epoch milliseconds.
    timestamp: i64,
}

/// Attaches read line/file details without incrementing `tool_counts.read` a second time.
fn attach_read_detail(state: &mut SessionParseState, path: &str, content: &str, ts: i64) {
    let invocation_count = state.tool_counts.read;
    state.add_read_detail(path, content, ts);
    state.tool_counts.read = invocation_count;
}

fn attach_read_counts(
    state: &mut SessionParseState,
    path: &str,
    line_count: usize,
    char_count: usize,
) {
    let invocation_count = state.tool_counts.read;
    state.add_read_counts(path, line_count, char_count);
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

/// Returns the read body or `view_range` line count for a `view`-family call, or `None` if neither is readable.
fn read_view_content_ref<'a>(arguments: &'a Value, result: &'a Value) -> Option<ViewContent<'a>> {
    if let Some(range) = arguments
        .get("view_range")
        .and_then(|v| v.as_array())
        .filter(|range| range.len() >= 2)
    {
        let start = range.first()?.as_i64()?;
        let end = range.get(1)?.as_i64()?;
        let line_count = (end - start + 1).max(0) as usize;
        return Some(ViewContent::Range(line_count));
    }

    Some(ViewContent::Text(result.get("content")?.as_str()?))
}

/// Returns the read body for a `view`-family call from `arguments.view_range` (synthesized line placeholder) or `result.content`, or `None` if neither is readable.
#[cfg(test)]
fn read_view_content(arguments: &Value, result: &Value) -> Option<String> {
    match read_view_content_ref(arguments, result)? {
        ViewContent::Range(0) => Some(String::new()),
        ViewContent::Range(line_count) => Some(vec!["-"; line_count].join("\n")),
        ViewContent::Text(text) => Some(text.to_string()),
    }
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
    fn read<'a>(tool_name: &str, arguments: &'a Value, result: &'a Value) -> Option<ToolArgs<'a>> {
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

    #[test]
    fn borrowed_jsonl_stream_matches_owned_parser_across_view_range_and_cjk() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("events.jsonl");
        let events = vec![
            event(
                "session.start",
                json!({
                    "sessionId": "cop-1",
                    "context": {
                        "cwd": "/workspace",
                        "repositoryHost": "github.com",
                        "repository": "owner/repo"
                    }
                }),
            ),
            event(
                "tool.execution_start",
                json!({
                    "toolCallId": "v1",
                    "toolName": "view",
                    "arguments": { "path": "/workspace/a.rs", "view_range": [10, 14] }
                }),
            ),
            event(
                "tool.execution_complete",
                json!({ "toolCallId": "v1", "success": true, "result": {} }),
            ),
            event(
                "tool.execution_start",
                json!({
                    "toolCallId": "v2",
                    "toolName": "view",
                    "arguments": { "path": "/workspace/zh.md" }
                }),
            ),
            event(
                "tool.execution_complete",
                json!({
                    "toolCallId": "v2",
                    "success": true,
                    "result": { "content": "繁體中文\n第二行🚀\n" }
                }),
            ),
            event(
                "tool.execution_start",
                json!({
                    "toolCallId": "w1",
                    "toolName": "create",
                    "arguments": { "path": "/workspace/new.md", "file_text": "寫入\n多行😀\n\n" }
                }),
            ),
            event(
                "tool.execution_complete",
                json!({ "toolCallId": "w1", "success": true, "result": {} }),
            ),
            event(
                "tool.execution_start",
                json!({
                    "toolCallId": "e1",
                    "toolName": "str_replace",
                    "arguments": { "path": "/workspace/new.md", "old_string": "", "new_string": "重分類\r\n寫入" }
                }),
            ),
            event(
                "tool.execution_complete",
                json!({ "toolCallId": "e1", "success": true, "result": {} }),
            ),
            event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "claude-sonnet-4.6": {
                            "usage": {
                                "inputTokens": 1000,
                                "outputTokens": 353,
                                "reasoningTokens": 341,
                                "cacheReadTokens": 200,
                                "cacheWriteTokens": 50
                            }
                        }
                    }
                }),
            ),
        ];

        let body: String = events
            .iter()
            .map(|ev| format!("{}\n", serde_json::to_string(ev).unwrap()))
            .collect();
        std::fs::write(&path, body).unwrap();

        let owned = parse_copilot_events_with_diagnostics(events, ParseMode::Full).unwrap();
        let streamed_full = super::parse_copilot_jsonl_path(&path, ParseMode::Full)
            .unwrap()
            .unwrap();
        let streamed_usage = super::parse_copilot_jsonl_path(&path, ParseMode::UsageOnly)
            .unwrap()
            .unwrap();

        assert_eq!(streamed_full.diagnostics, owned.diagnostics);
        assert_eq!(streamed_usage.diagnostics, owned.diagnostics);
        let o = &owned.analysis.records[0];
        let sf = &streamed_full.analysis.records[0];
        let su = &streamed_usage.analysis.records[0];
        assert_eq!(
            serde_json::to_value(sf).unwrap(),
            serde_json::to_value(o).unwrap()
        );
        assert_eq!(su.total_unique_files, o.total_unique_files);
        assert_eq!(su.total_read_lines, o.total_read_lines);
        assert_eq!(su.total_read_characters, o.total_read_characters);
        assert_eq!(su.total_write_lines, o.total_write_lines);
        assert_eq!(su.total_write_characters, o.total_write_characters);
        assert_eq!(su.total_edit_lines, o.total_edit_lines);
        assert_eq!(su.total_edit_characters, o.total_edit_characters);
        assert_eq!(su.conversation_usage, o.conversation_usage);
        assert_eq!(
            o.conversation_usage["claude-sonnet-4-6"]["input_tokens"],
            750
        );
    }

    #[test]
    fn shutdown_subtracts_cache_tokens_and_prefers_token_details_input() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("events.jsonl");
        let events = vec![event(
            "session.shutdown",
            json!({
                "modelMetrics": {
                    "claude-sonnet-5": {
                        "usage": {
                            "inputTokens": 1_860_010,
                            "outputTokens": 11_182,
                            "cacheReadTokens": 1_748_384,
                            "cacheWriteTokens": 111_513,
                            "reasoningTokens": 6_275
                        },
                        "tokenDetails": {
                            "input": { "tokenCount": 113 },
                            "cache_read": { "tokenCount": 1_748_384 },
                            "cache_write": { "tokenCount": 111_513 },
                            "output": { "tokenCount": 11_182 }
                        }
                    },
                    "claude-sonnet-4.6": {
                        "usage": {
                            "inputTokens": 186_809,
                            "outputTokens": 2_848,
                            "cacheReadTokens": 151_738,
                            "cacheWriteTokens": 0,
                            "reasoningTokens": 1_024
                        }
                    },
                    "clamped-model": {
                        "usage": {
                            "inputTokens": 100,
                            "outputTokens": 20,
                            "cacheReadTokens": 120,
                            "cacheWriteTokens": 30,
                            "reasoningTokens": 50
                        }
                    }
                }
            }),
        )];

        let body: String = events
            .iter()
            .map(|ev| format!("{}\n", serde_json::to_string(ev).unwrap()))
            .collect();
        std::fs::write(&path, body).unwrap();

        let owned = parse_copilot_events_with_diagnostics(events, ParseMode::Full).unwrap();
        let streamed = super::parse_copilot_jsonl_path(&path, ParseMode::Full)
            .unwrap()
            .unwrap();
        assert_eq!(owned.diagnostics.partial_failure_count(), 0);
        assert_eq!(streamed.diagnostics, owned.diagnostics);
        assert_eq!(
            streamed.analysis.records[0].conversation_usage,
            owned.analysis.records[0].conversation_usage
        );

        let usage = &owned.analysis.records[0].conversation_usage;
        assert_eq!(usage["claude-sonnet-5"]["input_tokens"], 113);
        assert_eq!(
            usage["claude-sonnet-5"]["cache_read_input_tokens"],
            1_748_384
        );
        assert_eq!(
            usage["claude-sonnet-5"]["cache_creation_input_tokens"],
            111_513
        );
        assert_eq!(usage["claude-sonnet-5"]["output_tokens"], 11_182 - 6_275);
        assert_eq!(usage["claude-sonnet-5"]["reasoning_output_tokens"], 6_275);

        assert_eq!(
            usage["claude-sonnet-4-6"]["input_tokens"],
            186_809 - 151_738
        );
        assert_eq!(usage["clamped-model"]["input_tokens"], 0);
        assert_eq!(usage["clamped-model"]["output_tokens"], 0);
    }

    #[test]
    fn multi_shutdown_accumulates_resets_and_preserves_cumulative_totals() {
        let dir = tempfile::TempDir::new().unwrap();

        // Older Copilot resets counters across `session.resume`.
        let reset_events = vec![
            event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "claude-sonnet-4.6": {
                            "usage": {
                                "inputTokens": 754_148,
                                "outputTokens": 5_220,
                                "cacheReadTokens": 694_725,
                                "cacheWriteTokens": 0,
                                "reasoningTokens": 0
                            }
                        }
                    }
                }),
            ),
            event("session.resume", json!({})),
            event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "claude-sonnet-4.6": {
                            "usage": {
                                "inputTokens": 470_648,
                                "outputTokens": 2_801,
                                "cacheReadTokens": 452_507,
                                "cacheWriteTokens": 0,
                                "reasoningTokens": 0
                            }
                        }
                    }
                }),
            ),
        ];
        let reset_path = dir.path().join("reset.jsonl");
        let reset_body: String = reset_events
            .iter()
            .map(|ev| format!("{}\n", serde_json::to_string(ev).unwrap()))
            .collect();
        std::fs::write(&reset_path, reset_body).unwrap();

        let owned_reset =
            parse_copilot_events_with_diagnostics(reset_events, ParseMode::Full).unwrap();
        let streamed_reset = super::parse_copilot_jsonl_path(&reset_path, ParseMode::Full)
            .unwrap()
            .unwrap();
        assert_eq!(
            streamed_reset.analysis.records[0].conversation_usage,
            owned_reset.analysis.records[0].conversation_usage
        );
        let reset_usage = &owned_reset.analysis.records[0].conversation_usage["claude-sonnet-4-6"];
        assert_eq!(
            reset_usage["input_tokens"],
            (754_148 - 694_725) + (470_648 - 452_507)
        );
        assert_eq!(reset_usage["output_tokens"], 5_220 + 2_801);
        assert_eq!(reset_usage["cache_read_input_tokens"], 694_725 + 452_507);

        // Newer Copilot reports cumulative totals across `session.resume` (including no-op resumes).
        let cumulative_events = vec![
            event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "claude-sonnet-5": {
                            "usage": {
                                "inputTokens": 1_860_010,
                                "outputTokens": 11_182,
                                "cacheReadTokens": 1_748_384,
                                "cacheWriteTokens": 111_513,
                                "reasoningTokens": 6_275
                            },
                            "tokenDetails": { "input": { "tokenCount": 113 } }
                        }
                    }
                }),
            ),
            event("session.resume", json!({})),
            event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "claude-sonnet-5": {
                            "usage": {
                                "inputTokens": 2_339_869,
                                "outputTokens": 13_502,
                                "cacheReadTokens": 2_151_104,
                                "cacheWriteTokens": 188_642,
                                "reasoningTokens": 7_379
                            },
                            "tokenDetails": { "input": { "tokenCount": 123 } }
                        }
                    }
                }),
            ),
            event("session.resume", json!({})),
            event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "claude-sonnet-5": {
                            "usage": {
                                "inputTokens": 2_339_869,
                                "outputTokens": 13_502,
                                "cacheReadTokens": 2_151_104,
                                "cacheWriteTokens": 188_642,
                                "reasoningTokens": 7_379
                            },
                            "tokenDetails": { "input": { "tokenCount": 123 } }
                        }
                    }
                }),
            ),
        ];
        let owned_cum =
            parse_copilot_events_with_diagnostics(cumulative_events, ParseMode::Full).unwrap();
        let cum_usage = &owned_cum.analysis.records[0].conversation_usage["claude-sonnet-5"];
        assert_eq!(cum_usage["input_tokens"], 123);
        assert_eq!(cum_usage["output_tokens"], 13_502 - 7_379);
        assert_eq!(cum_usage["reasoning_output_tokens"], 7_379);
        assert_eq!(cum_usage["cache_read_input_tokens"], 2_151_104);
        assert_eq!(cum_usage["cache_creation_input_tokens"], 188_642);
    }

    #[test]
    fn per_run_reset_propagates_to_sibling_models_and_subsequent_shutdowns() {
        let events = vec![
            // Run 1: sonnet-4.6 = 1000 in / 100 out, gpt-5.4 = 200 in / 20 out.
            event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "claude-sonnet-4.6": { "usage": { "inputTokens": 1_000, "outputTokens": 100 } },
                        "gpt-5.4": { "usage": { "inputTokens": 200, "outputTokens": 20 } }
                    }
                }),
            ),
            event("session.resume", json!({})),
            // Run 2: sonnet-4.6 drops (proves per-run reset), while gpt-5.4 happens to be larger than run 1.
            event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "claude-sonnet-4.6": { "usage": { "inputTokens": 400, "outputTokens": 40 } },
                        "gpt-5.4": { "usage": { "inputTokens": 300, "outputTokens": 30 } }
                    }
                }),
            ),
            event("session.resume", json!({})),
            // Run 3: sonnet-4.6 is larger than run 2, and must still add in full rather than subtracting run 2.
            event(
                "session.shutdown",
                json!({
                    "modelMetrics": {
                        "claude-sonnet-4.6": { "usage": { "inputTokens": 600, "outputTokens": 60 } }
                    }
                }),
            ),
        ];

        let parsed = parse_copilot_events_with_diagnostics(events, ParseMode::Full).unwrap();
        let usage = &parsed.analysis.records[0].conversation_usage;
        assert_eq!(usage["claude-sonnet-4-6"]["input_tokens"], 2_000);
        assert_eq!(usage["claude-sonnet-4-6"]["output_tokens"], 200);
        assert_eq!(usage["gpt-5.4"]["input_tokens"], 500);
        assert_eq!(usage["gpt-5.4"]["output_tokens"], 50);
    }

    #[test]
    fn lifecycle_checkpoint_and_permissions_events_are_recognized() {
        let events = vec![
            event("session.usage_checkpoint", json!({})),
            event("session.permissions_changed", json!({})),
        ];
        let parsed = parse_copilot_events_with_diagnostics(events, ParseMode::Full).unwrap();
        assert_eq!(parsed.diagnostics.recognized_records, 2);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert!(!parsed.diagnostics.is_complete_failure());
    }
}
