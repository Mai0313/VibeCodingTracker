//! Parser for Gemini CLI JSONL session logs (`~/.gemini/tmp/<project_hash>/chats/*.jsonl`).
//!
//! Line 1 is a session-meta record and subsequent lines are append-only events. Repeated message ids across incremental updates and `$set.messages` snapshots resolve to the latest revision while preserving already-billed messages across `$rewindTo`; only `type == "gemini"` messages contribute usage and tool metrics.
use crate::constants::FastHashMap;
use crate::models::*;
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::parser::stream_jsonl_borrowed;
use crate::session::state::{ParseMode, SessionParseState, TextMetrics, count_json_string_metrics};
use crate::utils::{parse_iso_timestamp, process_gemini_usage};
use anyhow::{Context, Result};
use serde::{
    Deserialize, Deserializer,
    de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, value::RawValue};
use std::borrow::Cow;
use std::fmt;
use std::path::Path;

/// Parses Gemini CLI JSONL `events` for `session` into a [`CodeAnalysis`].
///
/// `session` is the parsed first-line metadata record and `events` yields each subsequent JSONL record. Repeated message ids across incremental updates and `$set.messages` snapshots resolve to the latest revision while retaining messages hidden by `$rewindTo`; only `type == "gemini"` messages contribute metrics.
///
/// # Errors
///
/// Currently infallible; non-`gemini` events and unsupported message schemas are skipped.
pub fn parse_gemini_events<I>(
    session: GeminiSession,
    events: I,
    mode: ParseMode,
) -> Result<CodeAnalysis>
where
    I: IntoIterator<Item = Value>,
{
    Ok(parse_gemini_events_with_diagnostics(session, events, mode, None)?.analysis)
}

/// Parses Gemini CLI JSONL `events` into a [`ParsedAnalysis`] with schema diagnostics and optional context-tier classification.
pub(crate) fn parse_gemini_events_with_diagnostics<I>(
    session: GeminiSession,
    events: I,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<ParsedAnalysis>
where
    I: IntoIterator<Item = Value>,
{
    let mut fold = GeminiFold::new(mode, tiers);
    fold.session = Some(session);
    fold.diagnostics.record_recognized_source();
    for event in events {
        fold.observe_value_event(event);
    }
    fold.finish()
}

/// Streams and parses a Gemini CLI JSONL file at `path` without materializing per-line `Value` trees or cloning `$set.messages` snapshots.
pub(crate) fn parse_gemini_jsonl_path(
    path: &Path,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<Option<ParsedAnalysis>> {
    stream_jsonl_borrowed(
        path,
        ExtensionType::Gemini,
        GeminiFold::new(mode, tiers),
        |fold, bytes| {
            if !fold.seen_first_line {
                fold.seen_first_line = true;
                if !bytes.starts_with(b"{") {
                    let err: serde_json::Error =
                        serde::de::Error::custom("expected Gemini session object");
                    if serde_json::from_slice::<IgnoredAny>(bytes).is_ok() {
                        fold.session_error =
                            Some(anyhow::Error::new(err).context("Failed to parse Gemini session"));
                        return Ok(());
                    }
                    return Err(err);
                }
                match serde_json::from_slice::<GeminiSession>(bytes) {
                    Ok(session) => {
                        fold.session = Some(session);
                        fold.diagnostics.record_recognized_source();
                        return Ok(());
                    }
                    Err(err) => {
                        // Distinguish pretty-printed / non-JSON line 1 (`Ok(None)`) from valid JSON missing `GeminiSession` fields.
                        if serde_json::from_slice::<IgnoredAny>(bytes).is_ok() {
                            fold.session_error = Some(
                                anyhow::Error::new(err).context("Failed to parse Gemini session"),
                            );
                            return Ok(());
                        }
                        return Err(err);
                    }
                }
            }

            if fold.session_error.is_some() {
                return Ok(());
            }

            if !bytes.starts_with(b"{") {
                return Err(serde::de::Error::custom("expected Gemini event object"));
            }
            let event = serde_json::from_slice::<BorrowedGeminiEvent<'_>>(bytes)?;
            fold.observe_borrowed_event(event);
            Ok(())
        },
        GeminiFold::finish,
    )
}

/// One compacted assistant-message revision retained for historical metrics.
struct GeminiMessageAnalysis {
    last_ts: i64,
    state: Option<SessionParseState>,
    tokens: Option<GeminiTokens>,
    model: Option<String>,
    diagnostics: ParseDiagnostics,
}

struct GeminiFold<'a> {
    mode: ParseMode,
    tiers: Option<&'a TierThresholds>,
    seen_first_line: bool,
    session: Option<GeminiSession>,
    session_error: Option<anyhow::Error>,
    diagnostics: ParseDiagnostics,
    messages: Vec<GeminiMessageAnalysis>,
    positions: FastHashMap<String, usize>,
}

impl<'a> GeminiFold<'a> {
    fn new(mode: ParseMode, tiers: Option<&'a TierThresholds>) -> Self {
        Self {
            mode,
            tiers,
            seen_first_line: false,
            session: None,
            session_error: None,
            diagnostics: ParseDiagnostics::default(),
            messages: Vec::new(),
            positions: FastHashMap::default(),
        }
    }

    fn take_reusable_state(&mut self, id: &str) -> Option<SessionParseState> {
        let &index = self.positions.get(id)?;
        let mut state = self.messages[index].state.take()?;
        state.reset_for_reuse();
        Some(state)
    }

    fn store_message(&mut self, id: Cow<'_, str>, mut analysis: GeminiMessageAnalysis) {
        if let Some(&index) = self.positions.get(id.as_ref()) {
            // Preserve prior billed tokens and model when a later revision omits `tokens`.
            if analysis.tokens.is_none() {
                let prior = &mut self.messages[index];
                analysis.tokens = prior.tokens.take();
                if analysis.model.is_none() {
                    analysis.model = prior.model.take();
                }
            }
            self.messages[index] = analysis;
        } else {
            self.positions.insert(id.into_owned(), self.messages.len());
            self.messages.push(analysis);
        }
    }

    fn observe_borrowed_event(&mut self, event: BorrowedGeminiEvent<'_>) {
        if event.rewind_to.is_some() {
            self.diagnostics.record_recognized_source();
            return;
        }

        if let Some(id_raw) = event.id
            && let Some(type_raw) = event.message_type
            && let Ok(id) = serde_json::from_str::<Cow<'_, str>>(id_raw.get())
            && let Ok(message_type) = serde_json::from_str::<Cow<'_, str>>(type_raw.get())
        {
            if matches!(message_type.as_ref(), "gemini" | "user" | "info" | "error") {
                self.diagnostics.record_recognized_source();
            } else {
                self.diagnostics.record_unrecognized();
            }
            if message_type.as_ref() == "gemini" {
                self.upsert_borrowed_message(
                    Some(id),
                    event.timestamp,
                    event.tokens,
                    event.model,
                    event.tool_calls,
                );
            }
            return;
        }

        let Some(set_raw) = event.set else {
            self.diagnostics.record_unrecognized();
            return;
        };
        self.diagnostics.record_recognized_source();
        if !set_raw.get().starts_with('{') {
            return;
        }
        let Ok(set) = serde_json::from_str::<BorrowedGeminiSet<'_>>(set_raw.get()) else {
            return;
        };
        let Some(messages_raw) = set.messages else {
            return;
        };
        let mut deserializer = serde_json::Deserializer::from_str(messages_raw.get());
        if (SnapshotMessagesSeed { fold: self })
            .deserialize(&mut deserializer)
            .is_err()
        {
            self.diagnostics.record_relevant(false);
        }
    }

    fn upsert_borrowed_message(
        &mut self,
        id: Option<Cow<'_, str>>,
        timestamp_raw: Option<&RawValue>,
        tokens_raw: Option<&RawValue>,
        model_raw: Option<&RawValue>,
        tool_calls_raw: Option<&RawValue>,
    ) {
        let Some(id) = id.filter(|id| !id.is_empty()) else {
            self.diagnostics.record_relevant(false);
            return;
        };

        let mut reusable_state = self.take_reusable_state(id.as_ref());
        let analysis = match parse_borrowed_gemini_message(
            timestamp_raw,
            tokens_raw,
            model_raw,
            tool_calls_raw,
            self.mode,
            &mut reusable_state,
        ) {
            Some(analysis) => analysis,
            None => {
                let mut message_diagnostics = ParseDiagnostics::default();
                message_diagnostics.record_relevant(false);
                GeminiMessageAnalysis {
                    last_ts: 0,
                    state: reusable_state,
                    tokens: None,
                    model: None,
                    diagnostics: message_diagnostics,
                }
            }
        };

        self.store_message(id, analysis);
    }

    fn observe_value_event(&mut self, mut event: Value) {
        if event.get("$rewindTo").is_some() {
            self.diagnostics.record_recognized_source();
            return;
        }

        if event.get("id").and_then(Value::as_str).is_some()
            && let Some(message_type) = event.get("type").and_then(Value::as_str)
        {
            if matches!(message_type, "gemini" | "user" | "info" | "error") {
                self.diagnostics.record_recognized_source();
            } else {
                self.diagnostics.record_unrecognized();
            }
            self.upsert_value_message(event);
            return;
        }

        let Some(mut set) = event.as_object_mut().and_then(|map| map.remove("$set")) else {
            self.diagnostics.record_unrecognized();
            return;
        };
        self.diagnostics.record_recognized_source();
        let Some(messages_value) = set.as_object_mut().and_then(|map| map.remove("messages"))
        else {
            return;
        };
        let Value::Array(snapshot) = messages_value else {
            self.diagnostics.record_relevant(false);
            return;
        };

        for message in snapshot {
            self.upsert_value_message(message);
        }
    }

    fn upsert_value_message(&mut self, message: Value) {
        if message.get("type").and_then(Value::as_str) != Some("gemini") {
            return;
        }
        let Some(id) = message
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
        else {
            self.diagnostics.record_relevant(false);
            return;
        };

        let raw_tokens_present = message
            .get("tokens")
            .is_some_and(|tokens| !tokens.is_null());
        let tokens_supported = message
            .get("tokens")
            .filter(|tokens| !tokens.is_null())
            .is_some_and(gemini_tokens_supported);

        let mut reusable_state = self.take_reusable_state(&id);
        let analysis = match serde_json::from_value::<GeminiAnalysisMessage>(message) {
            Ok(mut message) => {
                let mut message_diagnostics = ParseDiagnostics::default();
                if raw_tokens_present {
                    let normalized = message
                        .model
                        .as_deref()
                        .is_some_and(|model| !model.is_empty())
                        && tokens_supported;
                    message_diagnostics.record_relevant(normalized);
                    if !normalized {
                        message.tokens = None;
                    }
                }
                let ts = parse_iso_timestamp(&message.timestamp);
                if message.message_type == "gemini" {
                    for tool_call in &message.tool_calls {
                        fold_value_tool_call(
                            tool_call,
                            ts,
                            self.mode,
                            &mut reusable_state,
                            &mut message_diagnostics,
                        );
                    }
                }
                GeminiMessageAnalysis {
                    last_ts: ts,
                    state: reusable_state,
                    tokens: message.tokens,
                    model: message.model,
                    diagnostics: message_diagnostics,
                }
            }
            Err(_) => {
                let mut message_diagnostics = ParseDiagnostics::default();
                message_diagnostics.record_relevant(false);
                GeminiMessageAnalysis {
                    last_ts: 0,
                    state: reusable_state,
                    tokens: None,
                    model: None,
                    diagnostics: message_diagnostics,
                }
            }
        };

        self.store_message(Cow::Owned(id), analysis);
    }

    fn finish(self) -> Result<ParsedAnalysis> {
        if let Some(err) = self.session_error {
            return Err(err);
        }
        let session = self
            .session
            .context("Gemini session missing top-level object")?;
        let mut classifier = self.tiers.map(TierClassifier::new);
        let mut state = SessionParseState::with_mode(self.mode);
        let mut conversation_usage: FastHashMap<String, Value> = FastHashMap::with_capacity(3);
        let mut diagnostics = self.diagnostics;

        for message in self.messages {
            diagnostics.merge(message.diagnostics);
            if message.last_ts > state.last_ts {
                state.last_ts = message.last_ts;
            }
            if let Some(model) = message.model.as_deref() {
                state.note_model(model);
            }
            if let (Some(tokens), Some(model)) = (&message.tokens, &message.model) {
                // `tokens.input` already includes cached tokens, so it is the full request prompt size.
                let level = classifier
                    .as_mut()
                    .map_or(0, |classifier| classifier.level(model, tokens.input));
                process_gemini_usage(&mut conversation_usage, model, tokens, level);
            }
            if let Some(msg_state) = message.state {
                state.record_for_model(message.model.as_deref(), |state| {
                    state.merge(msg_state);
                });
            }
        }

        let analysis = finalize_record(state, conversation_usage, session.session_id);
        Ok(ParsedAnalysis::new(analysis, diagnostics))
    }
}

/// Minimal Gemini message payload, omitting `content` and `thoughts` to keep deduplication memory bounded.
#[derive(Deserialize)]
struct GeminiAnalysisMessage {
    #[serde(default)]
    timestamp: String,
    #[serde(rename = "type", default)]
    message_type: String,
    tokens: Option<GeminiTokens>,
    model: Option<String>,
    #[serde(rename = "toolCalls", default)]
    tool_calls: Vec<Value>,
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
struct BorrowedGeminiEvent<'a> {
    #[serde(
        default,
        borrow,
        rename = "$rewindTo",
        deserialize_with = "deserialize_present_raw"
    )]
    rewind_to: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "$set",
        deserialize_with = "deserialize_present_raw"
    )]
    set: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    id: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "type",
        deserialize_with = "deserialize_present_raw"
    )]
    message_type: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    timestamp: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    tokens: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    model: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "toolCalls",
        deserialize_with = "deserialize_present_raw"
    )]
    tool_calls: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedGeminiSet<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    messages: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct BorrowedGeminiSnapshotMessage<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    id: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "type",
        deserialize_with = "deserialize_present_raw"
    )]
    message_type: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    timestamp: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    tokens: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    model: Option<&'a RawValue>,
    #[serde(
        default,
        borrow,
        rename = "toolCalls",
        deserialize_with = "deserialize_present_raw"
    )]
    tool_calls: Option<&'a RawValue>,
}

struct SnapshotMessagesSeed<'a, 'b> {
    fold: &'b mut GeminiFold<'a>,
}

impl<'de> Visitor<'de> for SnapshotMessagesSeed<'_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Gemini $set.messages array")
    }

    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        while let Some(raw) = seq.next_element::<&'de RawValue>()? {
            if !raw.get().starts_with('{') {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<BorrowedGeminiSnapshotMessage<'de>>(raw.get())
            else {
                continue;
            };
            let is_gemini = msg
                .message_type
                .and_then(|raw| serde_json::from_str::<Cow<'de, str>>(raw.get()).ok())
                .is_some_and(|kind| kind == "gemini");
            if !is_gemini {
                continue;
            }
            let id = msg
                .id
                .and_then(|raw| serde_json::from_str::<Cow<'de, str>>(raw.get()).ok());
            self.fold.upsert_borrowed_message(
                id,
                msg.timestamp,
                msg.tokens,
                msg.model,
                msg.tool_calls,
            );
        }
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for SnapshotMessagesSeed<'_, '_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

enum ParsedGeminiTokens {
    Valid(GeminiTokens),
    Unsupported,
}

impl<'de> Deserialize<'de> for ParsedGeminiTokens {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ParsedGeminiTokens;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Gemini tokens object")
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut tokens = GeminiTokens {
                    input: 0,
                    output: 0,
                    cached: 0,
                    thoughts: 0,
                    tool: 0,
                    total: 0,
                };
                let mut recognized = false;

                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "input" => {
                            tokens.input = map.next_value::<i64>()?;
                            recognized = true;
                        }
                        "output" => {
                            tokens.output = map.next_value::<i64>()?;
                            recognized = true;
                        }
                        "cached" => {
                            tokens.cached = map.next_value::<i64>()?;
                            recognized = true;
                        }
                        "thoughts" => {
                            tokens.thoughts = map.next_value::<i64>()?;
                            recognized = true;
                        }
                        "tool" => {
                            tokens.tool = map.next_value::<i64>()?;
                            recognized = true;
                        }
                        "total" => {
                            tokens.total = map.next_value::<i64>()?;
                            recognized = true;
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }

                if recognized {
                    Ok(ParsedGeminiTokens::Valid(tokens))
                } else {
                    Ok(ParsedGeminiTokens::Unsupported)
                }
            }
        }
        deserializer.deserialize_map(V)
    }
}

fn parse_borrowed_gemini_message(
    timestamp_raw: Option<&RawValue>,
    tokens_raw: Option<&RawValue>,
    model_raw: Option<&RawValue>,
    tool_calls_raw: Option<&RawValue>,
    mode: ParseMode,
    reusable_state: &mut Option<SessionParseState>,
) -> Option<GeminiMessageAnalysis> {
    let ts = match timestamp_raw {
        Some(raw) => {
            let timestamp = serde_json::from_str::<Cow<'_, str>>(raw.get()).ok()?;
            parse_iso_timestamp(&timestamp)
        }
        None => 0,
    };

    let model = match model_raw {
        Some(raw) => serde_json::from_str::<Option<Cow<'_, str>>>(raw.get()).ok()?,
        None => None,
    };

    let mut message_diagnostics = ParseDiagnostics::default();
    let tokens = match tokens_raw {
        Some(raw) if raw.get() != "null" => {
            let parsed = serde_json::from_str::<ParsedGeminiTokens>(raw.get()).ok()?;
            match parsed {
                ParsedGeminiTokens::Valid(tokens)
                    if model.as_deref().is_some_and(|m| !m.is_empty()) =>
                {
                    message_diagnostics.record_relevant(true);
                    Some(tokens)
                }
                _ => {
                    message_diagnostics.record_relevant(false);
                    None
                }
            }
        }
        _ => None,
    };

    if let Some(raw) = tool_calls_raw {
        let mut deserializer = serde_json::Deserializer::from_str(raw.get());
        ToolCallsSeed {
            ts,
            mode,
            state: reusable_state,
            diagnostics: &mut message_diagnostics,
        }
        .deserialize(&mut deserializer)
        .ok()?;
    }

    Some(GeminiMessageAnalysis {
        last_ts: ts,
        state: reusable_state.take(),
        tokens,
        model: model.map(Cow::into_owned),
        diagnostics: message_diagnostics,
    })
}

struct ToolCallsSeed<'s, 'd> {
    ts: i64,
    mode: ParseMode,
    state: &'s mut Option<SessionParseState>,
    diagnostics: &'d mut ParseDiagnostics,
}

impl<'de> Visitor<'de> for ToolCallsSeed<'_, '_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Gemini toolCalls array")
    }

    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        while let Some(raw) = seq.next_element::<&'de RawValue>()? {
            if !raw.get().starts_with('{') {
                continue;
            }
            if let Ok(tc) = serde_json::from_str::<BorrowedGeminiToolCall<'de>>(raw.get()) {
                fold_borrowed_tool_call(&tc, self.ts, self.mode, self.state, self.diagnostics);
            }
        }
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for ToolCallsSeed<'_, '_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

#[derive(Deserialize)]
struct BorrowedGeminiToolCall<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    name: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    status: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    args: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    result: Option<&'a RawValue>,
}

fn raw_as_str<'a>(raw: Option<&'a RawValue>) -> Option<Cow<'a, str>> {
    let raw = raw?;
    if !raw.get().starts_with('"') {
        return None;
    }
    serde_json::from_str::<Cow<'a, str>>(raw.get()).ok()
}

fn raw_is_empty_result(raw: Option<&RawValue>) -> bool {
    let Some(raw) = raw else {
        return true;
    };
    let s = raw.get();
    if s == "null" {
        return true;
    }
    s.starts_with('[')
        && s.ends_with(']')
        && s[1..s.len() - 1].bytes().all(|b| b.is_ascii_whitespace())
}

fn borrowed_tool_status(tc: &BorrowedGeminiToolCall<'_>) -> GeminiToolStatus {
    match raw_as_str(tc.status).as_deref() {
        Some("success") => GeminiToolStatus::Success,
        Some("error" | "failed") => GeminiToolStatus::Failed,
        Some("pending" | "running") => GeminiToolStatus::Pending,
        None if raw_is_empty_result(tc.result) => GeminiToolStatus::Pending,
        _ => GeminiToolStatus::Unsupported,
    }
}

#[derive(Default, Deserialize)]
struct BorrowedGeminiArgs<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    file_path: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    content: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    old_string: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    old_text: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    new_string: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    new_text: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    command: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    cmd: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    description: Option<&'a RawValue>,
}

fn first_raw_str<'a>(
    primary: Option<&'a RawValue>,
    fallback: Option<&'a RawValue>,
) -> Option<Cow<'a, str>> {
    raw_as_str(primary.or(fallback))
}

fn first_raw_metrics(
    primary: Option<&RawValue>,
    fallback: Option<&RawValue>,
) -> Option<TextMetrics> {
    count_json_string_metrics(primary.or(fallback)?.get())
}

#[derive(Deserialize)]
struct BorrowedGeminiReadItem<'a> {
    #[serde(default, borrow, rename = "functionResponse")]
    function_response: Option<BorrowedGeminiFunctionResponse<'a>>,
}

#[derive(Deserialize)]
struct BorrowedGeminiFunctionResponse<'a> {
    #[serde(default, borrow)]
    response: Option<BorrowedGeminiResponsePayload<'a>>,
}

#[derive(Deserialize)]
struct BorrowedGeminiResponsePayload<'a> {
    #[serde(default, borrow, deserialize_with = "deserialize_present_raw")]
    output: Option<&'a RawValue>,
}

fn borrowed_tool_result_output_raw<'a>(result: Option<&'a RawValue>) -> Option<&'a RawValue> {
    struct FirstItem;
    impl<'de> Visitor<'de> for FirstItem {
        type Value = Option<&'de RawValue>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a Gemini result array")
        }

        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let first = seq.next_element::<&'de RawValue>()?;
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(first)
        }
    }

    let raw = result?;
    if !raw.get().starts_with('[') {
        return None;
    }
    let mut de = serde_json::Deserializer::from_str(raw.get());
    let first = de.deserialize_seq(FirstItem).ok()??;
    if !first.get().starts_with('{') {
        return None;
    }
    let item = serde_json::from_str::<BorrowedGeminiReadItem<'a>>(first.get()).ok()?;
    item.function_response?.response?.output
}

fn fold_borrowed_tool_call(
    tc: &BorrowedGeminiToolCall<'_>,
    ts: i64,
    mode: ParseMode,
    state_slot: &mut Option<SessionParseState>,
    diagnostics: &mut ParseDiagnostics,
) {
    let Some(tool) = raw_as_str(tc.name)
        .as_deref()
        .and_then(TrackedTool::from_name)
    else {
        return;
    };
    let status = borrowed_tool_status(tc);
    match status {
        GeminiToolStatus::Unsupported => {
            diagnostics.record_relevant(false);
        }
        GeminiToolStatus::Failed => {
            diagnostics.record_relevant(true);
            let state = state_slot.get_or_insert_with(|| SessionParseState::with_mode(mode));
            tool.record_invocation(state);
        }
        GeminiToolStatus::Pending => {
            diagnostics.record_relevant(false);
            let state = state_slot.get_or_insert_with(|| SessionParseState::with_mode(mode));
            tool.record_invocation(state);
        }
        GeminiToolStatus::Success => {
            let state = state_slot.get_or_insert_with(|| SessionParseState::with_mode(mode));
            if record_borrowed_success(tool, tc.args, tc.result, state, ts) {
                diagnostics.record_relevant(true);
            } else {
                diagnostics.record_relevant(false);
                tool.record_invocation(state);
            }
        }
    }
}

fn record_borrowed_success(
    tool: TrackedTool,
    args_raw: Option<&RawValue>,
    result_raw: Option<&RawValue>,
    state: &mut SessionParseState,
    ts: i64,
) -> bool {
    match tool {
        TrackedTool::TodoWrite => {
            state.tool_counts.todo_write += 1;
            return true;
        }
        TrackedTool::ReadManyFiles => {
            state.tool_counts.read += 1;
            return true;
        }
        _ => {}
    }

    let Some(args_raw) = args_raw.filter(|raw| raw.get().starts_with('{')) else {
        return false;
    };
    let Ok(args) = serde_json::from_str::<BorrowedGeminiArgs<'_>>(args_raw.get()) else {
        return false;
    };

    let usage_only = matches!(state.mode, ParseMode::UsageOnly);
    match tool {
        TrackedTool::Read => {
            let Some(file_path) = raw_as_str(args.file_path).filter(|p| !p.is_empty()) else {
                return false;
            };
            let Some(output_raw) = borrowed_tool_result_output_raw(result_raw) else {
                return false;
            };
            if usage_only {
                let Some(metrics) = count_json_string_metrics(output_raw.get()) else {
                    return false;
                };
                state.tool_counts.read += 1;
                let count = state.tool_counts.read;
                state.add_read_counts(&file_path, metrics.lines, metrics.chars);
                state.tool_counts.read = count;
            } else {
                let Some(content) = raw_as_str(Some(output_raw)) else {
                    return false;
                };
                state.tool_counts.read += 1;
                attach_read_detail(state, &file_path, &content, ts);
            }
            true
        }
        TrackedTool::Write => {
            let Some(file_path) = raw_as_str(args.file_path).filter(|p| !p.is_empty()) else {
                return false;
            };
            if usage_only {
                let Some(metrics) = args
                    .content
                    .and_then(|raw| count_json_string_metrics(raw.get()))
                else {
                    return false;
                };
                state.add_write_counts(&file_path, metrics.lines, metrics.chars);
            } else {
                let Some(content) = raw_as_str(args.content) else {
                    return false;
                };
                state.add_write_detail(&file_path, &content, ts);
            }
            true
        }
        TrackedTool::Edit => {
            let Some(file_path) = raw_as_str(args.file_path).filter(|p| !p.is_empty()) else {
                return false;
            };
            if usage_only {
                // Validate `old_string`/`old_text` is a JSON string even though `add_edit_counts_raw` counts only `new`.
                let Some(old_raw) = args.old_string.or(args.old_text) else {
                    return false;
                };
                if count_json_string_metrics(old_raw.get()).is_none() {
                    return false;
                }
                let Some(new_metrics) = first_raw_metrics(args.new_string, args.new_text) else {
                    return false;
                };
                state.add_edit_counts_raw(&file_path, new_metrics.lines, new_metrics.chars);
            } else {
                let Some(old_string) = first_raw_str(args.old_string, args.old_text) else {
                    return false;
                };
                let Some(new_string) = first_raw_str(args.new_string, args.new_text) else {
                    return false;
                };
                state.add_edit_detail_raw(&file_path, &old_string, &new_string, ts);
            }
            true
        }
        TrackedTool::Shell => {
            let Some(command) =
                first_raw_str(args.command, args.cmd).filter(|cmd| !cmd.trim().is_empty())
            else {
                return false;
            };
            let description = if usage_only {
                Cow::Borrowed("")
            } else {
                raw_as_str(args.description).unwrap_or(Cow::Borrowed(""))
            };
            state.add_run_command(&command, &description, ts);
            true
        }
        TrackedTool::TodoWrite | TrackedTool::ReadManyFiles => unreachable!(),
    }
}

fn fold_value_tool_call(
    tool_call: &Value,
    ts: i64,
    mode: ParseMode,
    state_slot: &mut Option<SessionParseState>,
    diagnostics: &mut ParseDiagnostics,
) {
    let Some(tool) = tool_call
        .get("name")
        .and_then(Value::as_str)
        .and_then(TrackedTool::from_name)
    else {
        return;
    };
    let status = gemini_tool_status(tool_call);
    match status {
        GeminiToolStatus::Unsupported => {
            diagnostics.record_relevant(false);
        }
        GeminiToolStatus::Failed => {
            diagnostics.record_relevant(true);
            let state = state_slot.get_or_insert_with(|| SessionParseState::with_mode(mode));
            tool.record_invocation(state);
        }
        GeminiToolStatus::Pending => {
            diagnostics.record_relevant(false);
            let state = state_slot.get_or_insert_with(|| SessionParseState::with_mode(mode));
            tool.record_invocation(state);
        }
        GeminiToolStatus::Success => {
            let state = state_slot.get_or_insert_with(|| SessionParseState::with_mode(mode));
            if let Some(args) = tool.read_args(tool_call) {
                diagnostics.record_relevant(true);
                args.record(state, ts);
            } else {
                diagnostics.record_relevant(false);
                tool.record_invocation(state);
            }
        }
    }
}

fn gemini_tokens_supported(tokens: &Value) -> bool {
    const TOKEN_FIELDS: &[&str] = &["input", "output", "cached", "thoughts", "tool", "total"];
    let Some(tokens) = tokens.as_object() else {
        return false;
    };
    // Reject `{}` because all `GeminiTokens` fields default to zero and would silently bill nothing.
    if tokens.is_empty() {
        return false;
    }

    let mut recognized = false;
    for field in TOKEN_FIELDS {
        if let Some(value) = tokens.get(*field) {
            recognized = true;
            if value.as_i64().is_none() {
                return false;
            }
        }
    }
    recognized
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GeminiToolStatus {
    Success,
    Failed,
    Pending,
    Unsupported,
}

fn gemini_tool_status(tool_call: &Value) -> GeminiToolStatus {
    match tool_call.get("status").and_then(Value::as_str) {
        Some("success") => GeminiToolStatus::Success,
        Some("error" | "failed") => GeminiToolStatus::Failed,
        Some("pending" | "running") => GeminiToolStatus::Pending,
        None if tool_call.get("result").is_none_or(|result| {
            result.is_null() || result.as_array().is_some_and(Vec::is_empty)
        }) =>
        {
            GeminiToolStatus::Pending
        }
        _ => GeminiToolStatus::Unsupported,
    }
}

/// Gemini tools mapped to file-operation or invocation metrics; all tool-name aliases live in [`TrackedTool::from_name`].
#[derive(Clone, Copy)]
enum TrackedTool {
    Read,
    Write,
    Edit,
    Shell,
    TodoWrite,
    ReadManyFiles,
}

impl TrackedTool {
    /// Returns the tracked tool for `name`, or `None` for untracked tools such as `update_topic` and `task_complete`.
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "read_file" => Self::Read,
            "write_file" | "create_file" => Self::Write,
            // `replace` is the current name; `edit_file` and `replace_in_file` support older session logs.
            "edit_file" | "replace_in_file" | "replace" => Self::Edit,
            "run_command" | "run_shell_command" | "execute_command" | "shell" => Self::Shell,
            "write_todos" => Self::TodoWrite,
            // Distinct from `Read` because it reports no single-file content to attach.
            "read_many_files" => Self::ReadManyFiles,
            _ => return None,
        })
    }

    /// Extracts typed arguments for a successful call, or `None` if unreadable (caller falls back to an invocation count).
    fn read_args(self, tool_call: &Value) -> Option<ToolArgs<'_>> {
        let args = tool_call.get("args");
        Some(match self {
            Self::Read => ToolArgs::Read {
                file_path: arg_file_path(args)?,
                content: tool_result_output(tool_call)?,
            },
            Self::Write => ToolArgs::Write {
                file_path: arg_file_path(args)?,
                content: arg_str(args, &["content"])?,
            },
            Self::Edit => ToolArgs::Edit {
                file_path: arg_file_path(args)?,
                old_string: arg_str(args, &["old_string", "old_text"])?,
                new_string: arg_str(args, &["new_string", "new_text"])?,
            },
            Self::Shell => ToolArgs::Shell {
                // Treat blank commands as unreadable because `add_run_command` drops them silently.
                command: arg_str(args, &["command", "cmd"])
                    .filter(|command| !command.trim().is_empty())?,
                description: arg_str(args, &["description"]).unwrap_or(""),
            },
            Self::TodoWrite => ToolArgs::TodoWrite,
            Self::ReadManyFiles => ToolArgs::ReadManyFiles,
        })
    }

    /// Increments the tool's invocation counter without recording file-operation details.
    fn record_invocation(self, state: &mut SessionParseState) {
        match self {
            Self::Read | Self::ReadManyFiles => state.tool_counts.read += 1,
            Self::Write => state.tool_counts.write += 1,
            Self::Edit => state.tool_counts.edit += 1,
            Self::Shell => state.tool_counts.bash += 1,
            Self::TodoWrite => state.tool_counts.todo_write += 1,
        }
    }
}

/// Validated arguments of a successful [`TrackedTool`] call, shared by diagnostics and metric folding.
enum ToolArgs<'a> {
    Read {
        file_path: &'a str,
        content: &'a str,
    },
    Write {
        file_path: &'a str,
        content: &'a str,
    },
    Edit {
        file_path: &'a str,
        old_string: &'a str,
        new_string: &'a str,
    },
    Shell {
        command: &'a str,
        description: &'a str,
    },
    TodoWrite,
    ReadManyFiles,
}

impl ToolArgs<'_> {
    /// Folds the call into the file-operation metrics.
    fn record(self, state: &mut SessionParseState, ts: i64) {
        match self {
            Self::Read { file_path, content } => {
                state.tool_counts.read += 1;
                attach_read_detail(state, file_path, content, ts);
            }
            Self::Write { file_path, content } => state.add_write_detail(file_path, content, ts),
            Self::Edit {
                file_path,
                old_string,
                new_string,
            } => {
                // Use `add_edit_detail_raw` so an empty `old_string` stays an edit instead of converting into a write.
                state.add_edit_detail_raw(file_path, old_string, new_string, ts);
            }
            Self::Shell {
                command,
                description,
            } => state.add_run_command(command, description, ts),
            Self::TodoWrite => state.tool_counts.todo_write += 1,
            Self::ReadManyFiles => state.tool_counts.read += 1,
        }
    }
}

/// Returns the string value of the first matching key in `keys`, returning `None` if that key is present with a non-string value.
fn arg_str<'a>(args: Option<&'a Value>, keys: &[&str]) -> Option<&'a str> {
    let args = args?;
    keys.iter().find_map(|key| args.get(*key))?.as_str()
}

/// Returns the non-empty `file_path` string from `args`, if present.
fn arg_file_path(args: Option<&Value>) -> Option<&str> {
    arg_str(args, &["file_path"]).filter(|path| !path.is_empty())
}

/// Builds a single-record [`CodeAnalysis`] with `session_id` (Gemini CLI logs omit workspace paths, leaving `folder_path` and `git_remote` empty).
fn finalize_record(
    state: SessionParseState,
    conversation_usage: FastHashMap<String, Value>,
    session_id: String,
) -> CodeAnalysis {
    let last_ts = state.last_ts;
    let mut record = state.into_record(conversation_usage);
    record.task_id = session_id;
    record.timestamp = last_ts;

    CodeAnalysis {
        user: String::new(),
        extension_name: String::new(),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    }
}

/// Attaches read details while preserving the caller's existing `tool_counts.read`.
fn attach_read_detail(state: &mut SessionParseState, path: &str, content: &str, ts: i64) {
    let invocation_count = state.tool_counts.read;
    state.add_read_detail(path, content, ts);
    state.tool_counts.read = invocation_count;
}

/// Returns the output string from a Gemini tool call result.
fn tool_result_output(tool_call: &Value) -> Option<&str> {
    tool_call
        .get("result")
        .and_then(|r| r.as_array())
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("functionResponse"))
        .and_then(|fr| fr.get("response"))
        .and_then(|resp| resp.get("output"))
        .and_then(|o| o.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> GeminiSession {
        GeminiSession {
            session_id: "session-1".to_string(),
            project_hash: "project-1".to_string(),
            start_time: String::new(),
            last_updated: String::new(),
            kind: Some("main".to_string()),
        }
    }

    fn assistant(id: &str, model: &str, input_tokens: i64, mut tool_calls: Value) -> Value {
        if let Some(tool_calls) = tool_calls.as_array_mut() {
            for tool_call in tool_calls {
                if let Some(tool_call) = tool_call.as_object_mut() {
                    tool_call
                        .entry("status")
                        .or_insert_with(|| Value::String("success".to_string()));
                }
            }
        }
        json!({
            "id": id,
            "timestamp": "2026-07-12T00:00:00Z",
            "type": "gemini",
            "model": model,
            "tokens": {
                "input": input_tokens,
                "output": 1,
                "cached": 0,
                "thoughts": 0,
                "tool": 0,
                "total": input_tokens + 1
            },
            "toolCalls": tool_calls
        })
    }

    #[test]
    fn repeated_message_id_uses_only_latest_revision() {
        let first = assistant(
            "message-1",
            "gemini-test",
            10,
            json!([{ "name": "write_file", "args": {
                "file_path": "/tmp/a.txt", "content": "old"
            }}]),
        );
        let latest = assistant(
            "message-1",
            "gemini-test",
            20,
            json!([{ "name": "write_file", "args": {
                "file_path": "/tmp/a.txt", "content": "new"
            }}]),
        );

        let analysis =
            parse_gemini_events(session(), vec![first, latest], ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.conversation_usage["gemini-test"]["input_tokens"], 20);
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.write_file_details.len(), 1);
        assert_eq!(record.write_file_details[0].content, "new");
    }

    #[test]
    fn tokenless_revision_keeps_prior_billed_tokens() {
        let billed = assistant("message-1", "gemini-test", 42, json!([]));
        let mut tokenless = assistant(
            "message-1",
            "gemini-test",
            0,
            json!([{ "name": "write_file", "args": {
                "file_path": "/tmp/a.txt", "content": "x"
            }}]),
        );
        tokenless["tokens"] = json!(null);

        let analysis =
            parse_gemini_events(session(), vec![billed, tokenless], ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.conversation_usage["gemini-test"]["input_tokens"], 42);
        assert_eq!(record.tool_call_counts.write, 1);
    }

    #[test]
    fn messages_snapshot_merges_without_recounting_ids() {
        let prior = assistant("prior", "gemini-prior", 10, json!([]));
        let current = assistant("current", "gemini-current", 30, json!([]));
        let snapshot = json!({ "$set": { "messages": [current] } });

        let analysis =
            parse_gemini_events(session(), vec![prior, snapshot], ParseMode::Full).unwrap();
        let usage = &analysis.records[0].conversation_usage;
        assert_eq!(usage["gemini-prior"]["input_tokens"], 10);
        assert_eq!(usage["gemini-current"]["input_tokens"], 30);
    }

    #[test]
    fn rewind_keeps_already_billed_messages() {
        let first = assistant("first", "gemini-first", 10, json!([]));
        let second = assistant("second", "gemini-second", 20, json!([]));
        let third = assistant("third", "gemini-third", 30, json!([]));
        let rewind = json!({ "$rewindTo": "second" });

        let analysis = parse_gemini_events(
            session(),
            vec![first, second, third, rewind],
            ParseMode::Full,
        )
        .unwrap();
        let usage = &analysis.records[0].conversation_usage;
        assert_eq!(usage.len(), 3);
        assert!(usage.contains_key("gemini-first"));
        assert!(usage.contains_key("gemini-second"));
        assert!(usage.contains_key("gemini-third"));
    }

    #[test]
    fn current_tool_names_map_to_existing_metrics() {
        let message = assistant(
            "message-1",
            "gemini-test",
            10,
            json!([
                { "name": "run_shell_command", "args": { "command": "true" } },
                { "name": "write_todos", "args": { "todos": [] } },
                { "name": "read_many_files", "args": { "include": ["src/**"] } }
            ]),
        );

        let analysis = parse_gemini_events(session(), vec![message], ParseMode::Full).unwrap();
        let counts = &analysis.records[0].tool_call_counts;
        assert_eq!(counts.bash, 1);
        assert_eq!(counts.todo_write, 1);
        assert_eq!(counts.read, 1);
    }

    #[test]
    fn every_tracked_name_folds_into_its_own_metric() {
        // Counters in order: read, write, edit, bash, todo_write.
        let cases = [
            ("read_file", [1, 0, 0, 0, 0]),
            ("read_many_files", [1, 0, 0, 0, 0]),
            ("write_file", [0, 1, 0, 0, 0]),
            ("create_file", [0, 1, 0, 0, 0]),
            ("edit_file", [0, 0, 1, 0, 0]),
            ("replace_in_file", [0, 0, 1, 0, 0]),
            ("replace", [0, 0, 1, 0, 0]),
            ("run_command", [0, 0, 0, 1, 0]),
            ("run_shell_command", [0, 0, 0, 1, 0]),
            ("execute_command", [0, 0, 0, 1, 0]),
            ("shell", [0, 0, 0, 1, 0]),
            ("write_todos", [0, 0, 0, 0, 1]),
            // A meta tool stays untracked and moves no counter.
            ("update_topic", [0, 0, 0, 0, 0]),
        ];

        // Both `ToolArgs::record` (success) and `record_invocation` (error) must hit the same counter.
        for status in ["success", "error"] {
            for (name, expected) in cases {
                // Provide a superset of args so every tool passes validation and exercises `ToolArgs::record`.
                let message = assistant(
                    "message-1",
                    "gemini-test",
                    10,
                    json!([{
                        "name": name,
                        "status": status,
                        "args": {
                            "file_path": "/tmp/a.txt",
                            "content": "one",
                            "old_string": "one",
                            "new_string": "two",
                            "command": "true"
                        },
                        "result": [{
                            "functionResponse": { "response": { "output": "one" } }
                        }]
                    }]),
                );

                let parsed = parse_gemini_events_with_diagnostics(
                    session(),
                    vec![message],
                    ParseMode::Full,
                    None,
                )
                .unwrap();
                let counts = &parsed.analysis.records[0].tool_call_counts;
                assert_eq!(
                    [
                        counts.read,
                        counts.write,
                        counts.edit,
                        counts.bash,
                        counts.todo_write
                    ],
                    expected,
                    "{name} ({status}) folded into the wrong metric"
                );
                assert_eq!(
                    parsed.diagnostics.partial_failure_count(),
                    0,
                    "{name} ({status}) must not raise schema drift"
                );
            }
        }
    }

    #[test]
    fn read_result_validation_distinguishes_empty_files_from_schema_drift() {
        let drifted = assistant(
            "drifted",
            "gemini-test",
            10,
            json!([{
                "name": "read_file",
                "args": { "file_path": "/tmp/a.txt" },
                "result": [{
                    "functionResponse": { "response": { "futureOutput": "" } }
                }]
            }]),
        );
        let drifted =
            parse_gemini_events_with_diagnostics(session(), vec![drifted], ParseMode::Full, None)
                .unwrap();
        assert_eq!(drifted.diagnostics.partial_failure_count(), 1);
        assert_eq!(drifted.analysis.records[0].tool_call_counts.read, 1);
        assert_eq!(drifted.analysis.records[0].total_read_lines, 0);

        let empty = assistant(
            "empty",
            "gemini-test",
            10,
            json!([{
                "name": "read_file",
                "args": { "file_path": "/tmp/a.txt" },
                "result": [{
                    "functionResponse": { "response": { "output": "" } }
                }]
            }]),
        );
        let empty =
            parse_gemini_events_with_diagnostics(session(), vec![empty], ParseMode::Full, None)
                .unwrap();
        assert_eq!(empty.diagnostics.partial_failure_count(), 0);
        assert_eq!(empty.analysis.records[0].tool_call_counts.read, 1);
        assert_eq!(empty.analysis.records[0].total_read_lines, 0);
    }

    #[test]
    fn unknown_only_token_keys_do_not_become_zero_usage() {
        let mut message = assistant("drifted", "gemini-test", 10, json!([]));
        message["tokens"] = json!({ "prompt": 123, "completion": 45 });

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
                .unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert!(
            parsed.analysis.records[0].conversation_usage.is_empty(),
            "unknown token keys must not become a successful all-zero usage row"
        );

        let mut current = assistant("current", "gemini-test", 10, json!([]));
        current["tokens"] = json!({ "input": 0 });
        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![current], ParseMode::Full, None)
                .unwrap();
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(
            parsed.analysis.records[0].conversation_usage["gemini-test"]["input_tokens"],
            0
        );
    }

    #[test]
    fn an_empty_token_object_does_not_become_zero_usage() {
        let mut message = assistant("empty-tokens", "gemini-test", 10, json!([]));
        message["tokens"] = json!({});

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
                .unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert!(
            parsed.analysis.records[0].conversation_usage.is_empty(),
            "an empty token object must not become a successful all-zero usage row"
        );
    }

    #[test]
    fn edit_requires_both_old_and_new_text_without_falling_back_to_write() {
        let message = assistant(
            "drifted-edit",
            "gemini-test",
            10,
            json!([{
                "name": "replace",
                "args": {
                    "file_path": "/tmp/a.txt",
                    "future_old": "old",
                    "new_string": "new"
                }
            }]),
        );

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
                .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.total_edit_lines, 0);
        assert_eq!(record.total_write_lines, 0);

        // An empty `old_string` must remain an edit rather than converting into a write.
        let empty_old = assistant(
            "empty-old-edit",
            "gemini-test",
            10,
            json!([{
                "name": "replace",
                "args": {
                    "file_path": "/tmp/a.txt",
                    "old_string": "",
                    "new_string": "new"
                }
            }]),
        );

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![empty_old], ParseMode::Full, None)
                .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.total_edit_lines, 1);
        assert_eq!(record.total_write_lines, 0);
    }

    #[test]
    fn every_argument_alias_reaches_the_fold() {
        // Assert on folded content, not just counters, because an unresolved alias still increments the invocation count.
        for (old_key, new_key) in [("old_string", "new_string"), ("old_text", "new_text")] {
            let mut args = json!({ "file_path": "/tmp/a.txt" });
            args[old_key] = json!("one");
            args[new_key] = json!("two\nthree");
            let message = assistant(
                "edit",
                "gemini-test",
                10,
                json!([{ "name": "replace", "args": args }]),
            );

            let parsed = parse_gemini_events_with_diagnostics(
                session(),
                vec![message],
                ParseMode::Full,
                None,
            )
            .unwrap();
            let record = &parsed.analysis.records[0];
            assert_eq!(
                parsed.diagnostics.partial_failure_count(),
                0,
                "{old_key}/{new_key} must not raise schema drift"
            );
            assert_eq!(record.tool_call_counts.edit, 1);
            assert_eq!(
                record.total_edit_lines, 2,
                "{new_key} did not reach the fold"
            );
            assert_eq!(
                record.edit_file_details[0].old_string, "one",
                "{old_key} did not reach the fold"
            );
        }

        for command_key in ["command", "cmd"] {
            let mut args = json!({ "description": "list the tree" });
            args[command_key] = json!("ls -la");
            let message = assistant(
                "shell",
                "gemini-test",
                10,
                json!([{ "name": "run_shell_command", "args": args }]),
            );

            let parsed = parse_gemini_events_with_diagnostics(
                session(),
                vec![message],
                ParseMode::Full,
                None,
            )
            .unwrap();
            let record = &parsed.analysis.records[0];
            assert_eq!(
                parsed.diagnostics.partial_failure_count(),
                0,
                "{command_key} must not raise schema drift"
            );
            assert_eq!(record.tool_call_counts.bash, 1);
            assert_eq!(
                record.run_command_details[0].command, "ls -la",
                "{command_key} did not reach the fold"
            );
        }
    }

    #[test]
    fn a_blank_command_is_counted_rather_than_dropped() {
        // `add_run_command` drops blank commands, so they must fail argument parsing and fall back to an invocation count.
        let message = assistant(
            "blank-command",
            "gemini-test",
            10,
            json!([{ "name": "run_shell_command", "args": { "command": "   " } }]),
        );

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
                .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert_eq!(record.tool_call_counts.bash, 1);
        assert!(record.run_command_details.is_empty());
    }

    #[test]
    fn an_unsupported_tool_status_records_nothing() {
        let message = assistant(
            "unsupported-status",
            "gemini-test",
            10,
            json!([{
                "name": "write_file",
                "status": "cancelled",
                "args": { "file_path": "/tmp/a.txt", "content": "one" }
            }]),
        );

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
                .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.total_write_lines, 0);
    }

    #[test]
    fn failed_write_counts_the_invocation_without_claiming_file_changes() {
        let message = assistant(
            "failed-write",
            "gemini-test",
            10,
            json!([{
                "name": "write_file",
                "status": "error",
                "args": {
                    "file_path": "/tmp/a.txt",
                    "content": "one\ntwo"
                },
                "result": [{ "functionResponse": { "response": { "error": "denied" } } }]
            }]),
        );

        let parsed =
            parse_gemini_events_with_diagnostics(session(), vec![message], ParseMode::Full, None)
                .unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.total_write_lines, 0);
        assert!(record.write_file_details.is_empty());
    }

    #[test]
    fn superseded_pending_revision_does_not_leave_a_false_warning() {
        let pending = assistant(
            "message-1",
            "gemini-test",
            10,
            json!([{
                "name": "read_file",
                "status": "pending",
                "args": { "file_path": "/tmp/a.txt" }
            }]),
        );
        let complete = assistant(
            "message-1",
            "gemini-test",
            20,
            json!([{
                "name": "read_file",
                "status": "success",
                "args": { "file_path": "/tmp/a.txt" },
                "result": [{
                    "functionResponse": { "response": { "output": "one\ntwo" } }
                }]
            }]),
        );

        let parsed = parse_gemini_events_with_diagnostics(
            session(),
            vec![pending, complete],
            ParseMode::Full,
            None,
        )
        .unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.analysis.records[0].tool_call_counts.read, 1);
        assert_eq!(parsed.analysis.records[0].total_read_lines, 2);
    }

    #[test]
    fn borrowed_jsonl_stream_matches_value_parser_across_snapshots_and_cjk() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl");
        let meta = json!({
            "sessionId": "s-gemini",
            "projectHash": "proj",
            "startTime": "2026-07-12T00:00:00Z",
            "lastUpdated": "2026-07-12T00:01:00Z",
            "kind": "main"
        });
        let events = vec![
            json!({ "id": "u1", "timestamp": "2026-07-12T00:00:01Z", "type": "user", "content": "hi" }),
            assistant(
                "m1",
                "gemini-test",
                100,
                json!([{
                    "name": "read_file",
                    "args": { "file_path": "/tmp/zh.txt" },
                    "result": [{ "functionResponse": { "response": { "output": "繁體中文\n第二行🚀\n" } } }]
                }]),
            ),
            // Snapshot supersedes m1 and introduces m2 with write + replace.
            json!({
                "$set": {
                    "messages": [
                        { "id": "u1", "timestamp": "2026-07-12T00:00:01Z", "type": "user", "content": "hi" },
                        assistant(
                            "m1",
                            "gemini-test",
                            150,
                            json!([{
                                "name": "read_file",
                                "args": { "file_path": "/tmp/zh.txt" },
                                "result": [{ "functionResponse": { "response": { "output": "繁體中文\n第二行🚀\n第三行\n" } } }]
                            }]),
                        ),
                        assistant(
                            "m2",
                            "gemini-test",
                            200,
                            json!([
                                { "name": "write_file", "args": { "file_path": "/tmp/out.txt", "content": "寫入\n多行😀\n\n" } },
                                { "name": "replace", "args": { "file_path": "/tmp/out.txt", "old_text": "", "new_text": "修改\r\n後" } },
                                { "name": "run_shell_command", "args": { "cmd": "ls -la", "description": "list" } },
                                { "name": "write_todos", "args": {} },
                                { "name": "read_many_files", "args": {} }
                            ]),
                        )
                    ]
                }
            }),
            json!({ "$rewindTo": "m1" }),
        ];

        let mut body = format!("{meta}\n");
        for event in &events {
            body.push_str(&format!("{event}\n"));
        }
        std::fs::write(&path, body).unwrap();

        let session: GeminiSession = serde_json::from_value(meta).unwrap();
        let owned_full =
            parse_gemini_events_with_diagnostics(session, events, ParseMode::Full, None).unwrap();
        let streamed_full = parse_gemini_jsonl_path(&path, ParseMode::Full, None)
            .unwrap()
            .unwrap();
        let streamed_usage = parse_gemini_jsonl_path(&path, ParseMode::UsageOnly, None)
            .unwrap()
            .unwrap();

        assert_eq!(streamed_full.diagnostics, owned_full.diagnostics);
        assert_eq!(streamed_usage.diagnostics, owned_full.diagnostics);
        let o = &owned_full.analysis.records[0];
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
        assert_eq!(
            serde_json::to_value(&su.tool_call_counts).unwrap(),
            serde_json::to_value(&o.tool_call_counts).unwrap()
        );
        assert_eq!(su.conversation_usage, o.conversation_usage);
    }
}
