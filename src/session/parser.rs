use crate::VERSION;
use crate::constants::buffer;
use crate::models::{
    ClaudeCodeLog, CodeAnalysis, CodexLog, CopilotEvent, ExtensionType, GeminiSession,
};
use crate::session::claude::{parse_claude_log_values, parse_claude_logs};
use crate::session::codex::parse_codex_logs;
use crate::session::copilot::parse_copilot_events;
use crate::session::detector::{classify_records, detect_extension_type};
use crate::session::gemini::parse_gemini_events;
use crate::session::state::ParseMode;
use crate::utils::{get_current_user, get_machine_id, read_json, read_jsonl};
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

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
/// Returns an error if the file cannot be opened or read, or if the parsed
/// [`crate::CodeAnalysis`] fails to serialise to a `serde_json::Value`.
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
/// Returns an error if the file cannot be opened or read; an empty or
/// unparseable file resolves to an empty [`CodeAnalysis`] rather than an
/// error.
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
/// Returns an error if the file cannot be opened or read, or if the fallback
/// path is reached and the provider cannot be detected (only possible for a
/// non-empty, non-JSONL file). Empty input resolves to an empty
/// [`CodeAnalysis`].
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
    let path = path.as_ref();

    if let Some(analysis) = stream_parse_autodetect(path, mode)? {
        return Ok(analysis);
    }

    // Fallback for anything the streaming path could not peek (e.g. a
    // hand-edited file whose first line is not valid JSON). Every provider
    // we support writes JSONL today, so this path is rarely exercised.
    let data = match read_jsonl(path) {
        Ok(data) => data,
        Err(_) => read_json(path)?,
    };

    if data.is_empty() {
        return Ok(empty_analysis());
    }

    let ext_type = detect_extension_type(&data)?;
    dispatch_by_vec(data, ext_type, mode)
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
/// Returns an error if the file cannot be opened or read. No detection error
/// is possible — `provider` is supplied by the caller. Empty input resolves
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

    if let Some(analysis) = stream_parse_known(path, provider, mode)? {
        return Ok(analysis);
    }

    // Fallback for empty files or anything the streaming peek could not
    // parse on line one.
    let data = match read_jsonl(path) {
        Ok(data) => data,
        Err(_) => read_json(path)?,
    };

    if data.is_empty() {
        return Ok(empty_analysis());
    }

    dispatch_by_vec(data, provider, mode)
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
) -> Result<Option<CodeAnalysis>> {
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

    let analysis = dispatch_streaming_buffered(provider, vec![first_value], reader, mode)?;
    Ok(Some(finalize(analysis, provider)))
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
fn stream_parse_autodetect(path: &Path, mode: ParseMode) -> Result<Option<CodeAnalysis>> {
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);

    let mut buffered: Vec<Value> = Vec::with_capacity(8);
    let mut first_line_was_json = None::<bool>;
    let mut ext: Option<ExtensionType> = None;

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
            Err(_) => {
                // A non-JSON line on the very first record means the file
                // is a pretty-printed single-object dump (Copilot legacy
                // shape or similar); let the caller fall through to
                // `read_json`. Otherwise we have buffered at least one
                // valid record already — stop peeking and let the
                // dispatcher decide.
                if buffered.is_empty() {
                    first_line_was_json = Some(false);
                }
                break;
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
    // back to Codex — a JSONL file with no Claude / Gemini / Copilot
    // markers is almost certainly a Codex log (or a synthetic fixture).
    let ext = ext.unwrap_or(ExtensionType::Codex);
    let analysis = dispatch_streaming_buffered(ext, buffered, reader, mode)?;
    Ok(Some(finalize(analysis, ext)))
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
/// Every supported provider today writes a line-delimited JSONL stream, so
/// all four arms feed the buffered records through the typed shape and
/// then chain the remainder of the reader.
fn dispatch_streaming_buffered(
    ext: ExtensionType,
    buffered: Vec<Value>,
    mut reader: BufReader<File>,
    mode: ParseMode,
) -> Result<CodeAnalysis> {
    match ext {
        ExtensionType::ClaudeCode => {
            // Match the legacy `filter_map(..., Ok).ok()` behaviour: skip a
            // malformed record instead of failing the whole file.
            let buffered_iter = buffered
                .into_iter()
                .filter_map(|v| serde_json::from_value::<ClaudeCodeLog>(v).ok());
            let rest = iter_jsonl_typed::<ClaudeCodeLog>(&mut reader);
            parse_claude_logs(buffered_iter.chain(rest), mode)
        }
        ExtensionType::Codex => {
            let mut logs: Vec<CodexLog> = Vec::with_capacity(64);
            for v in buffered {
                if let Ok(log) = serde_json::from_value::<CodexLog>(v) {
                    logs.push(log);
                }
            }
            for log in iter_jsonl_typed::<CodexLog>(&mut reader) {
                logs.push(log);
            }
            parse_codex_logs(&logs, mode)
        }
        ExtensionType::Copilot => {
            // Copilot CLI emits one event per line under
            // `session-state/<uuid>/events.jsonl`. The streaming path sees
            // this as a sequence of parseable `Value`s whose very first
            // line is `type == "session.start"`.
            let buffered_events = buffered
                .into_iter()
                .filter_map(|v| serde_json::from_value::<CopilotEvent>(v).ok());
            let rest_events = iter_jsonl_typed::<CopilotEvent>(&mut reader);
            parse_copilot_events(buffered_events.chain(rest_events), mode)
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

            let rest_events = iter_jsonl_values(&mut reader);
            parse_gemini_events(session, iter.chain(rest_events), mode)
        }
        // OpenCode stores sessions in a SQLite database, not a JSONL file, so
        // it never flows through the file parser. See `session::opencode`.
        ExtensionType::OpenCode => Ok(empty_analysis()),
    }
}

/// Iterator that yields raw [`Value`]s, one per non-empty line in the reader.
///
/// Used by parsers (Gemini / Copilot) that need to dispatch per-event on a
/// runtime-typed shape before committing to a strongly-typed struct, since
/// different event types carry completely different payloads.
fn iter_jsonl_values<'a>(reader: &'a mut BufReader<File>) -> impl Iterator<Item = Value> + 'a {
    reader.lines().filter_map(|line| {
        let line = line.ok()?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        serde_json::from_str::<Value>(trimmed).ok()
    })
}

/// Iterator that yields `T` values, one per non-empty line in the reader.
/// Lines that fail to deserialise into `T` are silently skipped, matching the
/// legacy `from_value(...).ok()` behaviour the parsers already tolerate.
fn iter_jsonl_typed<'a, T>(reader: &'a mut BufReader<File>) -> impl Iterator<Item = T> + 'a
where
    T: serde::de::DeserializeOwned + 'a,
{
    reader.lines().filter_map(|line| {
        let line = line.ok()?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return None;
        }
        serde_json::from_str::<T>(trimmed).ok()
    })
}

/// Legacy dispatch used by the pretty-printed JSON fallback. Operates on an
/// already-materialised `Vec<Value>` — preferred to be avoided in the hot
/// path, but needed for Gemini/Copilot dumps that span multiple lines.
fn dispatch_by_vec(
    data: Vec<Value>,
    ext_type: ExtensionType,
    mode: ParseMode,
) -> Result<CodeAnalysis> {
    let analysis = match ext_type {
        ExtensionType::ClaudeCode => parse_claude_log_values(data, mode)?,
        ExtensionType::Codex => {
            let logs: Vec<CodexLog> = data
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            parse_codex_logs(&logs, mode)?
        }
        ExtensionType::Copilot | ExtensionType::Gemini | ExtensionType::OpenCode => {
            // Copilot/Gemini only support the JSONL event stream, and OpenCode
            // is read from a SQLite database (see `session::opencode`), not a
            // file. A file that falls through to this branch (e.g. a stray
            // pretty-printed export) has no parser for its shape — return an
            // empty analysis instead of silently mis-parsing.
            empty_analysis()
        }
    };
    Ok(finalize(analysis, ext_type))
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

/// Attaches runtime metadata (user, machine, version) expected in the output.
fn finalize(mut analysis: CodeAnalysis, ext_type: ExtensionType) -> CodeAnalysis {
    analysis.user = get_current_user();
    analysis.extension_name = ext_type.to_string();
    analysis.machine_id = get_machine_id().to_string();
    analysis.insights_version = VERSION.to_string();
    analysis
}
