//! Antigravity conversation transcript parser.
//!
//! Antigravity CLI (`agy`), Antigravity Desktop and Jetski keep one directory
//! per conversation under `<root>/brain/<conversation-id>/`, whose
//! `.system_generated/logs/transcript_full.jsonl` is the conversation's step
//! log. The model that served each generation is not in that log: it lives in
//! the sibling `<root>/conversations/<conversation-id>.db`, whose
//! `gen_metadata` rows are protobuf messages naming the model and the step the
//! generation answered.

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
use rusqlite::OptionalExtension;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// The model name given to tokens no generation record attributes.
const UNKNOWN_MODEL: &str = "unknown";

/// One request's tokens, in vct's disjoint buckets. The transcript reports no
/// reasoning or cache-write count.
#[derive(Debug, Clone, Copy, Default)]
struct Tokens {
    input: i64,
    output: i64,
    cache_read: i64,
}

impl Tokens {
    /// Reads a step's token fields; `None` when it carries none.
    ///
    /// `input_tokens` already excludes cache reads, so it maps straight onto
    /// the non-cached input bucket.
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

    fn merge(&mut self, other: Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
    }

    /// The prompt size one request is billed against.
    fn request_context(self) -> i64 {
        self.input + self.cache_read
    }
}

/// Per-model running totals plus the per-price-level slices of them.
///
/// `above_tiers[n]` is the slice classified into price level `n + 1`.
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

/// What the conversation database says about the transcript beside it.
#[derive(Debug, Default)]
struct ConversationMeta {
    /// The model of each generation that records the step it answered, keyed
    /// by that step's index.
    step_models: HashMap<i64, String>,
    /// The first generation's model, for steps before any keyed generation.
    first_model: Option<String>,
    workspace: Option<String>,
    git_remote: Option<String>,
}

/// Returns `<root>/conversations/<id>.db` for a transcript at
/// `<root>/brain/<id>/.system_generated/logs/<file>`.
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
    for (index, line) in BufReader::with_capacity(buffer::FILE_READ_BUFFER, file)
        .lines()
        .enumerate()
    {
        let line = line.with_context(|| {
            format!(
                "Failed to read Antigravity transcript line {}: {}",
                index + 1,
                path.display()
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(record) => parser.push(&record),
            Err(_) => parser.diagnostics.record_malformed(),
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
            let rows = statement.query_map([], |row| row.get::<_, Option<Vec<u8>>>(0))?;
            for row in rows {
                let Some((step, model)) = row?.as_deref().and_then(decode_generation) else {
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
                let blob: Option<Option<Vec<u8>>> = conn
                    .query_row(
                        "SELECT data FROM trajectory_metadata_blob WHERE id = 'main'",
                        [],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(trajectory) = blob.flatten() {
                    decode_trajectory(&trajectory, &mut meta);
                }
            }
            Ok(meta)
        },
    )
}

/// Reads one `gen_metadata` row: field 1 holds the model id (field 19) and
/// string key/value pairs (field 20), one of which is the index of the step the
/// generation followed. Returns the step it produced and its model.
fn decode_generation(data: &[u8]) -> Option<(Option<i64>, String)> {
    let generation = bytes_field(data, 1)?;
    let mut model = None;
    let mut step = None;
    for (field, value) in ProtoFields(generation, 0) {
        match (field, value) {
            (19, ProtoValue::Bytes(bytes)) => {
                model = std::str::from_utf8(bytes).ok().map(str::to_string);
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

/// Reads the trajectory blob: field 1 holds the workspace URI (field 1) and
/// the repository (field 3, whose field 2 is the remote URL).
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

/// One protobuf field value: a varint, or the bytes of a length-delimited one.
enum ProtoValue<'a> {
    Varint,
    Bytes(&'a [u8]),
}

/// Iterates a protobuf message's top-level fields, stopping at the first
/// malformed one.
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

/// The first length-delimited `field` of a message.
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

struct AntigravityParser<'a> {
    state: SessionParseState,
    usage: FastHashMap<String, ModelUsage>,
    /// Compaction requests' tokens per model. No generation record names a
    /// compaction's model, so each is billed to the model in use when it ran.
    compaction: FastHashMap<String, ModelUsage>,
    diagnostics: ParseDiagnostics,
    classifier: Option<TierClassifier<'a>>,
    meta: ConversationMeta,
    /// The model of the latest keyed generation, carried to steps whose own
    /// generation records no step index.
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
                if let Some(step) = object.get("step_index").and_then(Value::as_i64)
                    && let Some(model) = self.meta.step_models.get(&step)
                {
                    self.current_model = Some(model.clone());
                }
                let model = self
                    .current_model
                    .clone()
                    .unwrap_or_else(|| UNKNOWN_MODEL.to_string());
                self.apply_usage(object, model);
                // Results never outlive their step, so a step that wrote fewer
                // than it called cannot shift the pairing of the next one.
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
            // A compaction summary is a billed request, but no generation
            // record names its model, so it goes to the model in use.
            "CHECKPOINT" => {
                if let Some(tokens) = Tokens::from_record(object) {
                    self.diagnostics.record_relevant(true);
                    let model = self
                        .current_model
                        .clone()
                        .unwrap_or_else(|| UNKNOWN_MODEL.to_string());
                    let level = self.classifier.as_mut().map_or(0, |classifier| {
                        classifier.level(&model, tokens.request_context())
                    });
                    let entry = self.compaction.entry(model).or_default();
                    entry.total.merge(tokens);
                    entry.classify(level, tokens);
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

    fn apply_usage(&mut self, object: &Map<String, Value>, model: String) {
        let Some(tokens) = Tokens::from_record(object) else {
            return;
        };
        self.diagnostics.record_relevant(true);
        let level = self.classifier.as_mut().map_or(0, |classifier| {
            classifier.level(&model, tokens.request_context())
        });
        let entry = self.usage.entry(model).or_default();
        entry.total.merge(tokens);
        entry.classify(level, tokens);
    }

    fn apply_result(&mut self, call: &PendingCall, result: &Map<String, Value>, timestamp: i64) {
        if !matches!(
            call.name.as_str(),
            "read_file" | "write_file" | "edit_file" | "run_command"
        ) {
            return;
        }
        let status = result.get("status").and_then(Value::as_str);
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
                    let content = result
                        .get("content")
                        .and_then(Value::as_str)
                        .map(numbered_lines)
                        .unwrap_or_default();
                    if content.is_empty() {
                        self.state.tool_counts.read += 1;
                        self.state.add_non_text_read_path(path);
                    } else {
                        self.state.add_read_detail(path, &content, timestamp);
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
            .iter()
            .map(|(model, usage)| (model.clone(), usage_value(usage)))
            .collect();
        let mut record = self.state.into_record(conversation_usage);
        // Every model in `conversation_usage` is credited with the session's
        // tool counts, and a compaction request runs no tools.
        for (model, usage) in &self.compaction {
            record
                .advisor_usage
                .insert(model.clone(), usage_value(usage));
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

/// The file text inside a `read_file` result, whose lines are shown as
/// `<line number>: <text>` beneath a header.
fn numbered_lines(content: &str) -> String {
    content
        .split('\n')
        .filter_map(|line| {
            let digits = line.find(|c: char| !c.is_ascii_digit())?;
            let rest = line[digits..].strip_prefix(':')?;
            (digits > 0).then(|| rest.strip_prefix(' ').unwrap_or(rest))
        })
        .collect::<Vec<_>>()
        .join("\n")
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

    /// Lays out `<root>/brain/conv/.system_generated/logs/transcript_full.jsonl`
    /// and, when `generations` is given, `<root>/conversations/conv.db`.
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
}
