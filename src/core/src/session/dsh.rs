//! DeepSeek Harness (`dsh`) session-log parser.
//!
//! `dsh` writes one append-only JSONL log per session, either as concatenated zstd frames (header frame followed by one frame per append batch) or as uncompressed `session.jsonl` when `compression: 'none'`. When a live session ends inside an incomplete trailing frame, the reader keeps every record decoded before the error instead of rejecting the file.

use crate::constants::{FastHashMap, buffer};
use crate::models::CodeAnalysis;
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::state::{
    ParseMode, SessionParseState, StreamingTextCounter, count_json_string_metrics,
};
use crate::utils::token_merge::write_tier_slice;
use crate::utils::{MAX_TIER_LEVELS, TierSlice};
use anyhow::{Context, Result, bail};
use serde::{
    Deserialize, Deserializer,
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value, json, value::RawValue};
use std::borrow::Cow;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Magic bytes opening every zstd frame (`0xFD2FB528`, little-endian).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Only `SESSION_FORMAT_VERSION` supported by this parser; any other version fails the parse.
const SUPPORTED_FORMAT_VERSION: i64 = 0;

/// Returns whether `kind` is a known `dsh` event type or packed chunk-row tag carrying no extractable metrics.
///
/// Listed explicitly to flag unknown future event types as drift; mirrors upstream `KNOWN_SESSION_EVENT_TYPES` minus types handled in [`DshParser::push_borrowed`], plus the three slash-less packed chunk tags.
fn is_ignored_record_type(kind: &str) -> bool {
    matches!(
        kind,
        "agent-preset/selected"
            | "agent/inbox/spliced"
            | "approval/asked"
            | "approval/decided"
            | "approval/policy"
            | "command/done"
            | "command/run"
            | "compaction/end"
            | "compaction/prune"
            | "compaction/start"
            | "compaction/summary"
            | "feedback/record"
            | "goal/change"
            | "hook/invoked"
            | "hook/result"
            | "llm/retry"
            | "llm/retry-started"
            | "permission/preset"
            | "plan/mode"
            | "reasoning-chunks"
            | "request/header"
            | "sandbox/mode"
            | "schedule/change"
            | "session/end-seed"
            | "session/title"
            | "session/title-llm-request"
            | "step/end"
            | "step/start"
            | "subagent/descriptor"
            | "text-chunks"
            | "todo/write"
            | "tool-call-chunks"
            | "tool-workflow/agent-end"
            | "tool-workflow/agent-start"
            | "tool-workflow/run-end"
            | "tool-workflow/run-start"
            // Sub-dispatches of `run_code`; the enclosing `tool/call` already counts.
            | "tool/code-dispatch"
            | "tool/code-dispatch-start"
            | "turn/end"
            | "turn/start"
            | "user/message"
            | "web/deepseek-search-llm-request"
    )
}

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

enum ParsedDshUsage {
    Valid(DshTokens),
    Invalid,
}

struct OptI64(Option<i64>);

impl<'de> Deserialize<'de> for OptI64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OptI64;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_i64<E>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(OptI64(Some(v)))
            }

            fn visit_u64<E>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(OptI64(i64::try_from(v).ok()))
            }

            fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> {
                Ok(OptI64(None))
            }

            fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(OptI64(None))
            }

            fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> {
                Ok(OptI64(None))
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(OptI64(None))
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(OptI64(None))
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(OptI64(None))
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(OptI64(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

impl<'de> Deserialize<'de> for ParsedDshUsage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ParsedDshUsage;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a DeepSeek TokenUsage object")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut input = 0_i64;
                let mut output_total = 0_i64;
                let mut reasoning = 0_i64;
                let mut cache_read = 0_i64;
                let mut cache_write = 0_i64;

                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "inputTokens" => {
                            input = map.next_value::<OptI64>()?.0.unwrap_or(0);
                        }
                        "outputTokens" => {
                            output_total = map.next_value::<OptI64>()?.0.unwrap_or(0);
                        }
                        "reasoningTokens" => {
                            reasoning = map.next_value::<OptI64>()?.0.unwrap_or(0);
                        }
                        "cacheReadTokens" => {
                            cache_read = map.next_value::<OptI64>()?.0.unwrap_or(0);
                        }
                        "cacheWriteTokens" => {
                            cache_write = map.next_value::<OptI64>()?.0.unwrap_or(0);
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }

                Ok(ParsedDshUsage::Valid(DshTokens {
                    input,
                    output: (output_total - reasoning).max(0),
                    reasoning,
                    cache_read,
                    cache_write,
                }))
            }

            fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> {
                Ok(ParsedDshUsage::Invalid)
            }

            fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E> {
                Ok(ParsedDshUsage::Invalid)
            }

            fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E> {
                Ok(ParsedDshUsage::Invalid)
            }

            fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
                Ok(ParsedDshUsage::Invalid)
            }

            fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> {
                Ok(ParsedDshUsage::Invalid)
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(ParsedDshUsage::Invalid)
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(ParsedDshUsage::Invalid)
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(ParsedDshUsage::Invalid)
            }
        }
        deserializer.deserialize_any(V)
    }
}

fn deserialize_present_dsh_usage<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<ParsedDshUsage>, D::Error>
where
    D: Deserializer<'de>,
{
    ParsedDshUsage::deserialize(deserializer).map(Some)
}

fn deserialize_present_raw<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<&'de RawValue>, D::Error>
where
    D: Deserializer<'de>,
{
    <&'de RawValue>::deserialize(deserializer).map(Some)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DshToolKind {
    Read,
    Write,
    Edit,
    Bash,
    TodoWrite,
    Unmapped,
}

impl DshToolKind {
    fn from_name(name: &str) -> Self {
        match name {
            "read" | "read_image" => Self::Read,
            "write" => Self::Write,
            "edit" | "str_replace_editor" => Self::Edit,
            "bash" | "pwsh" => Self::Bash,
            "todo_write" => Self::TodoWrite,
            _ => Self::Unmapped,
        }
    }

    fn needs_arguments(self) -> bool {
        matches!(self, Self::Read | Self::Write | Self::Edit | Self::Bash)
    }
}

/// A `tool/call` waiting for the `tool/result` that reports its outcome.
#[derive(Debug)]
struct PendingCall {
    kind: DshToolKind,
    /// Raw JSON argument string from the model, parsed only when the paired result succeeds.
    arguments: String,
    /// Active model when `tool/call` was recorded, or empty if no model had appeared yet.
    model: String,
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
        let decoder = zstd::stream::read::Decoder::with_buffer(reader)
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
    #[serde(default, borrow, rename = "sourceEventSeqs")]
    source_event_seqs: Option<&'a RawValue>,
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

#[derive(Deserialize)]
struct BorrowedChunk<'a> {
    #[serde(default, borrow, rename = "type")]
    kind: Option<Cow<'a, str>>,
    #[serde(default, deserialize_with = "deserialize_present_dsh_usage")]
    usage: Option<ParsedDshUsage>,
}

#[derive(Default, Deserialize)]
struct BorrowedAssistantMessageData<'a> {
    #[serde(default)]
    turn: Option<i64>,
    #[serde(default)]
    step: Option<i64>,
    #[serde(default, borrow)]
    message: Option<BorrowedAssistantMessageBody<'a>>,
    #[serde(default, deserialize_with = "deserialize_present_dsh_usage")]
    usage: Option<ParsedDshUsage>,
}

#[derive(Deserialize)]
struct BorrowedAssistantMessageBody<'a> {
    #[serde(default, borrow)]
    source: Option<BorrowedAssistantMessageSource<'a>>,
}

#[derive(Deserialize)]
struct BorrowedAssistantMessageSource<'a> {
    #[serde(default, borrow)]
    model: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct BorrowedToolCallData<'a> {
    #[serde(default, borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    arguments: Option<Cow<'a, str>>,
}

#[derive(Default, Deserialize)]
struct BorrowedToolResultData<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    error: Option<&'a RawValue>,
    #[serde(default, borrow)]
    meta: Option<&'a RawValue>,
}

#[derive(Default, Deserialize)]
struct BorrowedDshReadMeta<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    path: Option<&'a RawValue>,
    #[serde(default, borrow)]
    lines: Option<&'a RawValue>,
}

#[derive(Default, Deserialize)]
struct BorrowedDshReadLine<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    text: Option<&'a RawValue>,
}

#[derive(Default, Deserialize)]
struct BorrowedDshToolArgs<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    file_path: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    content: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    old_string: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    new_string: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    command: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    description: Option<&'a RawValue>,
}

fn raw_as_str<'a>(raw: Option<&'a RawValue>) -> Option<Cow<'a, str>> {
    let raw = raw?;
    if !raw.get().starts_with('"') {
        return None;
    }
    serde_json::from_str::<Cow<'a, str>>(raw.get()).ok()
}

struct ReadLinesStats {
    content_is_empty: bool,
    metrics: crate::session::state::TextMetrics,
}

fn count_borrowed_read_lines(lines_raw: Option<&RawValue>) -> ReadLinesStats {
    struct LinesVisitor;
    impl<'de> Visitor<'de> for LinesVisitor {
        type Value = ReadLinesStats;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a DeepSeek read meta.lines array")
        }

        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut text_count = 0_usize;
            let mut any_non_empty = false;
            let mut counter = StreamingTextCounter::default();

            while let Some(raw_item) = seq.next_element::<&'de RawValue>()? {
                if !raw_item.get().starts_with('{') {
                    continue;
                }
                let Ok(line) = serde_json::from_str::<BorrowedDshReadLine<'de>>(raw_item.get())
                else {
                    continue;
                };
                let Some(text_raw) = line.text else {
                    continue;
                };
                if !text_raw.get().starts_with('"') {
                    continue;
                }
                let mut next = counter;
                if text_count > 0 {
                    next.push_newline();
                }
                let Some(is_empty) = next.push_json_string(text_raw.get()) else {
                    continue;
                };
                counter = next;
                text_count += 1;
                if !is_empty {
                    any_non_empty = true;
                }
            }

            let content_is_empty = text_count == 0 || (text_count == 1 && !any_non_empty);
            Ok(ReadLinesStats {
                content_is_empty,
                metrics: counter.finish(),
            })
        }
    }

    let Some(raw) = lines_raw else {
        return ReadLinesStats {
            content_is_empty: true,
            metrics: Default::default(),
        };
    };
    if !raw.get().starts_with('[') {
        return ReadLinesStats {
            content_is_empty: true,
            metrics: Default::default(),
        };
    }
    let mut de = serde_json::Deserializer::from_str(raw.get());
    de.deserialize_seq(LinesVisitor).unwrap_or(ReadLinesStats {
        content_is_empty: true,
        metrics: Default::default(),
    })
}

fn join_borrowed_read_lines(lines_raw: Option<&RawValue>) -> String {
    struct LinesVisitor;
    impl<'de> Visitor<'de> for LinesVisitor {
        type Value = String;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a DeepSeek read meta.lines array")
        }

        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut joined = String::new();
            let mut first = true;

            while let Some(raw_item) = seq.next_element::<&'de RawValue>()? {
                if !raw_item.get().starts_with('{') {
                    continue;
                }
                let Ok(line) = serde_json::from_str::<BorrowedDshReadLine<'de>>(raw_item.get())
                else {
                    continue;
                };
                let Some(text) = raw_as_str(line.text) else {
                    continue;
                };
                if !first {
                    joined.push('\n');
                }
                first = false;
                joined.push_str(text.as_ref());
            }
            Ok(joined)
        }
    }

    let Some(raw) = lines_raw else {
        return String::new();
    };
    if !raw.get().starts_with('[') {
        return String::new();
    }
    let mut de = serde_json::Deserializer::from_str(raw.get());
    de.deserialize_seq(LinesVisitor).unwrap_or_default()
}

struct DshParser<'a> {
    state: SessionParseState,
    usage: FastHashMap<String, ModelUsage>,
    diagnostics: ParseDiagnostics,
    classifier: Option<TierClassifier<'a>>,
    /// Model from the latest `request/context`, which is only emitted on route changes.
    route_model: String,
    /// Most recent explicit `assistant/message` model, used if no `request/context` preceded a tool call.
    last_model: String,
    pending_usage: Option<UsageSample>,
    pending_calls: FastHashMap<i64, PendingCall>,
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
            last_model: String::new(),
            pending_usage: None,
            pending_calls: FastHashMap::default(),
            unsupported_version: None,
        }
    }

    fn active_model(&self) -> Option<&str> {
        if !self.last_model.is_empty() {
            Some(self.last_model.as_str())
        } else if !self.route_model.is_empty() {
            Some(self.route_model.as_str())
        } else {
            None
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
            if trimmed.starts_with('{')
                && let Ok(env) = serde_json::from_str::<BorrowedDshRecord<'_>>(trimmed)
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

    fn take_pending_call_from_seqs(&mut self, raw: Option<&RawValue>) -> Option<PendingCall> {
        struct SeqScan<'m>(&'m mut FastHashMap<i64, PendingCall>);
        impl<'de> Visitor<'de> for SeqScan<'_> {
            type Value = Option<PendingCall>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a sourceEventSeqs array")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut matched = None;
                while let Some(OptI64(entry)) = seq.next_element::<OptI64>()? {
                    if matched.is_none()
                        && let Some(id) = entry
                        && let Some(call) = self.0.remove(&id)
                    {
                        matched = Some(call);
                    }
                }
                Ok(matched)
            }
        }

        let raw = raw?;
        if !raw.get().starts_with('[') {
            return None;
        }
        let mut de = serde_json::Deserializer::from_str(raw.get());
        de.deserialize_seq(SeqScan(&mut self.pending_calls))
            .ok()
            .flatten()
    }

    /// Handles standard envelope records without `Value` allocation, returning `false` before mutating state when [`Self::push`] is needed.
    fn push_borrowed(&mut self, env: BorrowedDshRecord<'_>) -> bool {
        let kind = env.kind.as_ref();
        if is_ignored_record_type(kind) {
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
                    Some(raw) if raw.get().starts_with('{') => {
                        let Ok(ctx) = serde_json::from_str::<BorrowedRequestContext<'_>>(raw.get())
                        else {
                            return false;
                        };
                        ctx.model
                    }
                    Some(_) => return false,
                    None => None,
                };
                if let Some(time) = env.time {
                    self.state.last_ts = self.state.last_ts.max(time);
                }
                if let Some(model) = model {
                    self.last_model.clear();
                    self.route_model.clear();
                    self.route_model.push_str(&model);
                    self.state.note_model(&self.route_model);
                }
                self.diagnostics.record_recognized_source();
                true
            }
            "assistant/chunk" => {
                let parsed_chunk = match env.data {
                    Some(raw) if raw.get().starts_with('{') => {
                        let Ok(data) = serde_json::from_str::<BorrowedChunkData<'_>>(raw.get())
                        else {
                            return false;
                        };
                        data
                    }
                    Some(_) => return false,
                    None => BorrowedChunkData::default(),
                };
                if let Some(time) = env.time {
                    self.state.last_ts = self.state.last_ts.max(time);
                }
                if parsed_chunk
                    .chunk
                    .as_ref()
                    .and_then(|chunk| chunk.kind.as_deref())
                    == Some("usage")
                    && let Some(usage) = parsed_chunk.chunk.and_then(|chunk| chunk.usage)
                {
                    let turn = parsed_chunk.turn.unwrap_or(0);
                    let step = parsed_chunk.step.unwrap_or(0);
                    let route_model = std::mem::take(&mut self.route_model);
                    self.apply_parsed_usage(turn, step, usage, &route_model);
                    self.route_model = route_model;
                }
                self.diagnostics.record_recognized_source();
                true
            }
            "assistant/message" => {
                let parsed_msg = match env.data {
                    Some(raw) if raw.get().starts_with('{') => {
                        let Ok(data) =
                            serde_json::from_str::<BorrowedAssistantMessageData<'_>>(raw.get())
                        else {
                            return false;
                        };
                        data
                    }
                    Some(_) => return false,
                    None => BorrowedAssistantMessageData::default(),
                };
                if let Some(time) = env.time {
                    self.state.last_ts = self.state.last_ts.max(time);
                }
                let explicit_model = parsed_msg
                    .message
                    .and_then(|m| m.source)
                    .and_then(|s| s.model);
                if let Some(model) = explicit_model.as_deref().filter(|m| !m.is_empty()) {
                    self.last_model.clear();
                    self.last_model.push_str(model);
                    self.state.note_model(model);
                } else if !self.route_model.is_empty() {
                    self.last_model.clear();
                }
                if let Some(usage) = parsed_msg.usage {
                    let turn = parsed_msg.turn.unwrap_or(0);
                    let step = parsed_msg.step.unwrap_or(0);
                    match explicit_model {
                        Some(model) => {
                            self.apply_parsed_usage(turn, step, usage, model.as_ref());
                        }
                        None => {
                            let route_model = std::mem::take(&mut self.route_model);
                            self.apply_parsed_usage(turn, step, usage, &route_model);
                            self.route_model = route_model;
                        }
                    }
                }
                self.diagnostics.record_recognized_source();
                true
            }
            "tool/call" => {
                let call = match env.data {
                    Some(raw) if raw.get().starts_with('{') => {
                        let Ok(call) = serde_json::from_str::<BorrowedToolCallData<'_>>(raw.get())
                        else {
                            return false;
                        };
                        Some(call)
                    }
                    Some(_) => return false,
                    None => None,
                };
                if let Some(time) = env.time {
                    self.state.last_ts = self.state.last_ts.max(time);
                }
                if let (Some(seq), Some(call)) = (env.seq, call)
                    && let (Some(name), Some(arguments)) = (call.name, call.arguments)
                {
                    let kind = DshToolKind::from_name(name.as_ref());
                    let stored_args = if kind.needs_arguments() {
                        arguments.into_owned()
                    } else {
                        String::new()
                    };
                    let model = self.active_model().unwrap_or_default().to_string();
                    self.pending_calls.insert(
                        seq,
                        PendingCall {
                            kind,
                            arguments: stored_args,
                            model,
                        },
                    );
                }
                self.diagnostics.record_recognized_source();
                true
            }
            "tool/result" => {
                let data = match env.data {
                    Some(raw) if raw.get().starts_with('{') => {
                        let Ok(parsed) =
                            serde_json::from_str::<BorrowedToolResultData<'_>>(raw.get())
                        else {
                            return false;
                        };
                        parsed
                    }
                    _ => BorrowedToolResultData::default(),
                };
                if let Some(time) = env.time {
                    self.state.last_ts = self.state.last_ts.max(time);
                }
                self.diagnostics.record_recognized_source();

                let Some(call) = self.take_pending_call_from_seqs(env.source_event_seqs) else {
                    return true;
                };
                if data.error.is_some_and(|raw| raw.get() != "null") {
                    return true;
                }
                let timestamp = env.time.unwrap_or(self.state.last_ts);
                self.apply_borrowed_tool_result(call, data.meta, timestamp);
                true
            }
            _ => {
                if let Some(time) = env.time {
                    self.state.last_ts = self.state.last_ts.max(time);
                }
                self.diagnostics.record_unrecognized();
                true
            }
        }
    }

    fn apply_borrowed_tool_result(
        &mut self,
        call: PendingCall,
        meta_raw: Option<&RawValue>,
        timestamp: i64,
    ) {
        let fallback_model = self.active_model().map(str::to_string);
        let attr_model = if !call.model.is_empty() {
            Some(call.model.as_str())
        } else {
            fallback_model.as_deref()
        };
        match call.kind {
            DshToolKind::TodoWrite => {
                self.state.record_for_model(attr_model, |state| {
                    state.tool_counts.todo_write += 1;
                });
                self.diagnostics.record_relevant(true);
                return;
            }
            // Tool sets are plugin-composable, so unmapped tool names are expected.
            DshToolKind::Unmapped => return,
            _ => {}
        }

        let args = if call.arguments.trim_start().starts_with('{') {
            serde_json::from_str::<BorrowedDshToolArgs<'_>>(&call.arguments).unwrap_or_default()
        } else {
            BorrowedDshToolArgs::default()
        };
        let usage_only = matches!(self.state.mode, ParseMode::UsageOnly);

        let applied = self
            .state
            .record_for_model(attr_model, |state| match call.kind {
                DshToolKind::Read => {
                    let meta = meta_raw
                        .filter(|raw| raw.get().starts_with('{'))
                        .and_then(|raw| {
                            serde_json::from_str::<BorrowedDshReadMeta<'_>>(raw.get()).ok()
                        })
                        .unwrap_or_default();
                    let path = raw_as_str(meta.path)
                        .or_else(|| raw_as_str(args.file_path))
                        .unwrap_or(Cow::Borrowed(""));
                    if path.is_empty() {
                        false
                    } else if usage_only {
                        let stats = count_borrowed_read_lines(meta.lines);
                        if stats.content_is_empty {
                            state.tool_counts.read += 1;
                            state.add_non_text_read_path(&path);
                        } else {
                            state.add_read_counts(&path, stats.metrics.lines, stats.metrics.chars);
                        }
                        true
                    } else {
                        let content = join_borrowed_read_lines(meta.lines);
                        if content.is_empty() {
                            state.tool_counts.read += 1;
                            state.add_non_text_read_path(&path);
                        } else {
                            state.add_read_detail(&path, &content, timestamp);
                        }
                        true
                    }
                }
                DshToolKind::Write => match raw_as_str(args.file_path) {
                    None => false,
                    Some(path) => {
                        if usage_only {
                            let metrics = args
                                .content
                                .and_then(|raw| count_json_string_metrics(raw.get()))
                                .unwrap_or_default();
                            state.add_write_counts(&path, metrics.lines, metrics.chars);
                        } else {
                            let content = raw_as_str(args.content).unwrap_or(Cow::Borrowed(""));
                            state.add_write_detail(&path, &content, timestamp);
                        }
                        true
                    }
                },
                DshToolKind::Edit => match raw_as_str(args.file_path) {
                    None => false,
                    Some(path) => {
                        if usage_only {
                            let old_is_empty = args
                                .old_string
                                .and_then(|raw| count_json_string_metrics(raw.get()))
                                .is_none_or(|m| m.is_empty());
                            let new_metrics = args
                                .new_string
                                .and_then(|raw| count_json_string_metrics(raw.get()))
                                .unwrap_or_default();
                            state.add_edit_counts(
                                &path,
                                old_is_empty,
                                new_metrics.lines,
                                new_metrics.chars,
                            );
                        } else {
                            let old = raw_as_str(args.old_string).unwrap_or(Cow::Borrowed(""));
                            let new = raw_as_str(args.new_string).unwrap_or(Cow::Borrowed(""));
                            state.add_edit_detail(&path, &old, &new, timestamp);
                        }
                        true
                    }
                },
                DshToolKind::Bash => match raw_as_str(args.command) {
                    None => false,
                    Some(command) => {
                        let description = if usage_only {
                            Cow::Borrowed("")
                        } else {
                            raw_as_str(args.description).unwrap_or(Cow::Borrowed(""))
                        };
                        state.add_run_command(&command, &description, timestamp);
                        true
                    }
                },
                DshToolKind::TodoWrite | DshToolKind::Unmapped => unreachable!(),
            });
        self.diagnostics.record_relevant(applied);
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
                    self.last_model.clear();
                    self.route_model = model.to_string();
                    self.state.note_model(&self.route_model);
                }
            }
            "assistant/message" => self.apply_assistant_message(data),
            "assistant/chunk" => self.apply_chunk(data),
            "tool/call" => self.apply_tool_call(object, data),
            "tool/result" => self.apply_tool_result(object, data),
            _ if is_ignored_record_type(kind) => {}
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
        let explicit = data
            .pointer("/message/source/model")
            .and_then(Value::as_str);
        if let Some(model) = explicit.filter(|m| !m.is_empty()) {
            self.last_model.clear();
            self.last_model.push_str(model);
            self.state.note_model(model);
        } else if !self.route_model.is_empty() {
            self.last_model.clear();
        }
        let model = explicit.unwrap_or(&self.route_model).to_string();
        self.apply_usage(data, data.get("usage"), &model);
    }

    fn apply_chunk(&mut self, data: &Value) {
        if data.pointer("/chunk/type").and_then(Value::as_str) != Some("usage") {
            return;
        }
        let route_model = std::mem::take(&mut self.route_model);
        self.apply_usage(data, data.pointer("/chunk/usage"), &route_model);
        self.route_model = route_model;
    }

    /// Records one step's usage sample in the last-wins slot, flushing the prior step when `(turn, step)` changes.
    fn apply_usage(&mut self, data: &Value, usage: Option<&Value>, model: &str) {
        // Missing `usage` means the adapter reported none, not a schema failure.
        let Some(usage) = usage else {
            return;
        };
        let parsed = match usage.as_object() {
            Some(obj) => ParsedDshUsage::Valid(DshTokens::from_usage(obj)),
            None => ParsedDshUsage::Invalid,
        };
        self.apply_parsed_usage(
            data.get("turn").and_then(Value::as_i64).unwrap_or(0),
            data.get("step").and_then(Value::as_i64).unwrap_or(0),
            parsed,
            model,
        );
    }

    fn apply_parsed_usage(&mut self, turn: i64, step: i64, usage: ParsedDshUsage, model: &str) {
        let ParsedDshUsage::Valid(tokens) = usage else {
            self.diagnostics.record_relevant(false);
            return;
        };
        if let Some(held) = self.pending_usage.as_mut()
            && held.turn == turn
            && held.step == step
        {
            if held.model != model {
                held.model.clear();
                held.model.push_str(model);
            }
            held.tokens = tokens;
            return;
        }
        self.flush_usage();
        self.pending_usage = Some(UsageSample {
            turn,
            step,
            model: model.to_string(),
            tokens,
        });
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
        let kind = DshToolKind::from_name(name);
        let model = self.active_model().unwrap_or_default().to_string();
        self.pending_calls.insert(
            seq,
            PendingCall {
                kind,
                arguments: if kind.needs_arguments() {
                    arguments.to_string()
                } else {
                    String::new()
                },
                model,
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
        let fallback_model = self.active_model().map(str::to_string);
        let attr_model = if !call.model.is_empty() {
            Some(call.model.as_str())
        } else {
            fallback_model.as_deref()
        };

        if call.kind == DshToolKind::Unmapped {
            // Tool sets are plugin-composable, so unmapped tool names are expected.
            return;
        }

        let applied = self
            .state
            .record_for_model(attr_model, |state| match call.kind {
                DshToolKind::Read => apply_dsh_read(state, &arguments, meta, timestamp),
                DshToolKind::Write => apply_dsh_write(state, &arguments, timestamp),
                DshToolKind::Edit => apply_dsh_edit(state, &arguments, timestamp),
                DshToolKind::Bash => apply_dsh_bash(state, &arguments, timestamp),
                DshToolKind::TodoWrite => {
                    state.tool_counts.todo_write += 1;
                    true
                }
                DshToolKind::Unmapped => unreachable!(),
            });
        self.diagnostics.record_relevant(applied);
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

fn apply_dsh_read(
    state: &mut SessionParseState,
    arguments: &Value,
    meta: &Value,
    timestamp: i64,
) -> bool {
    // Prefer `meta.path` (resolved absolute path) over `arguments.file_path`, which may be relative to cwd.
    let path = meta
        .get("path")
        .and_then(Value::as_str)
        .or_else(|| arguments.get("file_path").and_then(Value::as_str))
        .unwrap_or_default();
    if path.is_empty() {
        return false;
    }

    if matches!(state.mode, ParseMode::UsageOnly) {
        let mut text_count = 0_usize;
        let mut any_non_empty = false;
        let mut counter = StreamingTextCounter::default();
        if let Some(lines) = meta.get("lines").and_then(Value::as_array) {
            for line in lines {
                if let Some(text) = line.get("text").and_then(Value::as_str) {
                    if text_count > 0 {
                        counter.push_newline();
                    }
                    text_count += 1;
                    if !text.is_empty() {
                        any_non_empty = true;
                        counter.push_str(text);
                    }
                }
            }
        }
        if text_count == 0 || (text_count == 1 && !any_non_empty) {
            state.tool_counts.read += 1;
            state.add_non_text_read_path(path);
        } else {
            let metrics = counter.finish();
            state.add_read_counts(path, metrics.lines, metrics.chars);
        }
        return true;
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
        state.tool_counts.read += 1;
        state.add_non_text_read_path(path);
    } else {
        state.add_read_detail(path, &content, timestamp);
    }
    true
}

fn apply_dsh_write(state: &mut SessionParseState, arguments: &Value, timestamp: i64) -> bool {
    // Use the call's `content` because diff meta is empty for new files and pads hunks with context lines.
    let Some(path) = arguments.get("file_path").and_then(Value::as_str) else {
        return false;
    };
    let content = arguments
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    state.add_write_detail(path, content, timestamp);
    true
}

fn apply_dsh_edit(state: &mut SessionParseState, arguments: &Value, timestamp: i64) -> bool {
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
    state.add_edit_detail(path, old, new, timestamp);
    true
}

fn apply_dsh_bash(state: &mut SessionParseState, arguments: &Value, timestamp: i64) -> bool {
    let Some(command) = arguments.get("command").and_then(Value::as_str) else {
        return false;
    };
    let description = arguments
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    state.add_run_command(command, description, timestamp);
    true
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

    #[test]
    fn usage_only_matches_full_across_tools_cjk_emoji_and_blank_reads() {
        let dir = TempDir::new().unwrap();
        let route = json!({
            "type": "request/context",
            "seq": 1,
            "time": 100,
            "data": {"model": "deepseek-reasoner"},
        })
        .to_string();
        let chunk = json!({
            "type": "assistant/chunk",
            "seq": 2,
            "time": 101,
            "data": {
                "turn": 1,
                "step": 1,
                "chunk": {
                    "type": "usage",
                    "usage": {
                        "inputTokens": 500,
                        "outputTokens": 120,
                        "reasoningTokens": 40,
                        "cacheReadTokens": 100,
                        "cacheWriteTokens": 50
                    }
                }
            }
        })
        .to_string();
        let msg = json!({
            "type": "assistant/message",
            "seq": 3,
            "time": 102,
            "data": {
                "turn": 1,
                "step": 1,
                "message": {"source": {"kind": "model", "provider": "p", "model": "deepseek-reasoner"}},
                "usage": {
                    "inputTokens": 520,
                    "outputTokens": 150,
                    "reasoningTokens": 50,
                    "cacheReadTokens": 100,
                    "cacheWriteTokens": 50
                }
            }
        })
        .to_string();
        // Multi-line CJK/emoji/escaped read
        let read_call = json!({
            "type": "tool/call",
            "seq": 10,
            "time": 110,
            "data": {"name": "read", "arguments": r#"{"file_path":"src/a.rs"}"#}
        })
        .to_string();
        let read_res = json!({
            "type": "tool/result",
            "seq": 11,
            "time": 111,
            "sourceEventSeqs": [10],
            "data": {
                "error": null,
                "meta": {
                    "path": "/repo/src/a.rs",
                    "lines": [
                        {"text": "let s = \"你好世界🚀\";"},
                        {"text": ""},
                        {"text": "second\tline"}
                    ]
                }
            }
        })
        .to_string();
        // Single empty line read -> counted as non-text read (tool_counts.read += 1, 0 lines)
        let empty_read_call = json!({
            "type": "tool/call",
            "seq": 12,
            "time": 112,
            "data": {"name": "read_image", "arguments": r#"{"file_path":"img.png"}"#}
        })
        .to_string();
        let empty_read_res = json!({
            "type": "tool/result",
            "seq": 13,
            "time": 113,
            "sourceEventSeqs": [12],
            "data": {"meta": {"path": "/repo/img.png", "lines": [{"text": ""}]}}
        })
        .to_string();
        // Two empty lines read -> joined to "\n" -> trimmed empty -> 0 lines, read NOT incremented
        let blank_read_call = json!({
            "type": "tool/call",
            "seq": 14,
            "time": 114,
            "data": {"name": "read", "arguments": r#"{"file_path":"blank.txt"}"#}
        })
        .to_string();
        let blank_read_res = json!({
            "type": "tool/result",
            "seq": 15,
            "time": 115,
            "sourceEventSeqs": [14],
            "data": {"meta": {"lines": [{"text": ""}, {"text": ""}]}}
        })
        .to_string();
        // Write with CJK + trailing newline
        let write_call = json!({
            "type": "tool/call",
            "seq": 16,
            "time": 116,
            "data": {
                "name": "write",
                "arguments": serde_json::to_string(&json!({
                    "file_path": "src/w.rs",
                    "content": "第一行\n第二行🎉\n"
                })).unwrap()
            }
        })
        .to_string();
        let write_res = json!({
            "type": "tool/result",
            "seq": 17,
            "time": 117,
            "sourceEventSeqs": [16],
            "data": {"meta": {}}
        })
        .to_string();
        // Edit with empty/newline-only old_string -> reclassified as Write
        let edit_as_write_call = json!({
            "type": "tool/call",
            "seq": 18,
            "time": 118,
            "data": {
                "name": "edit",
                "arguments": serde_json::to_string(&json!({
                    "file_path": "src/e1.rs",
                    "old_string": "\n\n",
                    "new_string": "新增🔥\n"
                })).unwrap()
            }
        })
        .to_string();
        let edit_as_write_res = json!({
            "type": "tool/result",
            "seq": 19,
            "time": 119,
            "sourceEventSeqs": [18],
            "data": {"meta": {}}
        })
        .to_string();
        // Normal edit + bash + todo_write + unmapped + errored tool
        let edit_call = json!({
            "type": "tool/call",
            "seq": 20,
            "time": 120,
            "data": {
                "name": "str_replace_editor",
                "arguments": serde_json::to_string(&json!({
                    "file_path": "src/a.rs",
                    "old_string": "old",
                    "new_string": "甲\n乙\n"
                })).unwrap()
            }
        })
        .to_string();
        let edit_res = json!({
            "type": "tool/result",
            "seq": 21,
            "time": 121,
            "sourceEventSeqs": [20],
            "data": {"meta": {}}
        })
        .to_string();
        let bash_call = json!({
            "type": "tool/call",
            "seq": 22,
            "time": 122,
            "data": {"name": "bash", "arguments": r#"{"command":"cargo test","description":"run"}"#}
        })
        .to_string();
        let bash_res = json!({
            "type": "tool/result",
            "seq": 23,
            "time": 123,
            "sourceEventSeqs": [22],
            "data": {"meta": {}}
        })
        .to_string();
        let todo_call = json!({
            "type": "tool/call",
            "seq": 24,
            "time": 124,
            "data": {"name": "todo_write", "arguments": r#"{"items":[]}"#}
        })
        .to_string();
        let todo_res = json!({
            "type": "tool/result",
            "seq": 25,
            "time": 125,
            "sourceEventSeqs": [24],
            "data": {"meta": {}}
        })
        .to_string();
        let err_call = json!({
            "type": "tool/call",
            "seq": 26,
            "time": 126,
            "data": {"name": "bash", "arguments": r#"{"command":"false"}"#}
        })
        .to_string();
        let err_res = json!({
            "type": "tool/result",
            "seq": 27,
            "time": 127,
            "sourceEventSeqs": [26],
            "data": {"error": {"message": "failed"}}
        })
        .to_string();

        let lines = vec![
            HEADER.to_string(),
            route,
            chunk,
            msg,
            read_call,
            read_res,
            empty_read_call,
            empty_read_res,
            blank_read_call,
            blank_read_res,
            write_call,
            write_res,
            edit_as_write_call,
            edit_as_write_res,
            edit_call,
            edit_res,
            bash_call,
            bash_res,
            todo_call,
            todo_res,
            err_call,
            err_res,
        ];
        let path = write_frames(
            &dir,
            "session.jsonl.zstd",
            &[lines[..4].to_vec(), lines[4..].to_vec()],
        );

        let full = parse_dsh_session(&path, ParseMode::Full, None).expect("full");
        let usage = parse_dsh_session(&path, ParseMode::UsageOnly, None).expect("usage_only");
        let mut via_value = DshParser::new(ParseMode::Full, None);
        for line in &lines {
            let value: Value = serde_json::from_str(line).expect("valid json");
            via_value.push(&value);
        }
        let value_parsed = via_value.finish();

        let fr = &full.analysis.records[0];
        let ur = &usage.analysis.records[0];
        let vr = &value_parsed.analysis.records[0];

        assert_eq!(
            serde_json::to_value(fr).unwrap(),
            serde_json::to_value(vr).unwrap()
        );
        assert_eq!(full.diagnostics, value_parsed.diagnostics);

        assert_eq!(ur.task_id, fr.task_id);
        assert_eq!(ur.folder_path, fr.folder_path);
        assert_eq!(ur.timestamp, fr.timestamp);
        assert_eq!(ur.conversation_usage, fr.conversation_usage);
        assert_eq!(
            serde_json::to_value(&ur.tool_call_counts).unwrap(),
            serde_json::to_value(&fr.tool_call_counts).unwrap()
        );
        assert_eq!(ur.total_read_lines, fr.total_read_lines);
        assert_eq!(ur.total_read_characters, fr.total_read_characters);
        assert_eq!(ur.total_write_lines, fr.total_write_lines);
        assert_eq!(ur.total_write_characters, fr.total_write_characters);
        assert_eq!(ur.total_edit_lines, fr.total_edit_lines);
        assert_eq!(ur.total_edit_characters, fr.total_edit_characters);
        assert_eq!(ur.total_unique_files, fr.total_unique_files);
        assert_eq!(usage.diagnostics, full.diagnostics);
        assert!(ur.read_file_details.is_empty());
        assert!(ur.write_file_details.is_empty());
        assert!(ur.edit_file_details.is_empty());
        assert!(ur.run_command_details.is_empty());
        assert_eq!(fr.tool_call_counts.read, 2);
        assert_eq!(fr.tool_call_counts.write, 2);
        assert_eq!(fr.tool_call_counts.edit, 1);
        assert_eq!(fr.tool_call_counts.bash, 1);
        assert_eq!(fr.tool_call_counts.todo_write, 1);
    }

    #[test]
    fn multi_model_tool_attribution_prefers_explicit_model_and_matches_value_path() {
        let dir = TempDir::new().unwrap();
        let lines = vec![
            HEADER.to_string(),
            json!({"type": "request/context", "seq": 1, "time": 100, "data": {"model": "model-a"}})
                .to_string(),
            json!({
                "type": "tool/call",
                "seq": 2,
                "time": 101,
                "data": {"name": "read", "arguments": r#"{"file_path":"src/a.rs"}"#}
            })
            .to_string(),
            json!({
                "type": "tool/result",
                "seq": 3,
                "time": 102,
                "sourceEventSeqs": [2],
                "data": {"meta": {"path": "/repo/src/a.rs", "lines": [{"text": "line1"}, {"text": "line2"}]}}
            })
            .to_string(),
            // Explicit model override on assistant/message without a new request/context.
            json!({
                "type": "assistant/message",
                "seq": 4,
                "time": 103,
                "data": {
                    "turn": 1,
                    "step": 0,
                    "message": {"source": {"model": "model-b"}},
                    "usage": {"inputTokens": 20, "outputTokens": 10}
                }
            })
            .to_string(),
            json!({
                "type": "tool/call",
                "seq": 5,
                "time": 104,
                "data": {"name": "edit", "arguments": r#"{"file_path":"src/b.rs","old_string":"a","new_string":"b\nc\n"}"#}
            })
            .to_string(),
            json!({
                "type": "tool/result",
                "seq": 6,
                "time": 105,
                "sourceEventSeqs": [5],
                "data": {"meta": {}}
            })
            .to_string(),
            // Usage-less assistant/message with explicit model must still switch active attribution.
            json!({
                "type": "assistant/message",
                "seq": 7,
                "time": 106,
                "data": {
                    "turn": 2,
                    "step": 0,
                    "message": {"source": {"model": "model-c"}}
                }
            })
            .to_string(),
            json!({
                "type": "tool/call",
                "seq": 8,
                "time": 107,
                "data": {"name": "bash", "arguments": r#"{"command":"echo ok"}"#}
            })
            .to_string(),
            json!({
                "type": "tool/result",
                "seq": 9,
                "time": 108,
                "sourceEventSeqs": [8],
                "data": {"meta": {}}
            })
            .to_string(),
            // New request/context clears stale explicit last_model.
            json!({"type": "request/context", "seq": 10, "time": 109, "data": {"model": "model-a"}})
                .to_string(),
            json!({
                "type": "assistant/message",
                "seq": 11,
                "time": 110,
                "data": {
                    "turn": 3,
                    "step": 0,
                    "usage": {"inputTokens": 15, "outputTokens": 5}
                }
            })
            .to_string(),
            json!({
                "type": "tool/call",
                "seq": 12,
                "time": 111,
                "data": {"name": "write", "arguments": r#"{"file_path":"src/c.rs","content":"x\ny\n"}"#}
            })
            .to_string(),
            json!({
                "type": "tool/result",
                "seq": 13,
                "time": 112,
                "sourceEventSeqs": [12],
                "data": {"meta": {}}
            })
            .to_string(),
        ];
        let path = write_frames(&dir, "multi_model.jsonl.zstd", std::slice::from_ref(&lines));

        let full = parse_dsh_session(&path, ParseMode::Full, None).expect("full");
        let usage = parse_dsh_session(&path, ParseMode::UsageOnly, None).expect("usage_only");
        let mut via_value = DshParser::new(ParseMode::Full, None);
        for line in &lines {
            let value: Value = serde_json::from_str(line).expect("valid json");
            via_value.push(&value);
        }
        let value_parsed = via_value.finish();

        let fr = &full.analysis.records[0];
        let ur = &usage.analysis.records[0];
        let vr = &value_parsed.analysis.records[0];

        assert_eq!(fr.per_model_analysis, ur.per_model_analysis);
        assert_eq!(fr.per_model_analysis, vr.per_model_analysis);

        let row_a = &fr.per_model_analysis["model-a"];
        assert_eq!(row_a.read_count, 1);
        assert_eq!(row_a.read_lines, 2);
        assert_eq!(row_a.write_count, 1);
        assert_eq!(row_a.write_lines, 2);
        assert_eq!(row_a.edit_count, 0);
        assert_eq!(row_a.bash_count, 0);

        let row_b = &fr.per_model_analysis["model-b"];
        assert_eq!(row_b.edit_count, 1);
        assert_eq!(row_b.edit_lines, 2);
        assert_eq!(row_b.read_count, 0);
        assert_eq!(row_b.bash_count, 0);
        assert_eq!(row_b.write_count, 0);

        let row_c = &fr.per_model_analysis["model-c"];
        assert_eq!(row_c.bash_count, 1);
        assert_eq!(row_c.read_count, 0);
        assert_eq!(row_c.edit_count, 0);
        assert_eq!(row_c.write_count, 0);
    }
}
