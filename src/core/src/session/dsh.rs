//! DeepSeek Harness (`dsh`) session-log parser.
//!
//! `dsh` writes one append-only JSONL log per session. By default the file is a
//! concatenation of independent zstd frames — one holding the header line, then
//! one per durable append batch — so a live session's log can end inside a
//! structurally incomplete frame. The reader keeps every record decoded before
//! that point rather than rejecting the whole log, and it also accepts the
//! uncompressed `session.jsonl` a root configured with `compression: 'none'`
//! writes.

use crate::constants::{FastHashMap, buffer};
use crate::models::CodeAnalysis;
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::state::{ParseMode, SessionParseState};
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Magic bytes opening every zstd frame (`0xFD2FB528`, little-endian).
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// The only `SESSION_FORMAT_VERSION` upstream reads or writes. A log stamped
/// with anything else was produced by a harness whose layout this parser has
/// never seen, and upstream ships no migration for it either.
const SUPPORTED_FORMAT_VERSION: i64 = 0;

/// Event types this build knows about but has nothing to extract from.
///
/// Kept explicit so a type introduced by a future harness is reported as drift
/// instead of being silently skipped. Mirrors upstream's
/// `KNOWN_SESSION_EVENT_TYPES`, minus the types handled in [`DshParser::push`],
/// plus the three packed chunk-row tags (deliberately slash-less upstream, so
/// they can never collide with an event type).
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
    // Sub-dispatches of a `run_code` program. The enclosing `run_code`
    // `tool/call` already counts, so counting these would count twice.
    "tool/code-dispatch",
    "tool/code-dispatch-start",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
];

/// One assistant step's token usage, held until the next step supersedes it.
///
/// Upstream reports the same `(turn, step)` usage twice — once as an
/// `assistant/chunk` early sample that survives a later request failure, then
/// again on the assembled `assistant/message`. Its own fold is last-wins per
/// step for exactly that reason, and summing both would double every bucket.
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
    /// Reads one `TokenUsage` payload.
    ///
    /// `inputTokens` already excludes cache reads (the adapters subtract them
    /// out), so it maps straight onto the non-cached input bucket. Reasoning is
    /// the one overlapping field: upstream follows the OpenAI convention where
    /// `outputTokens` already includes `reasoningTokens`, so it is subtracted
    /// back out here and billed once from its own bucket.
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

    /// The prompt size one request is billed against, which is what a model's
    /// `*_above_Nk_tokens` context tier is compared with.
    fn request_context(self) -> i64 {
        self.input + self.cache_read + self.cache_write
    }

    fn is_empty(self) -> bool {
        self.request_context() == 0 && self.output == 0 && self.reasoning == 0
    }
}

/// Per-model running totals plus the above-tier slice of them.
#[derive(Debug, Default)]
struct ModelUsage {
    total: DshTokens,
    above_tier: DshTokens,
}

/// A `tool/call` waiting for the `tool/result` that reports its outcome.
#[derive(Debug)]
struct PendingCall {
    name: String,
    /// Raw JSON string exactly as the model produced it, so it is parsed
    /// defensively and only once the paired result reports success.
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

    // A foreign format version means the envelope's own semantics may have
    // moved, so every number derived from it would be a guess. Upstream ships
    // no migration either and refuses the log outright.
    if let Some(version) = parser.unsupported_version {
        bail!(
            "DeepSeek session {} uses format version {version}, but only version {SUPPORTED_FORMAT_VERSION} is supported",
            path.display()
        );
    }

    // A live session's log can end inside a half-written frame, so a read that
    // fails after some records decoded is expected and keeps what it got. One
    // that fails before any record did is a genuinely unreadable file.
    if let Some(error) = truncated {
        if parser.diagnostics.source_records == 0 {
            return Err(error)
                .with_context(|| format!("Failed to read DeepSeek session: {}", path.display()));
        }
        parser.diagnostics.record_malformed();
    }

    Ok(parser.finish())
}

/// Returns whether `path` opens with the zstd frame magic.
///
/// The single-file entry point uses this to route a compressed log to this
/// parser before anything tries to read it as UTF-8 text. An unreadable or
/// too-short file is simply not zstd as far as this check is concerned.
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

struct DshParser<'a> {
    state: SessionParseState,
    usage: FastHashMap<String, ModelUsage>,
    diagnostics: ParseDiagnostics,
    classifier: Option<TierClassifier<'a>>,
    /// Model from the latest `request/context`, which upstream logs only when
    /// the route changes, so it has to be carried forward as parser state.
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
    fn read_lines<R: BufRead>(&mut self, reader: R) -> Option<std::io::Error> {
        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(error) => return Some(error),
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&line) {
                Ok(record) => self.push(&record),
                Err(_) => self.diagnostics.record_malformed(),
            }
        }
        None
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
        // `cwd` is absent for a session started outside a workspace, and the
        // `--<normalized-cwd>--` directory name is a deliberately lossy slug
        // upstream, so there is nothing to fall back to.
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
        // The step's identity is on the event, while the usage payload sits
        // inside the chunk.
        let model = self.route_model.clone();
        self.apply_usage(data, data.pointer("/chunk/usage"), model);
    }

    /// Holds one step's usage sample, flushing the previous step's.
    fn apply_usage(&mut self, data: &Value, usage: Option<&Value>, model: String) {
        // An assistant message carries no `usage` when the adapter reported
        // none; that is a routing fact, not a schema failure.
        let Some(usage) = usage else {
            return;
        };
        let Some(usage) = usage.as_object() else {
            self.diagnostics.record_relevant(false);
            return;
        };
        self.diagnostics.record_relevant(true);

        let sample = UsageSample {
            turn: data.get("turn").and_then(Value::as_i64).unwrap_or(0),
            step: data.get("step").and_then(Value::as_i64).unwrap_or(0),
            model,
            tokens: DshTokens::from_usage(usage),
        };
        match &self.pending_usage {
            Some(held) if held.turn == sample.turn && held.step == sample.step => {}
            _ => self.flush_usage(),
        }
        self.pending_usage = Some(sample);
    }

    fn flush_usage(&mut self) {
        let Some(sample) = self.pending_usage.take() else {
            return;
        };
        if sample.model.is_empty() {
            return;
        }
        let above_tier = self.classifier.as_mut().is_some_and(|classifier| {
            classifier.is_above(&sample.model, sample.tokens.request_context())
        });
        let entry = self.usage.entry(sample.model).or_default();
        entry.total.merge(sample.tokens);
        if above_tier {
            entry.above_tier.merge(sample.tokens);
        }
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

    /// Applies a completed tool call's file operations.
    ///
    /// A result is a surface event whose `sourceEventSeqs` points back at its
    /// `tool/call`, which is what carries the operation's own arguments. A
    /// result reporting `error` leaves no metric behind.
    fn apply_tool_result(&mut self, object: &Map<String, Value>, data: &Value) {
        let call_seq = object
            .get("sourceEventSeqs")
            .and_then(Value::as_array)
            .and_then(|seqs| seqs.first())
            .and_then(Value::as_i64);
        // A call whose own record was lost with a torn frame cannot be
        // identified, and the result payload alone does not say which tool
        // produced it.
        let Some(call) = call_seq.and_then(|seq| self.pending_calls.remove(&seq)) else {
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
            // The tool set is composable per agent preset and plugins register
            // their own, so an unmapped name is normal operation.
            _ => return,
        };
        self.diagnostics.record_relevant(applied);
    }

    fn apply_read(&mut self, arguments: &Value, meta: &Value, timestamp: i64) -> bool {
        // The result meta carries the resolved absolute path; the call's own
        // `file_path` may be relative to the session's cwd.
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
        // The result's diff meta is empty for a newly created file and pads
        // every other hunk with three context lines on each side, so the call's
        // own content is the only exact record of what was written.
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
            if !usage.above_tier.is_empty() {
                value["above_tier"] = json!({
                    "input_tokens": usage.above_tier.input,
                    "output_tokens": usage.above_tier.output,
                    "reasoning_tokens": usage.above_tier.reasoning,
                    "cache_read_tokens": usage.above_tier.cache_read,
                    "cache_creation_5m_tokens": usage.above_tier.cache_write,
                });
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

    /// Compresses each group of lines into its own zstd frame and concatenates
    /// them, which is exactly how upstream writes one frame per append batch.
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
        // A live session's log can end mid-frame, which is a normal state, not
        // corruption: the harness truncates back to the torn frame on its next
        // open.
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
        // Opens with the frame magic, so it is routed here, but decodes to
        // nothing — a genuinely unreadable file rather than an empty session.
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
    fn an_empty_log_is_a_valid_blank_session() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("session.jsonl.zstd");
        std::fs::write(&path, []).unwrap();

        let parsed = parse(&path);
        assert!(!parsed.diagnostics.is_complete_failure());
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }
}
