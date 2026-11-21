//! Parser for OpenAI Codex rollout logs (`~/.codex/**/*.jsonl`).
//!
//! File operations arrive via `function_call` pairs, `custom_tool_call` pairs, or `patch_apply_end` events, joined to their outputs by `call_id`. Shell calls undergo command-level read/patch extraction, while a top-level custom `exec` counts as a single run command. Direct custom `apply_patch` calls are counted only when their output reports success, and a successful `patch_apply_end` is folded in unless its `call_id` was already counted.
use crate::constants::{FastHashMap, FastHashSet};
use crate::models::*;
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::parser::stream_jsonl_borrowed;
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::token_merge::write_tier_slice;
use crate::utils::{CodexTokenTotals, TierSlice, get_git_remote_url, parse_iso_timestamp};
use anyhow::Result;
use serde::Deserialize;
use serde::de::Deserializer;
use serde_json::{Value, value::RawValue};
use std::borrow::{Borrow, Cow};
use std::path::Path;

/// Parses Codex session `logs` into a single-record [`CodeAnalysis`].
///
/// Runtime metadata fields (`user`, `extension_name`, `insights_version`, `machine_id`) are left blank for the caller to populate.
///
/// # Errors
///
/// Never returns `Err`; returns `Result` for parity with other provider parsers.
pub fn parse_codex_logs(logs: &[CodexLog], mode: ParseMode) -> Result<CodeAnalysis> {
    parse_codex_log_iter(logs, mode)
}

/// Parses Codex session `logs` from any iterator into a [`CodeAnalysis`].
pub(crate) fn parse_codex_log_iter<I>(logs: I, mode: ParseMode) -> Result<CodeAnalysis>
where
    I: IntoIterator,
    I::Item: Borrow<CodexLog>,
{
    Ok(parse_codex_log_iter_with_diagnostics(logs, mode, None)?.analysis)
}

/// Parses Codex session `logs` with schema diagnostics, classifying per-turn context tiers when `tiers` is `Some`.
pub(crate) fn parse_codex_log_iter_with_diagnostics<I>(
    logs: I,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<ParsedAnalysis>
where
    I: IntoIterator,
    I::Item: Borrow<CodexLog>,
{
    let mut fold = CodexFold::new(mode, tiers);
    for entry in logs {
        let entry = entry.borrow();
        let payload_type = entry.payload.payload_type.as_deref();
        let token_count = if entry.log_type == "event_msg" && payload_type == Some("token_count") {
            entry
                .payload
                .info
                .as_ref()
                .filter(|info| !info.is_null())
                .map(parse_token_count_value)
        } else {
            None
        };
        fold.observe(CodexRecordRef {
            log_type: &entry.log_type,
            timestamp: &entry.timestamp,
            payload_type,
            cwd: entry.payload.cwd.as_deref(),
            id: entry.payload.id.as_deref(),
            git_repository_url: entry
                .payload
                .git
                .as_ref()
                .and_then(|git| git.repository_url.as_deref()),
            model: entry.payload.model.as_deref(),
            token_count,
            success: entry.payload.success,
            changes: entry.payload.changes.as_ref(),
            name: entry.payload.name.as_deref(),
            arguments: entry.payload.arguments.as_deref(),
            call_id: entry.payload.call_id.as_deref(),
            output: entry.payload.output.as_deref(),
        });
    }
    Ok(fold.finish())
}

/// Streams and parses a Codex JSONL rollout file at `path` with schema diagnostics, classifying per-turn context tiers when `tiers` is `Some`.
pub(crate) fn parse_codex_jsonl_path(
    path: &Path,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<Option<ParsedAnalysis>> {
    stream_jsonl_borrowed(
        path,
        ExtensionType::Codex,
        CodexFold::new(mode, tiers),
        |fold, bytes| {
            let record = serde_json::from_slice::<BorrowedCodexLog<'_>>(bytes)?;
            let payload_type = record.payload.payload_type.as_deref();

            let token_count =
                if record.log_type == "event_msg" && payload_type == Some("token_count") {
                    record
                        .payload
                        .info
                        .filter(|raw| raw.get() != "null")
                        .map(|raw| parse_token_count_raw(raw.get()))
                } else {
                    None
                };

            let changes = if record.log_type == "event_msg"
                && payload_type == Some("patch_apply_end")
                && record.payload.success == Some(true)
                && !record
                    .payload
                    .call_id
                    .as_deref()
                    .is_some_and(|id| fold.patch_counted_ids.contains(id))
            {
                match record.payload.changes {
                    Some(raw) => Some(serde_json::from_str::<Value>(raw.get())?),
                    None => None,
                }
            } else {
                None
            };

            let needs_arguments = record.log_type == "response_item"
                && matches!(
                    (payload_type, record.payload.name.as_deref()),
                    (Some("function_call"), Some("shell" | "exec_command"))
                        | (Some("custom_tool_call"), Some("exec" | "apply_patch"))
                );
            let arguments: Option<Cow<'_, str>> = match record.payload.arguments {
                None => None,
                Some(raw) => {
                    let raw_str = raw.get();
                    let first = raw_str.as_bytes().first().copied();
                    if !needs_arguments
                        && (matches!(first, Some(b'"' | b'{' | b'[')) || raw_str == "null")
                    {
                        None
                    } else if first == Some(b'"') {
                        Some(serde_json::from_str::<Cow<'_, str>>(raw_str)?)
                    } else {
                        let mut de = serde_json::Deserializer::from_str(raw_str);
                        crate::models::codex::deserialize_tool_arguments(&mut de)?.map(Cow::Owned)
                    }
                }
            };

            let needs_output = record.log_type == "response_item"
                && match (payload_type, record.payload.call_id.as_deref()) {
                    (Some("function_call_output"), Some(id)) => fold
                        .shell_calls
                        .get(id)
                        .is_some_and(PendingCodexShellCall::needs_output),
                    (Some("custom_tool_call_output"), Some(id)) => fold
                        .custom_calls
                        .get(id)
                        .is_some_and(CodexCustomCall::needs_output),
                    _ => false,
                };
            let output: Option<Cow<'_, str>> = match record.payload.output {
                None => None,
                Some(raw) => {
                    let raw_str = raw.get();
                    let first = raw_str.as_bytes().first().copied();
                    if !needs_output && (matches!(first, Some(b'"' | b'{')) || raw_str == "null") {
                        None
                    } else if first == Some(b'"') {
                        Some(serde_json::from_str::<Cow<'_, str>>(raw_str)?)
                    } else {
                        let mut de = serde_json::Deserializer::from_str(raw_str);
                        crate::models::codex::deserialize_tool_output(&mut de)?.map(Cow::Owned)
                    }
                }
            };

            fold.observe(CodexRecordRef {
                log_type: &record.log_type,
                timestamp: &record.timestamp,
                payload_type,
                cwd: record.payload.cwd.as_deref(),
                id: record.payload.id.as_deref(),
                git_repository_url: record
                    .payload
                    .git
                    .as_ref()
                    .and_then(|git| git.repository_url.as_deref()),
                model: record.payload.model.as_deref(),
                token_count,
                success: record.payload.success,
                changes: changes.as_ref(),
                name: record.payload.name.as_deref(),
                arguments: arguments.as_deref(),
                call_id: record.payload.call_id.as_deref(),
                output: output.as_deref(),
            });
            Ok(())
        },
        |fold| Ok(fold.finish()),
    )
}

#[derive(Deserialize)]
struct BorrowedCodexLog<'a> {
    #[serde(borrow)]
    timestamp: Cow<'a, str>,
    #[serde(borrow, rename = "type")]
    log_type: Cow<'a, str>,
    #[serde(borrow)]
    payload: BorrowedCodexPayload<'a>,
}

#[derive(Deserialize)]
struct BorrowedCodexPayload<'a> {
    #[serde(default, borrow, rename = "type")]
    payload_type: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(default, borrow, alias = "input")]
    arguments: Option<&'a RawValue>,
    #[serde(default, borrow)]
    call_id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    output: Option<&'a RawValue>,
    #[serde(default, borrow)]
    info: Option<&'a RawValue>,
    #[serde(default, borrow)]
    cwd: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    model: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    git: Option<BorrowedCodexGitInfo<'a>>,
    #[serde(default)]
    success: Option<bool>,
    #[serde(default, borrow)]
    changes: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedCodexGitInfo<'a> {
    #[serde(default, borrow)]
    repository_url: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct BorrowedOutputField<'a> {
    #[serde(borrow)]
    output: Cow<'a, str>,
}

#[derive(Deserialize)]
struct BorrowedExecCommandArgs<'a> {
    #[serde(borrow)]
    cmd: Cow<'a, str>,
}

#[derive(Default, Clone, Copy)]
struct CodexTokenFields {
    input_tokens: Option<i64>,
    cached_input_tokens: Option<i64>,
    cache_write_input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    reasoning_output_tokens: Option<i64>,
    total_tokens: Option<i64>,
}

impl CodexTokenFields {
    fn to_totals(self, prev: Option<&CodexTokenTotals>) -> CodexTokenTotals {
        let carried = prev.copied().unwrap_or_default();
        CodexTokenTotals {
            input_tokens: self.input_tokens.unwrap_or(carried.input_tokens),
            cached_input_tokens: self
                .cached_input_tokens
                .unwrap_or(carried.cached_input_tokens),
            cache_write_input_tokens: self
                .cache_write_input_tokens
                .unwrap_or(carried.cache_write_input_tokens),
            output_tokens: self.output_tokens.unwrap_or(carried.output_tokens),
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .unwrap_or(carried.reasoning_output_tokens),
            total_tokens: self.total_tokens.unwrap_or(carried.total_tokens),
        }
    }
}

enum LastTokenUsage<'a> {
    RawJson(&'a str),
    Value(&'a Value),
}

#[derive(Default)]
enum StoredLastUsage {
    #[default]
    Empty,
    RawJson(String),
    Owned(Value),
}

enum ParsedTokenCount<'a> {
    Valid {
        total: Option<CodexTokenFields>,
        last_input_tokens: Option<i64>,
        last_usage: Option<LastTokenUsage<'a>>,
        model_context_window: Option<Option<i64>>,
    },
    Invalid,
}

#[derive(Default)]
struct CodexModelUsage {
    input_tokens: Option<i64>,
    cached_input_tokens: Option<i64>,
    cache_write_input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    reasoning_output_tokens: Option<i64>,
    total_tokens: Option<i64>,
    last_token_usage: StoredLastUsage,
    model_context_window: Option<Option<i64>>,
    above_tier: Vec<(usize, TierSlice)>,
}

impl CodexModelUsage {
    fn add_tier_slice(&mut self, level: usize, slice: TierSlice) {
        if let Some((_, existing)) = self.above_tier.iter_mut().find(|(lvl, _)| *lvl == level) {
            existing.input_tokens += slice.input_tokens;
            existing.output_tokens += slice.output_tokens;
            existing.reasoning_tokens += slice.reasoning_tokens;
            existing.cache_read += slice.cache_read;
            existing.cache_creation_5m += slice.cache_creation_5m;
            existing.cache_creation_1h += slice.cache_creation_1h;
        } else {
            self.above_tier.push((level, slice));
        }
    }

    fn set_last_usage(&mut self, last: LastTokenUsage<'_>) {
        match last {
            LastTokenUsage::RawJson(raw) => {
                if let StoredLastUsage::RawJson(buf) = &mut self.last_token_usage {
                    buf.clear();
                    buf.push_str(raw);
                } else {
                    self.last_token_usage = StoredLastUsage::RawJson(raw.to_string());
                }
            }
            LastTokenUsage::Value(value) => {
                self.last_token_usage = StoredLastUsage::Owned(value.clone());
            }
        }
    }

    fn into_value(self) -> Value {
        let mut total_map = serde_json::Map::with_capacity(6);
        if let Some(v) = self.input_tokens {
            total_map.insert("input_tokens".to_string(), v.into());
        }
        if let Some(v) = self.cached_input_tokens {
            total_map.insert("cached_input_tokens".to_string(), v.into());
        }
        if let Some(v) = self.cache_write_input_tokens {
            total_map.insert("cache_write_input_tokens".to_string(), v.into());
        }
        if let Some(v) = self.output_tokens {
            total_map.insert("output_tokens".to_string(), v.into());
        }
        if let Some(v) = self.reasoning_output_tokens {
            total_map.insert("reasoning_output_tokens".to_string(), v.into());
        }
        if let Some(v) = self.total_tokens {
            total_map.insert("total_tokens".to_string(), v.into());
        }

        let last_usage = match self.last_token_usage {
            StoredLastUsage::Empty => Value::Object(serde_json::Map::new()),
            StoredLastUsage::RawJson(raw) => {
                serde_json::from_str(&raw).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
            }
            StoredLastUsage::Owned(value) => value,
        };

        let ctx_window = match self.model_context_window {
            Some(Some(n)) => Value::from(n),
            _ => Value::Null,
        };

        let mut obj = serde_json::Map::with_capacity(4);
        obj.insert("total_token_usage".to_string(), Value::Object(total_map));
        obj.insert("last_token_usage".to_string(), last_usage);
        obj.insert("model_context_window".to_string(), ctx_window);

        if !self.above_tier.is_empty() {
            let mut above = serde_json::Map::with_capacity(self.above_tier.len() * 4);
            for (level, slice) in self.above_tier {
                write_tier_slice(&mut above, level, &slice);
            }
            if !above.is_empty() {
                obj.insert("above_tier".to_string(), Value::Object(above));
            }
        }

        Value::Object(obj)
    }
}

struct CodexRecordRef<'a> {
    log_type: &'a str,
    timestamp: &'a str,
    payload_type: Option<&'a str>,
    cwd: Option<&'a str>,
    id: Option<&'a str>,
    git_repository_url: Option<&'a str>,
    model: Option<&'a str>,
    token_count: Option<ParsedTokenCount<'a>>,
    success: Option<bool>,
    changes: Option<&'a Value>,
    name: Option<&'a str>,
    arguments: Option<&'a str>,
    call_id: Option<&'a str>,
    output: Option<&'a str>,
}

struct CodexFold<'a> {
    mode: ParseMode,
    classifier: Option<TierClassifier<'a>>,
    state: SessionParseState,
    model_usages: FastHashMap<String, CodexModelUsage>,
    current_model: String,
    // Codex emits cumulative session totals on each `token_count`; each model is billed only the delta since this snapshot (pre-context snapshots advance it without attribution).
    prev_totals: Option<CodexTokenTotals>,
    shell_calls: FastHashMap<String, PendingCodexShellCall>,
    custom_calls: FastHashMap<String, CodexCustomCall>,
    // Call ids already counted by either a confirmed direct `apply_patch` output or a successful `patch_apply_end`; first to count wins, and unconfirmed direct outputs never register here.
    patch_counted_ids: FastHashSet<String>,
    diagnostics: ParseDiagnostics,
}

impl<'a> CodexFold<'a> {
    fn new(mode: ParseMode, tiers: Option<&'a TierThresholds>) -> Self {
        Self {
            mode,
            classifier: tiers.map(TierClassifier::new),
            state: SessionParseState::with_mode(mode),
            model_usages: FastHashMap::with_capacity(5),
            current_model: String::new(),
            prev_totals: None,
            shell_calls: FastHashMap::with_capacity(50),
            custom_calls: FastHashMap::with_capacity(32),
            patch_counted_ids: FastHashSet::default(),
            diagnostics: ParseDiagnostics::default(),
        }
    }

    fn observe(&mut self, entry: CodexRecordRef<'_>) {
        let recognized = matches!(
            entry.log_type,
            "session_meta"
                | "turn_context"
                | "event_msg"
                | "response_item"
                | "inter_agent_communication_metadata"
                | "world_state"
                | "compacted"
        );
        if recognized {
            self.diagnostics.record_recognized_source();
        } else {
            self.diagnostics.record_unrecognized();
        }
        let ts = parse_iso_timestamp(entry.timestamp);
        if ts > self.state.last_ts {
            self.state.last_ts = ts;
        }

        match entry.log_type {
            "session_meta" => {
                if self.state.folder_path.is_empty()
                    && let Some(cwd) = entry.cwd
                {
                    self.state.folder_path.push_str(cwd);
                }
                if self.state.task_id.is_empty()
                    && let Some(id) = entry.id
                {
                    self.state.task_id.push_str(id);
                }
                if self.state.git_remote.is_empty()
                    && let Some(url) = entry.git_repository_url
                {
                    self.state.git_remote.push_str(url);
                }
            }
            "turn_context" => {
                if self.state.folder_path.is_empty()
                    && let Some(cwd) = entry.cwd
                {
                    self.state.folder_path.push_str(cwd);
                }
                if let Some(model) = entry.model.filter(|model| !model.is_empty())
                    && self.current_model != model
                {
                    self.current_model.clear();
                    self.current_model.push_str(model);
                }
            }
            "event_msg" => {
                if entry.payload_type == Some("token_count")
                    && let Some(token_count) = entry.token_count
                {
                    match token_count {
                        ParsedTokenCount::Invalid => {
                            self.diagnostics.record_relevant(false);
                        }
                        ParsedTokenCount::Valid {
                            total,
                            last_input_tokens,
                            last_usage,
                            model_context_window,
                        } => {
                            self.diagnostics.record_relevant(true);
                            if self.current_model.is_empty() {
                                // Replayed pre-context totals advance the baseline without guessing a model.
                                if let Some(total) = total {
                                    self.prev_totals =
                                        Some(total.to_totals(self.prev_totals.as_ref()));
                                }
                            } else {
                                let (delta, next_totals) = match total {
                                    Some(fields) => {
                                        let current = fields.to_totals(self.prev_totals.as_ref());
                                        let base = match self.prev_totals {
                                            Some(prev)
                                                if current.total_tokens >= prev.total_tokens =>
                                            {
                                                prev
                                            }
                                            _ => CodexTokenTotals::default(),
                                        };
                                        let delta = CodexTokenFields {
                                            input_tokens: fields.input_tokens.map(|_| {
                                                (current.input_tokens - base.input_tokens).max(0)
                                            }),
                                            cached_input_tokens: fields.cached_input_tokens.map(
                                                |_| {
                                                    (current.cached_input_tokens
                                                        - base.cached_input_tokens)
                                                        .max(0)
                                                },
                                            ),
                                            cache_write_input_tokens: fields
                                                .cache_write_input_tokens
                                                .map(|_| {
                                                    (current.cache_write_input_tokens
                                                        - base.cache_write_input_tokens)
                                                        .max(0)
                                                }),
                                            output_tokens: fields.output_tokens.map(|_| {
                                                (current.output_tokens - base.output_tokens).max(0)
                                            }),
                                            reasoning_output_tokens: fields
                                                .reasoning_output_tokens
                                                .map(|_| {
                                                    (current.reasoning_output_tokens
                                                        - base.reasoning_output_tokens)
                                                        .max(0)
                                                }),
                                            total_tokens: fields.total_tokens.map(|_| {
                                                (current.total_tokens - base.total_tokens).max(0)
                                            }),
                                        };
                                        (delta, Some(current))
                                    }
                                    None => (CodexTokenFields::default(), None),
                                };
                                // Classify against the turn's own prompt (`last_token_usage.input_tokens`, which includes cached input), falling back to the delta input.
                                let level = self.classifier.as_mut().map_or(0, |classifier| {
                                    let request_context = last_input_tokens
                                        .filter(|tokens| *tokens > 0)
                                        .or(delta.input_tokens)
                                        .unwrap_or(0);
                                    classifier.level(&self.current_model, request_context)
                                });
                                if !self.current_model.contains("<synthetic>") {
                                    let usage = match self.model_usages.get_mut(&self.current_model)
                                    {
                                        Some(existing) => existing,
                                        None => self
                                            .model_usages
                                            .entry(self.current_model.clone())
                                            .or_default(),
                                    };
                                    add_opt_i64(&mut usage.input_tokens, delta.input_tokens);
                                    add_opt_i64(
                                        &mut usage.cached_input_tokens,
                                        delta.cached_input_tokens,
                                    );
                                    add_opt_i64(
                                        &mut usage.cache_write_input_tokens,
                                        delta.cache_write_input_tokens,
                                    );
                                    add_opt_i64(&mut usage.output_tokens, delta.output_tokens);
                                    add_opt_i64(
                                        &mut usage.reasoning_output_tokens,
                                        delta.reasoning_output_tokens,
                                    );
                                    add_opt_i64(&mut usage.total_tokens, delta.total_tokens);
                                    if level > 0 {
                                        let cached = delta.cached_input_tokens.unwrap_or(0);
                                        let cache_write =
                                            delta.cache_write_input_tokens.unwrap_or(0);
                                        let reasoning = delta.reasoning_output_tokens.unwrap_or(0);
                                        usage.add_tier_slice(
                                            level,
                                            TierSlice {
                                                input_tokens: (delta.input_tokens.unwrap_or(0)
                                                    - cached
                                                    - cache_write)
                                                    .max(0),
                                                output_tokens: (delta.output_tokens.unwrap_or(0)
                                                    - reasoning)
                                                    .max(0),
                                                reasoning_tokens: reasoning,
                                                cache_read: cached,
                                                cache_creation_5m: cache_write,
                                                cache_creation_1h: 0,
                                            },
                                        );
                                    }
                                    if let Some(last) = last_usage {
                                        usage.set_last_usage(last);
                                    }
                                    if let Some(ctx) = model_context_window {
                                        usage.model_context_window = Some(ctx);
                                    }
                                }
                                if let Some(next_totals) = next_totals {
                                    self.prev_totals = Some(next_totals);
                                }
                            }
                        }
                    }
                }

                // `patch_apply_end` is the authoritative record for edits applied inside opaque `exec` cells.
                if entry.payload_type == Some("patch_apply_end") {
                    let already_counted = entry
                        .call_id
                        .is_some_and(|id| self.patch_counted_ids.contains(id));
                    if matches!(entry.success, Some(true)) && !already_counted {
                        match parse_patch_changes(entry.changes) {
                            Some(patches) => {
                                for patch in patches {
                                    self.state.handle_patch(patch, ts);
                                }
                                if let Some(id) = entry.call_id {
                                    self.patch_counted_ids.insert(id.to_string());
                                }
                                self.diagnostics.record_relevant(true);
                            }
                            None => self.diagnostics.record_relevant(false),
                        }
                    }
                    // A failed or deduped event is recognized but creates no file ops.
                }
            }
            "response_item" => match entry.payload_type {
                Some("function_call") => {
                    if let Some(name) = entry.name
                        && matches!(name, "shell" | "exec_command")
                    {
                        let call = entry
                            .arguments
                            .and_then(|args| parse_function_call(name, args, ts));
                        if let Some(call_id) = entry.call_id {
                            if let Some(call) = call {
                                self.diagnostics.record_relevant(true);
                                self.shell_calls.insert(
                                    call_id.to_string(),
                                    PendingCodexShellCall::Parsed(call),
                                );
                            } else {
                                // Defer the schema verdict until the paired output, since Codex logs model argument errors as normal lifecycle records.
                                self.shell_calls.insert(
                                    call_id.to_string(),
                                    PendingCodexShellCall::InvalidArguments,
                                );
                            }
                        } else {
                            self.diagnostics.record_relevant(false);
                        }
                    }
                }
                Some("function_call_output") => {
                    if let Some(call_id) = entry.call_id
                        && let Some(call) = self.shell_calls.remove(call_id)
                    {
                        match call {
                            PendingCodexShellCall::Parsed(call) => {
                                self.diagnostics.record_relevant(true);
                                self.state.handle_shell_call(call, entry.output);
                            }
                            PendingCodexShellCall::InvalidArguments => {
                                self.diagnostics
                                    .record_relevant(output_reports_argument_error(entry.output));
                            }
                        }
                    }
                }
                Some("custom_tool_call") => {
                    if let Some(name) = entry.name
                        && matches!(name, "exec" | "apply_patch")
                    {
                        let call = entry
                            .arguments
                            .and_then(|input| parse_custom_call(name, input, ts, self.mode));
                        let normalized = call.is_some() && entry.call_id.is_some();
                        self.diagnostics.record_relevant(normalized);
                        if let (Some(call), Some(call_id)) = (call, entry.call_id) {
                            self.custom_calls.insert(call_id.to_string(), call);
                        }
                    }
                }
                Some("custom_tool_call_output") => {
                    if let Some(call_id) = entry.call_id
                        && let Some(call) = self.custom_calls.remove(call_id)
                    {
                        let normalized = dispatch_custom_call(
                            &mut self.state,
                            call,
                            entry.output,
                            call_id,
                            &mut self.patch_counted_ids,
                        );
                        self.diagnostics.record_relevant(normalized);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn finish(mut self) -> ParsedAnalysis {
        for call in self.shell_calls.into_values() {
            if matches!(call, PendingCodexShellCall::InvalidArguments) {
                self.diagnostics.record_relevant(false);
            }
        }
        if self.state.git_remote.is_empty() {
            self.state.git_remote = get_git_remote_url(&self.state.folder_path);
        }

        let mut conversation_usage = FastHashMap::with_capacity(self.model_usages.len());
        for (model, usage) in self.model_usages {
            conversation_usage.insert(model, usage.into_value());
        }

        let record = self.state.into_record(conversation_usage);

        let analysis = CodeAnalysis {
            user: String::new(),
            extension_name: String::new(),
            insights_version: String::new(),
            machine_id: String::new(),
            records: vec![record],
        };
        ParsedAnalysis::new(analysis, self.diagnostics)
    }
}

fn add_opt_i64(slot: &mut Option<i64>, delta: Option<i64>) {
    if let Some(delta) = delta {
        *slot = Some(slot.unwrap_or(0) + delta);
    }
}

/// Validates and extracts typed fields from a `token_count` `info` [`Value`].
fn parse_token_count_value(info: &Value) -> ParsedTokenCount<'_> {
    let Some(info) = info.as_object() else {
        return ParsedTokenCount::Invalid;
    };
    let mut recognized = false;
    let mut total = None;
    let mut last_input_tokens = None;
    let mut last_usage = None;
    let mut model_context_window = None;

    if let Some(usage) = info.get("total_token_usage") {
        let Some(fields) = parse_token_fields_value(usage) else {
            return ParsedTokenCount::Invalid;
        };
        total = Some(fields);
        recognized = true;
    }
    if let Some(usage) = info.get("last_token_usage") {
        let Some(fields) = parse_token_fields_value(usage) else {
            return ParsedTokenCount::Invalid;
        };
        last_input_tokens = fields.input_tokens;
        last_usage = Some(LastTokenUsage::Value(usage));
        recognized = true;
    }
    if let Some(context_window) = info.get("model_context_window") {
        if context_window.is_null() {
            // Older records emit `null` here alongside valid token objects.
            model_context_window = Some(None);
        } else if let Some(n) = context_window.as_i64() {
            model_context_window = Some(Some(n));
            recognized = true;
        } else {
            return ParsedTokenCount::Invalid;
        }
    }
    if recognized {
        ParsedTokenCount::Valid {
            total,
            last_input_tokens,
            last_usage,
            model_context_window,
        }
    } else {
        ParsedTokenCount::Invalid
    }
}

fn parse_token_fields_value(usage: &Value) -> Option<CodexTokenFields> {
    let usage = usage.as_object()?;
    if usage.is_empty() {
        return Some(CodexTokenFields::default());
    }
    let mut fields = CodexTokenFields::default();
    let mut recognized = false;
    let mut read_key = |key: &str, slot: &mut Option<i64>| -> bool {
        if let Some(value) = usage.get(key) {
            let Some(n) = value.as_i64() else {
                return false;
            };
            *slot = Some(n);
            recognized = true;
        }
        true
    };
    if !read_key("input_tokens", &mut fields.input_tokens)
        || !read_key("cached_input_tokens", &mut fields.cached_input_tokens)
        || !read_key(
            "cache_write_input_tokens",
            &mut fields.cache_write_input_tokens,
        )
        || !read_key("output_tokens", &mut fields.output_tokens)
        || !read_key(
            "reasoning_output_tokens",
            &mut fields.reasoning_output_tokens,
        )
        || !read_key("total_tokens", &mut fields.total_tokens)
    {
        return None;
    }
    recognized.then_some(fields)
}

/// Preserves explicit JSON `null` as `Some(RawValue("null"))` so absent keys remain distinguishable from `null`.
fn deserialize_present_raw<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<&'de RawValue>, D::Error>
where
    D: Deserializer<'de>,
{
    <&'de RawValue>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct BorrowedTokenCountInfo<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    total_token_usage: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    last_token_usage: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    model_context_window: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedTokenFields<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    input_tokens: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    cached_input_tokens: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    cache_write_input_tokens: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    output_tokens: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    reasoning_output_tokens: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    total_tokens: Option<&'a RawValue>,
}

/// Validates and extracts typed fields directly from a `token_count` `info` raw JSON slice without building a [`Value`] tree.
fn parse_token_count_raw(raw: &str) -> ParsedTokenCount<'_> {
    if !raw.trim_start().starts_with('{') {
        return ParsedTokenCount::Invalid;
    }
    let Ok(info) = serde_json::from_str::<BorrowedTokenCountInfo<'_>>(raw) else {
        return ParsedTokenCount::Invalid;
    };
    let mut recognized = false;
    let mut total = None;
    let mut last_input_tokens = None;
    let mut last_usage = None;
    let mut model_context_window = None;

    if let Some(usage_raw) = info.total_token_usage {
        let Some(fields) = parse_token_fields_raw(usage_raw.get()) else {
            return ParsedTokenCount::Invalid;
        };
        total = Some(fields);
        recognized = true;
    }
    if let Some(usage_raw) = info.last_token_usage {
        let Some(fields) = parse_token_fields_raw(usage_raw.get()) else {
            return ParsedTokenCount::Invalid;
        };
        last_input_tokens = fields.input_tokens;
        last_usage = Some(LastTokenUsage::RawJson(usage_raw.get()));
        recognized = true;
    }
    if let Some(ctx_raw) = info.model_context_window {
        if ctx_raw.get() == "null" {
            // Older records emit `null` here alongside valid token objects.
            model_context_window = Some(None);
        } else if let Ok(n) = serde_json::from_str::<i64>(ctx_raw.get()) {
            model_context_window = Some(Some(n));
            recognized = true;
        } else {
            return ParsedTokenCount::Invalid;
        }
    }

    if recognized {
        ParsedTokenCount::Valid {
            total,
            last_input_tokens,
            last_usage,
            model_context_window,
        }
    } else {
        ParsedTokenCount::Invalid
    }
}

fn parse_token_fields_raw(raw: &str) -> Option<CodexTokenFields> {
    if !raw.trim_start().starts_with('{') {
        return None;
    }
    if raw.bytes().filter(|b| !b.is_ascii_whitespace()).eq(*b"{}") {
        return Some(CodexTokenFields::default());
    }
    let borrowed = serde_json::from_str::<BorrowedTokenFields<'_>>(raw).ok()?;
    let mut fields = CodexTokenFields::default();
    let mut recognized = false;
    let mut parse_field = |raw: Option<&RawValue>, slot: &mut Option<i64>| -> bool {
        if let Some(raw) = raw {
            let Ok(n) = serde_json::from_str::<i64>(raw.get()) else {
                return false;
            };
            *slot = Some(n);
            recognized = true;
        }
        true
    };
    if !parse_field(borrowed.input_tokens, &mut fields.input_tokens)
        || !parse_field(
            borrowed.cached_input_tokens,
            &mut fields.cached_input_tokens,
        )
        || !parse_field(
            borrowed.cache_write_input_tokens,
            &mut fields.cache_write_input_tokens,
        )
        || !parse_field(borrowed.output_tokens, &mut fields.output_tokens)
        || !parse_field(
            borrowed.reasoning_output_tokens,
            &mut fields.reasoning_output_tokens,
        )
        || !parse_field(borrowed.total_tokens, &mut fields.total_tokens)
    {
        return None;
    }
    recognized.then_some(fields)
}

/// Returns whether a `token_count` `info` object contains at least one recognized, well-typed field.
#[cfg(test)]
fn is_supported_codex_usage(info: &Value) -> bool {
    matches!(
        parse_token_count_value(info),
        ParsedTokenCount::Valid { .. }
    )
}

enum PendingCodexShellCall {
    Parsed(CodexShellCall),
    InvalidArguments,
}

impl PendingCodexShellCall {
    fn needs_output(&self) -> bool {
        match self {
            Self::InvalidArguments => true,
            Self::Parsed(call) => {
                !call.script.contains("*** Begin Patch")
                    && (extract_sed_file_path(&call.script).is_some()
                        || extract_cat_file_path(&call.script).is_some())
            }
        }
    }
}

/// Returns whether `output` is a Codex argument-parse error rather than command output.
fn output_reports_argument_error(output: Option<&str>) -> bool {
    let output = extract_shell_output_text(output);
    let output = strip_exec_command_metadata_prefix(&output)
        .trim_start()
        .to_ascii_lowercase();
    (output.starts_with("error") || output.starts_with("failed"))
        && (output.contains("failed to parse")
            || output.contains("missing field")
            || output.contains("missing required")
            || output.contains("invalid argument"))
}

/// Strips the `exec_command` metadata header before `Output:\n`, leaving `output` unchanged when no recognized header precedes the marker.
fn strip_exec_command_metadata_prefix(output: &str) -> &str {
    const MARKER: &str = "\nOutput:\n";
    if let Some(idx) = output.find(MARKER)
        && is_exec_metadata_header(&output[..idx])
    {
        return &output[idx + MARKER.len()..];
    }
    output.strip_prefix("Output:\n").unwrap_or(output)
}

/// Returns whether every line in `prefix` matches a known `exec_command` metadata header.
fn is_exec_metadata_header(prefix: &str) -> bool {
    // Closed set so YAML or log output containing `Key: value` lines before an `Output:` line is never truncated.
    const HEADER_LINES: [&str; 5] = [
        "Chunk ID:",
        "Original token count:",
        "Process exited with code",
        "Script completed",
        "Wall time:",
    ];

    !prefix.is_empty()
        && prefix
            .lines()
            .all(|line| HEADER_LINES.iter().any(|known| line.starts_with(known)))
}

/// Extracts the output text from a shell call's raw or JSON-wrapped `output`, borrowing without JSON parsing when it does not start with `{`.
fn extract_shell_output_text(output: Option<&str>) -> Cow<'_, str> {
    let Some(output) = output else {
        return Cow::Borrowed("");
    };

    if output.trim_start().as_bytes().first() == Some(&b'{')
        && let Ok(parsed) = serde_json::from_str::<BorrowedOutputField<'_>>(output)
    {
        return parsed.output;
    }

    Cow::Borrowed(output)
}

/// Parses a `shell` or `exec_command` call into a [`CodexShellCall`], returning `None` for other tools or invalid arguments.
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
            let args = serde_json::from_str::<BorrowedExecCommandArgs<'_>>(args_str).ok()?;
            let cmd = args.cmd.into_owned();
            Some(CodexShellCall {
                timestamp: ts,
                script: cmd.clone(),
                full_command: vec![cmd],
            })
        }
        _ => None,
    }
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
    fn needs_output(&self) -> bool {
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
    call_id: &str,
    patch_counted_ids: &mut FastHashSet<String>,
) -> bool {
    match call {
        CodexCustomCall::Exec { source, timestamp } => {
            // Nested tool calls in JavaScript source cannot be proven to have executed; count the completed `exec` cell as one command.
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
                    if patch_counted_ids.insert(call_id.to_string()) {
                        for patch in patches {
                            state.handle_patch(patch, timestamp);
                        }
                    }
                    true
                }
                CustomApplyPatchResult::Failure => true,
                CustomApplyPatchResult::Unknown => false,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomApplyPatchResult {
    Success,
    Failure,
    Unknown,
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
        || output.starts_with("apply_patch handler received")
        || output.starts_with("apply_patch is unavailable")
    {
        CustomApplyPatchResult::Failure
    } else {
        CustomApplyPatchResult::Unknown
    }
}

/// Decodes a custom tool's normalized string or `JSON.stringify` result.
fn normalize_custom_output(output: Option<&str>) -> Cow<'_, str> {
    let body = strip_exec_command_metadata_prefix(output.unwrap_or_default()).trim();
    match body.as_bytes().first() {
        Some(b'{') => serde_json::from_str::<BorrowedOutputField<'_>>(body)
            .map(|parsed| parsed.output)
            .unwrap_or(Cow::Borrowed(body)),
        Some(b'"') => serde_json::from_str::<Cow<'_, str>>(body).unwrap_or(Cow::Borrowed(body)),
        _ => Cow::Borrowed(body),
    }
}

/// Codex-specific shell and patch dispatch helpers on [`SessionParseState`].
trait CodexAnalysisExt {
    /// Routes a completed shell call to read, patch, or run-command metrics based on its `script`.
    fn handle_shell_call(&mut self, call: CodexShellCall, output: Option<&str>);
    /// Applies one parsed `apply_patch` hunk as a write, delete, or edit.
    fn handle_patch(&mut self, patch: CodexPatch, ts: i64);
    /// Records a shell call that was not a file operation as a run command.
    fn record_run_command(&mut self, call: CodexShellCall);
}

impl CodexAnalysisExt for SessionParseState {
    fn handle_shell_call(&mut self, call: CodexShellCall, output: Option<&str>) {
        // Patch payloads carry a stable envelope regardless of the launcher name.
        if call.script.contains("*** Begin Patch") {
            let patches = parse_apply_patch_script(&call.script);
            for patch in patches {
                self.handle_patch(patch, call.timestamp);
            }
            return;
        }

        if let Some(path) = extract_sed_file_path(&call.script) {
            let output_text = extract_shell_output_text(output);
            let output_body = strip_exec_command_metadata_prefix(&output_text);
            self.add_read_detail(path, output_body, call.timestamp);
            return;
        }

        if let Some(path) = extract_cat_file_path(&call.script) {
            let output_text = extract_shell_output_text(output);
            let output_body = strip_exec_command_metadata_prefix(&output_text);
            let content = strip_cat_footer(output_body);
            self.add_read_detail(path, content, call.timestamp);
            return;
        }

        self.record_run_command(call);
    }

    fn handle_patch(&mut self, patch: CodexPatch, ts: i64) {
        if patch.file_path.is_empty() {
            return;
        }

        let resolved = self.normalize_path(&patch.file_path);
        if resolved.is_empty() {
            return;
        }

        match patch.action {
            CodexPatchAction::Add => {
                self.add_write_detail(&resolved, &patch.new_str, ts);
            }
            CodexPatchAction::Delete => {
                // Use `_raw` so deletes without a `unified_diff` body still count as a file operation.
                self.add_edit_detail_raw(&resolved, &patch.old_str, "", ts);
            }
            CodexPatchAction::Update => {
                self.add_edit_detail(&resolved, &patch.old_str, &patch.new_str, ts);
            }
        }
    }

    fn record_run_command(&mut self, call: CodexShellCall) {
        if matches!(self.mode, ParseMode::UsageOnly) {
            let non_empty = if call.full_command.is_empty() {
                !call.script.trim().is_empty()
            } else {
                call.full_command.iter().any(|part| !part.trim().is_empty())
            };
            if non_empty {
                self.tool_counts.bash += 1;
            }
            return;
        }

        let command_str = if call.full_command.is_empty() {
            Cow::Borrowed(call.script.trim())
        } else if call.full_command.len() == 1 {
            Cow::Borrowed(call.full_command[0].as_str())
        } else {
            Cow::Owned(call.full_command.join(" "))
        };

        self.add_run_command(&command_str, "", call.timestamp);
    }
}

/// A pending Codex `shell` or `exec_command` invocation awaiting its output.
struct CodexShellCall {
    /// Event timestamp in epoch milliseconds.
    timestamp: i64,
    /// The command line actually executed (used for `sed`/`cat`/patch sniffing).
    script: String,
    /// The full argv as written by the model, for verbatim run-command display.
    full_command: Vec<String>,
}

/// Action kind for one per-file hunk in an `apply_patch` script or `patch_apply_end` event.
#[derive(Clone, Copy)]
enum CodexPatchAction {
    Add,
    Delete,
    Update,
}

/// One per-file hunk from an `apply_patch` script or `patch_apply_end` event.
struct CodexPatch {
    /// Patch action (`Add`, `Delete`, or `Update`).
    action: CodexPatchAction,
    /// Target file path as written in the patch header.
    file_path: String,
    /// Removed (`-`) diff text with trailing newlines stripped.
    old_str: String,
    /// Added (`+`) diff text with trailing newlines stripped.
    new_str: String,
}

impl CodexPatch {
    fn new(action: CodexPatchAction, file_path: String) -> Self {
        Self {
            action,
            file_path,
            old_str: String::new(),
            new_str: String::new(),
        }
    }

    fn push_diff_line(&mut self, line: &str) {
        if line.is_empty() || line.starts_with("@@") {
            return;
        }
        match line.as_bytes()[0] {
            b'+' => {
                self.new_str.push_str(&line[1..]);
                self.new_str.push('\n');
            }
            b'-' => {
                self.old_str.push_str(&line[1..]);
                self.old_str.push('\n');
            }
            b'\\' => {}
            _ => {}
        }
    }

    fn finish(mut self) -> Self {
        let old_len = self.old_str.trim_end_matches('\n').len();
        self.old_str.truncate(old_len);
        let new_len = self.new_str.trim_end_matches('\n').len();
        self.new_str.truncate(new_len);
        self
    }
}

/// Parses an `apply_patch` script into per-file hunks, returning empty if no `*** Begin Patch` marker is present.
fn parse_apply_patch_script(script: &str) -> Vec<CodexPatch> {
    let start = match script.find("*** Begin Patch") {
        Some(idx) => idx,
        None => return Vec::new(),
    };

    let segment = &script[start..];
    let mut patches = Vec::with_capacity(3);
    let mut current: Option<CodexPatch> = None;

    for line in segment.lines() {
        let line = line.trim_end_matches('\r');

        if line.starts_with("*** End Patch") {
            if let Some(patch) = current.take() {
                patches.push(patch.finish());
            }
            break;
        } else if line.starts_with("*** Begin Patch") {
            continue;
        } else if let Some(rest) = line.strip_prefix("*** Update File:") {
            if let Some(patch) = current.take() {
                patches.push(patch.finish());
            }
            current = Some(CodexPatch::new(
                CodexPatchAction::Update,
                rest.trim().to_string(),
            ));
        } else if let Some(rest) = line.strip_prefix("*** Add File:") {
            if let Some(patch) = current.take() {
                patches.push(patch.finish());
            }
            current = Some(CodexPatch::new(
                CodexPatchAction::Add,
                rest.trim().to_string(),
            ));
        } else if let Some(rest) = line.strip_prefix("*** Delete File:") {
            if let Some(patch) = current.take() {
                patches.push(patch.finish());
            }
            current = Some(CodexPatch::new(
                CodexPatchAction::Delete,
                rest.trim().to_string(),
            ));
        } else if let Some(ref mut patch) = current {
            patch.push_diff_line(line);
        }
    }

    if let Some(patch) = current {
        patches.push(patch.finish());
    }

    patches
}

/// Parses a `patch_apply_end` `changes` object into patch hunks, returning `None` if missing, non-object, or containing only unrecognized entries (empty object returns `Some(vec![])`).
fn parse_patch_changes(changes: Option<&Value>) -> Option<Vec<CodexPatch>> {
    let obj = changes?.as_object()?;
    let mut patches = Vec::with_capacity(obj.len());
    for (path, entry) in obj {
        if let Some(patch) = parse_patch_change_entry(path, entry) {
            patches.push(patch);
        }
    }
    if !obj.is_empty() && patches.is_empty() {
        return None;
    }
    Some(patches)
}

/// Builds a [`CodexPatch`] from one `patch_apply_end` `changes` entry.
fn parse_patch_change_entry(path: &str, entry: &Value) -> Option<CodexPatch> {
    if path.is_empty() {
        return None;
    }
    let entry = entry.as_object()?;
    let action = match entry.get("type").and_then(Value::as_str)? {
        "add" => CodexPatchAction::Add,
        "update" => CodexPatchAction::Update,
        "delete" => CodexPatchAction::Delete,
        _ => return None,
    };
    // `unified_diff` may be omitted on `add`/`delete` entries; count the operation with empty lines.
    let mut patch = CodexPatch::new(action, path.to_string());
    if let Some(diff) = entry.get("unified_diff").and_then(Value::as_str) {
        for line in diff.lines() {
            patch.push_diff_line(line);
        }
    }
    Some(patch.finish())
}

/// Extracts the target file path from a `sed -n '<range>' <path>` command.
fn extract_sed_file_path(script: &str) -> Option<&str> {
    let mut rest = script;
    while let Some(idx) = rest.find("sed") {
        let after_sed = &rest[idx + 3..];
        let after_ws1 = after_sed.trim_start_matches(char::is_whitespace);
        if after_ws1.len() == after_sed.len() {
            rest = after_sed;
            continue;
        }
        let Some(after_flag) = after_ws1.strip_prefix("-n") else {
            rest = after_sed;
            continue;
        };
        let after_ws2 = after_flag.trim_start_matches(char::is_whitespace);
        if after_ws2.len() == after_flag.len() {
            rest = after_sed;
            continue;
        }
        let Some(after_open) = after_ws2.strip_prefix('\'') else {
            rest = after_sed;
            continue;
        };
        let close_idx = after_open.find('\'')?;
        let after_quote = &after_open[close_idx + 1..];
        let after_ws3 = after_quote.trim_start_matches(char::is_whitespace);
        if after_ws3.len() == after_quote.len() {
            rest = after_sed;
            continue;
        }
        let path_end = after_ws3
            .find(char::is_whitespace)
            .unwrap_or(after_ws3.len());
        if path_end == 0 {
            rest = after_sed;
            continue;
        }
        return Some(after_ws3[..path_end].trim_matches(|c| c == '"' || c == '\''));
    }
    None
}

/// Extracts the target file path from a `cat <path>` command line, if present.
fn extract_cat_file_path(script: &str) -> Option<&str> {
    if !script.contains("cat ") {
        return None;
    }
    for line in script.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("cat ") {
            continue;
        }

        let mut fields = trimmed.split_whitespace();
        fields.next();
        let Some(path_field) = fields.next() else {
            continue;
        };

        return Some(path_field.trim_matches(|c| c == '"' || c == '\''));
    }

    None
}

/// Strips any trailing `\n---` footer and trailing newlines from a `cat` command output.
fn strip_cat_footer(output: &str) -> &str {
    let prefix = output.find("\n---").map_or(output, |idx| &output[..idx]);
    prefix.trim_end_matches('\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_item(payload: Value) -> CodexLog {
        serde_json::from_value(serde_json::json!({
            "timestamp": "2026-07-12T00:00:00Z",
            "type": "response_item",
            "payload": payload
        }))
        .unwrap()
    }

    fn custom_output(call_id: &str, blocks: Value) -> CodexLog {
        response_item(serde_json::json!({
            "type": "custom_tool_call_output",
            "call_id": call_id,
            "output": blocks
        }))
    }

    fn patch_apply_end(call_id: &str, success: bool, changes: Value) -> CodexLog {
        serde_json::from_value(serde_json::json!({
            "timestamp": "2026-07-12T00:00:00Z",
            "type": "event_msg",
            "payload": {
                "type": "patch_apply_end",
                "call_id": call_id,
                "success": success,
                "changes": changes
            }
        }))
        .unwrap()
    }

    fn apply_patch_success_output(file: &str) -> CodexLog {
        let wire = serde_json::json!({
            "output": format!("Success. Updated the following files:\nM {file}")
        })
        .to_string();
        custom_output("shared-call", Value::String(wire))
    }

    #[test]
    fn legacy_shell_function_parses_into_call() {
        let args = r#"{"command":["bash","-lc","ls -la"]}"#;
        let call = parse_function_call("shell", args, 42).expect("shell call should parse");
        assert_eq!(call.timestamp, 42);
        assert_eq!(call.script, "ls -la");
        assert_eq!(call.full_command, vec!["bash", "-lc", "ls -la"]);
    }

    #[test]
    fn current_exec_command_function_parses_into_call() {
        let args =
            r#"{"cmd":"sed -n '1,260p' src/main.rs","workdir":"/repo","yield_time_ms":1000}"#;
        let call =
            parse_function_call("exec_command", args, 99).expect("exec_command should parse");
        assert_eq!(call.timestamp, 99);
        assert_eq!(call.script, "sed -n '1,260p' src/main.rs");
        // `full_command` collapses to the single cmd string so `record_run_command`'s `join(" ")` produces the verbatim command.
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
                "output": "done"
            })),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.run_command_details[0].command, "pwd");
    }

    #[test]
    fn unrelated_function_names_are_ignored() {
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

            let parsed =
                parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
            assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
            assert!(!parsed.diagnostics.is_complete_failure());
            assert_eq!(parsed.analysis.records[0].tool_call_counts.bash, 0);
            assert!(parsed.analysis.records[0].run_command_details.is_empty());
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

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.analysis.records[0].tool_call_counts.bash, 0);
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

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.diagnostics.relevant_records, 1);
        assert_eq!(parsed.diagnostics.normalized_records, 1);
        assert_eq!(parsed.diagnostics.failed_relevant_records, 0);
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }

    #[test]
    fn resumed_usage_bills_only_post_baseline_deltas() {
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

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
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

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }

    #[test]
    fn mid_session_model_switch_bills_each_model_its_own_delta() {
        let token_count = |input: i64, cached: i64, output: i64, total: i64| {
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": input,
                            "cached_input_tokens": cached,
                            "output_tokens": output,
                            "reasoning_output_tokens": 0,
                            "total_tokens": total
                        },
                        "model_context_window": 200000
                    }
                }
            })
        };
        let turn_context = |model: &str| {
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "turn_context",
                "payload": { "model": model }
            })
        };
        let logs: Vec<CodexLog> = [
            turn_context("gpt-5.6-luna"),
            token_count(20_000, 4_000, 3_289, 23_289),
            turn_context("gpt-5.6-sol"),
            token_count(60_000, 30_000, 10_000, 70_000),
            token_count(90_000, 56_000, 14_576, 104_576),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);

        let usage = &parsed.analysis.records[0].conversation_usage;
        let luna = usage["gpt-5.6-luna"]["total_token_usage"]
            .as_object()
            .unwrap();
        let sol = usage["gpt-5.6-sol"]["total_token_usage"]
            .as_object()
            .unwrap();
        assert_eq!(luna["total_tokens"].as_i64().unwrap(), 23_289);
        assert_eq!(sol["total_tokens"].as_i64().unwrap(), 104_576 - 23_289);
        assert_eq!(
            luna["total_tokens"].as_i64().unwrap() + sol["total_tokens"].as_i64().unwrap(),
            104_576,
            "the two models' attributed totals must reconstruct the session cumulative"
        );
    }

    #[test]
    fn per_turn_tier_classification_uses_the_turns_own_context() {
        let event = |cum_input: i64, cum_out: i64, cum_total: i64, last_input: i64| {
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": cum_input,
                            "cached_input_tokens": 0,
                            "output_tokens": cum_out,
                            "reasoning_output_tokens": 0,
                            "total_tokens": cum_total
                        },
                        "last_token_usage": { "input_tokens": last_input },
                        "model_context_window": 400000
                    }
                }
            })
        };
        let logs: Vec<CodexLog> = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "turn_context",
                "payload": { "model": "gpt-5.4" }
            }),
            event(200_000, 1_000, 201_000, 200_000),
            event(500_000, 2_500, 502_500, 300_000),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let tiers =
            crate::pricing::TierThresholds::from_entries([("gpt-5.4", vec![272_000])].into_iter());
        let parsed =
            parse_codex_log_iter_with_diagnostics(&logs, ParseMode::UsageOnly, Some(&tiers))
                .unwrap();
        let usage = &parsed.analysis.records[0].conversation_usage["gpt-5.4"];
        assert_eq!(usage["total_token_usage"]["total_tokens"], 502_500);
        assert_eq!(usage["above_tier"]["level_1_input_tokens"], 300_000);
        assert_eq!(usage["above_tier"]["level_1_output_tokens"], 1_500);
        assert!(
            usage["above_tier"]
                .get("level_1_cache_read_tokens")
                .is_none(),
            "no cached tokens in this session"
        );
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

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
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
    fn legacy_shell_output_passes_through_unchanged() {
        let raw = "line one\nline two\n";
        assert_eq!(strip_exec_command_metadata_prefix(raw), raw);
    }

    #[test]
    fn a_body_line_reading_output_is_not_a_metadata_header() {
        let raw = "fn main() {}\nUsage: demo\nOutput:\nthe answer\n";
        assert_eq!(strip_exec_command_metadata_prefix(raw), raw);
    }

    #[test]
    fn custom_exec_header_variant_is_still_stripped() {
        let raw = "Script completed\nWall time: 0.1 seconds\nOutput:\nthe content";
        assert_eq!(strip_exec_command_metadata_prefix(raw), "the content");
    }

    #[test]
    fn output_starting_with_marker_handles_no_leading_newline() {
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

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 0);
        assert!(record.edit_file_details.is_empty());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
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

            let parsed =
                parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
            let record = &parsed.analysis.records[0];
            assert_eq!(record.tool_call_counts.edit, 0);
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
        state.handle_shell_call(call, None);
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.edit_details.len(), 1);
    }

    #[test]
    fn patch_apply_end_success_counts_add_as_write_and_update_as_edit() {
        let changes = serde_json::json!({
            "/repo/new.rs": {
                "type": "add",
                "unified_diff": "@@ -0,0 +1,2 @@\n+fn main() {}\n+// added\n"
            },
            "/repo/lib.rs": {
                "type": "update",
                "unified_diff": "@@ -1,1 +1,1 @@\n-old\n+new\n"
            }
        });
        let logs = vec![patch_apply_end("call-1", true, changes)];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.total_unique_files, 2);
        assert_eq!(record.write_file_details[0].base.file_path, "/repo/new.rs");
        assert_eq!(record.edit_file_details[0].base.file_path, "/repo/lib.rs");
    }

    #[test]
    fn patch_apply_end_without_unified_diff_still_counts_the_operation() {
        let changes = serde_json::json!({
            "/repo/generated.bin": { "type": "add" }
        });
        let logs = vec![patch_apply_end("call-nodiff", true, changes)];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.total_unique_files, 1);
        assert_eq!(record.total_write_lines, 0);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
    }

    #[test]
    fn patch_apply_end_failure_creates_no_file_ops() {
        let changes = serde_json::json!({
            "/repo/x.rs": { "type": "add", "unified_diff": "@@ -0,0 +1,1 @@\n+nope\n" }
        });
        let logs = vec![patch_apply_end("call-fail", false, changes)];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.total_unique_files, 0);
        assert!(record.write_file_details.is_empty());
        // Recognized event, but a failed apply is not a relevant normalization.
        assert_eq!(parsed.diagnostics.relevant_records, 0);
        assert!(!parsed.diagnostics.is_complete_failure());
    }

    #[test]
    fn patch_apply_end_malformed_changes_is_relevant_failure() {
        let logs = vec![patch_apply_end(
            "call-x",
            true,
            Value::String("nope".into()),
        )];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert_eq!(parsed.analysis.records[0].tool_call_counts.edit, 0);
        assert_eq!(parsed.analysis.records[0].tool_call_counts.write, 0);
        assert!(parsed.diagnostics.is_complete_failure());
    }

    #[test]
    fn patch_apply_end_deduped_against_direct_apply_patch_before_output() {
        let patch = "*** Begin Patch\n*** Update File: lib.rs\n@@\n-old\n+new\n*** End Patch";
        let changes = serde_json::json!({
            "lib.rs": { "type": "update", "unified_diff": "@@ -1,1 +1,1 @@\n-old\n+new\n" }
        });
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "shared-call"
            })),
            patch_apply_end("shared-call", true, changes),
            apply_patch_success_output("lib.rs"),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.edit_file_details.len(), 1);
    }

    #[test]
    fn patch_apply_end_deduped_against_direct_apply_patch_after_output() {
        let patch = "*** Begin Patch\n*** Update File: lib.rs\n@@\n-old\n+new\n*** End Patch";
        let changes = serde_json::json!({
            "lib.rs": { "type": "update", "unified_diff": "@@ -1,1 +1,1 @@\n-old\n+new\n" }
        });
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "shared-call"
            })),
            apply_patch_success_output("lib.rs"),
            patch_apply_end("shared-call", true, changes),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.edit_file_details.len(), 1);
    }

    #[test]
    fn unrecognized_direct_output_does_not_suppress_authoritative_event() {
        let patch = "*** Begin Patch\n*** Update File: lib.rs\n@@\n-old\n+new\n*** End Patch";
        let changes = serde_json::json!({
            "lib.rs": { "type": "update", "unified_diff": "@@ -1,1 +1,1 @@\n-old\n+new\n" }
        });
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "shared-call"
            })),
            custom_output("shared-call", Value::String("<unknown envelope>".into())),
            patch_apply_end("shared-call", true, changes),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.edit_file_details.len(), 1);
    }

    #[test]
    fn patch_apply_end_delete_without_diff_counts_the_operation() {
        let changes = serde_json::json!({
            "/repo/gone.rs": { "type": "delete" }
        });
        let logs = vec![patch_apply_end("call-del", true, changes)];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.total_unique_files, 1);
        assert_eq!(record.total_edit_lines, 0);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
    }

    #[test]
    fn patch_apply_end_scalar_counts_match_across_parse_modes() {
        let changes = serde_json::json!({
            "/repo/new.rs": {
                "type": "add",
                "unified_diff": "@@ -0,0 +1,2 @@\n+fn main() {}\n+// added\n"
            },
            "/repo/lib.rs": {
                "type": "update",
                "unified_diff": "@@ -1,1 +1,1 @@\n-old\n+new\n"
            }
        });
        let build = || vec![patch_apply_end("call-1", true, changes.clone())];

        let full = parse_codex_logs(&build(), ParseMode::Full).unwrap();
        let usage = parse_codex_logs(&build(), ParseMode::UsageOnly).unwrap();
        let (f, u) = (&full.records[0], &usage.records[0]);

        assert_eq!(f.tool_call_counts.write, u.tool_call_counts.write);
        assert_eq!(f.tool_call_counts.edit, u.tool_call_counts.edit);
        assert_eq!(f.total_unique_files, u.total_unique_files);
        assert_eq!(f.total_write_lines, u.total_write_lines);
        assert_eq!(f.total_edit_lines, u.total_edit_lines);
        assert_eq!(f.total_write_characters, u.total_write_characters);
        assert_eq!(f.total_edit_characters, u.total_edit_characters);
        assert!(!f.write_file_details.is_empty());
        assert!(u.write_file_details.is_empty());
    }

    #[test]
    fn borrowed_jsonl_stream_handles_reasoning_arrays_and_schema_drift() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let lines = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "turn_context",
                "payload": { "cwd": "/tmp", "model": "gpt-5" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "response_item",
                "payload": { "type": "reasoning", "summary": [], "encrypted_content": "blob" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:02Z",
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
                        }
                    }
                }
            }),
        ];
        let body: String = lines.iter().map(|v| format!("{v}\n")).collect();
        std::fs::write(&path, body).unwrap();

        let parsed = parse_codex_jsonl_path(&path, ParseMode::UsageOnly, None)
            .unwrap()
            .unwrap();
        assert_eq!(parsed.diagnostics.recognized_records, 3);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(
            parsed.analysis.records[0].conversation_usage["gpt-5"]["total_token_usage"]["input_tokens"],
            100
        );

        let bad_path = dir.path().join("bad.jsonl");
        std::fs::write(
            &bad_path,
            "{\"timestamp\":\"2026-07-12T00:00:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":42}}\n",
        )
        .unwrap();
        let bad = parse_codex_jsonl_path(&bad_path, ParseMode::UsageOnly, None)
            .unwrap()
            .unwrap();
        assert!(bad.diagnostics.is_complete_failure());
    }

    #[test]
    fn extract_sed_file_path_matches_whitespace_quotes_and_edge_cases() {
        assert_eq!(
            extract_sed_file_path("sed -n '1,260p' src/main.rs"),
            Some("src/main.rs")
        );
        assert_eq!(
            extract_sed_file_path("sed   -n\t'10,20p'\t\t'src/lib.rs'"),
            Some("src/lib.rs")
        );
        assert_eq!(
            extract_sed_file_path("sed\n-n\n'1p'\n\"path/to/file.rs\" | head"),
            Some("path/to/file.rs")
        );
        assert_eq!(
            extract_sed_file_path("echo passed && sed -n '1,5p' src/a.rs"),
            Some("src/a.rs")
        );
        assert_eq!(
            extract_sed_file_path("sed -n 'foo sed -n '1,5p' src/b.rs"),
            Some("src/b.rs")
        );
        assert_eq!(
            extract_sed_file_path("sed -n '' empty_range.rs"),
            Some("empty_range.rs")
        );
        assert_eq!(extract_sed_file_path("sed -n '1p' ''"), Some(""));
        assert_eq!(
            extract_sed_file_path("sed -n '1p' 檔案/main.rs"),
            Some("檔案/main.rs")
        );

        assert_eq!(extract_sed_file_path("git status"), None);
        assert_eq!(extract_sed_file_path("sed -n '1,5p'"), None);
        assert_eq!(extract_sed_file_path("sed -n '1,5p'   "), None);
        assert_eq!(extract_sed_file_path("sed -n 'unclosed src/main.rs"), None);
        assert_eq!(extract_sed_file_path("sed -i 's/a/b/' src/main.rs"), None);
        assert_eq!(extract_sed_file_path("sed-n '1p' src/main.rs"), None);
        assert_eq!(extract_sed_file_path("sed -n'1p' src/main.rs"), None);
        assert_eq!(extract_sed_file_path("sed -n '1p'src/main.rs"), None);
    }

    #[test]
    fn raw_token_count_validation_matches_value_validation() {
        let cases = [
            (serde_json::json!({ "total_token_usage": {} }), true),
            (
                serde_json::json!({ "last_token_usage": { "input_tokens": 0, "future_metric": 5 } }),
                true,
            ),
            (
                serde_json::json!({
                    "total_token_usage": { "output_tokens": 0 },
                    "model_context_window": null
                }),
                true,
            ),
            (serde_json::json!({ "model_context_window": 200_000 }), true),
            (
                serde_json::json!({ "total_token_usage": { "prompt_tokens": 3 } }),
                false,
            ),
            (
                serde_json::json!({ "last_token_usage": { "output_tokens": "3" } }),
                false,
            ),
            (
                serde_json::json!({
                    "total_token_usage": { "input_tokens": 3 },
                    "last_token_usage": { "completion_tokens": 1 }
                }),
                false,
            ),
            (
                serde_json::json!({ "model_context_window": "200000" }),
                false,
            ),
            (serde_json::json!({ "model_context_window": null }), false),
            (serde_json::json!({ "total_token_usage": null }), false),
            (
                serde_json::json!({ "total_token_usage": { "input_tokens": null } }),
                false,
            ),
            (serde_json::json!([{ "input_tokens": 10 }]), false),
            (serde_json::json!({ "total_token_usage": [10, 20] }), false),
            (serde_json::json!({ "last_token_usage": [10] }), false),
            (serde_json::json!("not-an-object"), false),
            (serde_json::json!(42), false),
        ];

        for (value, expected) in cases {
            assert_eq!(
                is_supported_codex_usage(&value),
                expected,
                "value path mismatch for {value}"
            );
            let raw = value.to_string();
            assert_eq!(
                matches!(parse_token_count_raw(&raw), ParsedTokenCount::Valid { .. }),
                expected,
                "raw path mismatch for {raw}"
            );
        }
    }

    #[test]
    fn streamed_codex_path_gates_outputs_and_preserves_reads_and_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let lines = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "session_meta",
                "payload": { "id": "gated-session", "cwd": "/repo" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"git status\"}",
                    "call_id": "bash-1"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:02Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "bash-1",
                    "output": "On branch main\nnothing to commit"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:03Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"sed -n '1,2p' src/main.rs\"}",
                    "call_id": "sed-1"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:04Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "sed-1",
                    "output": "Chunk ID: abcd\nOutput:\nfn main() {}\n// end\n"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:05Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": "{not json",
                    "call_id": "bad-1"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:06Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "bad-1",
                    "output": "Error: failed to parse function arguments"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:07Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": "{\"cmd\":\"cat src/notes.txt\"}",
                    "call_id": "cat-1"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:08Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "cat-1",
                    "output": "alpha\nbeta\n---\nignored footer"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:09Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call",
                    "name": "exec",
                    "input": "await tools.exec_command({cmd: 'pwd'});",
                    "call_id": "cexec-1"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:10Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call_output",
                    "call_id": "cexec-1",
                    "output": "Script completed"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:11Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call",
                    "name": "apply_patch",
                    "input": "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch",
                    "call_id": "cpatch-1"
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:12Z",
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call_output",
                    "call_id": "cpatch-1",
                    "output": "{\"output\":\"Success. Updated the following files:\\nM src/lib.rs\"}"
                }
            }),
        ];
        let body: String = lines.iter().map(|v| format!("{v}\n")).collect();
        std::fs::write(&path, body).unwrap();

        let parsed = parse_codex_jsonl_path(&path, ParseMode::Full, None)
            .unwrap()
            .unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 2);
        assert_eq!(record.tool_call_counts.read, 2);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.total_read_lines, 4);
        assert_eq!(
            record.read_file_details[0].base.file_path,
            "/repo/src/main.rs"
        );
        assert_eq!(
            record.read_file_details[1].base.file_path,
            "/repo/src/notes.txt"
        );
        assert_eq!(
            record.edit_file_details[0].base.file_path,
            "/repo/src/lib.rs"
        );

        // Invalid argument/output shapes on gated records must still fail schema validation.
        let rejected = dir.path().join("rejected.jsonl");
        let rejected_body = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "response_item",
                "payload": { "type": "function_call", "name": "update_plan", "arguments": 7 }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "untracked",
                    "output": [{ "type": "future_block" }]
                }
            }),
        ]
        .iter()
        .map(|v| format!("{v}\n"))
        .collect::<String>();
        std::fs::write(&rejected, rejected_body).unwrap();
        let rejected_parsed = parse_codex_jsonl_path(&rejected, ParseMode::UsageOnly, None)
            .unwrap()
            .unwrap();
        assert!(rejected_parsed.diagnostics.is_complete_failure());
    }

    #[test]
    fn cache_write_input_tokens_match_between_value_and_streamed_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("cache-write.jsonl");
        let raw_lines = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "turn_context",
                "payload": { "model": "gpt-6-astra" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 100_000,
                            "cached_input_tokens": 40_000,
                            "cache_write_input_tokens": 20_000,
                            "output_tokens": 2_000,
                            "reasoning_output_tokens": 500,
                            "total_tokens": 102_000
                        },
                        "last_token_usage": {
                            "input_tokens": 100_000,
                            "cached_input_tokens": 40_000,
                            "cache_write_input_tokens": 20_000,
                            "output_tokens": 2_000,
                            "reasoning_output_tokens": 500,
                            "total_tokens": 102_000
                        },
                        "model_context_window": 400_000
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:02Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 400_000,
                            "cached_input_tokens": 240_000,
                            "cache_write_input_tokens": 70_000,
                            "output_tokens": 5_000,
                            "reasoning_output_tokens": 1_500,
                            "total_tokens": 405_000
                        },
                        "last_token_usage": {
                            "input_tokens": 300_000,
                            "cached_input_tokens": 200_000,
                            "cache_write_input_tokens": 50_000,
                            "output_tokens": 3_000,
                            "reasoning_output_tokens": 1_000,
                            "total_tokens": 303_000
                        },
                        "model_context_window": 400_000
                    }
                }
            }),
        ];
        let body: String = raw_lines.iter().map(|v| format!("{v}\n")).collect();
        std::fs::write(&path, body).unwrap();

        let logs: Vec<CodexLog> = raw_lines
            .into_iter()
            .map(|v| serde_json::from_value(v).unwrap())
            .collect();
        let tiers = crate::pricing::TierThresholds::from_entries(
            [("gpt-6-astra", vec![272_000])].into_iter(),
        );

        let value_parsed =
            parse_codex_log_iter_with_diagnostics(&logs, ParseMode::UsageOnly, Some(&tiers))
                .unwrap();
        let streamed_parsed = parse_codex_jsonl_path(&path, ParseMode::UsageOnly, Some(&tiers))
            .unwrap()
            .unwrap();

        assert_eq!(value_parsed.diagnostics, streamed_parsed.diagnostics);
        assert_eq!(value_parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(
            value_parsed.analysis.records[0].conversation_usage,
            streamed_parsed.analysis.records[0].conversation_usage
        );

        let usage = &streamed_parsed.analysis.records[0].conversation_usage["gpt-6-astra"];
        assert_eq!(
            usage["total_token_usage"]["cache_write_input_tokens"],
            70_000
        );
        assert_eq!(usage["above_tier"]["level_1_input_tokens"], 50_000);
        assert_eq!(usage["above_tier"]["level_1_cache_read_tokens"], 200_000);
        assert_eq!(
            usage["above_tier"]["level_1_cache_creation_5m_tokens"],
            50_000
        );
        assert_eq!(usage["above_tier"]["level_1_output_tokens"], 2_000);
        assert_eq!(usage["above_tier"]["level_1_reasoning_tokens"], 1_000);

        let valid_only_cw = serde_json::json!({
            "total_token_usage": { "cache_write_input_tokens": 12 }
        });
        assert!(is_supported_codex_usage(&valid_only_cw));
        assert!(matches!(
            parse_token_count_raw(&valid_only_cw.to_string()),
            ParsedTokenCount::Valid { .. }
        ));
        let invalid_cw = serde_json::json!({
            "total_token_usage": { "cache_write_input_tokens": "bad" }
        });
        assert!(!is_supported_codex_usage(&invalid_cw));
        assert!(matches!(
            parse_token_count_raw(&invalid_cw.to_string()),
            ParsedTokenCount::Invalid
        ));
    }
}
