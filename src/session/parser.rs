use crate::VERSION;
use crate::constants::buffer;
use crate::models::{
    ClaudeCodeLog, CodeAnalysis, CodexLog, CopilotEvent, ExtensionType, GeminiSession,
};
use crate::pricing::TierThresholds;
use crate::session::claude::parse_claude_logs_with_diagnostics;
use crate::session::codex::parse_codex_log_iter_with_diagnostics;
use crate::session::copilot::parse_copilot_events_with_diagnostics;
use crate::session::detector::{RecordClassifier, detect_extension_type};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::gemini::parse_gemini_events_with_diagnostics;
use crate::session::grok::{is_grok_signals, parse_grok_session};
use crate::session::state::ParseMode;
use crate::utils::{get_current_user, get_machine_id, read_json, read_jsonl};
use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
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

#[derive(Debug, Default)]
struct ParseWarningSummary {
    unreadable_records: usize,
    malformed_records: usize,
    unsupported_schema_records: usize,
    first_reason: Option<String>,
}

impl ParseWarningSummary {
    fn record_unreadable(&mut self, line: usize, error: &std::io::Error) {
        self.unreadable_records += 1;
        if self.first_reason.is_none() {
            self.first_reason = Some(format!("unreadable line {line}: {error}"));
        }
    }

    fn record_malformed(&mut self, line: usize, error: &serde_json::Error) {
        self.malformed_records += 1;
        if self.first_reason.is_none() {
            self.first_reason = Some(format!(
                "malformed line {line}: {} at line {} column {}",
                json_error_category(error),
                error.line(),
                error.column()
            ));
        }
    }

    fn record_unsupported_schema(&mut self, provider: ExtensionType, error: &serde_json::Error) {
        self.unsupported_schema_records += 1;
        if self.first_reason.is_none() {
            self.first_reason = Some(format!("unsupported {provider} schema: {error}"));
        }
    }

    fn emit(&self, path: &Path) {
        let total =
            self.unreadable_records + self.malformed_records + self.unsupported_schema_records;
        if total == 0 {
            return;
        }
        let first_reason = self
            .first_reason
            .as_deref()
            .unwrap_or("unknown parse failure");
        log::warn!(
            "session parser skipped {total} record(s) from {}: unreadable={}, malformed={}, unsupported_schema={}; first failure: {first_reason}",
            path.display(),
            self.unreadable_records,
            self.malformed_records,
            self.unsupported_schema_records
        );
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
/// use vibe_coding_tracker::parse_session_file_to_value;
///
/// let value = parse_session_file_to_value("session.jsonl")?;
/// assert!(value.is_object());
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn parse_session_file_to_value<P: AsRef<Path>>(path: P) -> Result<Value> {
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
/// Prefer [`parse_session_file_typed_as`] whenever the caller already knows
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
    Ok(parse_session_file_with_diagnostics(path, mode)?.0)
}

/// Single-file parse with a content-safe partial-failure summary for the CLI.
#[doc(hidden)]
pub fn parse_session_file_with_diagnostics<P: AsRef<Path>>(
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
    dispatch_by_vec(data, ext_type, mode, path, None)
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
/// use vibe_coding_tracker::session::parse_session_file_typed_as;
/// use vibe_coding_tracker::session::ParseMode;
/// use vibe_coding_tracker::ExtensionType;
///
/// // A file walked out of `~/.claude/projects` is known to be Claude Code.
/// let analysis = parse_session_file_typed_as(
///     "session.jsonl",
///     ExtensionType::ClaudeCode,
///     ParseMode::Full,
/// )?;
/// # let _ = analysis;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn parse_session_file_typed_as<P: AsRef<Path>>(
    path: P,
    provider: ExtensionType,
    mode: ParseMode,
) -> Result<CodeAnalysis> {
    let path = path.as_ref();
    let parsed = parse_session_file_typed_as_with_diagnostics(path, provider, mode, None)?;
    validate_parsed_source(path, &parsed.diagnostics)?;
    Ok(parsed.analysis)
}

pub(crate) fn parse_session_file_typed_as_with_diagnostics(
    path: &Path,
    provider: ExtensionType,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<ParsedAnalysis> {
    if let Some(parsed) = stream_parse_known(path, provider, mode, tiers)? {
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

    dispatch_by_vec(data, provider, mode, path, tiers)
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
    tiers: Option<&TierThresholds>,
) -> Result<Option<ParsedAnalysis>> {
    match provider {
        ExtensionType::ClaudeCode => {
            let Some(mut stream) = prepare_typed_stream::<ClaudeCodeLog>(path, provider)? else {
                return Ok(None);
            };
            let diagnostics = Rc::new(RefCell::new(stream.diagnostics));
            let warnings = Rc::new(RefCell::new(stream.warnings));
            let io_failure = Rc::new(RefCell::new(None));
            let rest = iter_jsonl_typed(
                &mut stream.reader,
                provider,
                Rc::clone(&diagnostics),
                Rc::clone(&warnings),
                Rc::clone(&io_failure),
            );
            let parsed = parse_claude_logs_with_diagnostics(
                stream.first.into_iter().chain(rest),
                mode,
                tiers,
            );
            warnings.borrow().emit(path);
            if let Some(error) = io_failure.borrow_mut().take() {
                bail!("Failed to read session file {}: {error}", path.display());
            }
            let parsed = parsed?;
            Ok(Some(finalize(
                merge_extra_diagnostics(parsed, &diagnostics),
                provider,
            )))
        }
        ExtensionType::Codex => {
            let Some(mut stream) = prepare_typed_stream::<CodexLog>(path, provider)? else {
                return Ok(None);
            };
            let diagnostics = Rc::new(RefCell::new(stream.diagnostics));
            let warnings = Rc::new(RefCell::new(stream.warnings));
            let io_failure = Rc::new(RefCell::new(None));
            let rest = iter_jsonl_typed(
                &mut stream.reader,
                provider,
                Rc::clone(&diagnostics),
                Rc::clone(&warnings),
                Rc::clone(&io_failure),
            );
            let parsed = parse_codex_log_iter_with_diagnostics(
                stream.first.into_iter().chain(rest),
                mode,
                tiers,
            );
            warnings.borrow().emit(path);
            if let Some(error) = io_failure.borrow_mut().take() {
                bail!("Failed to read session file {}: {error}", path.display());
            }
            let parsed = parsed?;
            Ok(Some(finalize(
                merge_extra_diagnostics(parsed, &diagnostics),
                provider,
            )))
        }
        ExtensionType::Copilot => {
            let Some(mut stream) = prepare_typed_stream::<CopilotEvent>(path, provider)? else {
                return Ok(None);
            };
            let diagnostics = Rc::new(RefCell::new(stream.diagnostics));
            let warnings = Rc::new(RefCell::new(stream.warnings));
            let io_failure = Rc::new(RefCell::new(None));
            let rest = iter_jsonl_typed(
                &mut stream.reader,
                provider,
                Rc::clone(&diagnostics),
                Rc::clone(&warnings),
                Rc::clone(&io_failure),
            );
            let parsed =
                parse_copilot_events_with_diagnostics(stream.first.into_iter().chain(rest), mode);
            warnings.borrow().emit(path);
            if let Some(error) = io_failure.borrow_mut().take() {
                bail!("Failed to read session file {}: {error}", path.display());
            }
            let parsed = parsed?;
            Ok(Some(finalize(
                merge_extra_diagnostics(parsed, &diagnostics),
                provider,
            )))
        }
        _ => stream_parse_known_dynamic(path, provider, mode, tiers),
    }
}

/// Dynamic streaming path for providers whose event schema stays as `Value`.
fn stream_parse_known_dynamic(
    path: &Path,
    provider: ExtensionType,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
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
        Rc::new(RefCell::new(ParseWarningSummary::default())),
        path,
        tiers,
    )?;
    Ok(Some(finalize(parsed, provider)))
}

struct TypedStream<T> {
    first: Option<T>,
    reader: BufReader<File>,
    diagnostics: ParseDiagnostics,
    warnings: ParseWarningSummary,
}

/// Opens a JSONL source and parses its first record directly into `T`.
///
/// A raw `Value` is only materialized after typed deserialization fails, so
/// successful records avoid building a second JSON tree solely for dispatch.
fn prepare_typed_stream<T>(path: &Path, provider: ExtensionType) -> Result<Option<TypedStream<T>>>
where
    T: DeserializeOwned,
{
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);
    let mut line = Vec::with_capacity(buffer::AVG_JSONL_LINE_SIZE);

    if !read_next_non_empty_bytes(&mut reader, &mut line)? {
        return Ok(None);
    }

    let bytes = trim_ascii_whitespace(&line);
    let mut diagnostics = ParseDiagnostics::default();
    let mut warnings = ParseWarningSummary::default();
    let first = match serde_json::from_slice::<T>(bytes) {
        Ok(record) => Some(record),
        Err(typed_error) => match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => {
                record_typed_schema_failure(
                    provider,
                    &value,
                    &mut diagnostics,
                    &mut warnings,
                    &typed_error,
                );
                None
            }
            // A first line that is not a complete JSON value indicates a
            // pretty-printed document. Let the caller use the whole-file
            // fallback instead of treating it as malformed JSONL.
            Err(_) => return Ok(None),
        },
    };

    Ok(Some(TypedStream {
        first,
        reader,
        diagnostics,
        warnings,
    }))
}

/// Streaming path when the provider is unknown.
///
/// Reads JSONL records one line at a time and feeds a stateful classifier that
/// commits to a provider as soon as any record carries a distinctive marker.
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
    let mut classifier = RecordClassifier::default();
    let warnings = Rc::new(RefCell::new(ParseWarningSummary::default()));
    let mut line_number = 0_usize;

    loop {
        let line = match read_next_non_empty_line(&mut reader)? {
            Some(line) => line,
            None => break,
        };
        line_number += 1;

        match serde_json::from_str::<Value>(line.trim()) {
            Ok(v) => {
                first_line_was_json.get_or_insert(true);
                let classification = classifier.push(&v);
                buffered.push(v);
                // Each record is inspected once by the stateful classifier.
                // As soon as it has a confident verdict, hand the buffered
                // prefix and remaining reader to the dispatcher.
                if let Some(found) = classification {
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
                warnings.borrow_mut().record_malformed(line_number, &err);
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
    let parsed = dispatch_streaming_buffered(
        ext,
        buffered,
        reader,
        mode,
        initial_diagnostics,
        warnings,
        path,
        None,
    )?;
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

/// Reuses `line` while reading through blank lines to the next JSONL record.
fn read_next_non_empty_bytes<R: BufRead>(reader: &mut R, line: &mut Vec<u8>) -> Result<bool> {
    loop {
        line.clear();
        let count = reader
            .read_until(b'\n', line)
            .context("Failed to read line from session file")?;
        if count == 0 {
            return Ok(false);
        }
        if !trim_ascii_whitespace(line).is_empty() {
            return Ok(true);
        }
    }
}

fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

/// Streams the rest of the file, prepending any already-parsed records.
///
/// JSONL providers feed buffered records through their typed shape and chain
/// the remaining reader. Grok reopens its single aggregate JSON object so its
/// sibling session files remain available to the provider parser.
#[allow(clippy::too_many_arguments)] // parse plumbing; a struct would obscure the seams
fn dispatch_streaming_buffered(
    ext: ExtensionType,
    buffered: Vec<Value>,
    mut reader: BufReader<File>,
    mode: ParseMode,
    initial_diagnostics: ParseDiagnostics,
    warnings: Rc<RefCell<ParseWarningSummary>>,
    path: &Path,
    tiers: Option<&TierThresholds>,
) -> Result<ParsedAnalysis> {
    let extra_diagnostics = Rc::new(RefCell::new(initial_diagnostics));
    let io_failure = Rc::new(RefCell::new(None));
    let parsed = match ext {
        ExtensionType::ClaudeCode => {
            let rest = iter_jsonl_values(
                &mut reader,
                Rc::clone(&extra_diagnostics),
                Rc::clone(&warnings),
                Rc::clone(&io_failure),
            );
            let logs = buffered.into_iter().chain(rest).filter_map(|value| {
                deserialize_record::<ClaudeCodeLog>(value, ext, &extra_diagnostics, &warnings)
            });
            parse_claude_logs_with_diagnostics(logs, mode, tiers)
                .map(|parsed| merge_extra_diagnostics(parsed, &extra_diagnostics))
        }
        ExtensionType::Codex => {
            let rest = iter_jsonl_values(
                &mut reader,
                Rc::clone(&extra_diagnostics),
                Rc::clone(&warnings),
                Rc::clone(&io_failure),
            );
            let logs = buffered.into_iter().chain(rest).filter_map(|value| {
                deserialize_record::<CodexLog>(value, ext, &extra_diagnostics, &warnings)
            });
            parse_codex_log_iter_with_diagnostics(logs, mode, tiers)
                .map(|parsed| merge_extra_diagnostics(parsed, &extra_diagnostics))
        }
        ExtensionType::Copilot => {
            // Copilot CLI emits one event per line under
            // `session-state/<uuid>/events.jsonl`. The streaming path sees
            // this as a sequence of parseable `Value`s whose very first
            // line is `type == "session.start"`.
            let rest = iter_jsonl_values(
                &mut reader,
                Rc::clone(&extra_diagnostics),
                Rc::clone(&warnings),
                Rc::clone(&io_failure),
            );
            let events = buffered.into_iter().chain(rest).filter_map(|value| {
                deserialize_record::<CopilotEvent>(value, ext, &extra_diagnostics, &warnings)
            });
            parse_copilot_events_with_diagnostics(events, mode)
                .map(|parsed| merge_extra_diagnostics(parsed, &extra_diagnostics))
        }
        ExtensionType::Gemini => {
            // Gemini sessions are line-delimited event streams: the first
            // line is a session-meta record carrying `sessionId` etc.,
            // and every subsequent line is an individual event. Feed the
            // already-buffered lines plus the rest of the reader into
            // `parse_gemini_events`.
            (|| {
                let mut iter = buffered.into_iter();
                let first = iter
                    .next()
                    .context("Gemini session missing top-level object")?;
                let session: GeminiSession =
                    serde_json::from_value(first).context("Failed to parse Gemini session")?;

                let rest_events = iter_jsonl_values(
                    &mut reader,
                    Rc::clone(&extra_diagnostics),
                    Rc::clone(&warnings),
                    Rc::clone(&io_failure),
                );
                parse_gemini_events_with_diagnostics(session, iter.chain(rest_events), mode, tiers)
                    .map(|parsed| merge_extra_diagnostics(parsed, &extra_diagnostics))
            })()
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
    };
    warnings.borrow().emit(path);
    if let Some(error) = io_failure.borrow_mut().take() {
        bail!("Failed to read session file {}: {error}", path.display());
    }
    parsed
}

fn json_error_category(error: &serde_json::Error) -> &'static str {
    match error.classify() {
        serde_json::error::Category::Io => "I/O error",
        serde_json::error::Category::Syntax => "syntax error",
        serde_json::error::Category::Data => "data error",
        serde_json::error::Category::Eof => "unexpected EOF",
    }
}

/// Iterator that deserializes JSONL records directly into the provider model.
///
/// The line buffer is retained across iterations. A raw `Value` is only built
/// when typed deserialization fails and diagnostics need to classify the
/// unsupported record.
fn iter_jsonl_typed<'a, T>(
    reader: &'a mut BufReader<File>,
    provider: ExtensionType,
    diagnostics: Rc<RefCell<ParseDiagnostics>>,
    warnings: Rc<RefCell<ParseWarningSummary>>,
    io_failure: Rc<RefCell<Option<String>>>,
) -> impl Iterator<Item = T> + 'a
where
    T: DeserializeOwned + 'a,
{
    let mut line = Vec::with_capacity(buffer::AVG_JSONL_LINE_SIZE);
    let mut line_number = 1_usize;

    std::iter::from_fn(move || {
        loop {
            line.clear();
            line_number += 1;
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => return None,
                Ok(_) => {}
                Err(err) => {
                    warnings.borrow_mut().record_unreadable(line_number, &err);
                    *io_failure.borrow_mut() = Some(err.to_string());
                    return None;
                }
            }

            let bytes = trim_ascii_whitespace(&line);
            if bytes.is_empty() {
                continue;
            }

            match serde_json::from_slice::<T>(bytes) {
                Ok(record) => return Some(record),
                Err(typed_error) => match serde_json::from_slice::<Value>(bytes) {
                    Ok(value) => {
                        record_typed_schema_failure(
                            provider,
                            &value,
                            &mut diagnostics.borrow_mut(),
                            &mut warnings.borrow_mut(),
                            &typed_error,
                        );
                    }
                    Err(error) => {
                        warnings.borrow_mut().record_malformed(line_number, &error);
                        diagnostics.borrow_mut().record_malformed();
                    }
                },
            }
        }
    })
}

/// Iterator that yields raw [`Value`]s, one per non-empty line in the reader.
///
/// Used by parsers (Gemini / Copilot) that need to dispatch per-event on a
/// runtime-typed shape before committing to a strongly-typed struct, since
/// different event types carry completely different payloads.
fn iter_jsonl_values<'a>(
    reader: &'a mut BufReader<File>,
    diagnostics: Rc<RefCell<ParseDiagnostics>>,
    warnings: Rc<RefCell<ParseWarningSummary>>,
    io_failure: Rc<RefCell<Option<String>>>,
) -> impl Iterator<Item = Value> + 'a {
    reader.lines().enumerate().filter_map(move |(index, line)| {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                warnings.borrow_mut().record_unreadable(index + 1, &err);
                *io_failure.borrow_mut() = Some(err.to_string());
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
                warnings.borrow_mut().record_malformed(index + 1, &err);
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
    warnings: &Rc<RefCell<ParseWarningSummary>>,
) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    let record_kind = raw_record_kind(provider, &value);
    match serde_json::from_value::<T>(value) {
        Ok(record) => Some(record),
        Err(error) => {
            record_typed_schema_failure_kind(
                provider,
                record_kind,
                &mut diagnostics.borrow_mut(),
                &mut warnings.borrow_mut(),
                &error,
            );
            None
        }
    }
}

fn record_typed_schema_failure(
    provider: ExtensionType,
    value: &Value,
    diagnostics: &mut ParseDiagnostics,
    warnings: &mut ParseWarningSummary,
    error: &serde_json::Error,
) {
    record_typed_schema_failure_kind(
        provider,
        raw_record_kind(provider, value),
        diagnostics,
        warnings,
        error,
    );
}

fn record_typed_schema_failure_kind(
    provider: ExtensionType,
    (recognized, relevant): (bool, bool),
    diagnostics: &mut ParseDiagnostics,
    warnings: &mut ParseWarningSummary,
    error: &serde_json::Error,
) {
    if recognized {
        diagnostics.record_recognized_source();
        if relevant {
            diagnostics.record_relevant(false);
        }
    } else {
        diagnostics.record_unrecognized();
    }
    if relevant {
        warnings.record_unsupported_schema(provider, error);
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
    tiers: Option<&TierThresholds>,
) -> Result<ParsedAnalysis> {
    let extra_diagnostics = Rc::new(RefCell::new(ParseDiagnostics::default()));
    let warnings = Rc::new(RefCell::new(ParseWarningSummary::default()));
    let parsed = match ext_type {
        ExtensionType::ClaudeCode => {
            let logs = data.into_iter().filter_map(|value| {
                deserialize_record::<ClaudeCodeLog>(value, ext_type, &extra_diagnostics, &warnings)
            });
            parse_claude_logs_with_diagnostics(logs, mode, tiers)?
        }
        ExtensionType::Codex => {
            let logs = data.into_iter().filter_map(|value| {
                deserialize_record::<CodexLog>(value, ext_type, &extra_diagnostics, &warnings)
            });
            parse_codex_log_iter_with_diagnostics(logs, mode, tiers)?
        }
        ExtensionType::Copilot => {
            let events = data.into_iter().filter_map(|value| {
                deserialize_record::<CopilotEvent>(value, ext_type, &extra_diagnostics, &warnings)
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
    warnings.borrow().emit(path);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::detector::{record_inspections, reset_record_inspections};
    use tempfile::TempDir;

    #[test]
    fn auto_detection_inspects_each_preamble_record_once() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("long-preamble.jsonl");
        let sentinel = r#"{"type":"file-history-snapshot","messageId":"m1","snapshot":{}}"#;
        let assistant = r#"{"type":"assistant","sessionId":"session","parentUuid":"parent","timestamp":"2026-07-14T00:00:00Z","message":{"model":"claude-opus-4-7","usage":{"input_tokens":1,"output_tokens":1},"content":[]}}"#;
        let mut contents =
            String::with_capacity((sentinel.len() + 1) * 10_000 + assistant.len() + 1);
        for _ in 0..10_000 {
            contents.push_str(sentinel);
            contents.push('\n');
        }
        contents.push_str(assistant);
        contents.push('\n');
        std::fs::write(&path, contents).unwrap();

        reset_record_inspections();
        let parsed = stream_parse_autodetect(&path, ParseMode::UsageOnly)
            .unwrap()
            .expect("JSONL source should use streaming detection");

        assert_eq!(parsed.analysis.extension_name, "Claude-Code");
        assert_eq!(record_inspections(), 10_001);
    }
}
