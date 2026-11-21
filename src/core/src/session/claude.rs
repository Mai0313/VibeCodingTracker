//! Parser for Claude Code session logs (`~/.claude/projects/**/*.jsonl`).
//!
//! Assistant records repeat their full `usage` object once per content block; the parser deduplicates by API response identity and keeps the last streamed snapshot. File-operation results arrive either in a top-level `toolUseResult` field (main sessions) or in a sidechain user record's `message.content[].tool_result` block (subagents), correlated with pending `tool_use` calls by `tool_use_id`.
use crate::constants::{FastHashMap, capacity};
use crate::models::*;
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::parser::stream_jsonl_borrowed;
use crate::session::state::{
    CountedJsonString, ParseMode, SessionParseState, TextMetrics, count_json_string_metrics,
};
use crate::utils::token_merge::write_tier_slice;
use crate::utils::{MAX_TIER_LEVELS, TierSlice, get_git_remote_url, parse_iso_timestamp};
use anyhow::Result;
use serde::{
    Deserialize, Deserializer,
    de::{Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, value::RawValue};
use std::borrow::Cow;
use std::fmt;
use std::path::Path;

const FLAG_HAS_CC: u8 = 1 << 0;
const FLAG_HAS_CC_5M: u8 = 1 << 1;
const FLAG_HAS_CC_1H: u8 = 1 << 2;
const FLAG_HAS_STU: u8 = 1 << 3;
const FLAG_HAS_WEB_SEARCH: u8 = 1 << 4;
const FLAG_HAS_WEB_FETCH: u8 = 1 << 5;

/// Parses pre-parsed JSON `records` into a single-record [`CodeAnalysis`].
///
/// Records that fail to deserialize into [`ClaudeCodeLog`] are skipped. Runtime metadata fields on the returned [`CodeAnalysis`] are left blank for the caller to populate.
///
/// # Errors
///
/// Never returns `Err`; returns `Result` for parity with other provider parsers.
pub fn parse_claude_log_values(records: Vec<Value>, mode: ParseMode) -> Result<CodeAnalysis> {
    let iter = records
        .into_iter()
        .filter_map(|value| serde_json::from_value::<ClaudeCodeLog>(value).ok());
    parse_claude_logs(iter, mode)
}

/// Parses Claude Code session `logs` into a single-record [`CodeAnalysis`].
///
/// Runtime metadata fields (`user`, `extension_name`, `insights_version`, `machine_id`) are left blank for the caller to populate.
///
/// # Errors
///
/// Never returns `Err`; returns `Result` for parity with other provider parsers.
pub fn parse_claude_logs<I>(logs: I, mode: ParseMode) -> Result<CodeAnalysis>
where
    I: IntoIterator<Item = ClaudeCodeLog>,
{
    Ok(parse_claude_logs_with_diagnostics(logs, mode, None)?.analysis)
}

/// Parses Claude Code `logs` with schema diagnostics, classifying per-request context tiers when `tiers` is `Some`.
pub(crate) fn parse_claude_logs_with_diagnostics<I>(
    logs: I,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<ParsedAnalysis>
where
    I: IntoIterator<Item = ClaudeCodeLog>,
{
    let mut fold = ClaudeFold::new(mode, tiers);

    for mut log in logs {
        let ts = fold.observe_envelope(
            &log.log_type,
            &log.cwd,
            &log.session_id,
            &log.timestamp,
            log.tool_use_result.is_some(),
        );

        if log.log_type == "assistant" {
            match &mut log.message {
                None => fold.diagnostics.record_relevant(false),
                Some(message) => {
                    let usage = message
                        .usage
                        .take()
                        .map(|value| ParsedClaudeUsage::from_value(&value));
                    fold.observe_assistant_usage(
                        message.id.as_deref(),
                        log.request_id.as_deref(),
                        message.model.as_deref(),
                        usage,
                    );
                    for item in std::mem::take(&mut message.content) {
                        if let ClaudeContentItem::ToolUse { id, name, input } = item {
                            fold.observe_tool_use_full(id, &name, input, ts);
                        }
                    }
                }
            }
        }

        if let Some(tur) = &log.tool_use_result {
            fold.observe_top_level_tool_result_full(
                tur,
                first_tool_result(log.message.as_ref()),
                ts,
            );
        } else if log.log_type == "user"
            && let Some(message) = &log.message
        {
            for item in &message.content {
                if let ClaudeContentItem::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } = item
                {
                    fold.observe_user_tool_result_full(
                        log.is_sidechain,
                        tool_use_id,
                        *is_error,
                        ts,
                        content.as_str(),
                    );
                }
            }
        }
    }

    Ok(fold.finish())
}

/// Returns the borrowed `(message.id, requestId)` key parts used to deduplicate streamed Claude assistant records.
fn claude_request_key_parts<'a>(
    message_id: Option<&'a str>,
    request_id: Option<&'a str>,
) -> Option<(&'a str, Option<&'a str>)> {
    let id = message_id.filter(|id| !id.is_empty())?;
    Some((id, request_id.filter(|id| !id.is_empty())))
}

/// Streams and parses a Claude Code JSONL file at `path` with schema diagnostics, classifying per-request context tiers when `tiers` is `Some`.
pub(crate) fn parse_claude_jsonl_path(
    path: &Path,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<Option<ParsedAnalysis>> {
    stream_jsonl_borrowed(
        path,
        ExtensionType::ClaudeCode,
        ClaudeFold::new(mode, tiers),
        |fold, bytes| {
            if !bytes.starts_with(b"{") {
                return Err(serde::de::Error::custom("expected Claude record object"));
            }
            let record = serde_json::from_slice::<BorrowedClaudeRecord<'_>>(bytes)?;
            let tur_looks_like_object = record
                .tool_use_result
                .is_some_and(|raw| raw.get().starts_with('{'));

            let needs_message = record.log_type == "assistant"
                || record.log_type == "user"
                || tur_looks_like_object;
            let mut message = if needs_message {
                match record.message {
                    Some(raw) if raw.get() == "null" => None,
                    Some(raw) if raw.get().starts_with('{') => {
                        Some(serde_json::from_str::<BorrowedClaudeMessage<'_>>(
                            raw.get(),
                        )?)
                    }
                    Some(_) => {
                        return Err(serde::de::Error::custom("expected Claude message object"));
                    }
                    None => None,
                }
            } else {
                None
            };

            // Validate and parse tracked tool_use inputs before mutating `fold` so schema failures report cleanly without partial state changes.
            let parsed_tool_uses = match message.as_ref() {
                Some(msg) => parse_content_tool_uses(&msg.content, fold.state.mode)?,
                None => SmallToolUses::Empty,
            };

            // Skip deserializing `toolUseResult` for non-file tools since neither result branch inspects it.
            if record.log_type == "user"
                && tur_looks_like_object
                && let Some(msg) = message.as_ref()
                && let Some((tool_use_id, _)) = msg.content.single_tool_result()
                && fold
                    .pending_tool_uses
                    .get(tool_use_id)
                    .is_some_and(|pending| pending.kind == PendingToolKind::Other)
            {
                fold.observe_envelope(
                    &record.log_type,
                    &record.cwd,
                    &record.session_id,
                    &record.timestamp,
                    true,
                );
                fold.pending_tool_uses.remove(tool_use_id);
                return Ok(());
            }

            if matches!(fold.state.mode, ParseMode::UsageOnly) {
                let tur = if tur_looks_like_object {
                    record.tool_use_result.and_then(|raw| {
                        serde_json::from_str::<UsageOnlyToolUseResult<'_>>(raw.get()).ok()
                    })
                } else {
                    None
                };

                let ts = fold.observe_envelope(
                    &record.log_type,
                    &record.cwd,
                    &record.session_id,
                    &record.timestamp,
                    tur.is_some(),
                );

                if record.log_type == "assistant" {
                    match &mut message {
                        None => fold.diagnostics.record_relevant(false),
                        Some(msg) => {
                            let usage = msg.usage.take();
                            fold.observe_assistant_usage(
                                msg.id.as_deref(),
                                record.request_id.as_deref(),
                                msg.model.as_deref(),
                                usage,
                            );
                            for tool_use in parsed_tool_uses {
                                match tool_use.input {
                                    ParsedToolInput::UsageOnly(input) => {
                                        fold.observe_tool_use_usage_only(
                                            tool_use.id,
                                            tool_use.name.as_ref(),
                                            input,
                                            ts,
                                        );
                                    }
                                    ParsedToolInput::Full(input) => {
                                        fold.observe_tool_use_full(
                                            tool_use.id.into_owned(),
                                            tool_use.name.as_ref(),
                                            input,
                                            ts,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(tur) = &tur {
                    let first = first_borrowed_tool_result(message.as_ref());
                    fold.observe_top_level_tool_result_usage_only(tur, first);
                } else if record.log_type == "user"
                    && let Some(msg) = &message
                {
                    msg.content
                        .for_each_tool_result(|tool_use_id, content, is_error| {
                            fold.observe_user_tool_result_usage_only(
                                record.is_sidechain,
                                tool_use_id,
                                is_error,
                                content,
                            );
                        });
                }

                return Ok(());
            }

            let tur = if tur_looks_like_object {
                record
                    .tool_use_result
                    .and_then(|raw| serde_json::from_str::<ClaudeToolUseResult>(raw.get()).ok())
            } else {
                None
            };

            let ts = fold.observe_envelope(
                &record.log_type,
                &record.cwd,
                &record.session_id,
                &record.timestamp,
                tur.is_some(),
            );

            if record.log_type == "assistant" {
                match &mut message {
                    None => fold.diagnostics.record_relevant(false),
                    Some(msg) => {
                        let usage = msg.usage.take();
                        fold.observe_assistant_usage(
                            msg.id.as_deref(),
                            record.request_id.as_deref(),
                            msg.model.as_deref(),
                            usage,
                        );
                        for tool_use in parsed_tool_uses {
                            if let ParsedToolInput::Full(input) = tool_use.input {
                                fold.observe_tool_use_full(
                                    tool_use.id.into_owned(),
                                    tool_use.name.as_ref(),
                                    input,
                                    ts,
                                );
                            }
                        }
                    }
                }
            }

            if let Some(tur) = &tur {
                let first = first_borrowed_tool_result(message.as_ref());
                fold.observe_top_level_tool_result_full(tur, first, ts);
            } else if record.log_type == "user"
                && let Some(msg) = &message
            {
                msg.content
                    .for_each_tool_result(|tool_use_id, content, is_error| {
                        fold.observe_user_tool_result_lazy(
                            record.is_sidechain,
                            tool_use_id,
                            is_error,
                            ts,
                            || decode_raw_tool_result_content(content),
                        );
                    });
            }

            Ok(())
        },
        |fold| Ok(fold.finish()),
    )
}

#[derive(Clone, Default)]
enum ServiceTier {
    #[default]
    Absent,
    Empty,
    Standard,
    Priority,
    Batch,
    Other(Box<str>),
}

impl ServiceTier {
    fn from_str_value(value: &str) -> Self {
        match value {
            "" => Self::Empty,
            "standard" => Self::Standard,
            "priority" => Self::Priority,
            "batch" => Self::Batch,
            other => Self::Other(other.into()),
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Self::Absent | Self::Empty => "",
            Self::Standard => "standard",
            Self::Priority => "priority",
            Self::Batch => "batch",
            Self::Other(value) => value,
        }
    }

    fn is_present(&self) -> bool {
        !matches!(self, Self::Absent)
    }
}

struct ClaudeAdvisorIteration {
    model: Option<String>,
    usage: ParsedClaudeUsage,
}

#[derive(Default)]
struct ExtraUsageFields {
    extra_cache_creation: Vec<(String, i64)>,
    extra_server_tool_use: Vec<(String, i64)>,
    advisor_iterations: Vec<ClaudeAdvisorIteration>,
}

#[derive(Default)]
struct ClaudeUsageBucket {
    input_tokens: i64,
    cache_creation_input_tokens: i64,
    cache_read_input_tokens: i64,
    output_tokens: i64,
    ephemeral_5m_input_tokens: i64,
    ephemeral_1h_input_tokens: i64,
    web_search_requests: i64,
    web_fetch_requests: i64,
    flags: u8,
    service_tier: ServiceTier,
    extra: Option<Box<ExtraUsageFields>>,
}

impl ClaudeUsageBucket {
    fn request_context(&self) -> i64 {
        let mut cache_creation = self.cache_creation_input_tokens;
        if cache_creation == 0 && (self.flags & FLAG_HAS_CC != 0) {
            cache_creation = self.ephemeral_5m_input_tokens
                + self.ephemeral_1h_input_tokens
                + self.extra.as_deref().map_or(0, |extra| {
                    extra.extra_cache_creation.iter().map(|(_, n)| *n).sum()
                });
        }
        self.input_tokens + self.cache_read_input_tokens + cache_creation
    }
}

enum ParsedClaudeUsage {
    Valid(ClaudeUsageBucket),
    Invalid,
}

impl ParsedClaudeUsage {
    fn from_value(value: &Value) -> Self {
        let Some(usage) = value.as_object() else {
            return Self::Invalid;
        };
        if usage.is_empty() {
            return Self::Valid(ClaudeUsageBucket::default());
        }

        let mut bucket = ClaudeUsageBucket::default();
        let mut recognized = false;

        for (key, target) in [
            ("input_tokens", &mut bucket.input_tokens),
            (
                "cache_creation_input_tokens",
                &mut bucket.cache_creation_input_tokens,
            ),
            (
                "cache_read_input_tokens",
                &mut bucket.cache_read_input_tokens,
            ),
            ("output_tokens", &mut bucket.output_tokens),
        ] {
            if let Some(val) = usage.get(key) {
                let Some(n) = val.as_i64() else {
                    return Self::Invalid;
                };
                *target = n;
                recognized = true;
            }
        }

        if let Some(service_tier) = usage.get("service_tier").and_then(Value::as_str) {
            bucket.service_tier = ServiceTier::from_str_value(service_tier);
        }

        if let Some(nested) = usage.get("cache_creation") {
            let Some(nested) = nested.as_object() else {
                return Self::Invalid;
            };
            bucket.flags |= FLAG_HAS_CC;
            if nested.is_empty() {
                recognized = true;
            } else {
                for (k, v) in nested {
                    match k.as_str() {
                        "ephemeral_5m_input_tokens" => {
                            let Some(n) = v.as_i64() else {
                                return Self::Invalid;
                            };
                            bucket.ephemeral_5m_input_tokens = n;
                            bucket.flags |= FLAG_HAS_CC_5M;
                            recognized = true;
                        }
                        "ephemeral_1h_input_tokens" => {
                            let Some(n) = v.as_i64() else {
                                return Self::Invalid;
                            };
                            bucket.ephemeral_1h_input_tokens = n;
                            bucket.flags |= FLAG_HAS_CC_1H;
                            recognized = true;
                        }
                        _ => {
                            if let Some(n) = v.as_i64() {
                                bucket
                                    .extra
                                    .get_or_insert_with(Default::default)
                                    .extra_cache_creation
                                    .push((k.clone(), n));
                            }
                        }
                    }
                }
            }
        }

        if let Some(nested) = usage.get("server_tool_use") {
            let Some(nested) = nested.as_object() else {
                return Self::Invalid;
            };
            bucket.flags |= FLAG_HAS_STU;
            if nested.is_empty() {
                recognized = true;
            } else {
                for (k, v) in nested {
                    match k.as_str() {
                        "web_search_requests" => {
                            let Some(n) = v.as_i64() else {
                                return Self::Invalid;
                            };
                            bucket.web_search_requests = n;
                            bucket.flags |= FLAG_HAS_WEB_SEARCH;
                            recognized = true;
                        }
                        "web_fetch_requests" => {
                            let Some(n) = v.as_i64() else {
                                return Self::Invalid;
                            };
                            bucket.web_fetch_requests = n;
                            bucket.flags |= FLAG_HAS_WEB_FETCH;
                            recognized = true;
                        }
                        _ => {
                            if let Some(n) = v.as_i64() {
                                bucket
                                    .extra
                                    .get_or_insert_with(Default::default)
                                    .extra_server_tool_use
                                    .push((k.clone(), n));
                            }
                        }
                    }
                }
            }
        }

        if !recognized {
            return Self::Invalid;
        }

        if let Some(iters) = usage.get("iterations").and_then(Value::as_array) {
            for iter in iters {
                if iter.get("type").and_then(Value::as_str) != Some("advisor_message") {
                    continue;
                }
                let model = iter
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let parsed_iter = Self::from_value(iter);
                bucket
                    .extra
                    .get_or_insert_with(Default::default)
                    .advisor_iterations
                    .push(ClaudeAdvisorIteration {
                        model,
                        usage: parsed_iter,
                    });
            }
        }

        Self::Valid(bucket)
    }
}

macro_rules! ignore_non_matching_json {
    ($fallback:expr, $( $kind:ident ),+ $(,)?) => {
        $( ignore_non_matching_json!(@one $kind, $fallback); )+
    };
    (@one bool, $fallback:expr) => {
        fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> { Ok($fallback) }
    };
    (@one i64, $fallback:expr) => {
        fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E> { Ok($fallback) }
    };
    (@one u64, $fallback:expr) => {
        fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E> { Ok($fallback) }
    };
    (@one f64, $fallback:expr) => {
        fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> { Ok($fallback) }
    };
    (@one str, $fallback:expr) => {
        fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> { Ok($fallback) }
    };
    (@one unit, $fallback:expr) => {
        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> { Ok($fallback) }
    };
    (@one none, $fallback:expr) => {
        fn visit_none<E>(self) -> std::result::Result<Self::Value, E> { Ok($fallback) }
    };
    (@one seq, $fallback:expr) => {
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok($fallback)
        }
    };
    (@one map, $fallback:expr) => {
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok($fallback)
        }
    };
}

struct MaybeI64(Option<i64>);

impl<'de> Deserialize<'de> for MaybeI64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct MaybeI64Visitor;
        impl<'de> Visitor<'de> for MaybeI64Visitor {
            type Value = MaybeI64;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_i64<E>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(MaybeI64(Some(v)))
            }

            fn visit_u64<E>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(MaybeI64(i64::try_from(v).ok()))
            }

            ignore_non_matching_json!(MaybeI64(None), bool, f64, str, unit, none, seq, map);
        }
        deserializer.deserialize_any(MaybeI64Visitor)
    }
}

struct MaybeServiceTier(ServiceTier);

impl<'de> Deserialize<'de> for MaybeServiceTier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = MaybeServiceTier;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E> {
                Ok(MaybeServiceTier(ServiceTier::from_str_value(v)))
            }

            ignore_non_matching_json!(
                MaybeServiceTier(ServiceTier::Absent),
                bool,
                i64,
                u64,
                f64,
                unit,
                none,
                seq,
                map
            );
        }
        deserializer.deserialize_any(V)
    }
}

struct MaybeString<'a>(Option<Cow<'a, str>>);

impl<'de> Deserialize<'de> for MaybeString<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = MaybeString<'de>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }

            fn visit_borrowed_str<E>(self, v: &'de str) -> std::result::Result<Self::Value, E> {
                Ok(MaybeString(Some(Cow::Borrowed(v))))
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E> {
                Ok(MaybeString(Some(Cow::Owned(v.to_owned()))))
            }

            fn visit_string<E>(self, v: String) -> std::result::Result<Self::Value, E> {
                Ok(MaybeString(Some(Cow::Owned(v))))
            }

            ignore_non_matching_json!(MaybeString(None), bool, i64, u64, f64, unit, none, seq, map);
        }
        deserializer.deserialize_any(V)
    }
}

struct NestedUsageOutcome {
    valid: bool,
    recognized: bool,
    key_a: Option<i64>,
    key_b: Option<i64>,
    extra: Option<Vec<(String, i64)>>,
}

struct NestedUsageSeed {
    key_a: &'static str,
    key_b: &'static str,
}

impl<'de> Visitor<'de> for NestedUsageSeed {
    type Value = NestedUsageOutcome;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a nested Claude usage object")
    }

    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut valid = true;
        let mut recognized = false;
        let mut key_count = 0_usize;
        let mut key_a = None;
        let mut key_b = None;
        let mut extra: Option<Vec<(String, i64)>> = None;

        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            key_count += 1;
            let MaybeI64(value) = map.next_value()?;
            if key.as_ref() == self.key_a {
                match value {
                    Some(n) => {
                        key_a = Some(n);
                        recognized = true;
                    }
                    None => valid = false,
                }
            } else if key.as_ref() == self.key_b {
                match value {
                    Some(n) => {
                        key_b = Some(n);
                        recognized = true;
                    }
                    None => valid = false,
                }
            } else if let Some(n) = value {
                extra
                    .get_or_insert_with(Vec::new)
                    .push((key.into_owned(), n));
            }
        }

        if key_count == 0 {
            recognized = true;
        }

        Ok(NestedUsageOutcome {
            valid,
            recognized,
            key_a,
            key_b,
            extra,
        })
    }

    ignore_non_matching_json!(invalid_nested(), bool, i64, u64, f64, str, unit, none, seq);
}

impl<'de> serde::de::DeserializeSeed<'de> for NestedUsageSeed {
    type Value = NestedUsageOutcome;

    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

fn invalid_nested() -> NestedUsageOutcome {
    NestedUsageOutcome {
        valid: false,
        recognized: false,
        key_a: None,
        key_b: None,
        extra: None,
    }
}

struct ParsedUsageMap<'de> {
    usage: ParsedClaudeUsage,
    iter_type: Option<Cow<'de, str>>,
    iter_model: Option<Cow<'de, str>>,
}

fn parse_usage_map<'de, A: MapAccess<'de>>(
    mut map: A,
    parse_iterations: bool,
) -> std::result::Result<ParsedUsageMap<'de>, A::Error> {
    let mut bucket = ClaudeUsageBucket::default();
    let mut valid = true;
    let mut recognized = false;
    let mut field_count = 0_usize;
    let mut iter_type: Option<Cow<'de, str>> = None;
    let mut iter_model: Option<Cow<'de, str>> = None;

    while let Some(key) = map.next_key::<Cow<'de, str>>()? {
        field_count += 1;
        match key.as_ref() {
            "input_tokens" => match map.next_value::<MaybeI64>()?.0 {
                Some(n) => {
                    bucket.input_tokens = n;
                    recognized = true;
                }
                None => valid = false,
            },
            "cache_creation_input_tokens" => match map.next_value::<MaybeI64>()?.0 {
                Some(n) => {
                    bucket.cache_creation_input_tokens = n;
                    recognized = true;
                }
                None => valid = false,
            },
            "cache_read_input_tokens" => match map.next_value::<MaybeI64>()?.0 {
                Some(n) => {
                    bucket.cache_read_input_tokens = n;
                    recognized = true;
                }
                None => valid = false,
            },
            "output_tokens" => match map.next_value::<MaybeI64>()?.0 {
                Some(n) => {
                    bucket.output_tokens = n;
                    recognized = true;
                }
                None => valid = false,
            },
            "service_tier" => {
                let tier = map.next_value::<MaybeServiceTier>()?.0;
                if tier.is_present() {
                    bucket.service_tier = tier;
                }
            }
            "cache_creation" => {
                let nested = map.next_value_seed(NestedUsageSeed {
                    key_a: "ephemeral_5m_input_tokens",
                    key_b: "ephemeral_1h_input_tokens",
                })?;
                if !nested.valid {
                    valid = false;
                } else {
                    bucket.flags |= FLAG_HAS_CC;
                    if nested.recognized {
                        recognized = true;
                    }
                    if let Some(n) = nested.key_a {
                        bucket.ephemeral_5m_input_tokens = n;
                        bucket.flags |= FLAG_HAS_CC_5M;
                    }
                    if let Some(n) = nested.key_b {
                        bucket.ephemeral_1h_input_tokens = n;
                        bucket.flags |= FLAG_HAS_CC_1H;
                    }
                    if let Some(extra) = nested.extra {
                        bucket
                            .extra
                            .get_or_insert_with(Default::default)
                            .extra_cache_creation
                            .extend(extra);
                    }
                }
            }
            "server_tool_use" => {
                let nested = map.next_value_seed(NestedUsageSeed {
                    key_a: "web_search_requests",
                    key_b: "web_fetch_requests",
                })?;
                if !nested.valid {
                    valid = false;
                } else {
                    bucket.flags |= FLAG_HAS_STU;
                    if nested.recognized {
                        recognized = true;
                    }
                    if let Some(n) = nested.key_a {
                        bucket.web_search_requests = n;
                        bucket.flags |= FLAG_HAS_WEB_SEARCH;
                    }
                    if let Some(n) = nested.key_b {
                        bucket.web_fetch_requests = n;
                        bucket.flags |= FLAG_HAS_WEB_FETCH;
                    }
                    if let Some(extra) = nested.extra {
                        bucket
                            .extra
                            .get_or_insert_with(Default::default)
                            .extra_server_tool_use
                            .extend(extra);
                    }
                }
            }
            "iterations" if parse_iterations => {
                let advisors = map.next_value::<AdvisorIterationsList>()?.0;
                if !advisors.is_empty() {
                    bucket
                        .extra
                        .get_or_insert_with(Default::default)
                        .advisor_iterations = advisors;
                }
            }
            "type" if !parse_iterations => {
                iter_type = map.next_value::<MaybeString<'de>>()?.0;
            }
            "model" if !parse_iterations => {
                iter_model = map.next_value::<MaybeString<'de>>()?.0;
            }
            _ => {
                map.next_value::<IgnoredAny>()?;
            }
        }
    }

    let parsed = if valid && (field_count == 0 || recognized) {
        ParsedClaudeUsage::Valid(bucket)
    } else {
        ParsedClaudeUsage::Invalid
    };
    Ok(ParsedUsageMap {
        usage: parsed,
        iter_type,
        iter_model,
    })
}

struct AdvisorIterationEntry(Option<ClaudeAdvisorIteration>);

impl<'de> Deserialize<'de> for AdvisorIterationEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = AdvisorIterationEntry;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Claude usage iteration entry")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let parsed = parse_usage_map(map, false)?;
                if parsed.iter_type.as_deref() == Some("advisor_message") {
                    Ok(AdvisorIterationEntry(Some(ClaudeAdvisorIteration {
                        model: parsed.iter_model.map(Cow::into_owned),
                        usage: parsed.usage,
                    })))
                } else {
                    Ok(AdvisorIterationEntry(None))
                }
            }

            ignore_non_matching_json!(
                AdvisorIterationEntry(None),
                bool,
                i64,
                u64,
                f64,
                str,
                unit,
                none,
                seq
            );
        }
        deserializer.deserialize_any(V)
    }
}

struct AdvisorIterationsList(Vec<ClaudeAdvisorIteration>);

impl<'de> Deserialize<'de> for AdvisorIterationsList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = AdvisorIterationsList;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Claude usage iterations array")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                while let Some(entry) = seq.next_element::<AdvisorIterationEntry>()? {
                    if let Some(advisor) = entry.0 {
                        items.push(advisor);
                    }
                }
                Ok(AdvisorIterationsList(items))
            }

            ignore_non_matching_json!(
                AdvisorIterationsList(Vec::new()),
                bool,
                i64,
                u64,
                f64,
                str,
                unit,
                none,
                map
            );
        }
        deserializer.deserialize_any(V)
    }
}

impl<'de> Deserialize<'de> for ParsedClaudeUsage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ParsedClaudeUsage;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Claude usage object")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                Ok(parse_usage_map(map, true)?.usage)
            }

            ignore_non_matching_json!(
                ParsedClaudeUsage::Invalid,
                bool,
                i64,
                u64,
                f64,
                str,
                unit,
                none,
                seq
            );
        }
        deserializer.deserialize_any(V)
    }
}

#[derive(Default)]
struct ModelUsageAccum {
    input_tokens: i64,
    cache_creation_input_tokens: i64,
    cache_read_input_tokens: i64,
    output_tokens: i64,
    flags: u8,
    ephemeral_5m_input_tokens: i64,
    ephemeral_1h_input_tokens: i64,
    web_search_requests: i64,
    web_fetch_requests: i64,
    service_tier: ServiceTier,
    extra_cache_creation: FastHashMap<String, i64>,
    extra_server_tool_use: FastHashMap<String, i64>,
    above_tiers: Vec<TierSlice>,
}

impl ModelUsageAccum {
    fn add(&mut self, bucket: &ClaudeUsageBucket, tier_level: usize) {
        self.input_tokens += bucket.input_tokens;
        self.cache_creation_input_tokens += bucket.cache_creation_input_tokens;
        self.cache_read_input_tokens += bucket.cache_read_input_tokens;
        self.output_tokens += bucket.output_tokens;
        self.flags |= bucket.flags;
        self.ephemeral_5m_input_tokens += bucket.ephemeral_5m_input_tokens;
        self.ephemeral_1h_input_tokens += bucket.ephemeral_1h_input_tokens;
        self.web_search_requests += bucket.web_search_requests;
        self.web_fetch_requests += bucket.web_fetch_requests;

        if bucket.service_tier.is_present() {
            self.service_tier = bucket.service_tier.clone();
        }
        if let Some(extra) = bucket.extra.as_deref() {
            for (k, v) in &extra.extra_cache_creation {
                *self.extra_cache_creation.entry(k.clone()).or_insert(0) += *v;
            }
            for (k, v) in &extra.extra_server_tool_use {
                *self.extra_server_tool_use.entry(k.clone()).or_insert(0) += *v;
            }
        }

        if tier_level > 0 && tier_level <= MAX_TIER_LEVELS {
            let scalar_cc = bucket.cache_creation_input_tokens;
            let cc_1h = bucket.ephemeral_1h_input_tokens;
            let ephemeral_5m = bucket.ephemeral_5m_input_tokens;
            let total_cc = scalar_cc.max(ephemeral_5m + cc_1h);
            if self.above_tiers.len() < tier_level {
                self.above_tiers.resize(tier_level, TierSlice::default());
            }
            self.above_tiers[tier_level - 1].merge(&TierSlice {
                input_tokens: bucket.input_tokens,
                output_tokens: bucket.output_tokens,
                reasoning_tokens: 0,
                cache_read: bucket.cache_read_input_tokens,
                cache_creation_5m: total_cc - cc_1h,
                cache_creation_1h: cc_1h,
            });
        }
    }

    fn into_value(self) -> Value {
        let mut obj = serde_json::Map::with_capacity(8);
        obj.insert("input_tokens".to_string(), self.input_tokens.into());
        obj.insert(
            "cache_creation_input_tokens".to_string(),
            self.cache_creation_input_tokens.into(),
        );
        obj.insert(
            "cache_read_input_tokens".to_string(),
            self.cache_read_input_tokens.into(),
        );

        let mut cc = serde_json::Map::new();
        if self.flags & FLAG_HAS_CC_5M != 0 {
            cc.insert(
                "ephemeral_5m_input_tokens".to_string(),
                self.ephemeral_5m_input_tokens.into(),
            );
        }
        if self.flags & FLAG_HAS_CC_1H != 0 {
            cc.insert(
                "ephemeral_1h_input_tokens".to_string(),
                self.ephemeral_1h_input_tokens.into(),
            );
        }
        for (k, v) in self.extra_cache_creation {
            cc.insert(k, v.into());
        }
        obj.insert("cache_creation".to_string(), Value::Object(cc));
        obj.insert("output_tokens".to_string(), self.output_tokens.into());
        obj.insert(
            "service_tier".to_string(),
            Value::String(self.service_tier.as_str().to_string()),
        );

        if self.flags & FLAG_HAS_STU != 0 {
            let mut stu = serde_json::Map::new();
            if self.flags & FLAG_HAS_WEB_SEARCH != 0 {
                stu.insert(
                    "web_search_requests".to_string(),
                    self.web_search_requests.into(),
                );
            }
            if self.flags & FLAG_HAS_WEB_FETCH != 0 {
                stu.insert(
                    "web_fetch_requests".to_string(),
                    self.web_fetch_requests.into(),
                );
            }
            for (k, v) in self.extra_server_tool_use {
                stu.insert(k, v.into());
            }
            obj.insert("server_tool_use".to_string(), Value::Object(stu));
        }

        if !self.above_tiers.is_empty() {
            let mut above = serde_json::Map::new();
            for (index, slice) in self.above_tiers.iter().enumerate() {
                write_tier_slice(&mut above, index + 1, slice);
            }
            if !above.is_empty() {
                obj.insert("above_tier".to_string(), Value::Object(above));
            }
        }

        Value::Object(obj)
    }
}

enum RequestSlots {
    Single {
        request_id: Option<String>,
        slot: usize,
    },
    Multiple(Vec<(Option<String>, usize)>),
}

struct ClaudeFold<'a> {
    classifier: Option<TierClassifier<'a>>,
    state: SessionParseState,
    // Interned model names referenced by index from `billed_requests` to avoid cloning per API block.
    models: Vec<String>,
    // Keyed by `tool_use_id` so top-level results dispatch by originating tool rather than ambiguous fields like `filePath` (also set by `ExitPlanMode`).
    pending_tool_uses: FastHashMap<String, PendingClaudeTool>,
    diagnostics: ParseDiagnostics,
    // Claude Code repeats the full `usage` object on each content block of one API response, with earlier blocks carrying partial `output_tokens`. Banking `(model_idx, usage)` by `message.id` + `requestId` keeps only the final snapshot per response without allocating on duplicates.
    billed_requests: Vec<(usize, ClaudeUsageBucket)>,
    billed_index: FastHashMap<String, RequestSlots>,
}

impl<'a> ClaudeFold<'a> {
    fn new(mode: ParseMode, tiers: Option<&'a TierThresholds>) -> Self {
        Self {
            classifier: tiers.map(TierClassifier::new),
            state: SessionParseState::with_mode(mode),
            models: Vec::with_capacity(capacity::MODELS_PER_SESSION),
            pending_tool_uses: FastHashMap::with_capacity(64),
            diagnostics: ParseDiagnostics::default(),
            billed_requests: Vec::with_capacity(64),
            billed_index: FastHashMap::with_capacity(64),
        }
    }

    fn intern_model(&mut self, model: &str) -> usize {
        if let Some(index) = self.models.iter().position(|existing| existing == model) {
            return index;
        }
        let index = self.models.len();
        self.models.push(model.to_string());
        index
    }

    fn observe_envelope(
        &mut self,
        log_type: &str,
        cwd: &str,
        session_id: &str,
        timestamp: &str,
        has_tool_use_result: bool,
    ) -> i64 {
        let recognized = matches!(
            log_type,
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
        ) || has_tool_use_result;
        if recognized {
            self.diagnostics.record_recognized_source();
        } else {
            self.diagnostics.record_unrecognized();
        }
        if self.state.folder_path.is_empty() && !cwd.is_empty() {
            self.state.folder_path.push_str(cwd);
        }
        if !session_id.is_empty() && self.state.task_id != session_id {
            self.state.task_id.clear();
            self.state.task_id.push_str(session_id);
        }

        let ts = parse_iso_timestamp(timestamp);
        if ts > self.state.last_ts {
            self.state.last_ts = ts;
        }
        ts
    }

    fn observe_assistant_usage(
        &mut self,
        message_id: Option<&str>,
        request_id: Option<&str>,
        model: Option<&str>,
        usage: Option<ParsedClaudeUsage>,
    ) {
        let Some(usage) = usage else {
            return;
        };
        let model = model.filter(|model| !model.is_empty());
        let (Some(model), ParsedClaudeUsage::Valid(bucket)) = (model, usage) else {
            self.diagnostics.record_relevant(false);
            return;
        };

        let model_idx = self.intern_model(model);
        let Some((msg_id, req_id)) = claude_request_key_parts(message_id, request_id) else {
            // Without a response key, treat the record as a distinct request.
            self.billed_requests.push((model_idx, bucket));
            return;
        };

        if let Some(slots) = self.billed_index.get_mut(msg_id) {
            match slots {
                RequestSlots::Single {
                    request_id: existing_req,
                    slot,
                } => {
                    if existing_req.as_deref() == req_id {
                        self.billed_requests[*slot] = (model_idx, bucket);
                        return;
                    }
                    let new_slot = self.billed_requests.len();
                    self.billed_requests.push((model_idx, bucket));
                    let first = (existing_req.take(), *slot);
                    *slots =
                        RequestSlots::Multiple(vec![first, (req_id.map(str::to_string), new_slot)]);
                }
                RequestSlots::Multiple(entries) => {
                    if let Some((_, slot)) = entries
                        .iter()
                        .find(|(existing_req, _)| existing_req.as_deref() == req_id)
                    {
                        self.billed_requests[*slot] = (model_idx, bucket);
                        return;
                    }
                    let new_slot = self.billed_requests.len();
                    self.billed_requests.push((model_idx, bucket));
                    entries.push((req_id.map(str::to_string), new_slot));
                }
            }
            return;
        }

        let slot = self.billed_requests.len();
        self.billed_requests.push((model_idx, bucket));
        self.billed_index.insert(
            msg_id.to_string(),
            RequestSlots::Single {
                request_id: req_id.map(str::to_string),
                slot,
            },
        );
    }

    fn observe_tool_use_full(
        &mut self,
        id: String,
        name: &str,
        input: Option<ClaudeToolInput>,
        ts: i64,
    ) {
        let kind = PendingToolKind::from_name(name);
        let tracked_file_tool = kind.is_tracked_file_tool();
        let input_supported =
            tracked_file_tool && tracked_tool_input_supported(name, input.as_ref());

        match name {
            "Read" => {
                if input_supported {
                    self.diagnostics.record_relevant(true);
                    self.state.tool_counts.read += 1;
                } else if id.is_empty() {
                    self.diagnostics.record_relevant(false);
                }
            }
            "Write" => {
                if input_supported {
                    self.diagnostics.record_relevant(true);
                    self.state.tool_counts.write += 1;
                } else if id.is_empty() {
                    self.diagnostics.record_relevant(false);
                }
            }
            "Edit" => {
                if input_supported {
                    self.diagnostics.record_relevant(true);
                    self.state.tool_counts.edit += 1;
                } else if id.is_empty() {
                    self.diagnostics.record_relevant(false);
                }
            }
            "TodoWrite" | "TaskCreate" | "TaskUpdate" | "TaskStop" => {
                self.diagnostics.record_relevant(true);
                self.state.tool_counts.todo_write += 1;
            }
            "Bash" => {
                self.diagnostics
                    .record_relevant(tracked_tool_input_supported(name, input.as_ref()));
                if let Some(input) = input.as_ref() {
                    let command = input.command.as_deref().unwrap_or("");
                    let description = input.description.as_deref().unwrap_or("");
                    self.state.add_run_command(command, description, ts);
                }
            }
            _ => {}
        }

        if !id.is_empty() {
            self.pending_tool_uses.insert(
                id,
                PendingClaudeTool {
                    kind,
                    input: tracked_file_tool
                        .then_some(input)
                        .flatten()
                        .map(PendingToolInput::Full),
                    input_supported,
                },
            );
        }
    }

    fn observe_tool_use_usage_only(
        &mut self,
        id: Cow<'_, str>,
        name: &str,
        input: Option<UsageOnlyClaudeToolInput<'_>>,
        ts: i64,
    ) {
        let kind = PendingToolKind::from_name(name);
        let tracked_file_tool = kind.is_tracked_file_tool();
        let input_supported =
            tracked_file_tool && usage_only_tool_input_supported(name, input.as_ref());

        match name {
            "Read" => {
                if input_supported {
                    self.diagnostics.record_relevant(true);
                    self.state.tool_counts.read += 1;
                } else if id.is_empty() {
                    self.diagnostics.record_relevant(false);
                }
            }
            "Write" => {
                if input_supported {
                    self.diagnostics.record_relevant(true);
                    self.state.tool_counts.write += 1;
                } else if id.is_empty() {
                    self.diagnostics.record_relevant(false);
                }
            }
            "Edit" => {
                if input_supported {
                    self.diagnostics.record_relevant(true);
                    self.state.tool_counts.edit += 1;
                } else if id.is_empty() {
                    self.diagnostics.record_relevant(false);
                }
            }
            "TodoWrite" | "TaskCreate" | "TaskUpdate" | "TaskStop" => {
                self.diagnostics.record_relevant(true);
                self.state.tool_counts.todo_write += 1;
            }
            "Bash" => {
                self.diagnostics
                    .record_relevant(usage_only_tool_input_supported(name, input.as_ref()));
                if let Some(input) = input.as_ref() {
                    let command = input.command.as_deref().unwrap_or("");
                    self.state.add_run_command(command, "", ts);
                }
            }
            _ => {}
        }

        if !id.is_empty() {
            let stored_input = if tracked_file_tool {
                input.map(|inp| {
                    PendingToolInput::UsageOnly(UsageOnlyPendingInput {
                        file_path: inp.file_path.map(Cow::into_owned),
                        content: inp.content.map(|c| c.0),
                        new_metrics: inp.new_string.map(|c| c.0),
                    })
                })
            } else {
                None
            };
            self.pending_tool_uses.insert(
                id.into_owned(),
                PendingClaudeTool {
                    kind,
                    input: stored_input,
                    input_supported,
                },
            );
        }
    }

    fn observe_top_level_tool_result_full(
        &mut self,
        tur: &ClaudeToolUseResult,
        first_result: Option<(&str, bool)>,
        ts: i64,
    ) {
        let correlated = first_result.and_then(|(tool_use_id, is_error)| {
            self.pending_tool_uses
                .remove(tool_use_id)
                .map(|pending| (pending, is_error))
        });

        if let Some((pending, true)) = correlated.as_ref() {
            if pending.kind.is_tracked_file_tool() && !pending.input_supported {
                // Explicit rejection of bad arguments is a valid lifecycle, not schema drift.
                self.diagnostics.record_relevant(true);
            }
        } else {
            let expected_tool = correlated.as_ref().map(|(pending, _)| pending.kind);
            match validate_top_level_tool_result_for(tur, expected_tool) {
                TopLevelToolResult::Irrelevant => {}
                TopLevelToolResult::Unsupported => self.diagnostics.record_relevant(false),
                TopLevelToolResult::NonTextRead => {
                    self.diagnostics.record_relevant(true);
                    if let Some(path) = correlated
                        .as_ref()
                        .and_then(|(pending, _)| pending.file_path())
                    {
                        self.state.add_non_text_read_path(path);
                    }
                }
                TopLevelToolResult::Supported(kind) => {
                    self.diagnostics.record_relevant(true);
                    dispatch_top_level_tool_result(&mut self.state, tur, kind, ts);
                }
            }
        }
    }

    fn observe_top_level_tool_result_usage_only(
        &mut self,
        tur: &UsageOnlyToolUseResult<'_>,
        first_result: Option<(&str, bool)>,
    ) {
        let correlated = first_result.and_then(|(tool_use_id, is_error)| {
            self.pending_tool_uses
                .remove(tool_use_id)
                .map(|pending| (pending, is_error))
        });

        if let Some((pending, true)) = correlated.as_ref() {
            if pending.kind.is_tracked_file_tool() && !pending.input_supported {
                self.diagnostics.record_relevant(true);
            }
        } else {
            let expected_tool = correlated.as_ref().map(|(pending, _)| pending.kind);
            match validate_top_level_tool_result_usage_only(tur, expected_tool) {
                TopLevelToolResult::Irrelevant => {}
                TopLevelToolResult::Unsupported => self.diagnostics.record_relevant(false),
                TopLevelToolResult::NonTextRead => {
                    self.diagnostics.record_relevant(true);
                    if let Some(path) = correlated
                        .as_ref()
                        .and_then(|(pending, _)| pending.file_path())
                    {
                        self.state.add_non_text_read_path(path);
                    }
                }
                TopLevelToolResult::Supported(kind) => {
                    self.diagnostics.record_relevant(true);
                    dispatch_top_level_tool_result_usage_only(&mut self.state, tur, kind);
                }
            }
        }
    }

    // Subagent fallback for tool results inside `message.content[].tool_result`. Gated on `is_sidechain` because a main-session rejection has a string `toolUseResult` (deserializing to `None`) alongside a `tool_result` block whose text is not a file body.
    fn observe_user_tool_result_full(
        &mut self,
        is_sidechain: bool,
        tool_use_id: &str,
        is_error: bool,
        ts: i64,
        content: &str,
    ) {
        self.observe_user_tool_result_lazy(is_sidechain, tool_use_id, is_error, ts, || {
            Cow::Borrowed(content)
        });
    }

    fn observe_user_tool_result_lazy<'c>(
        &mut self,
        is_sidechain: bool,
        tool_use_id: &str,
        is_error: bool,
        ts: i64,
        content: impl FnOnce() -> Cow<'c, str>,
    ) {
        let Some(pending) = self.pending_tool_uses.remove(tool_use_id) else {
            return;
        };

        if is_error {
            if pending.kind.is_tracked_file_tool() && !pending.input_supported {
                self.diagnostics.record_relevant(true);
            }
            return;
        }
        if !is_sidechain || !pending.kind.is_tracked_file_tool() {
            return;
        }

        let normalized = pending.input_supported;
        self.diagnostics.record_relevant(normalized);
        if normalized && let Some(PendingToolInput::Full(input)) = pending.input.as_ref() {
            let body = if pending.kind == PendingToolKind::Read {
                content()
            } else {
                Cow::Borrowed("")
            };
            dispatch_subagent_tool_result(&mut self.state, pending.kind, input, &body, ts);
        }
    }

    fn observe_user_tool_result_usage_only(
        &mut self,
        is_sidechain: bool,
        tool_use_id: &str,
        is_error: bool,
        raw_content: Option<&RawValue>,
    ) {
        let Some(pending) = self.pending_tool_uses.remove(tool_use_id) else {
            return;
        };

        if is_error {
            if pending.kind.is_tracked_file_tool() && !pending.input_supported {
                self.diagnostics.record_relevant(true);
            }
            return;
        }
        if !is_sidechain || !pending.kind.is_tracked_file_tool() {
            return;
        }

        let normalized = pending.input_supported;
        self.diagnostics.record_relevant(normalized);
        if normalized && let Some(PendingToolInput::UsageOnly(input)) = pending.input.as_ref() {
            let Some(file_path) = input.file_path.as_deref() else {
                return;
            };
            match pending.kind {
                PendingToolKind::Read => {
                    let metrics = count_raw_tool_result_metrics(raw_content);
                    preserve_file_tool_counts(&mut self.state, |state| {
                        state.add_read_counts(file_path, metrics.lines, metrics.chars);
                    });
                }
                PendingToolKind::Write => {
                    let metrics = input.content.unwrap_or_default();
                    preserve_file_tool_counts(&mut self.state, |state| {
                        state.add_write_counts(file_path, metrics.lines, metrics.chars);
                    });
                }
                PendingToolKind::Edit => {
                    let metrics = input.new_metrics.unwrap_or_default();
                    preserve_file_tool_counts(&mut self.state, |state| {
                        state.add_edit_counts_raw(file_path, metrics.lines, metrics.chars);
                    });
                }
                PendingToolKind::Other => {}
            }
        }
    }

    fn finish(mut self) -> ParsedAnalysis {
        let mut conv_accums: Vec<Option<ModelUsageAccum>> =
            (0..self.models.len()).map(|_| None).collect();
        // Kept separate from `conversation_usage` so file-op counts are not attributed to advisor models; usage aggregation merges both.
        let mut advisor_accums: FastHashMap<String, ModelUsageAccum> = FastHashMap::default();

        for (model_idx, bucket) in self.billed_requests {
            let model = &self.models[model_idx];
            self.diagnostics.record_relevant(true);
            if !model.contains("<synthetic>") {
                let level = self.classifier.as_mut().map_or(0, |classifier| {
                    classifier.level(model, bucket.request_context())
                });
                conv_accums[model_idx]
                    .get_or_insert_with(ModelUsageAccum::default)
                    .add(&bucket, level);
            }

            // Top-level `usage` sums `message` iterations and excludes `advisor_message` iterations, so fold advisor iterations separately under their own model (falling back to the main model).
            if let Some(extra) = bucket.extra.as_deref() {
                for iter in &extra.advisor_iterations {
                    let adv_model = iter.model.as_deref().unwrap_or(model.as_str());
                    match &iter.usage {
                        ParsedClaudeUsage::Valid(adv_bucket) if !adv_model.is_empty() => {
                            self.diagnostics.record_relevant(true);
                            if !adv_model.contains("<synthetic>") {
                                let level = self.classifier.as_mut().map_or(0, |classifier| {
                                    classifier.level(adv_model, adv_bucket.request_context())
                                });
                                if let Some(accum) = advisor_accums.get_mut(adv_model) {
                                    accum.add(adv_bucket, level);
                                } else {
                                    let mut accum = ModelUsageAccum::default();
                                    accum.add(adv_bucket, level);
                                    advisor_accums.insert(adv_model.to_string(), accum);
                                }
                            }
                        }
                        _ => self.diagnostics.record_relevant(false),
                    }
                }
            }
        }

        let mut conversation_usage = FastHashMap::with_capacity(self.models.len());
        for (model, accum) in self.models.into_iter().zip(conv_accums) {
            if let Some(accum) = accum {
                conversation_usage.insert(model, accum.into_value());
            }
        }
        let mut advisor_usage = FastHashMap::with_capacity(advisor_accums.len());
        for (model, accum) in advisor_accums {
            advisor_usage.insert(model, accum.into_value());
        }

        for pending in self.pending_tool_uses.into_values() {
            if pending.kind.is_tracked_file_tool() && !pending.input_supported {
                self.diagnostics.record_relevant(false);
            }
        }

        if self.state.git_remote.is_empty() {
            self.state.git_remote = get_git_remote_url(&self.state.folder_path);
        }

        let mut record = self.state.into_record(conversation_usage);
        record.advisor_usage = advisor_usage;

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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedClaudeRecord<'a> {
    #[serde(default, borrow)]
    cwd: Cow<'a, str>,
    #[serde(default, borrow)]
    session_id: Cow<'a, str>,
    #[serde(default, borrow, rename = "type")]
    log_type: Cow<'a, str>,
    #[serde(default, borrow)]
    timestamp: Cow<'a, str>,
    #[serde(default, borrow)]
    message: Option<&'a RawValue>,
    #[serde(default, borrow)]
    request_id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    tool_use_result: Option<&'a RawValue>,
    #[serde(default)]
    is_sidechain: bool,
}

#[derive(Deserialize)]
struct BorrowedClaudeMessage<'a> {
    #[serde(default, borrow)]
    id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    model: Option<Cow<'a, str>>,
    #[serde(default)]
    usage: Option<ParsedClaudeUsage>,
    #[serde(
        default,
        borrow,
        deserialize_with = "deserialize_borrowed_content_items"
    )]
    content: ContentItems<'a>,
}

/// Inline-one container for Claude content blocks, avoiding `Vec` allocations on the common single-block record layout.
#[derive(Default)]
enum ContentItems<'a> {
    #[default]
    Empty,
    One(BorrowedContentItem<'a>),
    Many(Vec<BorrowedContentItem<'a>>),
}

impl<'a> ContentItems<'a> {
    fn push(&mut self, item: BorrowedContentItem<'a>) {
        match std::mem::take(self) {
            Self::Empty => *self = Self::One(item),
            Self::One(first) => *self = Self::Many(vec![first, item]),
            Self::Many(mut items) => {
                items.push(item);
                *self = Self::Many(items);
            }
        }
    }

    fn single_tool_result(&self) -> Option<(&str, bool)> {
        match self {
            Self::One(BorrowedContentItem::ToolResult {
                tool_use_id,
                is_error,
                ..
            }) => Some((tool_use_id.as_ref(), *is_error)),
            _ => None,
        }
    }

    fn first_tool_result(&self) -> Option<(&str, bool)> {
        match self {
            Self::Empty => None,
            Self::One(item) => match item {
                BorrowedContentItem::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } => Some((tool_use_id.as_ref(), *is_error)),
                _ => None,
            },
            Self::Many(items) => items.iter().find_map(|item| match item {
                BorrowedContentItem::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } => Some((tool_use_id.as_ref(), *is_error)),
                _ => None,
            }),
        }
    }

    fn for_each_tool_result(&self, mut f: impl FnMut(&str, Option<&'a RawValue>, bool)) {
        match self {
            Self::Empty => {}
            Self::One(item) => {
                if let BorrowedContentItem::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } = item
                {
                    f(tool_use_id.as_ref(), *content, *is_error);
                }
            }
            Self::Many(items) => {
                for item in items {
                    if let BorrowedContentItem::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } = item
                    {
                        f(tool_use_id.as_ref(), *content, *is_error);
                    }
                }
            }
        }
    }
}

enum BorrowedContentItem<'a> {
    ToolUse {
        id: Cow<'a, str>,
        name: Cow<'a, str>,
        input: Option<&'a RawValue>,
    },
    ToolResult {
        tool_use_id: Cow<'a, str>,
        content: Option<&'a RawValue>,
        is_error: bool,
    },
    Other,
}

impl<'de> Deserialize<'de> for BorrowedContentItem<'de> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ItemVisitor;

        impl<'de> Visitor<'de> for ItemVisitor {
            type Value = BorrowedContentItem<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a Claude content block object")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut item_type: Option<Cow<'de, str>> = None;
                let mut id: Option<&'de RawValue> = None;
                let mut name: Option<&'de RawValue> = None;
                let mut input: Option<&'de RawValue> = None;
                let mut tool_use_id: Option<&'de RawValue> = None;
                let mut content: Option<&'de RawValue> = None;
                let mut is_error: Option<&'de RawValue> = None;

                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "type" => item_type = map.next_value()?,
                        "id" => id = map.next_value()?,
                        "name" => name = map.next_value()?,
                        "input" => input = map.next_value()?,
                        "tool_use_id" => tool_use_id = map.next_value()?,
                        "content" => content = map.next_value()?,
                        "is_error" => is_error = map.next_value()?,
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }

                match item_type.as_deref() {
                    Some("tool_use") => {
                        let id = match id {
                            Some(raw) => serde_json::from_str::<Option<Cow<'de, str>>>(raw.get())
                                .map_err(A::Error::custom)?
                                .unwrap_or(Cow::Borrowed("")),
                            None => Cow::Borrowed(""),
                        };
                        let name = match name {
                            Some(raw) => serde_json::from_str::<Option<Cow<'de, str>>>(raw.get())
                                .map_err(A::Error::custom)?
                                .unwrap_or(Cow::Borrowed("")),
                            None => Cow::Borrowed(""),
                        };
                        let input = match input {
                            Some(raw)
                                if matches!(name.as_ref(), "Read" | "Write" | "Edit" | "Bash") =>
                            {
                                Some(raw)
                            }
                            Some(raw) => {
                                let trimmed = raw.get();
                                if !trimmed.starts_with('{') && trimmed != "null" {
                                    return Err(A::Error::custom(
                                        "tool_use input must be an object",
                                    ));
                                }
                                None
                            }
                            None => None,
                        };
                        Ok(BorrowedContentItem::ToolUse { id, name, input })
                    }
                    Some("tool_result") => {
                        let tool_use_id = match tool_use_id {
                            Some(raw) => serde_json::from_str::<Option<Cow<'de, str>>>(raw.get())
                                .map_err(A::Error::custom)?
                                .unwrap_or_default(),
                            None => Cow::Borrowed(""),
                        };
                        let is_error = match is_error {
                            Some(raw) => serde_json::from_str::<Option<bool>>(raw.get())
                                .map_err(A::Error::custom)?
                                .unwrap_or(false),
                            None => false,
                        };
                        Ok(BorrowedContentItem::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        })
                    }
                    _ => Ok(BorrowedContentItem::Other),
                }
            }
        }

        deserializer.deserialize_map(ItemVisitor)
    }
}

fn deserialize_borrowed_content_items<'de, D>(
    deserializer: D,
) -> std::result::Result<ContentItems<'de>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ItemsVisitor;

    impl<'de> Visitor<'de> for ItemsVisitor {
        type Value = ContentItems<'de>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a content array or ignorable scalar")
        }

        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut items = ContentItems::Empty;
            while let Some(item) = seq.next_element::<BorrowedContentItem<'de>>()? {
                if !matches!(item, BorrowedContentItem::Other) {
                    items.push(item);
                }
            }
            Ok(items)
        }

        fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(ContentItems::Empty)
        }

        fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(ContentItems::Empty)
        }

        fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
            Ok(ContentItems::Empty)
        }

        fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
            Ok(ContentItems::Empty)
        }

        fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
            Ok(ContentItems::Empty)
        }

        fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
            Ok(ContentItems::Empty)
        }

        fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<Self::Value, E> {
            Ok(ContentItems::Empty)
        }

        fn visit_string<E: serde::de::Error>(
            self,
            _: String,
        ) -> std::result::Result<Self::Value, E> {
            Ok(ContentItems::Empty)
        }

        fn visit_map<A: MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(ContentItems::Empty)
        }
    }

    deserializer.deserialize_any(ItemsVisitor)
}

#[derive(Default, Deserialize)]
struct UsageOnlyClaudeToolInput<'a> {
    #[serde(default, borrow)]
    command: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    description: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    file_path: Option<Cow<'a, str>>,
    #[serde(default)]
    content: Option<CountedJsonString>,
    #[serde(default)]
    old_string: Option<CountedJsonString>,
    #[serde(default)]
    new_string: Option<CountedJsonString>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageOnlyToolUseResult<'a> {
    #[serde(default, borrow, rename = "type")]
    result_type: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    file: Option<UsageOnlyToolUseFile<'a>>,
    #[serde(default, borrow)]
    file_path: Option<Cow<'a, str>>,
    #[serde(default)]
    content: Option<CountedJsonString>,
    #[serde(default)]
    new_string: Option<CountedJsonString>,
    #[serde(default)]
    old_string: Option<CountedJsonString>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageOnlyToolUseFile<'a> {
    #[serde(default, borrow)]
    file_path: Option<Cow<'a, str>>,
    #[serde(default)]
    content: Option<CountedJsonString>,
}

enum ParsedToolInput<'a> {
    Full(Option<ClaudeToolInput>),
    UsageOnly(Option<UsageOnlyClaudeToolInput<'a>>),
}

struct ParsedToolUse<'a> {
    id: Cow<'a, str>,
    name: Cow<'a, str>,
    input: ParsedToolInput<'a>,
}

enum SmallToolUses<'a> {
    Empty,
    One(ParsedToolUse<'a>),
    Many(Vec<ParsedToolUse<'a>>),
}

impl<'a> SmallToolUses<'a> {
    fn push(&mut self, item: ParsedToolUse<'a>) {
        match std::mem::replace(self, Self::Empty) {
            Self::Empty => *self = Self::One(item),
            Self::One(first) => *self = Self::Many(vec![first, item]),
            Self::Many(mut items) => {
                items.push(item);
                *self = Self::Many(items);
            }
        }
    }
}

enum SmallToolUsesIter<'a> {
    Empty,
    One(Option<ParsedToolUse<'a>>),
    Many(std::vec::IntoIter<ParsedToolUse<'a>>),
}

impl<'a> Iterator for SmallToolUsesIter<'a> {
    type Item = ParsedToolUse<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty => None,
            Self::One(item) => item.take(),
            Self::Many(iter) => iter.next(),
        }
    }
}

impl<'a> IntoIterator for SmallToolUses<'a> {
    type Item = ParsedToolUse<'a>;
    type IntoIter = SmallToolUsesIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        match self {
            Self::Empty => SmallToolUsesIter::Empty,
            Self::One(item) => SmallToolUsesIter::One(Some(item)),
            Self::Many(items) => SmallToolUsesIter::Many(items.into_iter()),
        }
    }
}

fn parse_single_tool_use<'a>(
    id: &Cow<'a, str>,
    name: &Cow<'a, str>,
    raw_input: Option<&'a RawValue>,
    mode: ParseMode,
) -> std::result::Result<ParsedToolUse<'a>, serde_json::Error> {
    let input = match (mode, raw_input) {
        (ParseMode::UsageOnly, Some(raw)) if raw.get() == "null" => {
            ParsedToolInput::UsageOnly(None)
        }
        (ParseMode::UsageOnly, Some(raw)) if raw.get().starts_with('{') => {
            ParsedToolInput::UsageOnly(Some(serde_json::from_str::<UsageOnlyClaudeToolInput<'a>>(
                raw.get(),
            )?))
        }
        (ParseMode::UsageOnly, Some(_)) => {
            return Err(serde::de::Error::custom(
                "expected Claude tool input object",
            ));
        }
        (ParseMode::UsageOnly, None) => ParsedToolInput::UsageOnly(None),
        (ParseMode::Full, Some(raw)) if raw.get() == "null" => ParsedToolInput::Full(None),
        (ParseMode::Full, Some(raw)) if raw.get().starts_with('{') => {
            ParsedToolInput::Full(Some(serde_json::from_str::<ClaudeToolInput>(raw.get())?))
        }
        (ParseMode::Full, Some(_)) => {
            return Err(serde::de::Error::custom(
                "expected Claude tool input object",
            ));
        }
        (ParseMode::Full, None) => ParsedToolInput::Full(None),
    };
    Ok(ParsedToolUse {
        id: id.clone(),
        name: name.clone(),
        input,
    })
}

fn parse_content_tool_uses<'a>(
    content: &ContentItems<'a>,
    mode: ParseMode,
) -> std::result::Result<SmallToolUses<'a>, serde_json::Error> {
    let mut out = SmallToolUses::Empty;
    match content {
        ContentItems::Empty => {}
        ContentItems::One(item) => {
            if let BorrowedContentItem::ToolUse { id, name, input } = item {
                out.push(parse_single_tool_use(id, name, *input, mode)?);
            }
        }
        ContentItems::Many(items) => {
            for item in items {
                if let BorrowedContentItem::ToolUse { id, name, input } = item {
                    out.push(parse_single_tool_use(id, name, *input, mode)?);
                }
            }
        }
    }
    Ok(out)
}

fn count_raw_tool_result_metrics(raw: Option<&RawValue>) -> TextMetrics {
    let Some(raw) = raw else {
        return TextMetrics::default();
    };
    if let Some(metrics) = count_json_string_metrics(raw.get()) {
        return metrics;
    }
    let text = decode_raw_tool_result_content(Some(raw));
    TextMetrics::from_str(&text)
}

fn decode_raw_tool_result_content(raw: Option<&RawValue>) -> Cow<'_, str> {
    let Some(raw) = raw else {
        return Cow::Borrowed("");
    };
    if let Ok(text) = serde_json::from_str::<Cow<'_, str>>(raw.get()) {
        return text;
    }
    match serde_json::from_str::<Value>(raw.get()) {
        Ok(Value::String(s)) => Cow::Owned(s),
        Ok(Value::Array(arr)) => {
            let texts: Vec<&str> = arr
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect();
            Cow::Owned(texts.join("\n"))
        }
        Ok(Value::Null) | Err(_) => Cow::Borrowed(""),
        Ok(other) => Cow::Owned(other.to_string()),
    }
}

fn first_borrowed_tool_result<'a>(
    message: Option<&'a BorrowedClaudeMessage<'a>>,
) -> Option<(&'a str, bool)> {
    message?.content.first_tool_result()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingToolKind {
    Read,
    Write,
    Edit,
    Other,
}

impl PendingToolKind {
    fn from_name(name: &str) -> Self {
        match name {
            "Read" => Self::Read,
            "Write" => Self::Write,
            "Edit" => Self::Edit,
            _ => Self::Other,
        }
    }

    fn is_tracked_file_tool(self) -> bool {
        matches!(self, Self::Read | Self::Write | Self::Edit)
    }
}

struct UsageOnlyPendingInput {
    file_path: Option<String>,
    content: Option<TextMetrics>,
    new_metrics: Option<TextMetrics>,
}

enum PendingToolInput {
    Full(ClaudeToolInput),
    UsageOnly(UsageOnlyPendingInput),
}

struct PendingClaudeTool {
    kind: PendingToolKind,
    input: Option<PendingToolInput>,
    input_supported: bool,
}

impl PendingClaudeTool {
    fn file_path(&self) -> Option<&str> {
        match self.input.as_ref()? {
            PendingToolInput::Full(input) => input.file_path.as_deref(),
            PendingToolInput::UsageOnly(input) => input.file_path.as_deref(),
        }
    }
}

fn first_tool_result(message: Option<&ClaudeMessage>) -> Option<(&str, bool)> {
    message?.content.iter().find_map(|item| {
        let ClaudeContentItem::ToolResult {
            tool_use_id,
            is_error,
            ..
        } = item
        else {
            return None;
        };
        Some((tool_use_id.as_str(), *is_error))
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
        "Bash" => input
            .command
            .as_deref()
            .is_some_and(|command| !command.trim().is_empty()),
        _ => false,
    }
}

fn usage_only_tool_input_supported(
    name: &str,
    input: Option<&UsageOnlyClaudeToolInput<'_>>,
) -> bool {
    let Some(input) = input else {
        return false;
    };
    // Keeps `description` deserialized for schema validation parity with `ClaudeToolInput`.
    let _ = &input.description;
    let has_path = input
        .file_path
        .as_deref()
        .is_some_and(|path| !path.trim().is_empty());

    match name {
        "Read" => has_path,
        "Write" => has_path && input.content.is_some(),
        "Edit" => has_path && input.old_string.is_some() && input.new_string.is_some(),
        "Bash" => input
            .command
            .as_deref()
            .is_some_and(|command| !command.trim().is_empty()),
        _ => false,
    }
}

#[cfg(test)]
fn is_supported_claude_usage(usage: &Value) -> bool {
    matches!(
        ParsedClaudeUsage::from_value(usage),
        ParsedClaudeUsage::Valid(_)
    )
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

struct TopLevelResultShape<'a> {
    result_type: Option<&'a str>,
    has_file: bool,
    file_has_non_empty_path: bool,
    file_has_content: bool,
    has_path: bool,
    has_non_empty_path: bool,
    has_content: bool,
    has_old: bool,
    has_new: bool,
}

impl<'a> TopLevelResultShape<'a> {
    fn from_full(result: &'a ClaudeToolUseResult) -> Self {
        Self {
            result_type: result.result_type.as_deref(),
            has_file: result.file.is_some(),
            file_has_non_empty_path: result
                .file
                .as_ref()
                .and_then(|file| file.file_path.as_deref())
                .is_some_and(|path| !path.trim().is_empty()),
            file_has_content: result
                .file
                .as_ref()
                .is_some_and(|file| file.content.is_some()),
            has_path: result.file_path.is_some(),
            has_non_empty_path: result
                .file_path
                .as_deref()
                .is_some_and(|path| !path.trim().is_empty()),
            has_content: result.content.is_some(),
            has_old: result.old_string.is_some(),
            has_new: result.new_string.is_some(),
        }
    }

    fn from_usage_only(result: &'a UsageOnlyToolUseResult<'_>) -> Self {
        Self {
            result_type: result.result_type.as_deref(),
            has_file: result.file.is_some(),
            file_has_non_empty_path: result
                .file
                .as_ref()
                .and_then(|file| file.file_path.as_deref())
                .is_some_and(|path| !path.trim().is_empty()),
            file_has_content: result
                .file
                .as_ref()
                .is_some_and(|file| file.content.is_some()),
            has_path: result.file_path.is_some(),
            has_non_empty_path: result
                .file_path
                .as_deref()
                .is_some_and(|path| !path.trim().is_empty()),
            has_content: result.content.is_some(),
            has_old: result.old_string.is_some(),
            has_new: result.new_string.is_some(),
        }
    }

    fn validate_for(&self, expected_tool: Option<PendingToolKind>) -> TopLevelToolResult {
        if let Some(expected_tool) = expected_tool {
            return match expected_tool {
                PendingToolKind::Read => self.validate_read(),
                PendingToolKind::Write => self.validate_write(),
                PendingToolKind::Edit => self.validate_edit(),
                PendingToolKind::Other => TopLevelToolResult::Irrelevant,
            };
        }

        if self.result_type == Some("image") {
            return if self.has_file {
                TopLevelToolResult::NonTextRead
            } else {
                TopLevelToolResult::Unsupported
            };
        }
        if self.result_type == Some("text") || self.has_file {
            return self.validate_read();
        }
        if matches!(self.result_type, Some("create" | "update"))
            || (self.has_path && self.has_content)
        {
            return self.validate_write();
        }
        if self.has_path || self.has_old || self.has_new {
            // ExitPlanMode carries a plan file path but no file-operation body.
            if self.result_type.is_none() && !self.has_content && !self.has_old && !self.has_new {
                return TopLevelToolResult::Irrelevant;
            }
            return self.validate_edit();
        }

        TopLevelToolResult::Irrelevant
    }

    fn validate_read(&self) -> TopLevelToolResult {
        if self.result_type == Some("image") {
            return if self.has_file {
                TopLevelToolResult::NonTextRead
            } else {
                TopLevelToolResult::Unsupported
            };
        }
        if self.result_type == Some("text") || self.has_file {
            return if self.result_type == Some("text")
                && self.file_has_non_empty_path
                && self.file_has_content
            {
                TopLevelToolResult::Supported(FileToolResultKind::Read)
            } else {
                TopLevelToolResult::Unsupported
            };
        }
        TopLevelToolResult::Unsupported
    }

    fn validate_write(&self) -> TopLevelToolResult {
        if matches!(self.result_type, Some("create" | "update"))
            || (self.has_path && self.has_content)
        {
            return if matches!(self.result_type, Some("create" | "update"))
                && self.has_non_empty_path
                && self.has_content
            {
                TopLevelToolResult::Supported(FileToolResultKind::Write)
            } else {
                TopLevelToolResult::Unsupported
            };
        }
        TopLevelToolResult::Unsupported
    }

    fn validate_edit(&self) -> TopLevelToolResult {
        if self.has_path || self.has_old || self.has_new {
            return if self.has_non_empty_path && self.has_old && self.has_new {
                TopLevelToolResult::Supported(FileToolResultKind::Edit)
            } else {
                TopLevelToolResult::Unsupported
            };
        }
        TopLevelToolResult::Unsupported
    }
}

#[cfg(test)]
fn validate_top_level_tool_result(result: &ClaudeToolUseResult) -> TopLevelToolResult {
    validate_top_level_tool_result_for(result, None)
}

fn validate_top_level_tool_result_for(
    result: &ClaudeToolUseResult,
    expected_tool: Option<PendingToolKind>,
) -> TopLevelToolResult {
    TopLevelResultShape::from_full(result).validate_for(expected_tool)
}

fn validate_top_level_tool_result_usage_only(
    result: &UsageOnlyToolUseResult<'_>,
    expected_tool: Option<PendingToolKind>,
) -> TopLevelToolResult {
    TopLevelResultShape::from_usage_only(result).validate_for(expected_tool)
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
                // Keep empty `old_string` edits under edit metrics to match `tool_counts.edit`.
                state.add_edit_detail_raw(file_path, old_string, new_string, ts);
            });
        }
    }
}

fn dispatch_top_level_tool_result_usage_only(
    state: &mut SessionParseState,
    result: &UsageOnlyToolUseResult<'_>,
    kind: FileToolResultKind,
) {
    match kind {
        FileToolResultKind::Read => {
            let file = result.file.as_ref().expect("validated read result");
            let file_path = file.file_path.as_deref().expect("validated read path");
            let metrics = file.content.expect("validated read content").0;
            preserve_file_tool_counts(state, |state| {
                state.add_read_counts(file_path, metrics.lines, metrics.chars);
            });
        }
        FileToolResultKind::Write => {
            let file_path = result.file_path.as_deref().expect("validated write path");
            let metrics = result.content.expect("validated write content").0;
            preserve_file_tool_counts(state, |state| {
                state.add_write_counts(file_path, metrics.lines, metrics.chars);
            });
        }
        FileToolResultKind::Edit => {
            let file_path = result.file_path.as_deref().expect("validated edit path");
            let new_metrics = result.new_string.expect("validated new string").0;
            preserve_file_tool_counts(state, |state| {
                state.add_edit_counts_raw(file_path, new_metrics.lines, new_metrics.chars);
            });
        }
    }
}

/// Records a subagent `Read`, `Write`, or `Edit` result using `result_content` for `Read` and `input` for `Write`/`Edit`.
fn dispatch_subagent_tool_result(
    state: &mut SessionParseState,
    tool_kind: PendingToolKind,
    input: &ClaudeToolInput,
    result_content: &str,
    ts: i64,
) {
    let Some(file_path) = input.file_path.as_deref() else {
        return;
    };

    match tool_kind {
        PendingToolKind::Read => {
            preserve_file_tool_counts(state, |state| {
                state.add_read_detail(file_path, result_content, ts);
            });
        }
        PendingToolKind::Write => {
            let body = input.content.as_deref().unwrap_or("");
            preserve_file_tool_counts(state, |state| {
                state.add_write_detail(file_path, body, ts);
            });
        }
        PendingToolKind::Edit => {
            let new_string = input.new_string.as_deref().unwrap_or("");
            let old_string = input.old_string.as_deref().unwrap_or("");
            preserve_file_tool_counts(state, |state| {
                state.add_edit_detail_raw(file_path, old_string, new_string, ts);
            });
        }
        PendingToolKind::Other => {}
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
    fn per_request_tier_classification_splits_above_slice() {
        let usage_log = |ts: &str, input: i64, cache_read: i64, output: i64| -> ClaudeCodeLog {
            let raw = serde_json::json!({
                "type": "assistant",
                "timestamp": ts,
                "message": {
                    "model": "claude-sonnet-5",
                    "usage": {
                        "input_tokens": input,
                        "cache_read_input_tokens": cache_read,
                        "output_tokens": output
                    },
                    "content": []
                }
            });
            serde_json::from_value(raw).unwrap()
        };
        let logs = vec![
            usage_log("2026-07-01T00:00:00Z", 1_000, 50_000, 200),
            usage_log("2026-07-01T00:01:00Z", 2_000, 220_000, 300),
        ];

        let tiers = crate::pricing::TierThresholds::from_entries(
            [("claude-sonnet-5", vec![200_000])].into_iter(),
        );
        let parsed =
            parse_claude_logs_with_diagnostics(logs.clone(), ParseMode::UsageOnly, Some(&tiers))
                .unwrap();
        let usage = &parsed.analysis.records[0].conversation_usage["claude-sonnet-5"];
        assert_eq!(usage["input_tokens"], 3_000);
        assert_eq!(usage["output_tokens"], 500);
        assert_eq!(usage["above_tier"]["level_1_input_tokens"], 2_000);
        assert_eq!(usage["above_tier"]["level_1_cache_read_tokens"], 220_000);
        assert_eq!(usage["above_tier"]["level_1_output_tokens"], 300);

        let parsed = parse_claude_logs_with_diagnostics(logs, ParseMode::UsageOnly, None).unwrap();
        let usage = &parsed.analysis.records[0].conversation_usage["claude-sonnet-5"];
        assert!(usage.get("above_tier").is_none());
    }

    #[test]
    fn subagent_user_message_tool_result_is_dispatched_via_fallback() {
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
    fn main_session_tool_result_block_is_not_read_as_file_content() {
        // A string `toolUseResult` (rejection) deserializes to `None`; `!is_sidechain` keeps the `tool_result` text out of the read tally.
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
            "main-session tool_result block must not be dispatched as a file read"
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
                { "type": "tool_use", "id": "task-2", "name": "TaskUpdate", "input": {} },
                { "type": "tool_use", "id": "task-3", "name": "TaskStop", "input": {} }
            ]),
        );

        let analysis = parse_claude_logs(vec![log], ParseMode::Full).unwrap();
        assert_eq!(analysis.records[0].tool_call_counts.todo_write, 3);
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
                parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full, None)
                    .unwrap();
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
            parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full, None).unwrap();
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
            parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full, None).unwrap();
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
    fn explicitly_errored_invalid_reads_are_metric_free_and_supported() {
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
                parse_claude_logs_with_diagnostics([assistant, user], ParseMode::Full, None)
                    .unwrap();
            assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
            assert!(!parsed.diagnostics.is_complete_failure());
            assert_eq!(parsed.analysis.records[0].tool_call_counts.read, 0);
        }
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

        let parsed =
            parse_claude_logs_with_diagnostics([assistant], ParseMode::Full, None).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert_eq!(parsed.analysis.records[0].tool_call_counts.read, 0);
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

        let parsed = parse_claude_logs_with_diagnostics([log], ParseMode::Full, None).unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }

    #[test]
    fn advisor_message_usage_is_separated_from_conversation_usage() {
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

        let conv = &record.conversation_usage;
        assert_eq!(conv.len(), 1);
        let main = conv.get("claude-haiku-4-5").unwrap();
        assert_eq!(main["input_tokens"].as_i64().unwrap(), 4);
        assert_eq!(main["output_tokens"].as_i64().unwrap(), 7440);
        assert!(conv.get("claude-opus-4-8").is_none());

        let advisor = record.advisor_usage.get("claude-opus-4-8").unwrap();
        assert_eq!(advisor["input_tokens"].as_i64().unwrap(), 47579);
        assert_eq!(advisor["output_tokens"].as_i64().unwrap(), 10521);
    }

    #[test]
    fn one_response_split_across_content_blocks_bills_once() {
        let response = |content: serde_json::Value, output: i64| {
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2025-01-01T00:00:00Z",
                "requestId": "req_1",
                "message": {
                    "id": "msg_1",
                    "model": "claude-opus-4-8",
                    "content": content,
                    "usage": {
                        "input_tokens": 11,
                        "output_tokens": output,
                        "cache_read_input_tokens": 16788,
                        "cache_creation_input_tokens": 1328
                    }
                }
            })
        };
        let logs: Vec<ClaudeCodeLog> = vec![
            response(serde_json::json!([{ "type": "thinking" }]), 8),
            response(serde_json::json!([{ "type": "text" }]), 8),
            response(
                serde_json::json!([
                    { "type": "tool_use", "id": "tool_1", "name": "Read",
                      "input": { "file_path": "/tmp/a.rs" } }
                ]),
                167,
            ),
        ]
        .into_iter()
        .map(|raw| serde_json::from_value(raw).unwrap())
        .collect();

        let analysis = parse_claude_logs(logs, ParseMode::Full).unwrap();
        let usage = &analysis.records[0].conversation_usage["claude-opus-4-8"];
        assert_eq!(usage["input_tokens"].as_i64().unwrap(), 11);
        assert_eq!(usage["cache_read_input_tokens"].as_i64().unwrap(), 16788);
        assert_eq!(usage["cache_creation_input_tokens"].as_i64().unwrap(), 1328);
        assert_eq!(usage["output_tokens"].as_i64().unwrap(), 167);

        assert_eq!(analysis.records[0].tool_call_counts.read, 1);
    }

    #[test]
    fn advisor_iterations_of_one_response_bill_once() {
        let response = |output: i64| {
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2025-01-01T00:00:00Z",
                "requestId": "req_1",
                "message": {
                    "id": "msg_1",
                    "model": "claude-haiku-4-5",
                    "content": [],
                    "usage": {
                        "input_tokens": 4,
                        "output_tokens": output,
                        "iterations": [
                            { "type": "advisor_message", "model": "claude-opus-4-8",
                              "input_tokens": 47579, "output_tokens": 10521 }
                        ]
                    }
                }
            })
        };
        let logs: Vec<ClaudeCodeLog> = vec![response(12), response(7440)]
            .into_iter()
            .map(|raw| serde_json::from_value(raw).unwrap())
            .collect();

        let analysis = parse_claude_logs(logs, ParseMode::Full).unwrap();
        let advisor = analysis.records[0]
            .advisor_usage
            .get("claude-opus-4-8")
            .unwrap();
        assert_eq!(advisor["input_tokens"].as_i64().unwrap(), 47579);
        assert_eq!(advisor["output_tokens"].as_i64().unwrap(), 10521);
    }

    #[test]
    fn records_naming_no_response_each_bill() {
        // Guards the fallback when `message.id` is absent: records without a key must bill separately.
        let response = serde_json::json!({
            "type": "assistant",
            "timestamp": "2025-01-01T00:00:00Z",
            "message": {
                "model": "claude-opus-4-8",
                "content": [],
                "usage": { "input_tokens": 5, "output_tokens": 100 }
            }
        });
        let logs: Vec<ClaudeCodeLog> = vec![response.clone(), response]
            .into_iter()
            .map(|raw| serde_json::from_value(raw).unwrap())
            .collect();

        let analysis = parse_claude_logs(logs, ParseMode::Full).unwrap();
        let usage = &analysis.records[0].conversation_usage["claude-opus-4-8"];
        assert_eq!(usage["input_tokens"].as_i64().unwrap(), 10);
        assert_eq!(usage["output_tokens"].as_i64().unwrap(), 200);
    }

    #[test]
    fn responses_with_distinct_ids_each_bill() {
        let response = |id: &str, request: &str| {
            serde_json::json!({
                "type": "assistant",
                "timestamp": "2025-01-01T00:00:00Z",
                "requestId": request,
                "message": {
                    "id": id,
                    "model": "claude-opus-4-8",
                    "content": [],
                    "usage": { "input_tokens": 5, "output_tokens": 100 }
                }
            })
        };
        let logs: Vec<ClaudeCodeLog> = vec![
            response("msg_1", "req_1"),
            response("msg_2", "req_2"),
            // A retry reusing the response id still bills separately.
            response("msg_2", "req_3"),
        ]
        .into_iter()
        .map(|raw| serde_json::from_value(raw).unwrap())
        .collect();

        let analysis = parse_claude_logs(logs, ParseMode::Full).unwrap();
        let usage = &analysis.records[0].conversation_usage["claude-opus-4-8"];
        assert_eq!(usage["input_tokens"].as_i64().unwrap(), 15);
        assert_eq!(usage["output_tokens"].as_i64().unwrap(), 300);
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
        let parsed = parse_claude_logs_with_diagnostics([log], ParseMode::Full, None).unwrap();

        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert!(parsed.analysis.records[0].advisor_usage.is_empty());
        assert_eq!(parsed.analysis.records[0].conversation_usage.len(), 1);
    }

    #[test]
    fn message_only_iterations_leave_advisor_usage_empty() {
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

    #[test]
    fn borrowed_jsonl_stream_matches_owned_parser_across_edge_cases() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let lines = [
            serde_json::json!({
                "type": "attachment",
                "timestamp": "2026-07-12T00:00:00Z",
                "message": { "ignored": "large attachment payload" }
            }),
            serde_json::json!({
                "type": "assistant",
                "sessionId": "s-1",
                "cwd": "/tmp",
                "requestId": "req-1",
                "timestamp": "2026-07-12T00:00:01Z",
                "message": {
                    "id": "msg-1",
                    "model": "claude-opus-4-8",
                    "usage": { "input_tokens": 10, "output_tokens": 5 },
                    "content": [
                        { "type": "thinking", "thinking": "deep trace" },
                        { "type": "tool_use", "id": "b1", "name": "Bash", "input": { "command": "ls" } },
                        { "type": "tool_use", "id": "r1", "name": "Read", "input": { "file_path": "/tmp/a.rs" } }
                    ]
                }
            }),
            serde_json::json!({
                "type": "user",
                "sessionId": "s-1",
                "timestamp": "2026-07-12T00:00:02Z",
                "toolUseResult": { "stdout": "large bash output" },
                "message": {
                    "content": [{ "type": "tool_result", "tool_use_id": "b1", "content": "large bash output" }]
                }
            }),
            serde_json::json!({
                "type": "user",
                "sessionId": "s-1",
                "isSidechain": true,
                "timestamp": "2026-07-12T00:00:03Z",
                "message": {
                    "content": [{ "type": "tool_result", "tool_use_id": "r1", "content": "1\tfn a() {}\n2\tfn b() {}" }]
                }
            }),
        ];
        let body: String = lines.iter().map(|v| format!("{v}\n")).collect();
        std::fs::write(&path, body).unwrap();

        let streamed = parse_claude_jsonl_path(&path, ParseMode::Full, None)
            .unwrap()
            .unwrap();
        let owned_logs: Vec<ClaudeCodeLog> = lines
            .into_iter()
            .map(|v| serde_json::from_value(v).unwrap())
            .collect();
        let owned = parse_claude_logs_with_diagnostics(owned_logs, ParseMode::Full, None).unwrap();

        assert_eq!(streamed.diagnostics, owned.diagnostics);
        assert_eq!(
            serde_json::to_value(&streamed.analysis.records[0].tool_call_counts).unwrap(),
            serde_json::to_value(&owned.analysis.records[0].tool_call_counts).unwrap()
        );
        assert_eq!(streamed.analysis.records[0].total_read_lines, 2);
        assert_eq!(
            streamed.analysis.records[0].conversation_usage,
            owned.analysis.records[0].conversation_usage
        );

        // Malformed `message` on an assistant line still reports a relevant schema failure.
        let bad_path = dir.path().join("bad.jsonl");
        std::fs::write(
            &bad_path,
            "{\"type\":\"assistant\",\"timestamp\":\"2026-07-12T00:00:00Z\",\"message\":42}\n",
        )
        .unwrap();
        let bad = parse_claude_jsonl_path(&bad_path, ParseMode::UsageOnly, None)
            .unwrap()
            .unwrap();
        assert!(bad.diagnostics.is_complete_failure());
    }

    #[test]
    fn usage_only_stream_matches_full_on_cjk_emoji_dedup_and_sidechain() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let lines = [
            serde_json::json!({
                "type": "assistant",
                "sessionId": "s-2",
                "cwd": "/workspace",
                "requestId": "req-1",
                "timestamp": "2026-07-12T00:00:01Z",
                "message": {
                    "id": "msg-1",
                    "model": "claude-opus-4-8",
                    "usage": {
                        "input_tokens": 100,
                        "output_tokens": 5,
                        "cache_creation_input_tokens": 200,
                        "cache_read_input_tokens": 300,
                        "cache_creation": {
                            "ephemeral_5m_input_tokens": 200,
                            "ephemeral_1h_input_tokens": 0
                        },
                        "service_tier": "standard",
                        "server_tool_use": {
                            "web_search_requests": 1,
                            "web_fetch_requests": 0
                        }
                    },
                    "content": [
                        { "type": "thinking", "thinking": "ignored" }
                    ]
                }
            }),
            // Duplicate block for the same (msg-1, req-1) with final output_tokens + tool uses.
            serde_json::json!({
                "type": "assistant",
                "sessionId": "s-2",
                "cwd": "/workspace",
                "requestId": "req-1",
                "timestamp": "2026-07-12T00:00:02Z",
                "message": {
                    "id": "msg-1",
                    "model": "claude-opus-4-8",
                    "usage": {
                        "input_tokens": 100,
                        "output_tokens": 99,
                        "cache_creation_input_tokens": 200,
                        "cache_read_input_tokens": 300,
                        "cache_creation": {
                            "ephemeral_5m_input_tokens": 200,
                            "ephemeral_1h_input_tokens": 0
                        },
                        "service_tier": "standard",
                        "server_tool_use": {
                            "web_search_requests": 1,
                            "web_fetch_requests": 0
                        },
                        "iterations": [
                            { "type": "message", "input_tokens": 100, "output_tokens": 99 },
                            { "type": "advisor_message", "model": "claude-sonnet-4-6", "input_tokens": 50, "output_tokens": 25 }
                        ]
                    },
                    "content": [
                        { "type": "tool_use", "id": "w1", "name": "Write", "input": { "file_path": "/workspace/zh.md", "content": "繁體中文\n第二行🚀\n\n" } },
                        { "type": "tool_use", "id": "e1", "name": "Edit", "input": { "file_path": "/workspace/zh.md", "old_string": "", "new_string": "新增內容\r\n第二行\r\n" } },
                        { "type": "tool_use", "id": "r1", "name": "Read", "input": { "file_path": "/workspace/read.txt" } },
                        { "type": "tool_use", "id": "sr1", "name": "Read", "input": { "file_path": "/workspace/sub.txt" } },
                        { "type": "tool_use", "id": "sw1", "name": "Write", "input": { "file_path": "/workspace/sub_w.txt", "content": "子代理寫入\n🎉" } },
                        { "type": "tool_use", "id": "se1", "name": "Edit", "input": { "file_path": "/workspace/sub_e.txt", "old_string": "舊", "new_string": "新\n多行" } }
                    ]
                }
            }),
            // Retry reusing msg-1 with a new requestId must bill separately.
            serde_json::json!({
                "type": "assistant",
                "sessionId": "s-2",
                "cwd": "/workspace",
                "requestId": "req-2",
                "timestamp": "2026-07-12T00:00:03Z",
                "message": {
                    "id": "msg-1",
                    "model": "claude-opus-4-8",
                    "usage": { "input_tokens": 10, "output_tokens": 20 },
                    "content": []
                }
            }),
            serde_json::json!({
                "type": "user",
                "sessionId": "s-2",
                "timestamp": "2026-07-12T00:00:04Z",
                "toolUseResult": { "type": "create", "filePath": "/workspace/zh.md", "content": "繁體中文\n第二行🚀\n\n" },
                "message": { "content": [{ "type": "tool_result", "tool_use_id": "w1", "content": "ok" }] }
            }),
            serde_json::json!({
                "type": "user",
                "sessionId": "s-2",
                "timestamp": "2026-07-12T00:00:05Z",
                "toolUseResult": { "filePath": "/workspace/zh.md", "oldString": "", "newString": "新增內容\r\n第二行\r\n" },
                "message": { "content": [{ "type": "tool_result", "tool_use_id": "e1", "content": "ok" }] }
            }),
            serde_json::json!({
                "type": "user",
                "sessionId": "s-2",
                "timestamp": "2026-07-12T00:00:06Z",
                "toolUseResult": { "type": "text", "file": { "filePath": "/workspace/read.txt", "content": "行1\n行2😀\n" } },
                "message": { "content": [{ "type": "tool_result", "tool_use_id": "r1", "content": "ok" }] }
            }),
            serde_json::json!({
                "type": "user",
                "sessionId": "s-2",
                "isSidechain": true,
                "timestamp": "2026-07-12T00:00:07Z",
                "message": {
                    "content": [
                        { "type": "tool_result", "tool_use_id": "sr1", "content": "側鏈\n讀取🚀\n" },
                        { "type": "tool_result", "tool_use_id": "sw1", "content": "ok" },
                        { "type": "tool_result", "tool_use_id": "se1", "content": "ok" }
                    ]
                }
            }),
        ];
        let body: String = lines.iter().map(|v| format!("{v}\n")).collect();
        std::fs::write(&path, body).unwrap();

        let tiers = crate::pricing::TierThresholds::from_entries(
            [("claude-opus-4-8", vec![500])].into_iter(),
        );
        let full = parse_claude_jsonl_path(&path, ParseMode::Full, Some(&tiers))
            .unwrap()
            .unwrap();
        let usage_only = parse_claude_jsonl_path(&path, ParseMode::UsageOnly, Some(&tiers))
            .unwrap()
            .unwrap();

        assert_eq!(usage_only.diagnostics, full.diagnostics);
        let f = &full.analysis.records[0];
        let u = &usage_only.analysis.records[0];
        assert_eq!(u.total_unique_files, f.total_unique_files);
        assert_eq!(u.total_write_lines, f.total_write_lines);
        assert_eq!(u.total_read_lines, f.total_read_lines);
        assert_eq!(u.total_edit_lines, f.total_edit_lines);
        assert_eq!(u.total_write_characters, f.total_write_characters);
        assert_eq!(u.total_read_characters, f.total_read_characters);
        assert_eq!(u.total_edit_characters, f.total_edit_characters);
        assert_eq!(
            serde_json::to_value(&u.tool_call_counts).unwrap(),
            serde_json::to_value(&f.tool_call_counts).unwrap()
        );
        assert_eq!(u.conversation_usage, f.conversation_usage);
        assert_eq!(u.advisor_usage, f.advisor_usage);
        assert!(u.write_file_details.is_empty());
        assert!(u.read_file_details.is_empty());
        assert!(u.edit_file_details.is_empty());
    }

    #[test]
    fn edit_with_empty_old_string_stays_under_edit_metrics() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("empty_old_edit.jsonl");
        let lines = [
            serde_json::json!({
                "type": "assistant",
                "sessionId": "s-edit",
                "cwd": "/workspace",
                "timestamp": "2026-07-12T00:00:01Z",
                "message": {
                    "id": "msg-1",
                    "model": "claude-opus-4-8",
                    "usage": { "input_tokens": 10, "output_tokens": 5 },
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "e1",
                            "name": "Edit",
                            "input": { "file_path": "/workspace/a.rs", "old_string": "", "new_string": "line1\nline2\n" }
                        },
                        {
                            "type": "tool_use",
                            "id": "e2",
                            "name": "Edit",
                            "input": { "file_path": "/workspace/a.rs", "old_string": "\n", "new_string": "line3" }
                        },
                        {
                            "type": "tool_use",
                            "id": "se1",
                            "name": "Edit",
                            "input": { "file_path": "/workspace/b.rs", "old_string": "", "new_string": "sub1\nsub2\n" }
                        }
                    ]
                }
            }),
            serde_json::json!({
                "type": "user",
                "sessionId": "s-edit",
                "timestamp": "2026-07-12T00:00:02Z",
                "toolUseResult": { "filePath": "/workspace/a.rs", "oldString": "", "newString": "line1\nline2\n" },
                "message": { "content": [{ "type": "tool_result", "tool_use_id": "e1", "content": "ok" }] }
            }),
            serde_json::json!({
                "type": "user",
                "sessionId": "s-edit",
                "timestamp": "2026-07-12T00:00:03Z",
                "toolUseResult": { "filePath": "/workspace/a.rs", "oldString": "\n", "newString": "line3" },
                "message": { "content": [{ "type": "tool_result", "tool_use_id": "e2", "content": "ok" }] }
            }),
            serde_json::json!({
                "type": "user",
                "sessionId": "s-edit",
                "isSidechain": true,
                "timestamp": "2026-07-12T00:00:04Z",
                "message": { "content": [{ "type": "tool_result", "tool_use_id": "se1", "content": "ok" }] }
            }),
        ];
        let body: String = lines.iter().map(|v| format!("{v}\n")).collect();
        std::fs::write(&path, body).unwrap();

        let owned_logs: Vec<ClaudeCodeLog> = lines
            .into_iter()
            .map(|v| serde_json::from_value(v).unwrap())
            .collect();
        let expected_chars =
            "line1\nline2".chars().count() + "line3".chars().count() + "sub1\nsub2".chars().count();

        for mode in [ParseMode::Full, ParseMode::UsageOnly] {
            let streamed = parse_claude_jsonl_path(&path, mode, None).unwrap().unwrap();
            let owned = parse_claude_logs_with_diagnostics(owned_logs.clone(), mode, None).unwrap();

            for parsed in [&streamed, &owned] {
                assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
                let rec = &parsed.analysis.records[0];
                assert_eq!(rec.tool_call_counts.edit, 3);
                assert_eq!(rec.tool_call_counts.write, 0);
                assert_eq!(rec.total_edit_lines, 5);
                assert_eq!(rec.total_edit_characters, expected_chars);
                assert_eq!(rec.total_write_lines, 0);
                assert_eq!(rec.total_write_characters, 0);
                assert_eq!(rec.total_unique_files, 2);
                assert!(rec.write_file_details.is_empty());
                if matches!(mode, ParseMode::Full) {
                    assert_eq!(rec.edit_file_details.len(), 3);
                } else {
                    assert!(rec.edit_file_details.is_empty());
                }
            }
        }
    }

}
