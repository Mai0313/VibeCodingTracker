use crate::VERSION;
use crate::constants::buffer;
use crate::models::{
    ClaudeCodeLog, CodeAnalysis, CodexLog, CopilotEvent, ExtensionType, GeminiSession,
};
use crate::session::claude::parse_claude_logs_with_diagnostics;
use crate::session::codex::parse_codex_log_iter_with_diagnostics;
use crate::session::copilot::parse_copilot_events_with_diagnostics;
use crate::session::detector::{classify_records, detect_extension_type};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::gemini::parse_gemini_events_with_diagnostics;
use crate::session::grok::{is_grok_signals, parse_grok_session};
use crate::session::state::ParseMode;
use crate::utils::{get_current_user, get_machine_id, read_json, read_jsonl};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::cell::RefCell;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::rc::Rc;

/// Content-safe warning summary used by the CLI's single-file path.
///
/// This type is public only because Cargo builds `src/main.rs` as a separate
/// crate from the library. Provider diagnostics remain crate-private.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionFileParseDiagnostics {
    skipped_records: usize,
}

impl SessionFileParseDiagnostics {
    /// Number of malformed, unrecognized, or analyzer-relevant records skipped
    /// after another record from the same source was recognized successfully.
    pub fn skipped_records(self) -> usize {
        self.skipped_records
    }
}

/// Parses a session file (JSONL or JSON) and returns the result as a
/// `serde_json::Value` (the CLI single-file dump path).
///
/// Internally this is a thin wrapper over [`parse_session_file_typed`]; the
/// conversion to `Value` happens once at the edge here rather than inside the
/// cache, which keeps long sessions from being duplicated between typed and
/// `Value` forms when multiple commands run against the same file.
///
/// An empty input yields `{}` (not a populated-but-empty `CodeAnalysis`), to
/// preserve historical CLI behaviour.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or read, if a nonempty source
/// has no supported analyzer payload, or if the parsed [`crate::CodeAnalysis`]
/// fails to serialise to a `serde_json::Value`.
///
/// # Examples
///
/// ```no_run
/// use vibe_coding_tracker::parse_session_file;
///
/// let value = parse_session_file("session.jsonl")?;
/// assert!(value.is_object());
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn parse_session_file<P: AsRef<Path>>(path: P) -> Result<Value> {
    let analysis = parse_session_file_typed(path)?;
    if analysis.records.is_empty() && analysis.extension_name.is_empty() {
        // Preserve historical behaviour: empty input → `{}` rather than a
        // fully-populated but empty `CodeAnalysis` object.
        return Ok(serde_json::json!({}));
    }
    Ok(serde_json::to_value(&analysis)?)
}

/// Typed entry point that auto-detects the provider from file contents.
///
/// Prefer [`parse_session_file_as`] whenever the caller already knows
/// which provider the file belongs to (e.g. when walking
/// `~/.claude/projects/*.jsonl` vs `~/.codex/sessions/*.jsonl`). Content-based
/// detection is only intended for the CLI single-file path where the user
/// hands us an arbitrary path.
///
/// Parses in [`ParseMode::Full`] — for callers that only consume tool
/// counts and token usage (usage / aggregated analysis), use
/// [`parse_session_file_typed_with_mode`] with [`ParseMode::UsageOnly`]
/// to avoid allocating `write_file_details`/`edit_file_details` bodies.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or read, if no record in a
/// nonempty source has a recognized provider schema, or if every
/// analyzer-relevant payload uses an unsupported schema. Empty input resolves
/// to an empty [`CodeAnalysis`].
///
/// # Examples
///
/// ```no_run
/// use vibe_coding_tracker::parse_session_file_typed;
///
/// let analysis = parse_session_file_typed("session.jsonl")?;
/// println!("provider: {}", analysis.extension_name);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn parse_session_file_typed<P: AsRef<Path>>(path: P) -> Result<CodeAnalysis> {
    parse_session_file_typed_with_mode(path, ParseMode::Full)
}

/// Content-detecting typed parse with an explicit [`ParseMode`].
///
/// Same auto-detection as [`parse_session_file_typed`], but the caller
/// chooses whether to retain per-operation detail ([`ParseMode::Full`]) or
/// only counts and totals ([`ParseMode::UsageOnly`]). The streaming path is
/// tried first; only a file whose first line is not valid JSON (e.g. a
/// pretty-printed single-object dump) falls back to reading the whole file.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or read, if the fallback path
/// cannot detect a provider, if no record in a nonempty source has a recognized
/// provider schema, or if every analyzer-relevant payload uses an unsupported
/// schema. Empty input resolves to an empty [`CodeAnalysis`].
///
/// # Examples
///
/// ```no_run
/// use vibe_coding_tracker::session::parse_session_file_typed_with_mode;
/// use vibe_coding_tracker::session::ParseMode;
///
/// let analysis =
///     parse_session_file_typed_with_mode("session.jsonl", ParseMode::UsageOnly)?;
/// // UsageOnly skips per-file detail bodies; counts still populate.
/// assert!(analysis.records.iter().all(|r| r.write_file_details.is_empty()));
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn parse_session_file_typed_with_mode<P: AsRef<Path>>(
    path: P,
    mode: ParseMode,
) -> Result<CodeAnalysis> {
    Ok(parse_session_file_typed_with_mode_and_diagnostics(path, mode)?.0)
}

/// Single-file parse with a content-safe partial-failure summary for the CLI.
#[doc(hidden)]
pub fn parse_session_file_typed_with_mode_and_diagnostics<P: AsRef<Path>>(
    path: P,
    mode: ParseMode,
) -> Result<(CodeAnalysis, SessionFileParseDiagnostics)> {
    let path = path.as_ref();
    let parsed = parse_session_file_typed_with_mode_internal(path, mode)?;
    validate_parsed_source(path, &parsed.diagnostics)?;
    let diagnostics = SessionFileParseDiagnostics {
        skipped_records: parsed.diagnostics.partial_failure_count(),
    };
    Ok((parsed.analysis, diagnostics))
}

fn parse_session_file_typed_with_mode_internal(
    path: &Path,
    mode: ParseMode,
) -> Result<ParsedAnalysis> {
    if let Some(parsed) = stream_parse_autodetect(path, mode)? {
        return Ok(parsed);
    }

    // Fallback for anything the streaming path could not peek (e.g. a
    // hand-edited file whose first line is not valid JSON). This is also the
    // normal path for Grok's pretty-printed `signals.json` object.
    let data = match read_jsonl(path) {
        Ok(data) => data,
        Err(_) => read_json(path)?,
    };

    if data.is_empty() {
        return Ok(empty_parsed_analysis());
    }

    let ext_type = detect_extension_type(&data)?;
    dispatch_by_vec(data, ext_type, mode, path)
}

/// Typed entry point when the caller already knows the provider.
///
/// Directory scanners should use this instead of [`parse_session_file_typed`]
/// so that provider classification follows the source path — `~/.claude/projects`
/// is always [`ExtensionType::ClaudeCode`], `~/.codex/sessions` is always
/// [`ExtensionType::Codex`], and so on. This eliminates a whole class of bug
/// where a metadata sentinel record at the top of a session (`permission-mode`,
/// `file-history-snapshot`) leads the content-based detector to mis-file the
/// log under another provider and silently drop its usage totals.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or read, if no record in a
/// nonempty source has a recognized schema for `provider`, or if every
/// analyzer-relevant payload uses an unsupported schema. Empty input resolves
/// to an empty [`CodeAnalysis`].
///
/// # Examples
///
/// ```no_run
/// use vibe_coding_tracker::session::parse_session_file_as;
/// use vibe_coding_tracker::session::ParseMode;
/// use vibe_coding_tracker::ExtensionType;
///
/// // A file walked out of `~/.claude/projects` is known to be Claude Code.
/// let analysis = parse_session_file_as(
///     "session.jsonl",
///     ExtensionType::ClaudeCode,
///     ParseMode::Full,
/// )?;
/// # let _ = analysis;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn parse_session_file_as<P: AsRef<Path>>(
    path: P,
    provider: ExtensionType,
    mode: ParseMode,
) -> Result<CodeAnalysis> {
    let path = path.as_ref();
    let parsed = parse_session_file_as_with_diagnostics(path, provider, mode)?;
    validate_parsed_source(path, &parsed.diagnostics)?;
    Ok(parsed.analysis)
}

pub(crate) fn parse_session_file_as_with_diagnostics(
    path: &Path,
    provider: ExtensionType,
    mode: ParseMode,
) -> Result<ParsedAnalysis> {
    if let Some(parsed) = stream_parse_known(path, provider, mode)? {
        return Ok(parsed);
    }

    // Fallback for empty files or anything the streaming peek could not
    // parse on line one.
    let data = match read_jsonl(path) {
        Ok(data) => data,
        Err(_) => read_json(path)?,
    };

    if data.is_empty() {
        return Ok(empty_parsed_analysis());
    }

    dispatch_by_vec(data, provider, mode, path)
}

/// Streaming path when the provider is known from the caller's source.
///
/// Peeks only the first non-empty line to split a JSONL file (where each line
/// is a record) from a pretty-printed single-object JSON (which parses as a
/// multi-line block and therefore fails `from_str` on line one). No detection
/// happens here — the provider was decided by the path the file came from.
///
/// Returns `Ok(None)` for an empty file or one whose first line is not JSONL,
/// signalling the caller to use the `read_json` fallback.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or a line cannot be read, or
/// if the chosen provider's dispatch step fails (the Gemini arm requires a
/// parseable session-meta first line).
fn stream_parse_known(
    path: &Path,
    provider: ExtensionType,
    mode: ParseMode,
) -> Result<Option<ParsedAnalysis>> {
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);

    let first_line = match read_next_non_empty_line(&mut reader)? {
        Some(line) => line,
        None => return Ok(None), // empty file — caller returns the empty shape
    };

    let first_value: Value = match serde_json::from_str(first_line.trim()) {
        Ok(v) => v,
        Err(_) => return Ok(None), // not JSONL → caller falls back to read_json
    };

    let parsed = dispatch_streaming_buffered(
        provider,
        vec![first_value],
        reader,
        mode,
        ParseDiagnostics::default(),
        path,
    )?;
    Ok(Some(finalize(parsed, provider)))
}

/// Streaming path when the provider is unknown.
///
/// Reads JSONL records one line at a time and asks `classify_records` to
/// commit to a provider as soon as any of them carry a distinctive marker.
/// Because the classifier returns `None` when it has not seen a positive
/// signal yet, we simply keep peeking until one appears (or EOF). There is
/// no arbitrary upper bound on how long a Claude metadata preamble may
/// be — previously a 7+ line preamble silently mis-classified the whole
/// session as Codex because the 8-record peek window ran out before the
/// first `parentUuid` record was read.
///
/// If the entire file is consumed without any marker firing, the default
/// is Codex: Codex rollout logs usually contain one of the recognised
/// `type` values (`session_meta`, `turn_context`, …) so a synthetic file
/// with no markers is most likely a deliberately-empty Codex fixture
/// rather than a silently-broken Claude log.
///
/// Returns `Ok(None)` when the file is empty or its first line is not JSON
/// (a pretty-printed dump), signalling the caller to use the `read_json`
/// fallback.
///
/// # Errors
///
/// Returns an error if the file cannot be opened or a line cannot be read, or
/// if the resolved provider's dispatch step fails.
fn stream_parse_autodetect(path: &Path, mode: ParseMode) -> Result<Option<ParsedAnalysis>> {
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);

    let mut buffered: Vec<Value> = Vec::with_capacity(8);
    let mut first_line_was_json = None::<bool>;
    let mut ext: Option<ExtensionType> = None;
    let mut initial_diagnostics = ParseDiagnostics::default();

    loop {
        let line = match read_next_non_empty_line(&mut reader)? {
            Some(line) => line,
            None => break,
        };

        match serde_json::from_str::<Value>(line.trim()) {
            Ok(v) => {
                first_line_was_json.get_or_insert(true);
                buffered.push(v);
                // Try to classify after every new record. As soon as we
                // have a confident verdict we stop peeking and hand both
                // the buffer and the remaining reader to the dispatcher.
                if let Some(found) = classify_records(&buffered) {
                    ext = Some(found);
                    break;
                }
            }
            Err(err) => {
                // A non-JSON line on the very first record means the file
                // is a pretty-printed single-object dump (Copilot legacy
                // shape or similar); let the caller fall through to
                // `read_json`. Otherwise we have buffered at least one
                // valid record already — stop peeking and let the
                // dispatcher decide.
                if buffered.is_empty() {
                    first_line_was_json = Some(false);
                    break;
                }
                log::warn!(
                    "skipping malformed JSONL record while detecting provider: {} at line {} column {}",
                    json_error_category(&err),
                    err.line(),
                    err.column()
                );
                initial_diagnostics.record_malformed();
            }
        }
    }

    if first_line_was_json == Some(false) {
        return Ok(None);
    }

    if buffered.is_empty() {
        return Ok(None);
    }

    // If the whole file was consumed without any distinctive marker, fall
    // back to Codex — a JSONL file with no Claude / Gemini / Copilot / Grok
    // markers is almost certainly a Codex log (or a synthetic fixture).
    let ext = ext.unwrap_or(ExtensionType::Codex);
    let parsed =
        dispatch_streaming_buffered(ext, buffered, reader, mode, initial_diagnostics, path)?;
    Ok(Some(finalize(parsed, ext)))
}

/// Reads lines from `reader` until it finds a non-empty one. Returns `Ok(None)`
/// at EOF.
///
/// # Errors
///
/// Returns an error if the underlying `read_line` fails (e.g. an I/O error or
/// invalid UTF-8 in the stream).
fn read_next_non_empty_line<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .context("Failed to read line from session file")?;
        if n == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            return Ok(Some(line));
        }
    }
}

/// Streams the rest of the file, prepending any already-parsed records.
///
/// JSONL providers feed buffered records through their typed shape and chain
/// the remaining reader. Grok reopens its single aggregate JSON object so its
/// sibling session files remain available to the provider parser.
fn dispatch_streaming_buffered(
    ext: ExtensionType,
    buffered: Vec<Value>,
    mut reader: BufReader<File>,
    mode: ParseMode,
    initial_diagnostics: ParseDiagnostics,
    path: &Path,
) -> Result<ParsedAnalysis> {
    let extra_diagnostics = Rc::new(RefCell::new(initial_diagnostics));
    match ext {
        ExtensionType::ClaudeCode => {
            let rest = iter_jsonl_values(&mut reader, Rc::clone(&extra_diagnostics));
            let logs = buffered.into_iter().chain(rest).filter_map(|value| {
                deserialize_record::<ClaudeCodeLog>(value, ext, &extra_diagnostics)
            });
            let parsed = parse_claude_logs_with_diagnostics(logs, mode)?;
            Ok(merge_extra_diagnostics(parsed, &extra_diagnostics))
        }
        ExtensionType::Codex => {
            let rest = iter_jsonl_values(&mut reader, Rc::clone(&extra_diagnostics));
            let logs = buffered
                .into_iter()
                .chain(rest)
                .filter_map(|value| deserialize_record::<CodexLog>(value, ext, &extra_diagnostics));
            let parsed = parse_codex_log_iter_with_diagnostics(logs, mode)?;
            Ok(merge_extra_diagnostics(parsed, &extra_diagnostics))
        }
        ExtensionType::Copilot => {
            // Copilot CLI emits one event per line under
            // `session-state/<uuid>/events.jsonl`. The streaming path sees
            // this as a sequence of parseable `Value`s whose very first
            // line is `type == "session.start"`.
            let rest = iter_jsonl_values(&mut reader, Rc::clone(&extra_diagnostics));
            let events = buffered.into_iter().chain(rest).filter_map(|value| {
                deserialize_record::<CopilotEvent>(value, ext, &extra_diagnostics)
            });
            let parsed = parse_copilot_events_with_diagnostics(events, mode)?;
            Ok(merge_extra_diagnostics(parsed, &extra_diagnostics))
        }
        ExtensionType::Gemini => {
            // Gemini sessions are line-delimited event streams: the first
            // line is a session-meta record carrying `sessionId` etc.,
            // and every subsequent line is an individual event. Feed the
            // already-buffered lines plus the rest of the reader into
            // `parse_gemini_events`.
            let mut iter = buffered.into_iter();
            let first = iter
                .next()
                .context("Gemini session missing top-level object")?;
            let session: GeminiSession =
                serde_json::from_value(first).context("Failed to parse Gemini session")?;

            let rest_events = iter_jsonl_values(&mut reader, Rc::clone(&extra_diagnostics));
            let parsed =
                parse_gemini_events_with_diagnostics(session, iter.chain(rest_events), mode)?;
            Ok(merge_extra_diagnostics(parsed, &extra_diagnostics))
        }
        ExtensionType::Grok => {
            drop(reader);
            parse_grok_session(path, mode)
        }
        // OpenCode stores sessions in a SQLite database, not a JSONL file, so
        // it never flows through the file parser. See `session::opencode`.
        ExtensionType::OpenCode => Ok(empty_parsed_analysis()),
        // Cursor sessions live in per-conversation SQLite blob stores (analysis)
        // and its billing tokens come from an API (usage), never a JSONL file.
        // See `session::cursor`.
        ExtensionType::Cursor => Ok(empty_parsed_analysis()),
        // Hermes stores usage in a single SQLite database, not a JSONL file, so
        // it never flows through the file parser. See `session::hermes`.
        ExtensionType::Hermes => Ok(empty_parsed_analysis()),
    }
}

fn json_error_category(error: &serde_json::Error) -> &'static str {
    match error.classify() {
        serde_json::error::Category::Io => "I/O error",
        serde_json::error::Category::Syntax => "syntax error",
        serde_json::error::Category::Data => "data error",
        serde_json::error::Category::Eof => "unexpected EOF",
    }
}

/// Iterator that yields raw [`Value`]s, one per non-empty line in the reader.
///
/// Used by parsers (Gemini / Copilot) that need to dispatch per-event on a
/// runtime-typed shape before committing to a strongly-typed struct, since
/// different event types carry completely different payloads.
fn iter_jsonl_values<'a>(
    reader: &'a mut BufReader<File>,
    diagnostics: Rc<RefCell<ParseDiagnostics>>,
) -> impl Iterator<Item = Value> + 'a {
    reader.lines().enumerate().filter_map(move |(index, line)| {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                log::warn!(
                    "skipping unreadable JSONL record at streamed line {}: {err}",
                    index + 1
                );
                diagnostics.borrow_mut().record_malformed();
                return None;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => Some(value),
            Err(err) => {
                log::warn!(
                    "skipping malformed JSONL record at streamed line {}: {} at line {} column {}",
                    index + 1,
                    json_error_category(&err),
                    err.line(),
                    err.column()
                );
                diagnostics.borrow_mut().record_malformed();
                None
            }
        }
    })
}

fn deserialize_record<T>(
    value: Value,
    provider: ExtensionType,
    diagnostics: &Rc<RefCell<ParseDiagnostics>>,
) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    let record_kind = raw_record_kind(provider, &value);
    match serde_json::from_value::<T>(value) {
        Ok(record) => Some(record),
        Err(_) => {
            let (recognized, relevant) = record_kind;
            let mut diagnostics = diagnostics.borrow_mut();
            if recognized {
                diagnostics.record_recognized_source();
                if relevant {
                    diagnostics.record_relevant(false);
                }
            } else {
                diagnostics.record_unrecognized();
            }
            drop(diagnostics);
            if relevant {
                log::warn!("skipping {provider} analyzer record with unsupported top-level schema");
            }
            None
        }
    }
}

fn raw_record_kind(provider: ExtensionType, value: &Value) -> (bool, bool) {
    let record_type = value.get("type").and_then(Value::as_str);
    match provider {
        ExtensionType::ClaudeCode => {
            let recognized = matches!(
                record_type,
                Some(
                    "assistant"
                        | "user"
                        | "system"
                        | "summary"
                        | "progress"
                        | "file-history-snapshot"
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
                )
            ) || value.get("toolUseResult").is_some();
            let user_tool_result = record_type == Some("user")
                && value
                    .pointer("/message/content")
                    .and_then(Value::as_array)
                    .is_some_and(|items| {
                        items.iter().any(|item| {
                            item.get("type").and_then(Value::as_str) == Some("tool_result")
                        })
                    });
            let relevant = record_type == Some("assistant")
                || value.get("toolUseResult").is_some()
                || user_tool_result;
            (recognized, relevant)
        }
        ExtensionType::Codex => {
            let recognized = matches!(
                record_type,
                Some(
                    "session_meta"
                        | "turn_context"
                        | "event_msg"
                        | "response_item"
                        | "inter_agent_communication_metadata"
                        | "world_state"
                        | "compacted"
                )
            );
            let payload_type = value.pointer("/payload/type");
            let relevant = match record_type {
                Some("event_msg") => payload_type
                    .is_some_and(|kind| kind.as_str() == Some("token_count") || !kind.is_string()),
                Some("response_item") => payload_type.is_some_and(|kind| {
                    matches!(
                        kind.as_str(),
                        Some(
                            "function_call"
                                | "function_call_output"
                                | "custom_tool_call"
                                | "custom_tool_call_output"
                        )
                    ) || !kind.is_string()
                }),
                _ => false,
            };
            (recognized, relevant)
        }
        ExtensionType::Copilot => {
            let recognized = matches!(
                record_type,
                Some(
                    "session.start"
                        | "session.model_change"
                        | "session.task_complete"
                        | "session.shutdown"
                        | "session.info"
                        | "session.mode_changed"
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
                )
            );
            let relevant = matches!(
                record_type,
                Some("session.shutdown" | "tool.execution_start" | "tool.execution_complete")
            ) || (record_type == Some("assistant.message")
                && value.pointer("/data/outputTokens").is_some());
            (recognized, relevant)
        }
        ExtensionType::Gemini => (false, false),
        ExtensionType::Grok => (is_grok_signals(value), is_grok_signals(value)),
        ExtensionType::OpenCode | ExtensionType::Cursor | ExtensionType::Hermes => (false, false),
    }
}

fn merge_extra_diagnostics(
    mut parsed: ParsedAnalysis,
    extra: &Rc<RefCell<ParseDiagnostics>>,
) -> ParsedAnalysis {
    parsed.diagnostics.merge(*extra.borrow());
    parsed
}

/// Legacy dispatch used by the pretty-printed JSON fallback. Operates on an
/// already-materialised `Vec<Value>` — preferred to be avoided in the hot
/// path, but needed for Gemini/Copilot dumps that span multiple lines.
fn dispatch_by_vec(
    data: Vec<Value>,
    ext_type: ExtensionType,
    mode: ParseMode,
    path: &Path,
) -> Result<ParsedAnalysis> {
    let extra_diagnostics = Rc::new(RefCell::new(ParseDiagnostics::default()));
    let parsed = match ext_type {
        ExtensionType::ClaudeCode => {
            let logs = data.into_iter().filter_map(|value| {
                deserialize_record::<ClaudeCodeLog>(value, ext_type, &extra_diagnostics)
            });
            parse_claude_logs_with_diagnostics(logs, mode)?
        }
        ExtensionType::Codex => {
            let logs = data.into_iter().filter_map(|value| {
                deserialize_record::<CodexLog>(value, ext_type, &extra_diagnostics)
            });
            parse_codex_log_iter_with_diagnostics(logs, mode)?
        }
        ExtensionType::Copilot => {
            let events = data.into_iter().filter_map(|value| {
                deserialize_record::<CopilotEvent>(value, ext_type, &extra_diagnostics)
            });
            parse_copilot_events_with_diagnostics(events, mode)?
        }
        ExtensionType::Gemini
        | ExtensionType::OpenCode
        | ExtensionType::Cursor
        | ExtensionType::Hermes => {
            // Copilot/Gemini only support the JSONL event stream, while OpenCode,
            // Cursor, and Hermes are read from SQLite (see `session::opencode` /
            // `session::cursor` / `session::hermes`), not a file. A file that
            // falls through to this branch (e.g. a stray pretty-printed export)
            // has no parser for its shape — return an empty analysis instead of
            // silently mis-parsing.
            let mut diagnostics = ParseDiagnostics::default();
            for _ in data {
                diagnostics.record_unrecognized();
            }
            ParsedAnalysis::new(empty_analysis(), diagnostics)
        }
        ExtensionType::Grok => parse_grok_session(path, mode)?,
    };
    Ok(finalize(
        merge_extra_diagnostics(parsed, &extra_diagnostics),
        ext_type,
    ))
}

fn empty_analysis() -> CodeAnalysis {
    CodeAnalysis {
        user: String::new(),
        extension_name: String::new(),
        insights_version: String::new(),
        machine_id: String::new(),
        records: Vec::new(),
    }
}

fn empty_parsed_analysis() -> ParsedAnalysis {
    ParsedAnalysis::new(empty_analysis(), ParseDiagnostics::default())
}

/// Attaches runtime metadata (user, machine, version) expected in the output.
fn finalize(mut parsed: ParsedAnalysis, ext_type: ExtensionType) -> ParsedAnalysis {
    parsed.analysis.user = get_current_user();
    parsed.analysis.extension_name = ext_type.to_string();
    parsed.analysis.machine_id = get_machine_id().to_string();
    parsed.analysis.insights_version = VERSION.to_string();
    parsed
}

fn validate_parsed_source(path: &Path, diagnostics: &ParseDiagnostics) -> Result<()> {
    if diagnostics.source_records == 0 {
        return Ok(());
    }
    if diagnostics.recognized_records == 0 {
        bail!(
            "session file {} contained no recognized provider records",
            path.display()
        );
    }
    if diagnostics.relevant_records > 0 && diagnostics.normalized_records == 0 {
        bail!(
            "session file {} contained {} analyzer-relevant provider records, but none used a supported schema",
            path.display(),
            diagnostics.relevant_records
        );
    }
    Ok(())
}
