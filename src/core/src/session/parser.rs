//! Entry points for parsing file-backed session logs into [`CodeAnalysis`].
//!
//! Callers that know the provider from the source directory should use [`parse_session_file_typed_as`]; content-based detection is provided by [`parse_session_file_typed`] and [`parse_session_file_typed_with_mode`].

use crate::VERSION;
use crate::constants::buffer;
use crate::models::{
    ClaudeCodeLog, CodeAnalysis, CodexLog, CopilotEvent, ExtensionType, GeminiSession,
};
use crate::pricing::TierThresholds;
use crate::session::antigravity::parse_antigravity_session;
use crate::session::claude::{parse_claude_jsonl_path, parse_claude_logs_with_diagnostics};
use crate::session::codex::{parse_codex_jsonl_path, parse_codex_log_iter_with_diagnostics};
use crate::session::copilot::parse_copilot_events_with_diagnostics;
use crate::session::detector::{RecordClassifier, detect_extension_type};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::dsh::{has_zstd_magic, parse_dsh_session};
use crate::session::gemini::parse_gemini_events_with_diagnostics;
use crate::session::grok::parse_grok_session;
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

/// Content-safe partial-parse summary for single-file parsing.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionFileParseDiagnostics {
    skipped_records: usize,
}

impl SessionFileParseDiagnostics {
    /// Returns the number of malformed, unrecognized, or unsupported records skipped during parsing.
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

/// Parses a session file at `path` and returns the result as a [`serde_json::Value`].
///
/// Auto-detects the provider via [`parse_session_file_typed`] in [`ParseMode::Full`]. Empty input yields `{}` rather than an empty [`CodeAnalysis`].
///
/// # Examples
///
/// ```no_run
/// use vct_core::parse_session_file_to_value;
///
/// let value = parse_session_file_to_value("session.jsonl")?;
/// assert!(value.is_object());
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
///
/// Returns an error if `path` cannot be read, if a non-empty file has no supported provider payload, or if serialization to [`serde_json::Value`] fails.
pub fn parse_session_file_to_value<P: AsRef<Path>>(path: P) -> Result<Value> {
    let analysis = parse_session_file_typed(path)?;
    if analysis.records.is_empty() && analysis.extension_name.is_empty() {
        // Empty input bypasses `finalize`, leaving `extension_name` blank as the sentinel.
        return Ok(serde_json::json!({}));
    }
    Ok(serde_json::to_value(&analysis)?)
}

/// Parses a session file at `path` in [`ParseMode::Full`], auto-detecting the provider from its contents.
///
/// Prefer [`parse_session_file_typed_as`] when the provider is known from the source directory, or [`parse_session_file_typed_with_mode`] with [`ParseMode::UsageOnly`] when per-operation detail bodies are not needed. Empty input returns an empty [`CodeAnalysis`].
///
/// # Examples
///
/// ```no_run
/// use vct_core::parse_session_file_typed;
///
/// let analysis = parse_session_file_typed("session.jsonl")?;
/// println!("provider: {}", analysis.extension_name);
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
///
/// Returns an error if `path` cannot be opened or read, if a non-empty file has no recognized provider records, or if every analyzer-relevant record uses an unsupported schema.
pub fn parse_session_file_typed<P: AsRef<Path>>(path: P) -> Result<CodeAnalysis> {
    parse_session_file_typed_with_mode(path, ParseMode::Full)
}

/// Parses a session file at `path` with the given [`ParseMode`], auto-detecting the provider from its contents.
///
/// Empty input returns an empty [`CodeAnalysis`].
///
/// # Examples
///
/// ```no_run
/// use vct_core::session::parse_session_file_typed_with_mode;
/// use vct_core::session::ParseMode;
///
/// let analysis =
///     parse_session_file_typed_with_mode("session.jsonl", ParseMode::UsageOnly)?;
/// assert!(analysis.records.iter().all(|r| r.write_file_details.is_empty()));
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
///
/// Returns an error if `path` cannot be opened or read, if no provider can be detected, if a non-empty file has no recognized provider records, or if every analyzer-relevant record uses an unsupported schema.
pub fn parse_session_file_typed_with_mode<P: AsRef<Path>>(
    path: P,
    mode: ParseMode,
) -> Result<CodeAnalysis> {
    Ok(parse_session_file_with_diagnostics(path, mode)?.0)
}

/// Parses a session file at `path` with auto-detection and returns a [`SessionFileParseDiagnostics`] summary alongside the [`CodeAnalysis`].
///
/// # Errors
///
/// Returns the same errors as [`parse_session_file_typed_with_mode`].
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
    // Route compressed `dsh` logs by zstd frame magic before attempting UTF-8 line reading.
    if has_zstd_magic(path) {
        return Ok(finalize(
            parse_dsh_session(path, mode, None)?,
            ExtensionType::DeepSeek,
        ));
    }

    if let Some(parsed) = stream_parse_autodetect(path, mode)? {
        return Ok(parsed);
    }

    // Whole-file fallback for non-JSONL files, including pretty-printed Grok `signals.json`.
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

/// Parses a session file at `path` as `provider` with the given [`ParseMode`], skipping content-based detection.
///
/// Empty input returns an empty [`CodeAnalysis`].
///
/// # Examples
///
/// ```no_run
/// use vct_core::session::parse_session_file_typed_as;
/// use vct_core::session::ParseMode;
/// use vct_core::ExtensionType;
///
/// let analysis = parse_session_file_typed_as(
///     "session.jsonl",
///     ExtensionType::ClaudeCode,
///     ParseMode::Full,
/// )?;
/// # let _ = analysis;
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
///
/// Returns an error if `path` cannot be opened or read, if a non-empty file has no recognized records for `provider`, or if every analyzer-relevant record uses an unsupported schema.
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

    // Whole-file fallback for empty files or multi-line JSON documents.
    let data = match read_jsonl(path) {
        Ok(data) => data,
        Err(_) => read_json(path)?,
    };

    if data.is_empty() {
        return Ok(empty_parsed_analysis());
    }

    dispatch_by_vec(data, provider, mode, path, tiers)
}

/// Streams a file for a known `provider`, returning `Ok(None)` if empty or not line-delimited JSON.
fn stream_parse_known(
    path: &Path,
    provider: ExtensionType,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<Option<ParsedAnalysis>> {
    match provider {
        ExtensionType::ClaudeCode => parse_claude_jsonl_path(path, mode, tiers),
        ExtensionType::Codex => parse_codex_jsonl_path(path, mode, tiers),
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
                stream.lines_read,
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
        // DeepSeek logs are concatenated zstd frames by default, so `parse_dsh_session` reads them directly.
        ExtensionType::DeepSeek => Ok(Some(finalize(
            parse_dsh_session(path, mode, tiers)?,
            provider,
        ))),
        // Antigravity joins transcript steps against a sibling SQLite DB, so its parser opens the path directly.
        ExtensionType::Antigravity => Ok(Some(finalize(
            parse_antigravity_session(path, mode, tiers)?,
            provider,
        ))),
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

    let mut lines_read = 0_usize;
    let first_line = match read_next_non_empty_line(&mut reader, &mut lines_read)? {
        Some(line) => line,
        None => return Ok(None),
    };

    let first_value: Value = match serde_json::from_str(first_line.trim()) {
        Ok(v) => v,
        // Not JSONL; defer to whole-file `read_json`.
        Err(_) => return Ok(None),
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
        lines_read,
    )?;
    Ok(Some(finalize(parsed, provider)))
}

struct TypedStream<T> {
    first: Option<T>,
    reader: BufReader<File>,
    diagnostics: ParseDiagnostics,
    warnings: ParseWarningSummary,
    /// Physical lines consumed while reading the first record, keeping subsequent warning line numbers accurate.
    lines_read: usize,
}

/// Streams JSONL lines into `on_line` using one reusable byte buffer, returning `Ok(None)` if empty or non-JSONL.
pub(crate) fn stream_jsonl_borrowed<S>(
    path: &Path,
    provider: ExtensionType,
    mut state: S,
    mut on_line: impl FnMut(&mut S, &[u8]) -> std::result::Result<(), serde_json::Error>,
    finish: impl FnOnce(S) -> Result<ParsedAnalysis>,
) -> Result<Option<ParsedAnalysis>> {
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);
    let mut line = Vec::with_capacity(buffer::AVG_JSONL_LINE_SIZE);
    let mut lines_read = 0_usize;

    if !read_next_non_empty_bytes(&mut reader, &mut line, &mut lines_read)? {
        return Ok(None);
    }

    let bytes = trim_ascii_whitespace(&line);
    let mut diagnostics = ParseDiagnostics::default();
    let mut warnings = ParseWarningSummary::default();
    if let Err(typed_error) = on_line(&mut state, bytes) {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => {
                record_typed_schema_failure(
                    provider,
                    &value,
                    &mut diagnostics,
                    &mut warnings,
                    &typed_error,
                );
            }
            Err(_) => return Ok(None),
        }
    }

    let mut io_failure: Option<String> = None;
    loop {
        line.clear();
        lines_read += 1;
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => {
                warnings.record_unreadable(lines_read, &err);
                io_failure = Some(err.to_string());
                break;
            }
        }

        let bytes = trim_ascii_whitespace(&line);
        if bytes.is_empty() {
            continue;
        }

        if let Err(typed_error) = on_line(&mut state, bytes) {
            match serde_json::from_slice::<Value>(bytes) {
                Ok(value) => {
                    record_typed_schema_failure(
                        provider,
                        &value,
                        &mut diagnostics,
                        &mut warnings,
                        &typed_error,
                    );
                }
                Err(error) => {
                    warnings.record_malformed(lines_read, &error);
                    diagnostics.record_malformed();
                }
            }
        }
    }

    let parsed = finish(state);
    warnings.emit(path);
    if let Some(error) = io_failure {
        bail!("Failed to read session file {}: {error}", path.display());
    }
    let mut parsed = parsed?;
    parsed.diagnostics.merge(diagnostics);
    Ok(Some(finalize(parsed, provider)))
}

/// Opens a JSONL file and deserializes its first non-empty line into `T`, returning `Ok(None)` if empty or non-JSONL.
fn prepare_typed_stream<T>(path: &Path, provider: ExtensionType) -> Result<Option<TypedStream<T>>>
where
    T: DeserializeOwned,
{
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);
    let mut line = Vec::with_capacity(buffer::AVG_JSONL_LINE_SIZE);
    let mut lines_read = 0_usize;

    if !read_next_non_empty_bytes(&mut reader, &mut line, &mut lines_read)? {
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
            // Incomplete JSON on line one indicates a pretty-printed document; defer to whole-file fallback.
            Err(_) => return Ok(None),
        },
    };

    Ok(Some(TypedStream {
        first,
        reader,
        diagnostics,
        warnings,
        lines_read,
    }))
}

/// Streams a JSONL file while classifying its provider, returning `Ok(None)` if empty or non-JSONL.
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
    // Count all physical lines (including blanks) so warning line numbers match editor lines.
    let mut lines_read = 0_usize;

    loop {
        let line = match read_next_non_empty_line(&mut reader, &mut lines_read)? {
            Some(line) => line,
            None => break,
        };

        match serde_json::from_str::<Value>(line.trim()) {
            Ok(v) => {
                first_line_was_json.get_or_insert(true);
                let classification = classifier.push(&v);
                buffered.push(v);
                if let Some(found) = classification {
                    ext = Some(found);
                    break;
                }
            }
            Err(err) => {
                // Non-JSON on the first record means a multi-line JSON file; defer to `read_json`. Later lines are malformed JSONL records.
                if buffered.is_empty() {
                    first_line_was_json = Some(false);
                    break;
                }
                warnings.borrow_mut().record_malformed(lines_read, &err);
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
        lines_read,
    )?;
    Ok(Some(finalize(parsed, ext)))
}

/// Returns the next non-empty line from `reader` (or `None` at EOF), incrementing `consumed` for every physical line read.
fn read_next_non_empty_line<R: BufRead>(
    reader: &mut R,
    consumed: &mut usize,
) -> Result<Option<String>> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .context("Failed to read line from session file")?;
        if n == 0 {
            return Ok(None);
        }
        *consumed += 1;
        if !line.trim().is_empty() {
            return Ok(Some(line));
        }
    }
}

/// Reads the next non-empty line into `line`, incrementing `consumed` for every physical line read and returning `false` at EOF.
fn read_next_non_empty_bytes<R: BufRead>(
    reader: &mut R,
    line: &mut Vec<u8>,
    consumed: &mut usize,
) -> Result<bool> {
    loop {
        line.clear();
        let count = reader
            .read_until(b'\n', line)
            .context("Failed to read line from session file")?;
        if count == 0 {
            return Ok(false);
        }
        *consumed += 1;
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

/// Dispatches `buffered` records followed by the remaining lines of `reader` to the parser for `ext`.
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
    lines_read: usize,
) -> Result<ParsedAnalysis> {
    let extra_diagnostics = Rc::new(RefCell::new(initial_diagnostics));
    let io_failure = Rc::new(RefCell::new(None));
    let parsed = match ext {
        ExtensionType::ClaudeCode => {
            let rest = iter_jsonl_values(
                &mut reader,
                lines_read,
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
                lines_read,
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
            let rest = iter_jsonl_values(
                &mut reader,
                lines_read,
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
            // Peel off Gemini's first-line session-meta record before streaming event lines.
            (|| {
                let mut iter = buffered.into_iter();
                let first = iter
                    .next()
                    .context("Gemini session missing top-level object")?;
                let session: GeminiSession =
                    serde_json::from_value(first).context("Failed to parse Gemini session")?;

                let rest_events = iter_jsonl_values(
                    &mut reader,
                    lines_read,
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
        // Reopen uncompressed `dsh` logs so plain and zstd layouts share `parse_dsh_session`.
        ExtensionType::DeepSeek => {
            drop(reader);
            parse_dsh_session(path, mode, tiers)
        }
        ExtensionType::Antigravity => {
            drop(reader);
            parse_antigravity_session(path, mode, tiers)
        }
        // SQLite-backed providers are not file streams; see `session::opencode`.
        ExtensionType::OpenCode => Ok(empty_parsed_analysis()),
        // SQLite-backed providers are not file streams; see `session::cursor`.
        ExtensionType::Cursor => Ok(empty_parsed_analysis()),
        // SQLite-backed providers are not file streams; see `session::hermes`.
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

/// Yields JSONL records deserialized directly into `T`, offsetting reported line numbers by `lines_read`.
fn iter_jsonl_typed<'a, T>(
    reader: &'a mut BufReader<File>,
    provider: ExtensionType,
    lines_read: usize,
    diagnostics: Rc<RefCell<ParseDiagnostics>>,
    warnings: Rc<RefCell<ParseWarningSummary>>,
    io_failure: Rc<RefCell<Option<String>>>,
) -> impl Iterator<Item = T> + 'a
where
    T: DeserializeOwned + 'a,
{
    let mut line = Vec::with_capacity(buffer::AVG_JSONL_LINE_SIZE);
    let mut line_number = lines_read;

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

/// Yields one [`Value`] per non-empty JSONL line from `reader`, offsetting reported line numbers by `lines_read`.
fn iter_jsonl_values<'a>(
    reader: &'a mut BufReader<File>,
    lines_read: usize,
    diagnostics: Rc<RefCell<ParseDiagnostics>>,
    warnings: Rc<RefCell<ParseWarningSummary>>,
    io_failure: Rc<RefCell<Option<String>>>,
) -> impl Iterator<Item = Value> + 'a {
    // Reuse one byte buffer via `read_until` instead of allocating a `String` per line.
    let mut line = Vec::with_capacity(buffer::AVG_JSONL_LINE_SIZE);
    let mut line_number = lines_read;

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

            match serde_json::from_slice::<Value>(bytes) {
                Ok(value) => return Some(value),
                Err(err) => {
                    warnings.borrow_mut().record_malformed(line_number, &err);
                    diagnostics.borrow_mut().record_malformed();
                }
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
        // These providers either use dedicated readers or SQLite and never enter the typed-stream path.
        ExtensionType::Grok
        | ExtensionType::DeepSeek
        | ExtensionType::Antigravity
        | ExtensionType::OpenCode
        | ExtensionType::Cursor
        | ExtensionType::Hermes => (false, false),
    }
}

fn merge_extra_diagnostics(
    mut parsed: ParsedAnalysis,
    extra: &Rc<RefCell<ParseDiagnostics>>,
) -> ParsedAnalysis {
    parsed.diagnostics.merge(*extra.borrow());
    parsed
}

/// Dispatches a whole-file `Vec<Value>` fallback (such as pretty-printed JSON) to the parser for `ext_type`.
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
            // Gemini requires JSONL and the SQLite providers are not file-backed; mark whole-file fallbacks unrecognized.
            let mut diagnostics = ParseDiagnostics::default();
            for _ in data {
                diagnostics.record_unrecognized();
            }
            ParsedAnalysis::new(empty_analysis(), diagnostics)
        }
        ExtensionType::Grok => parse_grok_session(path, mode)?,
        ExtensionType::DeepSeek => parse_dsh_session(path, mode, tiers)?,
        ExtensionType::Antigravity => parse_antigravity_session(path, mode, tiers)?,
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

/// Stamps the output-level metadata: user, provider name, machine, version.
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

    #[test]
    fn a_malformed_record_is_reported_under_its_physical_line() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("session.jsonl");
        // Record, blank, record, malformed: the failure sits on line 4.
        std::fs::write(&path, "{\"a\":1}\n\n{\"b\":2}\nnot json\n").unwrap();

        let mut reader = BufReader::new(File::open(&path).unwrap());
        let mut lines_read = 0_usize;
        // Mimic streaming entry points that consume the first record before creating the iterator.
        read_next_non_empty_line(&mut reader, &mut lines_read)
            .unwrap()
            .unwrap();
        assert_eq!(lines_read, 1);

        let warnings = Rc::new(RefCell::new(ParseWarningSummary::default()));
        let rest: Vec<Value> = iter_jsonl_values(
            &mut reader,
            lines_read,
            Rc::new(RefCell::new(ParseDiagnostics::default())),
            Rc::clone(&warnings),
            Rc::new(RefCell::new(None)),
        )
        .collect();

        assert_eq!(rest.len(), 1);
        let reason = warnings.borrow().first_reason.clone().unwrap();
        assert!(reason.starts_with("malformed line 4:"), "{reason}");
    }
}
