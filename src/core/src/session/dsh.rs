//! DeepSeek Harness (`dsh`) session-log parser.
//!
//! `dsh` writes one append-only JSONL log per session, either as concatenated zstd frames (header frame followed by one frame per append batch) or as uncompressed `session.jsonl` when `compression: 'none'`. When a live session ends inside an incomplete trailing frame, the reader keeps every record decoded before the error instead of rejecting the file.

use crate::constants::{FastHashMap, buffer};
use crate::models::CodeAnalysis;
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::token_merge::write_tier_slice;
use crate::utils::{MAX_TIER_LEVELS, TierSlice};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json, value::RawValue};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Magic bytes opening every zstd frame (`0xFD2FB528`, little-endian).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Only `SESSION_FORMAT_VERSION` supported by this parser; any other version fails the parse.
const SUPPORTED_FORMAT_VERSION: i64 = 0;

/// Known event types and packed chunk-row tags that carry no extractable metrics.
///
/// Listed explicitly to flag unknown future event types as drift; mirrors upstream `KNOWN_SESSION_EVENT_TYPES` minus types handled in [`DshParser::push`], plus the three slash-less packed chunk tags.
const IGNORED_RECORD_TYPES: [&str; 42] = [
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "llm/retry",
    "llm/retry-started",
    "permission/preset",
    "plan/mode",
    "reasoning-chunks",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/descriptor",
    "text-chunks",
    "todo/write",
    "tool-call-chunks",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    // Sub-dispatches of `run_code`; the enclosing `tool/call` already counts.
    "tool/code-dispatch",
    "tool/code-dispatch-start",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
];

/// Held token usage for one `(turn, step)`, kept last-wins until the next step arrives.
///
/// Each step is reported as an early `assistant/chunk` sample and again on the assembled `assistant/message`.
#[derive(Debug, Default)]
struct UsageSample {
    turn: i64,
    step: i64,
    model: String,
    tokens: DshTokens,
}

/// One model's accumulated tokens, in vct's disjoint buckets.
#[derive(Debug, Clone, Copy, Default)]
struct DshTokens {
    input: i64,
    output: i64,
    reasoning: i64,
    cache_read: i64,
    cache_write: i64,
}

impl DshTokens {
    /// Returns disjoint token counts parsed from a `TokenUsage` object.
    ///
    /// `inputTokens` already excludes cache reads; `outputTokens` includes `reasoningTokens` and is reduced accordingly.
    fn from_usage(usage: &Map<String, Value>) -> Self {
        let field = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0);
        let reasoning = field("reasoningTokens");
        Self {
            input: field("inputTokens"),
            output: (field("outputTokens") - reasoning).max(0),
            reasoning,
            cache_read: field("cacheReadTokens"),
            cache_write: field("cacheWriteTokens"),
        }
    }

    fn merge(&mut self, other: Self) {
        self.input += other.input;
        self.output += other.output;
        self.reasoning += other.reasoning;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    /// Returns the prompt token count compared against a model's context-tier thresholds.
    fn request_context(self) -> i64 {
        self.input + self.cache_read + self.cache_write
    }
}

/// Per-model running totals and above-tier slices (`above_tiers[n]` holds price level `n + 1`; the base level is the remainder).
#[derive(Debug, Default)]
struct ModelUsage {
    total: DshTokens,
    above_tiers: Vec<DshTokens>,
}

impl ModelUsage {
    /// Merges `tokens` into the slice for price `level` (level 0 is the base level and stores no slice).
    fn classify(&mut self, level: usize, tokens: DshTokens) {
        if level == 0 || level > MAX_TIER_LEVELS {
            return;
        }
        if self.above_tiers.len() < level {
            self.above_tiers.resize(level, DshTokens::default());
        }
        self.above_tiers[level - 1].merge(tokens);
    }
}

/// A `tool/call` waiting for the `tool/result` that reports its outcome.
#[derive(Debug)]
struct PendingCall {
    name: String,
    /// Raw JSON argument string from the model, parsed only when the paired result succeeds.
    arguments: String,
}

/// Parses one `dsh` session log into a [`ParsedAnalysis`].
pub(crate) fn parse_dsh_session(
    path: &Path,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<ParsedAnalysis> {
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);
    let compressed = reader
        .fill_buf()
        .with_context(|| format!("Failed to read file: {}", path.display()))?
        .starts_with(&ZSTD_MAGIC);

    let mut parser = DshParser::new(mode, tiers);
    let truncated = if compressed {
        let decoder = zstd::stream::read::Decoder::new(reader)
            .with_context(|| format!("Failed to open Zstandard stream: {}", path.display()))?;
        parser.read_lines(BufReader::with_capacity(buffer::FILE_READ_BUFFER, decoder))
    } else {
        parser.read_lines(reader)
    };

    // Reject unknown format versions because envelope semantics may have changed.
    if let Some(version) = parser.unsupported_version {
        bail!(
            "DeepSeek session {} uses format version {version}, but only version {SUPPORTED_FORMAT_VERSION} is supported",
            path.display()
        );
    }

    // A live session can end mid-frame: keep partial records if any decoded, and fail only when nothing decoded.
    if let Some(error) = truncated {
        if parser.diagnostics.source_records == 0 {
            return Err(error)
                .with_context(|| format!("Failed to read DeepSeek session: {}", path.display()));
        }
        parser.diagnostics.record_malformed();
    }

    Ok(parser.finish())
}

/// Returns whether `path` begins with the zstd frame magic, returning `false` if unreadable or shorter than 4 bytes.
pub(crate) fn has_zstd_magic(path: &Path) -> bool {
    File::open(path).is_ok_and(|file| {
        BufReader::with_capacity(ZSTD_MAGIC.len(), file)
            .fill_buf()
            .is_ok_and(|header| header.starts_with(&ZSTD_MAGIC))
    })
}

/// Returns whether a record is a `dsh` session header line.
pub(crate) fn is_dsh_header(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.get("type").and_then(Value::as_str) == Some("session")
        && object.get("version").is_some_and(Value::is_number)
        && object.get("id").is_some_and(Value::is_string)
}

#[derive(Deserialize)]
struct BorrowedDshRecord<'a> {
    #[serde(borrow, rename = "type")]
    kind: Cow<'a, str>,
    #[serde(default)]
    time: Option<i64>,
    #[serde(default)]
    seq: Option<i64>,
    #[serde(default)]
    version: Option<i64>,
    #[serde(default, borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    cwd: Option<Cow<'a, str>>,
    #[serde(default, rename = "createdAt")]
    created_at: Option<i64>,
    #[serde(default, borrow)]
    data: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedRequestContext<'a> {
    #[serde(default, borrow)]
    model: Option<Cow<'a, str>>,
}

#[derive(Default, Deserialize)]
struct BorrowedChunkData<'a> {
    #[serde(default)]
    turn: Option<i64>,
    #[serde(default)]
    step: Option<i64>,
    #[serde(default, borrow)]
    chunk: Option<BorrowedChunk<'a>>,
}

fn deserialize_present_raw<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<&'de RawValue>, D::Error>
where
    D: Deserializer<'de>,
{
    <&'de RawValue>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct BorrowedChunk<'a> {
    #[serde(default, borrow, rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    usage: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedToolCallData<'a> {
    #[serde(default, borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    arguments: Option<Cow<'a, str>>,
}

struct DshParser<'a> {
    state: SessionParseState,
    usage: FastHashMap<String, ModelUsage>,
    diagnostics: ParseDiagnostics,
    classifier: Option<TierClassifier<'a>>,
    /// Model from the latest `request/context`, which is only emitted on route changes.
    route_model: String,
    pending_usage: Option<UsageSample>,
    pending_calls: HashMap<i64, PendingCall>,
    /// Set when the header declares a format version this parser cannot read.
    unsupported_version: Option<i64>,
}

impl<'a> DshParser<'a> {
    fn new(mode: ParseMode, tiers: Option<&'a TierThresholds>) -> Self {
        Self {
            state: SessionParseState::with_mode(mode),
            usage: FastHashMap::default(),
            diagnostics: ParseDiagnostics::default(),
            classifier: tiers.map(TierClassifier::new),
            route_model: String::new(),
            pending_usage: None,
            pending_calls: HashMap::new(),
            unsupported_version: None,
        }
    }

    /// Consumes every complete line, returning the read error that stopped it.
    fn read_lines<R: BufRead>(&mut self, mut reader: R) -> Option<std::io::Error> {
        let mut line = Vec::with_capacity(buffer::AVG_JSONL_LINE_SIZE);
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) => return Some(error),
            }
            let Ok(text) = std::str::from_utf8(&line) else {
                return Some(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "stream did not contain valid UTF-8",
                ));
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(env) = serde_json::from_str::<BorrowedDshRecord<'_>>(trimmed)
                && self.push_borrowed(env)
            {
                continue;
            }
            match serde_json::from_str::<Value>(trimmed) {
                Ok(record) => self.push(&record),
                Err(_) => self.diagnostics.record_malformed(),
            }
        }
        None
    }

    /// Handles standard envelope records without `Value` allocation, returning `false` before mutating state when [`Self::push`] is needed.
    fn push_borrowed(&mut self, env: BorrowedDshRecord<'_>) -> bool {
        let kind = env.kind.as_ref();
        if IGNORED_RECORD_TYPES.contains(&kind) {
            if let Some(time) = env.time {
                self.state.last_ts = self.state.last_ts.max(time);
            }
            self.diagnostics.record_recognized_source();
            return true;
        }

        match kind {
            "session" => {
                if let Some(time) = env.time {
                    self.state.last_ts = self.state.last_ts.max(time);
                }
                match env.version {
                    Some(SUPPORTED_FORMAT_VERSION) => {
                        if let Some(id) = env.id {
                            self.state.task_id = id.into_owned();
                        }
                        if let Some(cwd) = env.cwd {
                            self.state.folder_path = cwd.into_owned();
                        }
                        if let Some(created) = env.created_at {
                            self.state.last_ts = self.state.last_ts.max(created);
                        }
                    }
                    version => {
                        self.unsupported_version = Some(version.unwrap_or(-1));
                    }
                }
                self.diagnostics.record_recognized_source();
                true
            }
            "request/context" => {
                let model = match env.data {
                    Some(raw) => {
                        let Ok(ctx) = serde_json::from_str::<BorrowedRequestContext<'_>>(raw.get())
                        else {
                            return false;
                        };
                        ctx.model
                    }
                    None => None,
                };
                if let Some(time) = env.time {
                    self.state.last_ts = self.state.last_ts.max(time);
                }
                if let Some(model) = model {
                    self.route_model.clear();
                    self.route_model.push_str(&model);
                }
                self.diagnostics.record_recognized_source();
                true
            }
            "assistant/chunk" => {
                let parsed_chunk = match env.data {
                    Some(raw) => {
                        let Ok(data) = serde_json::from_str::<BorrowedChunkData<'_>>(raw.get())
                        else {
                            return false;
                        };
                        data
                    }
                    None => BorrowedChunkData::default(),
                };
                if parsed_chunk
                    .chunk
                    .as_ref()
                    .and_then(|chunk| chunk.kind.as_deref())
                    != Some("usage")
                {
                    if let Some(time) = env.time {
                        self.state.last_ts = self.state.last_ts.max(time);
                    }
                    self.diagnostics.record_recognized_source();
                    return true;
                }
                let Some(usage_raw) = parsed_chunk.chunk.and_then(|chunk| chunk.usage) else {
                    if let Some(time) = env.time {
                        self.state.last_ts = self.state.last_ts.max(time);
                    }
                    self.diagnostics.record_recognized_source();
                    return true;
                };
                let Ok(usage_val) = serde_json::from_str::<Value>(usage_raw.get()) else {
                    return false;
                };
                if let Some(time) = env.time {
                    self.state.last_ts = self.state.last_ts.max(time);
                }
                let model = self.route_model.clone();
                self.apply_usage_fields(
                    parsed_chunk.turn.unwrap_or(0),
                    parsed_chunk.step.unwrap_or(0),
                    Some(&usage_val),
                    model,
                );
                self.diagnostics.record_recognized_source();
                true
            }
            "tool/call" => {
                let call = match env.data {
                    Some(raw) => {
                        let Ok(call) = serde_json::from_str::<BorrowedToolCallData<'_>>(raw.get())
                        else {
                            return false;
                        };
                        Some(call)
                    }
                    None => None,
                };
                if let Some(time) = env.time {
                    self.state.last_ts = self.state.last_ts.max(time);
                }
                if let (Some(seq), Some(call)) = (env.seq, call)
                    && let (Some(name), Some(arguments)) = (call.name, call.arguments)
                {
                    self.pending_calls.insert(
                        seq,
                        PendingCall {
                            name: name.into_owned(),
                            arguments: arguments.into_owned(),
                        },
                    );
                }
                self.diagnostics.record_recognized_source();
                true
            }
            "assistant/message" | "tool/result" => false,
            _ => {
                if let Some(time) = env.time {
                    self.state.last_ts = self.state.last_ts.max(time);
                }
                self.diagnostics.record_unrecognized();
                true
            }
        }
    }

    fn push(&mut self, record: &Value) {
        let Some(object) = record.as_object() else {
            self.diagnostics.record_unrecognized();
            return;
        };
        let Some(kind) = object.get("type").and_then(Value::as_str) else {
            self.diagnostics.record_unrecognized();
            return;
        };

        if let Some(time) = object.get("time").and_then(Value::as_i64) {
            self.state.last_ts = self.state.last_ts.max(time);
        }
        let data = object.get("data").unwrap_or(&Value::Null);

        match kind {
            "session" => self.apply_header(object),
            "request/context" => {
                if let Some(model) = data.get("model").and_then(Value::as_str) {
                    self.route_model = model.to_string();
                }
            }
            "assistant/message" => self.apply_assistant_message(data),
            "assistant/chunk" => self.apply_chunk(data),
            "tool/call" => self.apply_tool_call(object, data),
            "tool/result" => self.apply_tool_result(object, data),
            _ if IGNORED_RECORD_TYPES.contains(&kind) => {}
            _ => {
                self.diagnostics.record_unrecognized();
                return;
            }
        }
        self.diagnostics.record_recognized_source();
    }

    fn apply_header(&mut self, object: &Map<String, Value>) {
        match object.get("version").and_then(Value::as_i64) {
            Some(SUPPORTED_FORMAT_VERSION) => {}
            version => {
                self.unsupported_version = Some(version.unwrap_or(-1));
                return;
            }
        }
        if let Some(id) = object.get("id").and_then(Value::as_str) {
            self.state.task_id = id.to_string();
        }
        // `cwd` is absent outside a workspace, and the directory name slug is lossy, so there is no fallback.
        if let Some(cwd) = object.get("cwd").and_then(Value::as_str) {
            self.state.folder_path = cwd.to_string();
        }
        if let Some(created) = object.get("createdAt").and_then(Value::as_i64) {
            self.state.last_ts = self.state.last_ts.max(created);
        }
    }

    fn apply_assistant_message(&mut self, data: &Value) {
        let model = data
            .pointer("/message/source/model")
            .and_then(Value::as_str)
            .unwrap_or(&self.route_model)
            .to_string();
        self.apply_usage(data, data.get("usage"), model);
    }

    fn apply_chunk(&mut self, data: &Value) {
        if data.pointer("/chunk/type").and_then(Value::as_str) != Some("usage") {
            return;
        }
        let model = self.route_model.clone();
        self.apply_usage(data, data.pointer("/chunk/usage"), model);
    }

    /// Records one step's usage sample in the last-wins slot, flushing the prior step when `(turn, step)` changes.
    fn apply_usage(&mut self, data: &Value, usage: Option<&Value>, model: String) {
        self.apply_usage_fields(
            data.get("turn").and_then(Value::as_i64).unwrap_or(0),
            data.get("step").and_then(Value::as_i64).unwrap_or(0),
            usage,
            model,
        );
    }

    fn apply_usage_fields(&mut self, turn: i64, step: i64, usage: Option<&Value>, model: String) {
        // Missing `usage` means the adapter reported none, not a schema failure.
        let Some(usage) = usage else {
            return;
        };
        let Some(usage) = usage.as_object() else {
            self.diagnostics.record_relevant(false);
            return;
        };
        let sample = UsageSample {
            turn,
            step,
            model,
            tokens: DshTokens::from_usage(usage),
        };
        match &self.pending_usage {
            Some(held) if held.turn == sample.turn && held.step == sample.step => {}
            _ => self.flush_usage(),
        }
        self.pending_usage = Some(sample);
    }

    /// Folds the held usage sample into per-model totals and records one diagnostics verdict for the surviving `(turn, step)` report.
    ///
    /// Usage without a resolved model is recorded as an unnormalized relevant record.
    fn flush_usage(&mut self) {
        let Some(sample) = self.pending_usage.take() else {
            return;
        };
        self.diagnostics.record_relevant(!sample.model.is_empty());
        if sample.model.is_empty() {
            return;
        }
        let level = self.classifier.as_mut().map_or(0, |classifier| {
            classifier.level(&sample.model, sample.tokens.request_context())
        });
        let entry = self.usage.entry(sample.model).or_default();
        entry.total.merge(sample.tokens);
        entry.classify(level, sample.tokens);
    }

    fn apply_tool_call(&mut self, object: &Map<String, Value>, data: &Value) {
        let (Some(seq), Some(name), Some(arguments)) = (
            object.get("seq").and_then(Value::as_i64),
            data.get("name").and_then(Value::as_str),
            data.get("arguments").and_then(Value::as_str),
        ) else {
            return;
        };
        self.pending_calls.insert(
            seq,
            PendingCall {
                name: name.to_string(),
                arguments: arguments.to_string(),
            },
        );
    }

    /// Pairs a `tool/result` with its pending `tool/call` via `sourceEventSeqs` and records its metrics unless it reports an `error`.
    fn apply_tool_result(&mut self, object: &Map<String, Value>, data: &Value) {
        // Search every `sourceEventSeqs` entry rather than index 0 because multiple events can be listed, and the result payload alone does not name the tool.
        let Some(call) = object
            .get("sourceEventSeqs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .find_map(|seq| self.pending_calls.remove(&seq))
        else {
            return;
        };
        if data.get("error").is_some_and(|error| !error.is_null()) {
            return;
        }

        let arguments = serde_json::from_str::<Value>(&call.arguments).unwrap_or(Value::Null);
        let meta = data.get("meta").unwrap_or(&Value::Null);
        let timestamp = object
            .get("time")
            .and_then(Value::as_i64)
            .unwrap_or(self.state.last_ts);

        let applied = match call.name.as_str() {
            "read" | "read_image" => self.apply_read(&arguments, meta, timestamp),
            "write" => self.apply_write(&arguments, timestamp),
            "edit" | "str_replace_editor" => self.apply_edit(&arguments, timestamp),
            "bash" | "pwsh" => self.apply_bash(&arguments, timestamp),
            "todo_write" => {
                self.state.tool_counts.todo_write += 1;
                true
            }
            // Tool sets are plugin-composable, so unmapped tool names are expected.
            _ => return,
        };
        self.diagnostics.record_relevant(applied);
    }

    fn apply_read(&mut self, arguments: &Value, meta: &Value, timestamp: i64) -> bool {
        // Prefer `meta.path` (resolved absolute path) over `arguments.file_path`, which may be relative to cwd.
        let path = meta
            .get("path")
            .and_then(Value::as_str)
            .or_else(|| arguments.get("file_path").and_then(Value::as_str))
            .unwrap_or_default();
        if path.is_empty() {
            return false;
        }

        let content = meta
            .get("lines")
            .and_then(Value::as_array)
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|line| line.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if content.is_empty() {
            // An empty file, or an image read, which reports no text lines.
            self.state.tool_counts.read += 1;
            self.state.add_non_text_read_path(path);
        } else {
            self.state.add_read_detail(path, &content, timestamp);
        }
        true
    }

    fn apply_write(&mut self, arguments: &Value, timestamp: i64) -> bool {
        // Use the call's `content` because diff meta is empty for new files and pads hunks with context lines.
        let Some(path) = arguments.get("file_path").and_then(Value::as_str) else {
            return false;
        };
        let content = arguments
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        self.state.add_write_detail(path, content, timestamp);
        true
    }

    fn apply_edit(&mut self, arguments: &Value, timestamp: i64) -> bool {
        let Some(path) = arguments.get("file_path").and_then(Value::as_str) else {
            return false;
        };
        let old = arguments
            .get("old_string")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let new = arguments
            .get("new_string")
            .and_then(Value::as_str)
            .unwrap_or_default();
        self.state.add_edit_detail(path, old, new, timestamp);
        true
    }

    fn apply_bash(&mut self, arguments: &Value, timestamp: i64) -> bool {
        let Some(command) = arguments.get("command").and_then(Value::as_str) else {
            return false;
        };
        let description = arguments
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        self.state.add_run_command(command, description, timestamp);
        true
    }

    fn finish(mut self) -> ParsedAnalysis {
        self.flush_usage();

        let mut conversation_usage = FastHashMap::default();
        for (model, usage) in self.usage {
            let mut value = json!({
                "input_tokens": usage.total.input,
                "output_tokens": usage.total.output,
                "reasoning_output_tokens": usage.total.reasoning,
                "cache_read_input_tokens": usage.total.cache_read,
                "cache_creation_input_tokens": usage.total.cache_write,
            });
            let mut above = Map::new();
            for (index, slice) in usage.above_tiers.iter().enumerate() {
                write_tier_slice(
                    &mut above,
                    index + 1,
                    &TierSlice {
                        input_tokens: slice.input,
                        output_tokens: slice.output,
                        reasoning_tokens: slice.reasoning,
                        cache_read: slice.cache_read,
                        cache_creation_5m: slice.cache_write,
                        cache_creation_1h: 0,
                    },
                );
            }
            if !above.is_empty() {
                value["above_tier"] = Value::Object(above);
            }
            conversation_usage.insert(model, value);
        }

        ParsedAnalysis::new(
            CodeAnalysis {
                user: String::new(),
                extension_name: String::new(),
                insights_version: String::new(),
                machine_id: String::new(),
                records: vec![self.state.into_record(conversation_usage)],
            },
            self.diagnostics,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const HEADER: &str = r#"{"type":"session","version":0,"id":"session-a","createdAt":1,"cwd":"/repo","delegationDepth":0}"#;

    fn usage_event(seq: i64, turn: i64, step: i64, model: &str, output: i64) -> String {
        json!({
            "type": "assistant/message",
            "seq": seq,
            "time": 1000 + seq,
            "data": {
                "turn": turn,
                "step": step,
                "message": {"source": {"kind": "model", "provider": "p", "model": model}},
                "usage": {"inputTokens": 1, "outputTokens": output},
            },
        })
        .to_string()
    }

    /// Writes one zstd frame per line batch, matching how `dsh` appends batches.
    fn write_frames(dir: &TempDir, name: &str, batches: &[Vec<String>]) -> std::path::PathBuf {
        let mut bytes = Vec::new();
        for batch in batches {
            let mut plain = batch.join("\n");
            plain.push('\n');
            bytes.extend(zstd::stream::encode_all(plain.as_bytes(), 0).expect("compress batch"));
        }
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).expect("write log");
        path
    }

    fn parse(path: &std::path::Path) -> ParsedAnalysis {
        parse_dsh_session(path, ParseMode::Full, None).expect("parse session")
    }

    #[test]
    fn each_step_lands_in_the_price_level_its_own_context_reached() {
        let sized_event = |seq: i64, step: i64, input: i64| {
            json!({
                "type": "assistant/message",
                "seq": seq,
                "time": 1000 + seq,
                "data": {
                    "turn": 1,
                    "step": step,
                    "message": {"source": {"kind": "model", "provider": "p", "model": "qwen3"}},
                    "usage": {"inputTokens": input, "outputTokens": 100},
                },
            })
            .to_string()
        };
        let dir = TempDir::new().unwrap();
        let path = write_frames(
            &dir,
            "session.jsonl.zstd",
            &[
                vec![HEADER.to_string()],
                vec![sized_event(1, 1, 10_000)],
                vec![sized_event(2, 2, 60_000)],
                vec![sized_event(3, 3, 500_000)],
            ],
        );

        let tiers = crate::pricing::TierThresholds::from_entries(
            [("qwen3", vec![31_999, 127_999, 255_999])].into_iter(),
        );
        let parsed =
            parse_dsh_session(&path, ParseMode::UsageOnly, Some(&tiers)).expect("parse session");
        let above = &parsed.analysis.records[0].conversation_usage["qwen3"]["above_tier"];

        // First step stays at base level; the other two must land in distinct above-tier slices.
        assert_eq!(above["level_1_input_tokens"], 60_000);
        assert_eq!(above["level_3_input_tokens"], 500_000);
        assert!(above.get("level_2_input_tokens").is_none());
    }

    #[test]
    fn decodes_every_frame_in_a_concatenated_log() {
        let dir = TempDir::new().unwrap();
        let path = write_frames(
            &dir,
            "session.jsonl.zstd",
            &[
                vec![HEADER.to_string()],
                vec![usage_event(1, 1, 1, "m", 10)],
                vec![usage_event(2, 1, 2, "m", 20)],
                vec![usage_event(3, 1, 3, "m", 30)],
            ],
        );

        let record = &parse(&path).analysis.records[0];
        assert_eq!(record.task_id, "session-a");
        // A single-frame decoder would stop after the header and report zero.
        assert_eq!(record.conversation_usage["m"]["output_tokens"], 60);
    }

    #[test]
    fn keeps_records_decoded_before_a_torn_trailing_frame() {
        let dir = TempDir::new().unwrap();
        let path = write_frames(
            &dir,
            "session.jsonl.zstd",
            &[
                vec![HEADER.to_string()],
                vec![usage_event(1, 1, 1, "m", 10)],
                vec![usage_event(2, 1, 2, "m", 20)],
            ],
        );
        // Simulate a live session ending mid-frame by truncating the trailing frame.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 5);
        std::fs::write(&path, bytes).unwrap();

        let parsed = parse(&path);
        assert_eq!(
            parsed.analysis.records[0].conversation_usage["m"]["output_tokens"],
            10
        );
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
    }

    #[test]
    fn rejects_a_log_whose_very_first_frame_is_unreadable() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl.zstd");
        // Valid zstd magic followed by corrupt bytes must error instead of producing an empty session.
        std::fs::write(&path, [&ZSTD_MAGIC[..], &[0xff; 16]].concat()).unwrap();

        assert!(parse_dsh_session(&path, ParseMode::Full, None).is_err());
    }

    #[test]
    fn refuses_a_foreign_format_version() {
        let dir = TempDir::new().unwrap();
        let header =
            r#"{"type":"session","version":1,"id":"session-a","createdAt":1,"delegationDepth":0}"#;
        let path = write_frames(&dir, "session.jsonl.zstd", &[vec![header.to_string()]]);

        let error = parse_dsh_session(&path, ParseMode::Full, None)
            .expect_err("a version with unknown semantics must not be parsed optimistically");
        assert!(error.to_string().contains("format version 1"), "{error}");
    }

    #[test]
    fn an_unknown_event_type_is_drift_but_a_known_one_is_not() {
        let dir = TempDir::new().unwrap();
        let known =
            json!({"type": "permission/preset", "seq": 1, "time": 2, "data": {}}).to_string();
        let unknown =
            json!({"type": "quantum/entangled", "seq": 2, "time": 3, "data": {}}).to_string();
        let path = write_frames(
            &dir,
            "session.jsonl.zstd",
            &[vec![HEADER.to_string()], vec![known, unknown]],
        );

        let parsed = parse(&path);
        assert_eq!(parsed.diagnostics.unrecognized_records, 1);
        assert_eq!(parsed.diagnostics.recognized_records, 2);
    }

    #[test]
    fn one_step_reported_twice_is_one_relevant_record() {
        let dir = TempDir::new().unwrap();
        let route = json!({"type": "request/context", "seq": 1, "time": 2, "data": {"model": "m"}})
            .to_string();
        let chunk = json!({
            "type": "assistant/chunk",
            "seq": 2,
            "time": 3,
            "data": {
                "turn": 1,
                "step": 1,
                "chunk": {"type": "usage", "usage": {"inputTokens": 1, "outputTokens": 2}},
            },
        })
        .to_string();
        let path = write_frames(
            &dir,
            "session.jsonl.zstd",
            &[
                vec![HEADER.to_string()],
                vec![route, chunk, usage_event(3, 1, 1, "m", 2)],
            ],
        );

        let parsed = parse(&path);
        assert_eq!(
            parsed.analysis.records[0].conversation_usage["m"]["output_tokens"],
            2
        );
        assert_eq!(parsed.diagnostics.relevant_records, 1);
        assert_eq!(parsed.diagnostics.normalized_records, 1);
    }

    #[test]
    fn usage_with_no_route_is_a_failed_record_not_a_silent_drop() {
        let dir = TempDir::new().unwrap();
        // Usage chunk arrives before any `request/context`, leaving its model unresolved.
        let chunk = json!({
            "type": "assistant/chunk",
            "seq": 1,
            "time": 2,
            "data": {
                "turn": 1,
                "step": 1,
                "chunk": {"type": "usage", "usage": {"inputTokens": 5, "outputTokens": 7}},
            },
        })
        .to_string();
        let path = write_frames(
            &dir,
            "session.jsonl.zstd",
            &[vec![HEADER.to_string()], vec![chunk]],
        );

        let parsed = parse(&path);
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
        assert_eq!(parsed.diagnostics.relevant_records, 1);
        assert_eq!(parsed.diagnostics.failed_relevant_records, 1);
    }

    #[test]
    fn a_result_pairs_on_any_source_event_seq() {
        let dir = TempDir::new().unwrap();
        let call = json!({
            "type": "tool/call",
            "seq": 4,
            "time": 5,
            "data": {
                "name": "write",
                "arguments": r#"{"file_path":"/repo/new.rs","content":"one\ntwo\n"}"#,
            },
        })
        .to_string();
        // Call seq is at index 1 to ensure pairing scans all `sourceEventSeqs` entries.
        let result = json!({
            "type": "tool/result",
            "seq": 6,
            "time": 7,
            "sourceEventSeqs": [3, 4],
            "data": {"meta": {}},
        })
        .to_string();
        let path = write_frames(
            &dir,
            "session.jsonl.zstd",
            &[vec![HEADER.to_string()], vec![call, result]],
        );

        let record = &parse(&path).analysis.records[0];
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.total_write_lines, 2);
    }

    #[test]
    fn an_empty_log_is_a_valid_blank_session() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl.zstd");
        std::fs::write(&path, []).unwrap();

        let parsed = parse(&path);
        assert!(!parsed.diagnostics.is_complete_failure());
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }

    #[test]
    fn explicit_null_chunk_usage_is_reported_as_schema_drift() {
        let dir = TempDir::new().unwrap();
        let route = json!({
            "type": "request/context",
            "seq": 1,
            "time": 1,
            "data": {"model": "deepseek-reasoner"},
        })
        .to_string();
        let chunk = json!({
            "type": "assistant/chunk",
            "seq": 2,
            "time": 2,
            "data": {
                "turn": 1,
                "step": 1,
                "chunk": {"type": "usage", "usage": null},
            },
        })
        .to_string();
        let path = write_frames(
            &dir,
            "session.jsonl.zstd",
            &[vec![HEADER.to_string()], vec![route, chunk]],
        );

        let parsed = parse(&path);
        assert_eq!(parsed.diagnostics.relevant_records, 1);
        assert_eq!(parsed.diagnostics.failed_relevant_records, 1);
        assert!(parsed.diagnostics.is_complete_failure());
    }
}
