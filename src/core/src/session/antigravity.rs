//! Antigravity conversation transcript parser.
//!
//! Antigravity CLI (`agy`), Antigravity Desktop, and other builds keep one directory per conversation under `<root>/brain/<conversation-id>/`, whose `.system_generated/logs/transcript_full.jsonl` is the conversation's step log. The model that served each generation lives in the sibling `<root>/conversations/<conversation-id>.db`, whose `gen_metadata` rows are protobuf messages naming the model and the step the generation answered.

use crate::constants::{FastHashMap, buffer};
use crate::models::CodeAnalysis;
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::cursor::read_varint;
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::sqlite::with_readonly_connection;
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::parse_iso_timestamp;
use crate::utils::token_merge::write_tier_slice;
use crate::utils::{MAX_TIER_LEVELS, TierSlice};
use anyhow::{Context, Result};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json, value::RawValue};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// The model name given to tokens no generation record attributes.
const UNKNOWN_MODEL: &str = "unknown";

/// One request's tokens in vct's disjoint buckets; Antigravity transcripts report no reasoning or cache-write counts.
#[derive(Debug, Clone, Copy, Default)]
struct Tokens {
    input: i64,
    output: i64,
    cache_read: i64,
}

impl Tokens {
    /// Returns a step's token counts, or `None` when none are present; `input_tokens` already excludes cache reads.
    fn from_record(record: &Map<String, Value>) -> Option<Self> {
        if !["input_tokens", "cache_read_tokens", "output_tokens"]
            .iter()
            .any(|key| record.contains_key(*key))
        {
            return None;
        }
        let field = |key: &str| record.get(key).and_then(Value::as_i64).unwrap_or(0);
        Some(Self {
            input: field("input_tokens"),
            output: field("output_tokens"),
            cache_read: field("cache_read_tokens"),
        })
    }

    fn from_borrowed(
        input: Option<Option<i64>>,
        output: Option<Option<i64>>,
        cache_read: Option<Option<i64>>,
    ) -> Option<Self> {
        if input.is_none() && output.is_none() && cache_read.is_none() {
            return None;
        }
        Some(Self {
            input: input.flatten().unwrap_or(0),
            output: output.flatten().unwrap_or(0),
            cache_read: cache_read.flatten().unwrap_or(0),
        })
    }

    fn merge(&mut self, other: Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
    }

    /// Returns the prompt size one request is billed against.
    fn request_context(self) -> i64 {
        self.input + self.cache_read
    }
}

/// Per-model running totals and above-tier slices (`above_tiers[n]` holds price level `n + 1`).
#[derive(Debug, Default)]
struct ModelUsage {
    total: Tokens,
    above_tiers: Vec<Tokens>,
}

impl ModelUsage {
    fn classify(&mut self, level: usize, tokens: Tokens) {
        if level == 0 || level > MAX_TIER_LEVELS {
            return;
        }
        if self.above_tiers.len() < level {
            self.above_tiers.resize(level, Tokens::default());
        }
        self.above_tiers[level - 1].merge(tokens);
    }
}

/// Metadata read from the sibling conversation database.
#[derive(Debug, Default)]
struct ConversationMeta {
    /// Model of each generation that records the step it answered, keyed by step index.
    step_models: FastHashMap<i64, String>,
    /// First generation's model, used for steps before any keyed generation.
    first_model: Option<String>,
    workspace: Option<String>,
    git_remote: Option<String>,
}

/// Returns `<root>/conversations/<id>.db` for a transcript at `<root>/brain/<id>/.system_generated/logs/<file>`.
pub(crate) fn conversation_db_path(transcript: &Path) -> Option<PathBuf> {
    let conversation = transcript.parent()?.parent()?.parent()?;
    let root = conversation.parent()?.parent()?;
    let mut name = conversation.file_name()?.to_os_string();
    name.push(".db");
    Some(root.join("conversations").join(name))
}

/// Returns whether a JSON record is an Antigravity transcript step.
pub(crate) fn is_antigravity_step(value: &Value) -> bool {
    value.get("step_index").is_some_and(Value::is_i64)
        && value.get("source").is_some_and(Value::is_string)
        && value.get("type").is_some_and(Value::is_string)
}

/// Parses one Antigravity `transcript_full.jsonl` into a [`ParsedAnalysis`].
pub(crate) fn parse_antigravity_session(
    path: &Path,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<ParsedAnalysis> {
    let meta = match conversation_db_path(path) {
        Some(db) if db.exists() => read_conversation_meta(&db)?,
        _ => ConversationMeta::default(),
    };
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;

    let mut parser = AntigravityParser::new(mode, tiers, meta);
    if let Some(id) = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::file_name)
    {
        parser.state.task_id = id.to_string_lossy().into_owned();
    }
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);
    let mut line = Vec::with_capacity(buffer::AVG_JSONL_LINE_SIZE);
    let mut index = 0_usize;
    loop {
        line.clear();
        index += 1;
        let count = reader.read_until(b'\n', &mut line).with_context(|| {
            format!(
                "Failed to read Antigravity transcript line {index}: {}",
                path.display()
            )
        })?;
        if count == 0 {
            break;
        }
        let text = std::str::from_utf8(&line)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
            .with_context(|| {
                format!(
                    "Failed to read Antigravity transcript line {index}: {}",
                    path.display()
                )
            })?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<BorrowedStep<'_>>(trimmed) {
            Ok(step) => parser.push_borrowed(step),
            Err(_) => match serde_json::from_str::<Value>(trimmed) {
                Ok(record) => parser.push(&record),
                Err(_) => parser.diagnostics.record_malformed(),
            },
        }
    }
    Ok(parser.finish())
}

fn read_conversation_meta(db: &Path) -> Result<ConversationMeta> {
    with_readonly_connection(
        db,
        "gen_metadata",
        "vct-antigravity-",
        "Antigravity conversation",
        |conn| {
            let mut meta = ConversationMeta::default();
            let mut statement = conn.prepare("SELECT data FROM gen_metadata ORDER BY idx")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let Some(bytes) = row.get_ref(0)?.as_blob_or_null()? else {
                    continue;
                };
                let Some((step, model)) = decode_generation(bytes) else {
                    continue;
                };
                meta.first_model.get_or_insert_with(|| model.clone());
                if let Some(step) = step {
                    meta.step_models.insert(step, model);
                }
            }

            let has_trajectory: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'trajectory_metadata_blob')",
                [],
                |row| row.get(0),
            )?;
            if has_trajectory {
                let mut traj_stmt =
                    conn.prepare("SELECT data FROM trajectory_metadata_blob WHERE id = 'main'")?;
                let mut traj_rows = traj_stmt.query([])?;
                if let Some(row) = traj_rows.next()?
                    && let Some(trajectory) = row.get_ref(0)?.as_blob_or_null()?
                {
                    decode_trajectory(trajectory, &mut meta);
                }
            }
            Ok(meta)
        },
    )
}

/// Decodes a `gen_metadata` protobuf row (field 1: model id in 19, `last_step_index` key/value in 20) into `(produced_step, model)`.
fn decode_generation(data: &[u8]) -> Option<(Option<i64>, String)> {
    let generation = bytes_field(data, 1)?;
    let mut model = None;
    let mut step = None;
    for (field, value) in ProtoFields(generation, 0) {
        match (field, value) {
            (19, ProtoValue::Bytes(bytes)) => {
                model = std::str::from_utf8(bytes).ok().map(model_name);
            }
            (20, ProtoValue::Bytes(pair))
                if bytes_field(pair, 1) == Some(b"last_step_index".as_slice()) =>
            {
                step = bytes_field(pair, 2)
                    .and_then(|value| std::str::from_utf8(value).ok())
                    .and_then(|value| value.parse::<i64>().ok())
                    .map(|last| last + 1);
            }
            _ => {}
        }
    }
    let model = model.filter(|model| !model.is_empty())?;
    Some((step, model))
}

/// Returns the model name from `id`, resolving `<config>|<backend>` to `<model>` for a Vertex `<model>@<version>` backend and `<config>` for a serving path.
fn model_name(id: &str) -> String {
    let Some((name, backend)) = id.split_once('|') else {
        return id.to_string();
    };
    match backend.split_once('@') {
        Some((model, _)) if !model.contains('/') => model.to_string(),
        _ => name.to_string(),
    }
}

/// Populates `meta` from a trajectory protobuf blob (field 1: workspace URI in 1, repository remote URL in 3.2).
fn decode_trajectory(data: &[u8], meta: &mut ConversationMeta) {
    let Some(workspace) = bytes_field(data, 1) else {
        return;
    };
    meta.workspace = bytes_field(workspace, 1)
        .and_then(|uri| std::str::from_utf8(uri).ok())
        .map(workspace_path);
    meta.git_remote = bytes_field(workspace, 3)
        .and_then(|repository| bytes_field(repository, 2))
        .and_then(|url| std::str::from_utf8(url).ok())
        .map(str::to_string);
}

fn workspace_path(uri: &str) -> String {
    match uri.strip_prefix("file://") {
        Some(path) => percent_decode_str(path).decode_utf8_lossy().into_owned(),
        None => uri.to_string(),
    }
}

/// One protobuf field value: an unparsed scalar (`Varint` or fixed-width) or length-delimited `Bytes`.
enum ProtoValue<'a> {
    Varint,
    Bytes(&'a [u8]),
}

/// Iterates a protobuf message's top-level fields, stopping at the first malformed wire entry.
struct ProtoFields<'a>(&'a [u8], usize);

impl<'a> Iterator for ProtoFields<'a> {
    type Item = (u64, ProtoValue<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        let data = self.0;
        let (tag, next) = read_varint(data, self.1)?;
        let value = match tag & 7 {
            0 => {
                self.1 = read_varint(data, next)?.1;
                ProtoValue::Varint
            }
            2 => {
                let (len, start) = read_varint(data, next)?;
                let end = start
                    .checked_add(usize::try_from(len).ok()?)
                    .filter(|end| *end <= data.len())?;
                self.1 = end;
                ProtoValue::Bytes(&data[start..end])
            }
            1 => {
                self.1 = next.checked_add(8)?;
                ProtoValue::Varint
            }
            5 => {
                self.1 = next.checked_add(4)?;
                ProtoValue::Varint
            }
            _ => return None,
        };
        Some((tag >> 3, value))
    }
}

/// Returns the first length-delimited `field` in `data`.
fn bytes_field(data: &[u8], field: u64) -> Option<&[u8]> {
    ProtoFields(data, 0).find_map(|(number, value)| match value {
        ProtoValue::Bytes(bytes) if number == field => Some(bytes),
        _ => None,
    })
}

/// A tool call waiting for the result record that follows it.
struct PendingCall {
    name: String,
    args: Value,
}

fn deserialize_present_opt_i64<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Option<i64>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<i64>::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
struct BorrowedStep<'a> {
    #[serde(borrow, rename = "type")]
    kind: Cow<'a, str>,
    #[serde(default, borrow)]
    created_at: Option<Cow<'a, str>>,
    #[serde(default)]
    step_index: Option<i64>,
    #[serde(default, deserialize_with = "deserialize_present_opt_i64")]
    input_tokens: Option<Option<i64>>,
    #[serde(default, deserialize_with = "deserialize_present_opt_i64")]
    output_tokens: Option<Option<i64>>,
    #[serde(default, deserialize_with = "deserialize_present_opt_i64")]
    cache_read_tokens: Option<Option<i64>>,
    #[serde(default, borrow)]
    status: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    content: Option<&'a RawValue>,
    #[serde(default, borrow)]
    tool_calls: Option<Vec<BorrowedToolCall<'a>>>,
}

#[derive(Deserialize)]
struct BorrowedToolCall<'a> {
    #[serde(default)]
    name: String,
    #[serde(default, borrow)]
    args: Option<&'a RawValue>,
}

struct AntigravityParser<'a> {
    state: SessionParseState,
    usage: FastHashMap<String, ModelUsage>,
    /// Compaction request tokens per model, billed to the model active when each `CHECKPOINT` runs.
    compaction: FastHashMap<String, ModelUsage>,
    diagnostics: ParseDiagnostics,
    classifier: Option<TierClassifier<'a>>,
    meta: ConversationMeta,
    /// Model of the latest keyed generation, carried forward to steps whose generation has no step index.
    current_model: Option<String>,
    /// A step's N tool calls are followed by exactly N result records, in order.
    pending_calls: VecDeque<PendingCall>,
}

impl<'a> AntigravityParser<'a> {
    fn new(mode: ParseMode, tiers: Option<&'a TierThresholds>, meta: ConversationMeta) -> Self {
        let mut state = SessionParseState::with_mode(mode);
        if let Some(workspace) = &meta.workspace {
            state.folder_path.clone_from(workspace);
        }
        if let Some(remote) = &meta.git_remote {
            state.git_remote.clone_from(remote);
        }
        Self {
            state,
            usage: FastHashMap::default(),
            compaction: FastHashMap::default(),
            diagnostics: ParseDiagnostics::default(),
            classifier: tiers.map(TierClassifier::new),
            current_model: meta.first_model.clone(),
            meta,
            pending_calls: VecDeque::new(),
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
        let timestamp = object
            .get("created_at")
            .and_then(Value::as_str)
            .map_or(0, parse_iso_timestamp);
        self.state.last_ts = self.state.last_ts.max(timestamp);

        match kind {
            "PLANNER_RESPONSE" => {
                if let Some(step) = object.get("step_index").and_then(Value::as_i64) {
                    self.update_current_model(step);
                }
                if let Some(tokens) = Tokens::from_record(object) {
                    self.apply_tokens(tokens);
                }
                // Results never outlive their step, so clearing prevents a short step from shifting the next step's pairing.
                self.pending_calls.clear();
                for call in object
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    self.pending_calls.push_back(PendingCall {
                        name: call
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        args: call.get("args").cloned().unwrap_or(Value::Null),
                    });
                }
            }
            // Compaction summaries are billed requests without a generation record, so attribute them to the active model.
            "CHECKPOINT" => {
                if let Some(tokens) = Tokens::from_record(object) {
                    self.apply_compaction_tokens(tokens);
                }
            }
            "GENERIC" => {
                if let Some(call) = self.pending_calls.pop_front() {
                    self.apply_result(&call, object, timestamp);
                }
            }
            "USER_INPUT" | "SYSTEM_MESSAGE" => {}
            _ => {
                self.diagnostics.record_unrecognized();
                return;
            }
        }
        self.diagnostics.record_recognized_source();
    }

    fn push_borrowed(&mut self, step: BorrowedStep<'_>) {
        let timestamp = step.created_at.as_deref().map_or(0, parse_iso_timestamp);
        self.state.last_ts = self.state.last_ts.max(timestamp);

        match step.kind.as_ref() {
            "PLANNER_RESPONSE" => {
                if let Some(idx) = step.step_index {
                    self.update_current_model(idx);
                }
                if let Some(tokens) = Tokens::from_borrowed(
                    step.input_tokens,
                    step.output_tokens,
                    step.cache_read_tokens,
                ) {
                    self.apply_tokens(tokens);
                }
                self.pending_calls.clear();
                for call in step.tool_calls.into_iter().flatten() {
                    let args = if matches!(
                        call.name.as_str(),
                        "read_file" | "write_file" | "edit_file" | "run_command"
                    ) {
                        call.args
                            .and_then(|raw| serde_json::from_str::<Value>(raw.get()).ok())
                            .unwrap_or(Value::Null)
                    } else {
                        Value::Null
                    };
                    self.pending_calls.push_back(PendingCall {
                        name: call.name,
                        args,
                    });
                }
            }
            "CHECKPOINT" => {
                if let Some(tokens) = Tokens::from_borrowed(
                    step.input_tokens,
                    step.output_tokens,
                    step.cache_read_tokens,
                ) {
                    self.apply_compaction_tokens(tokens);
                }
            }
            "GENERIC" => {
                if let Some(call) = self.pending_calls.pop_front() {
                    self.apply_result_fields(
                        &call,
                        step.status.as_deref(),
                        step.content,
                        timestamp,
                    );
                }
            }
            "USER_INPUT" | "SYSTEM_MESSAGE" => {}
            _ => {
                self.diagnostics.record_unrecognized();
                return;
            }
        }
        self.diagnostics.record_recognized_source();
    }

    fn update_current_model(&mut self, step: i64) {
        if let Some(model) = self.meta.step_models.get(&step)
            && self.current_model.as_deref() != Some(model.as_str())
        {
            self.current_model = Some(model.clone());
        }
    }

    fn apply_tokens(&mut self, tokens: Tokens) {
        self.diagnostics.record_relevant(true);
        let model = self.current_model.as_deref().unwrap_or(UNKNOWN_MODEL);
        record_model_tokens(&mut self.usage, self.classifier.as_mut(), model, tokens);
    }

    fn apply_compaction_tokens(&mut self, tokens: Tokens) {
        self.diagnostics.record_relevant(true);
        let model = self.current_model.as_deref().unwrap_or(UNKNOWN_MODEL);
        record_model_tokens(
            &mut self.compaction,
            self.classifier.as_mut(),
            model,
            tokens,
        );
    }

    fn apply_result(&mut self, call: &PendingCall, result: &Map<String, Value>, timestamp: i64) {
        self.apply_result_inner(
            call,
            result.get("status").and_then(Value::as_str),
            || {
                result
                    .get("content")
                    .and_then(Value::as_str)
                    .map(Cow::Borrowed)
            },
            timestamp,
        );
    }

    fn apply_result_fields(
        &mut self,
        call: &PendingCall,
        status: Option<&str>,
        content_raw: Option<&RawValue>,
        timestamp: i64,
    ) {
        self.apply_result_inner(
            call,
            status,
            || content_raw.and_then(|raw| serde_json::from_str::<Cow<'_, str>>(raw.get()).ok()),
            timestamp,
        );
    }

    fn apply_result_inner<'c>(
        &mut self,
        call: &PendingCall,
        status: Option<&str>,
        read_content: impl FnOnce() -> Option<Cow<'c, str>>,
        timestamp: i64,
    ) {
        if !matches!(
            call.name.as_str(),
            "read_file" | "write_file" | "edit_file" | "run_command"
        ) {
            return;
        }
        // A command moved to a background task has already started running.
        let ran =
            status == Some("DONE") || (status == Some("RUNNING") && call.name == "run_command");
        if !ran {
            self.diagnostics.record_relevant(status == Some("ERROR"));
            return;
        }
        let arg = |key: &str| call.args.get(key).and_then(Value::as_str);
        let applied = match call.name.as_str() {
            "read_file" => arg("Path")
                .filter(|path| !path.is_empty())
                .is_some_and(|path| {
                    if matches!(self.state.mode, ParseMode::Full) {
                        let content = read_content()
                            .as_deref()
                            .map(numbered_lines)
                            .unwrap_or_default();
                        if content.is_empty() {
                            self.state.tool_counts.read += 1;
                            self.state.add_non_text_read_path(path);
                        } else {
                            self.state.add_read_detail(path, &content, timestamp);
                        }
                    } else {
                        let mut total_lines = 0usize;
                        let mut non_empty_lines = 0usize;
                        let mut chars_so_far = 0usize;
                        let mut chars_up_to_last_non_empty = 0usize;
                        if let Some(raw) = read_content() {
                            for stripped in raw.split('\n').filter_map(strip_numbered_line) {
                                if total_lines > 0 {
                                    chars_so_far += 1;
                                }
                                chars_so_far += bytecount::num_chars(stripped.as_bytes());
                                total_lines += 1;
                                if !stripped.is_empty() {
                                    non_empty_lines = total_lines;
                                    chars_up_to_last_non_empty = chars_so_far;
                                }
                            }
                        }
                        if non_empty_lines > 0 {
                            let resolved = self.state.normalize_path(path);
                            if !resolved.is_empty() {
                                self.state.unique_files.insert(resolved);
                                self.state.total_read_lines += non_empty_lines;
                                self.state.total_read_characters += chars_up_to_last_non_empty;
                                self.state.tool_counts.read += 1;
                            }
                        } else if total_lines <= 1 {
                            self.state.tool_counts.read += 1;
                            self.state.add_non_text_read_path(path);
                        }
                    }
                    true
                }),
            "write_file" => match (arg("Path"), arg("Content")) {
                (Some(path), Some(content)) => {
                    self.state.add_write_detail(path, content, timestamp);
                    true
                }
                _ => false,
            },
            "edit_file" => match (arg("Path"), arg("Target"), arg("Replacement")) {
                (Some(path), Some(old), Some(new)) => {
                    self.state.add_edit_detail(path, old, new, timestamp);
                    true
                }
                _ => false,
            },
            _ => match arg("CommandLine").filter(|command| !command.trim().is_empty()) {
                Some(command) => {
                    let description = arg("toolSummary").unwrap_or_default();
                    self.state.add_run_command(command, description, timestamp);
                    true
                }
                None => false,
            },
        };
        self.diagnostics.record_relevant(applied);
    }

    fn finish(self) -> ParsedAnalysis {
        let conversation_usage = self
            .usage
            .into_iter()
            .map(|(model, usage)| (model, usage_value(&usage)))
            .collect();
        let mut record = self.state.into_record(conversation_usage);
        // Route compaction usage to `advisor_usage` because models in `conversation_usage` are credited with the session's tool counts.
        for (model, usage) in self.compaction {
            record.advisor_usage.insert(model, usage_value(&usage));
        }

        ParsedAnalysis::new(
            CodeAnalysis {
                user: String::new(),
                extension_name: String::new(),
                insights_version: String::new(),
                machine_id: String::new(),
                records: vec![record],
            },
            self.diagnostics,
        )
    }
}

fn record_model_tokens(
    target: &mut FastHashMap<String, ModelUsage>,
    classifier: Option<&mut TierClassifier<'_>>,
    model: &str,
    tokens: Tokens,
) {
    let level = classifier.map_or(0, |classifier| {
        classifier.level(model, tokens.request_context())
    });
    let entry = if let Some(entry) = target.get_mut(model) {
        entry
    } else {
        target.entry(model.to_owned()).or_default()
    };
    entry.total.merge(tokens);
    entry.classify(level, tokens);
}

fn usage_value(usage: &ModelUsage) -> Value {
    let mut value = json!({
        "input_tokens": usage.total.input,
        "output_tokens": usage.total.output,
        "cache_read_input_tokens": usage.total.cache_read,
    });
    let mut above = Map::new();
    for (index, slice) in usage.above_tiers.iter().enumerate() {
        write_tier_slice(
            &mut above,
            index + 1,
            &TierSlice {
                input_tokens: slice.input,
                output_tokens: slice.output,
                reasoning_tokens: 0,
                cache_read: slice.cache_read,
                cache_creation_5m: 0,
                cache_creation_1h: 0,
            },
        );
    }
    if !above.is_empty() {
        value["above_tier"] = Value::Object(above);
    }
    value
}

/// Strips `<line>: ` from a single `read_file` line, returning `None` for non-numbered header lines.
fn strip_numbered_line(line: &str) -> Option<&str> {
    let digits = line.find(|c: char| !c.is_ascii_digit())?;
    let rest = line[digits..].strip_prefix(':')?;
    (digits > 0).then(|| rest.strip_prefix(' ').unwrap_or(rest))
}

/// Returns the file text from a `read_file` result by stripping the header and `<line>: ` prefixes.
fn numbered_lines(content: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for line in content.split('\n').filter_map(strip_numbered_line) {
        if !first {
            out.push('\n');
        }
        first = false;
        out.push_str(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn varint(mut value: u64, out: &mut Vec<u8>) {
        while value >= 0x80 {
            out.push((value as u8) | 0x80);
            value >>= 7;
        }
        out.push(value as u8);
    }

    fn bytes(field: u64, value: &[u8], out: &mut Vec<u8>) {
        varint((field << 3) | 2, out);
        varint(value.len() as u64, out);
        out.extend_from_slice(value);
    }

    fn generation(model: &str, last_step: Option<i64>) -> Vec<u8> {
        let mut inner = Vec::new();
        varint(3 << 3, &mut inner);
        varint(1427, &mut inner);
        bytes(19, model.as_bytes(), &mut inner);
        if let Some(last_step) = last_step {
            let mut pair = Vec::new();
            bytes(1, b"last_step_index", &mut pair);
            bytes(2, last_step.to_string().as_bytes(), &mut pair);
            bytes(20, &pair, &mut inner);
        }
        let mut row = Vec::new();
        bytes(1, &inner, &mut row);
        row
    }

    /// Creates a fake transcript under `<root>/brain/conv/` and an optional sibling `conv.db`.
    fn conversation(
        root: &Path,
        lines: &[Value],
        generations: Option<&[Vec<u8>]>,
    ) -> std::path::PathBuf {
        let logs = root.join("brain/conv/.system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let transcript = logs.join("transcript_full.jsonl");
        let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(&transcript, body).unwrap();
        if let Some(generations) = generations {
            std::fs::create_dir_all(root.join("conversations")).unwrap();
            let conn = Connection::open(root.join("conversations/conv.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE gen_metadata (idx integer PRIMARY KEY, data blob, size integer);",
            )
            .unwrap();
            for (idx, data) in generations.iter().enumerate() {
                conn.execute(
                    "INSERT INTO gen_metadata (idx, data, size) VALUES (?1, ?2, 0)",
                    rusqlite::params![idx as i64, data],
                )
                .unwrap();
            }
        }
        transcript
    }

    fn planner(step: i64, input: i64, cache_read: i64, output: i64) -> Value {
        json!({
            "step_index": step, "source": "MODEL", "type": "PLANNER_RESPONSE",
            "status": "DONE", "created_at": "2026-11-11T12:00:00Z",
            "input_tokens": input, "cache_read_tokens": cache_read, "output_tokens": output
        })
    }

    #[test]
    fn each_request_is_billed_to_the_model_of_its_generation() {
        let temp = tempfile::tempdir().unwrap();
        let mut checkpoint = planner(4, 50, 60, 7);
        checkpoint["type"] = json!("CHECKPOINT");
        checkpoint["source"] = json!("SYSTEM");
        let path = conversation(
            temp.path(),
            &[
                planner(1, 100, 0, 10),
                planner(3, 20, 100, 5),
                checkpoint,
                planner(6, 1, 300, 2),
            ],
            Some(&[
                generation("gemini-a", Some(0)),
                // No step index: its request keeps the model before it.
                generation("gemini-a", None),
                generation("gemini-b", Some(5)),
            ]),
        );

        let parsed = parse_antigravity_session(&path, ParseMode::UsageOnly, None).unwrap();
        let usage = &parsed.analysis.records[0].conversation_usage;
        assert_eq!(usage["gemini-a"]["input_tokens"], 120);
        assert_eq!(usage["gemini-a"]["cache_read_input_tokens"], 100);
        assert_eq!(usage["gemini-a"]["output_tokens"], 15);
        assert_eq!(usage["gemini-b"]["cache_read_input_tokens"], 300);
        assert!(!usage.contains_key(UNKNOWN_MODEL));
        // The compaction ran while gemini-a was in use.
        let advisor = &parsed.analysis.records[0].advisor_usage;
        assert_eq!(advisor["gemini-a"]["input_tokens"], 50);
        assert!(!advisor.contains_key(UNKNOWN_MODEL));
        assert_eq!(parsed.analysis.records[0].task_id, "conv");
    }

    #[test]
    fn a_conversation_without_its_database_bills_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let mut checkpoint = planner(2, 50, 60, 7);
        checkpoint["type"] = json!("CHECKPOINT");
        let path = conversation(temp.path(), &[planner(1, 100, 0, 10), checkpoint], None);

        let parsed = parse_antigravity_session(&path, ParseMode::UsageOnly, None).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(
            record.conversation_usage[UNKNOWN_MODEL]["input_tokens"],
            100
        );
        assert_eq!(record.advisor_usage[UNKNOWN_MODEL]["input_tokens"], 50);
    }

    #[test]
    fn a_model_id_with_a_backend_is_named_by_its_model_not_its_backend() {
        assert_eq!(
            model_name("opus-5.5-high|claude-opus-5-5@default"),
            "claude-opus-5-5"
        );
        assert_eq!(
            model_name("gemini-3.5-pro-high|backend-pool/serving/checkpoints/1"),
            "gemini-3.5-pro-high"
        );
        assert_eq!(
            model_name("gemini-3.5-pro-high|backend-pool"),
            "gemini-3.5-pro-high"
        );
        assert_eq!(model_name("gemini-3.5-flash"), "gemini-3.5-flash");
    }

    #[test]
    fn a_request_lands_in_the_price_level_its_own_context_reached() {
        let temp = tempfile::tempdir().unwrap();
        let path = conversation(
            temp.path(),
            &[planner(1, 100, 0, 1), planner(3, 100, 300_000, 2)],
            Some(&[generation("gemini-a", Some(0))]),
        );
        let tiers = TierThresholds::from_entries([("gemini-a", vec![200_000])].into_iter());

        let parsed = parse_antigravity_session(&path, ParseMode::UsageOnly, Some(&tiers)).unwrap();
        let usage = &parsed.analysis.records[0].conversation_usage["gemini-a"];
        assert_eq!(usage["cache_read_input_tokens"], 300_000);
        assert_eq!(usage["above_tier"]["level_1_cache_read_tokens"], 300_000);
        assert_eq!(usage["above_tier"]["level_1_output_tokens"], 2);
    }

    #[test]
    fn a_compaction_lands_in_the_price_level_its_own_context_reached() {
        let temp = tempfile::tempdir().unwrap();
        let mut checkpoint = planner(2, 100, 300_000, 5);
        checkpoint["type"] = json!("CHECKPOINT");
        let path = conversation(
            temp.path(),
            &[planner(1, 100, 0, 1), checkpoint],
            Some(&[generation("gemini-a", Some(0))]),
        );
        let tiers = TierThresholds::from_entries([("gemini-a", vec![200_000])].into_iter());

        let parsed = parse_antigravity_session(&path, ParseMode::UsageOnly, Some(&tiers)).unwrap();
        let usage = &parsed.analysis.records[0].advisor_usage["gemini-a"];
        assert_eq!(usage["cache_read_input_tokens"], 300_000);
        assert_eq!(usage["above_tier"]["level_1_cache_read_tokens"], 300_000);
        assert_eq!(usage["above_tier"]["level_1_output_tokens"], 5);
    }

    #[test]
    fn tool_results_pair_in_order_and_only_completed_ones_count() {
        let temp = tempfile::tempdir().unwrap();
        let mut step = planner(1, 1, 0, 1);
        step["tool_calls"] = json!([
            {"name": "read_file", "args": {"Path": "/w/a.rs"}},
            {"name": "edit_file", "args": {"Path": "/w/a.rs", "Target": "x", "Replacement": "y\nz"}},
            {"name": "write_file", "args": {"Path": "/w/b.rs", "Content": "1\n2\n3\n", "Overwrite": false}},
            {"name": "run_command", "args": {"CommandLine": "cargo test", "toolSummary": "Run tests"}},
            {"name": "search_web", "args": {"query": "q"}}
        ]);
        let result = |status: &str, content: &str| {
            json!({"step_index": 2, "source": "MODEL", "type": "GENERIC", "status": status,
                   "created_at": "2026-11-11T12:00:01Z", "content": content})
        };
        let path = conversation(
            temp.path(),
            &[
                step,
                result(
                    "DONE",
                    "Created At: x\nShowing lines 1-2 of 2\n1: fn a() {}\n2: // 3: not a prefix",
                ),
                result("ERROR", "Encountered error in step execution"),
                result("DONE", "Created file."),
                result("RUNNING", "Tool is running as a background task"),
                result("DONE", "summary"),
            ],
            None,
        );

        let parsed = parse_antigravity_session(&path, ParseMode::Full, None).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_read_lines, 2);
        assert_eq!(record.tool_call_counts.edit, 0);
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.total_write_lines, 3);
        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
    }

    #[test]
    fn an_unknown_step_type_is_drift() {
        let temp = tempfile::tempdir().unwrap();
        let path = conversation(
            temp.path(),
            &[
                planner(1, 1, 0, 1),
                json!({"step_index": 2, "source": "MODEL", "type": "HOLOGRAM", "status": "DONE"}),
            ],
            None,
        );

        let parsed = parse_antigravity_session(&path, ParseMode::UsageOnly, None).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
    }

    #[test]
    fn read_file_usage_only_matches_full_mode_across_edge_cases() {
        let samples = [
            "Showing lines 1-3 of 3\n1: \n2: 你好🚀\n3: ",
            "1: ",
            "1: \n2: ",
            "Binary file (no numbered lines)",
            "1: first\n2: second\n3: third",
        ];
        for (idx, content) in samples.iter().enumerate() {
            let temp = tempfile::tempdir().unwrap();
            let mut step = planner(1, 10, 0, 5);
            step["tool_calls"] = json!([
                {"name": "read_file", "args": {"Path": "/w/sample.rs"}}
            ]);
            let result = json!({
                "step_index": 2,
                "source": "MODEL",
                "type": "GENERIC",
                "status": "DONE",
                "created_at": "2026-11-11T12:00:01Z",
                "content": content,
            });
            let path = conversation(temp.path(), &[step, result], None);
            let full = parse_antigravity_session(&path, ParseMode::Full, None).unwrap();
            let usage_only = parse_antigravity_session(&path, ParseMode::UsageOnly, None).unwrap();
            let full_rec = &full.analysis.records[0];
            let usage_rec = &usage_only.analysis.records[0];
            assert_eq!(
                usage_rec.tool_call_counts.read, full_rec.tool_call_counts.read,
                "sample {idx} read count"
            );
            assert_eq!(
                usage_rec.total_read_lines, full_rec.total_read_lines,
                "sample {idx} read lines"
            );
            assert_eq!(
                usage_rec.total_read_characters, full_rec.total_read_characters,
                "sample {idx} read chars"
            );
            assert_eq!(
                usage_rec.total_unique_files, full_rec.total_unique_files,
                "sample {idx} unique files"
            );
        }
    }
}
