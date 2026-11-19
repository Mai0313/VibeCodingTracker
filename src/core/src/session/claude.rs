//! Parser for Claude Code session logs (`~/.claude/projects/**/*.jsonl`).
//!
//! Assistant records repeat their full `usage` object once per content block; the parser deduplicates by API response identity and keeps the last streamed snapshot. File-operation results arrive either in a top-level `toolUseResult` field (main sessions) or in a sidechain user record's `message.content[].tool_result` block (subagents), correlated with pending `tool_use` calls by `tool_use_id`.
use crate::constants::{FastHashMap, capacity};
use crate::models::*;
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::parser::stream_jsonl_borrowed;
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{
    claude_request_context, get_git_remote_url, parse_iso_timestamp, process_claude_usage,
};
use anyhow::Result;
use serde::{
    Deserialize, Deserializer,
    de::{Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, value::RawValue};
use std::borrow::Cow;
use std::fmt;
use std::path::Path;

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
                    let usage = message.usage.take();
                    fold.observe_assistant_usage(
                        message.id.as_deref(),
                        log.request_id.as_deref(),
                        message.model.as_deref(),
                        usage,
                    );
                    for item in std::mem::take(&mut message.content) {
                        if let ClaudeContentItem::ToolUse { id, name, input } = item {
                            fold.observe_tool_use(id, name, input, ts);
                        }
                    }
                }
            }
        }

        if let Some(tur) = &log.tool_use_result {
            fold.observe_top_level_tool_result(tur, first_tool_result(log.message.as_ref()), ts);
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
                    fold.observe_user_tool_result(
                        log.is_sidechain,
                        tool_use_id,
                        *is_error,
                        ts,
                        || Cow::Borrowed(content.as_str()),
                    );
                }
            }
        }
    }

    Ok(fold.finish())
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
            let record = serde_json::from_slice::<BorrowedClaudeRecord<'_>>(bytes)?;
            let tur_looks_like_object = record
                .tool_use_result
                .is_some_and(|raw| raw.get().starts_with('{'));

            let needs_message = record.log_type == "assistant"
                || record.log_type == "user"
                || tur_looks_like_object;
            let mut message = if needs_message {
                match record.message {
                    Some(raw) => {
                        serde_json::from_str::<Option<BorrowedClaudeMessage<'_>>>(raw.get())?
                    }
                    None => None,
                }
            } else {
                None
            };

            // Skip deserializing `toolUseResult` for non-file tools since neither result branch inspects it.
            if record.log_type == "user"
                && tur_looks_like_object
                && let Some(msg) = message.as_ref()
                && msg.content.len() == 1
                && let BorrowedContentItem::ToolResult { tool_use_id, .. } = &msg.content[0]
                && fold
                    .pending_tool_uses
                    .get(tool_use_id.as_ref())
                    .is_some_and(|pending| !is_tracked_file_tool(&pending.name))
            {
                fold.observe_envelope(
                    &record.log_type,
                    &record.cwd,
                    &record.session_id,
                    &record.timestamp,
                    true,
                );
                fold.pending_tool_uses.remove(tool_use_id.as_ref());
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
                        for item in std::mem::take(&mut msg.content) {
                            if let BorrowedContentItem::ToolUse { id, name, input } = item {
                                fold.observe_tool_use(id, name, input, ts);
                            }
                        }
                    }
                }
            }

            if let Some(tur) = &tur {
                let first = first_borrowed_tool_result(message.as_ref());
                fold.observe_top_level_tool_result(tur, first, ts);
            } else if record.log_type == "user"
                && let Some(msg) = &message
            {
                for item in &msg.content {
                    if let BorrowedContentItem::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } = item
                    {
                        fold.observe_user_tool_result(
                            record.is_sidechain,
                            tool_use_id,
                            *is_error,
                            ts,
                            || decode_raw_tool_result_content(*content),
                        );
                    }
                }
            }

            Ok(())
        },
        |fold| Ok(fold.finish()),
    )
}

struct ClaudeFold<'a> {
    classifier: Option<TierClassifier<'a>>,
    state: SessionParseState,
    conversation_usage: FastHashMap<String, Value>,
    // Kept separate from `conversation_usage` so file-op counts are not attributed to advisor models; usage aggregation merges both.
    advisor_usage: FastHashMap<String, Value>,
    // Keyed by `tool_use_id` so top-level results dispatch by originating tool rather than ambiguous fields like `filePath` (also set by `ExitPlanMode`).
    pending_tool_uses: FastHashMap<String, PendingClaudeTool>,
    diagnostics: ParseDiagnostics,
    // Claude Code repeats the full `usage` object on each content block of one API response, with earlier blocks carrying partial `output_tokens`. Banking `(model, usage)` by request key in `billed_index` keeps only the final snapshot per response.
    billed_requests: Vec<(String, Value)>,
    billed_index: FastHashMap<String, usize>,
}

impl<'a> ClaudeFold<'a> {
    fn new(mode: ParseMode, tiers: Option<&'a TierThresholds>) -> Self {
        Self {
            classifier: tiers.map(TierClassifier::new),
            state: SessionParseState::with_mode(mode),
            conversation_usage: FastHashMap::with_capacity(capacity::MODELS_PER_SESSION),
            advisor_usage: FastHashMap::default(),
            pending_tool_uses: FastHashMap::with_capacity(64),
            diagnostics: ParseDiagnostics::default(),
            billed_requests: Vec::with_capacity(64),
            billed_index: FastHashMap::with_capacity(64),
        }
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
        usage: Option<Value>,
    ) {
        let Some(usage) = usage else {
            return;
        };
        let request_key = claude_request_key_parts(message_id, request_id);
        let model = model.filter(|model| !model.is_empty());
        if !(is_supported_claude_usage(&usage) && model.is_some()) {
            self.diagnostics.record_relevant(false);
        } else if let Some(model) = model {
            let entry = (model.to_string(), usage);
            match request_key {
                Some(key) => match self.billed_index.get(&key) {
                    Some(&slot) => self.billed_requests[slot] = entry,
                    None => {
                        self.billed_index.insert(key, self.billed_requests.len());
                        self.billed_requests.push(entry);
                    }
                },
                // Without a response key, treat the record as a distinct request.
                None => self.billed_requests.push(entry),
            }
        }
    }

    fn observe_tool_use(
        &mut self,
        id: String,
        name: String,
        input: Option<ClaudeToolInput>,
        ts: i64,
    ) {
        let tracked_file_tool = is_tracked_file_tool(&name);
        let input_supported =
            tracked_file_tool && tracked_tool_input_supported(&name, input.as_ref());

        match name.as_str() {
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
                    .record_relevant(tracked_tool_input_supported(&name, input.as_ref()));
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
                    name,
                    input: tracked_file_tool.then_some(input).flatten(),
                    input_supported,
                },
            );
        }
    }

    fn observe_top_level_tool_result(
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
            if is_tracked_file_tool(&pending.name) && !pending.input_supported {
                // Explicit rejection of bad arguments is a valid lifecycle, not schema drift.
                self.diagnostics.record_relevant(true);
            }
        } else {
            let expected_tool = correlated
                .as_ref()
                .map(|(pending, _)| pending.name.as_str());
            match validate_top_level_tool_result_for(tur, expected_tool) {
                TopLevelToolResult::Irrelevant => {}
                TopLevelToolResult::Unsupported => self.diagnostics.record_relevant(false),
                TopLevelToolResult::NonTextRead => {
                    self.diagnostics.record_relevant(true);
                    if let Some(path) = correlated
                        .as_ref()
                        .and_then(|(pending, _)| pending.input.as_ref())
                        .and_then(|input| input.file_path.as_deref())
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

    // Subagent fallback for tool results inside `message.content[].tool_result`. Gated on `is_sidechain` because a main-session rejection has a string `toolUseResult` (deserializing to `None`) alongside a `tool_result` block whose text is not a file body.
    fn observe_user_tool_result<'c>(
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
            if is_tracked_file_tool(&pending.name) && !pending.input_supported {
                self.diagnostics.record_relevant(true);
            }
            return;
        }
        if !is_sidechain || !is_tracked_file_tool(&pending.name) {
            return;
        }

        let normalized = pending.input_supported;
        self.diagnostics.record_relevant(normalized);
        if normalized && let Some(input) = pending.input.as_ref() {
            let body = if pending.name == "Read" {
                content()
            } else {
                Cow::Borrowed("")
            };
            dispatch_subagent_tool_result(&mut self.state, &pending.name, input, &body, ts);
        }
    }

    fn finish(mut self) -> ParsedAnalysis {
        for (model, usage) in self.billed_requests {
            self.diagnostics.record_relevant(true);
            let level = self.classifier.as_mut().map_or(0, |classifier| {
                usage.as_object().map_or(0, |usage_obj| {
                    classifier.level(&model, claude_request_context(usage_obj))
                })
            });
            process_claude_usage(&mut self.conversation_usage, &model, &usage, level);

            // Top-level `usage` sums `message` iterations and excludes `advisor_message` iterations, so fold advisor iterations separately under their own model (falling back to the main model).
            if let Some(iters) = usage.get("iterations").and_then(|v| v.as_array()) {
                for iter in iters {
                    if iter.get("type").and_then(|t| t.as_str()) != Some("advisor_message") {
                        continue;
                    }
                    let adv_model = iter
                        .get("model")
                        .and_then(|m| m.as_str())
                        .unwrap_or(model.as_str());
                    let normalized = !adv_model.is_empty() && is_supported_claude_usage(iter);
                    self.diagnostics.record_relevant(normalized);
                    if normalized {
                        let level = self.classifier.as_mut().map_or(0, |classifier| {
                            iter.as_object().map_or(0, |usage_obj| {
                                classifier.level(adv_model, claude_request_context(usage_obj))
                            })
                        });
                        process_claude_usage(&mut self.advisor_usage, adv_model, iter, level);
                    }
                }
            }
        }

        for pending in self.pending_tool_uses.into_values() {
            if is_tracked_file_tool(&pending.name) && !pending.input_supported {
                self.diagnostics.record_relevant(false);
            }
        }

        if self.state.git_remote.is_empty() {
            self.state.git_remote = get_git_remote_url(&self.state.folder_path);
        }

        let mut record = self.state.into_record(self.conversation_usage);
        record.advisor_usage = self.advisor_usage;

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
    usage: Option<Value>,
    #[serde(
        default,
        borrow,
        deserialize_with = "deserialize_borrowed_content_items"
    )]
    content: Vec<BorrowedContentItem<'a>>,
}

enum BorrowedContentItem<'a> {
    ToolUse {
        id: String,
        name: String,
        input: Option<ClaudeToolInput>,
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
                            Some(raw) => serde_json::from_str::<Option<String>>(raw.get())
                                .map_err(A::Error::custom)?
                                .unwrap_or_default(),
                            None => String::new(),
                        };
                        let name = match name {
                            Some(raw) => serde_json::from_str::<Option<String>>(raw.get())
                                .map_err(A::Error::custom)?
                                .unwrap_or_default(),
                            None => String::new(),
                        };
                        let input = match input {
                            Some(raw)
                                if matches!(name.as_str(), "Read" | "Write" | "Edit" | "Bash") =>
                            {
                                serde_json::from_str::<Option<ClaudeToolInput>>(raw.get())
                                    .map_err(A::Error::custom)?
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
) -> std::result::Result<Vec<BorrowedContentItem<'de>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ItemsVisitor;

    impl<'de> Visitor<'de> for ItemsVisitor {
        type Value = Vec<BorrowedContentItem<'de>>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a content array or ignorable scalar")
        }

        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut items = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(16));
            while let Some(item) = seq.next_element::<BorrowedContentItem<'de>>()? {
                items.push(item);
            }
            Ok(items)
        }

        fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_str<E: serde::de::Error>(self, _: &str) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_string<E: serde::de::Error>(
            self,
            _: String,
        ) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_map<A: MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(Vec::new())
        }
    }

    deserializer.deserialize_any(ItemsVisitor)
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
    message?.content.iter().find_map(|item| {
        let BorrowedContentItem::ToolResult {
            tool_use_id,
            is_error,
            ..
        } = item
        else {
            return None;
        };
        Some((tool_use_id.as_ref(), *is_error))
    })
}

#[derive(Clone)]
struct PendingClaudeTool {
    name: String,
    input: Option<ClaudeToolInput>,
    input_supported: bool,
}

fn is_tracked_file_tool(name: &str) -> bool {
    matches!(name, "Read" | "Write" | "Edit")
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

/// Returns the deduplication key for an assistant response (`message_id`, with `:request_id` appended when present so retries bill separately), or `None` if `message_id` is absent.
fn claude_request_key_parts(message_id: Option<&str>, request_id: Option<&str>) -> Option<String> {
    let id = message_id.filter(|id| !id.is_empty())?;
    match request_id.filter(|id| !id.is_empty()) {
        Some(request_id) => Some(format!("{id}:{request_id}")),
        None => Some(id.to_string()),
    }
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

/// Records a subagent `Read`, `Write`, or `Edit` result using `result_content` for `Read` and `input` for `Write`/`Edit`.
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
            preserve_file_tool_counts(state, |state| {
                state.add_read_detail(file_path, result_content, ts);
            });
        }
        "Write" => {
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
}
