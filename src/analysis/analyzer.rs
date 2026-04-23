use crate::VERSION;
use crate::analysis::claude_analyzer::{
    analyze_claude_conversations_with_mode, analyze_claude_logs,
};
use crate::analysis::codex_analyzer::analyze_codex_conversations_with_mode;
use crate::analysis::common_state::AnalysisMode;
use crate::analysis::copilot_analyzer::analyze_copilot_conversations_with_mode;
use crate::analysis::detector::detect_extension_type;
use crate::analysis::gemini_analyzer::{
    analyze_gemini_conversations_with_mode, analyze_gemini_session,
};
use crate::constants::buffer;
use crate::models::{
    ClaudeCodeLog, CodeAnalysis, CodexLog, CopilotSession, ExtensionType, GeminiSession,
};
use crate::utils::{get_current_user, get_machine_id, read_json, read_jsonl};
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// How many JSONL records the auto-detect path may pre-parse before deciding
/// which provider the file belongs to. Claude Code can prefix a session file
/// with a handful of metadata sentinel records (`permission-mode`,
/// `file-history-snapshot`, `queue-operation`) that don't carry the
/// `parentUuid` field the detector keys on, so looking at only the first line
/// misclassifies the file as Codex and silently drops its usage. Eight lines
/// is comfortably larger than any sentinel prelude Claude writes today.
const AUTODETECT_PEEK_LINES: usize = 8;

/// Analyzes a session file (JSONL or JSON) and returns the result as a
/// `serde_json::Value` (the CLI single-file dump path).
///
/// Internally this is a thin wrapper over [`analyze_jsonl_file_typed`]; the
/// conversion to `Value` happens once at the edge here rather than inside the
/// cache, which keeps long sessions from being duplicated between typed and
/// `Value` forms when multiple commands run against the same file.
pub fn analyze_jsonl_file<P: AsRef<Path>>(path: P) -> Result<Value> {
    let analysis = analyze_jsonl_file_typed(path)?;
    if analysis.records.is_empty() && analysis.extension_name.is_empty() {
        // Preserve historical behaviour: empty input → `{}` rather than a
        // fully-populated but empty `CodeAnalysis` object.
        return Ok(serde_json::json!({}));
    }
    Ok(serde_json::to_value(&analysis)?)
}

/// Typed entry point that auto-detects the provider from file contents.
///
/// Prefer [`analyze_session_file_typed_as`] whenever the caller already knows
/// which provider the file belongs to (e.g. when walking
/// `~/.claude/projects/*.jsonl` vs `~/.codex/sessions/*.jsonl`). Content-based
/// detection is only intended for the CLI single-file path where the user
/// hands us an arbitrary path.
///
/// Parses in [`AnalysisMode::Full`] — for callers that only consume tool
/// counts and token usage (usage / aggregated analysis), use
/// [`analyze_jsonl_file_typed_with_mode`] with [`AnalysisMode::UsageOnly`]
/// to avoid allocating `write_file_details`/`edit_file_details` bodies.
pub fn analyze_jsonl_file_typed<P: AsRef<Path>>(path: P) -> Result<CodeAnalysis> {
    analyze_jsonl_file_typed_with_mode(path, AnalysisMode::Full)
}

pub fn analyze_jsonl_file_typed_with_mode<P: AsRef<Path>>(
    path: P,
    mode: AnalysisMode,
) -> Result<CodeAnalysis> {
    let path = path.as_ref();

    if let Some(analysis) = stream_analyze_autodetect(path, mode)? {
        return Ok(analysis);
    }

    // Fallback: pretty-printed single-object JSON (Gemini/Copilot) or anything
    // the streaming path could not peek. Keeps legacy behaviour intact.
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
/// Directory scanners should use this instead of [`analyze_jsonl_file_typed`]
/// so that provider classification follows the source path — `~/.claude/projects`
/// is always [`ExtensionType::ClaudeCode`], `~/.codex/sessions` is always
/// [`ExtensionType::Codex`], and so on. This eliminates a whole class of bug
/// where a metadata sentinel record at the top of a session (`permission-mode`,
/// `file-history-snapshot`) leads the content-based detector to mis-file the
/// log under another provider and silently drop its usage totals.
pub fn analyze_session_file_typed_as<P: AsRef<Path>>(
    path: P,
    provider: ExtensionType,
    mode: AnalysisMode,
) -> Result<CodeAnalysis> {
    let path = path.as_ref();

    if let Some(analysis) = stream_analyze_known(path, provider, mode)? {
        return Ok(analysis);
    }

    // Pretty-printed JSON dumps (Gemini/Copilot exports) or empty files.
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
fn stream_analyze_known(
    path: &Path,
    provider: ExtensionType,
    mode: AnalysisMode,
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
/// Pre-buffers up to [`AUTODETECT_PEEK_LINES`] records so that content-based
/// detection sees past any Claude metadata preamble (which doesn't carry the
/// `parentUuid` marker). The pre-parsed records are then handed to the
/// dispatcher alongside the remainder of the reader so no record is read
/// twice.
fn stream_analyze_autodetect(path: &Path, mode: AnalysisMode) -> Result<Option<CodeAnalysis>> {
    let file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);

    let mut buffered: Vec<Value> = Vec::with_capacity(AUTODETECT_PEEK_LINES);
    let mut first_line_was_json = None::<bool>;

    while buffered.len() < AUTODETECT_PEEK_LINES {
        let line = match read_next_non_empty_line(&mut reader)? {
            Some(line) => line,
            None => break,
        };

        match serde_json::from_str::<Value>(line.trim()) {
            Ok(v) => {
                first_line_was_json.get_or_insert(true);
                buffered.push(v);
            }
            Err(_) => {
                // If we've already buffered at least one valid JSONL record,
                // stop buffering and let the dispatcher handle what we have.
                // If this is the very first line, the whole file is likely
                // pretty-printed JSON — bail so the caller falls back.
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

    let ext = detect_extension_type(&buffered)?;
    let analysis = dispatch_streaming_buffered(ext, buffered, reader, mode)?;
    Ok(Some(finalize(analysis, ext)))
}

/// Reads lines from `reader` until it finds a non-empty one. Returns `Ok(None)`
/// at EOF.
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
/// Claude/Codex paths feed the buffered records through the typed shape and
/// then chain the remainder of the reader. Copilot/Gemini are pretty-printed
/// single-object formats — they should never reach this path in practice
/// (their first line fails JSONL parse, sending the caller down the
/// `read_json` fallback), but the arms are kept for completeness so a hand-
/// crafted one-line JSON fixture still works.
fn dispatch_streaming_buffered(
    ext: ExtensionType,
    buffered: Vec<Value>,
    mut reader: BufReader<File>,
    mode: AnalysisMode,
) -> Result<CodeAnalysis> {
    match ext {
        ExtensionType::ClaudeCode => {
            // Match the legacy `filter_map(..., Ok).ok()` behaviour: skip a
            // malformed record instead of failing the whole file.
            let buffered_iter = buffered
                .into_iter()
                .filter_map(|v| serde_json::from_value::<ClaudeCodeLog>(v).ok());
            let rest = iter_jsonl_typed::<ClaudeCodeLog>(&mut reader);
            analyze_claude_logs(buffered_iter.chain(rest), mode)
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
            analyze_codex_conversations_with_mode(&logs, mode)
        }
        ExtensionType::Copilot => {
            let first = buffered
                .into_iter()
                .next()
                .context("Copilot session missing top-level object")?;
            let session: CopilotSession =
                serde_json::from_value(first).context("Failed to parse Copilot session")?;
            analyze_copilot_conversations_with_mode(session, mode)
        }
        ExtensionType::Gemini => {
            let first = buffered
                .into_iter()
                .next()
                .context("Gemini session missing top-level object")?;
            let session: GeminiSession =
                serde_json::from_value(first).context("Failed to parse Gemini session")?;
            analyze_gemini_session(session, mode)
        }
    }
}

/// Iterator that yields `T` values, one per non-empty line in the reader.
/// Lines that fail to deserialise into `T` are silently skipped, matching the
/// legacy `from_value(...).ok()` behaviour the analyzers already tolerate.
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
    mode: AnalysisMode,
) -> Result<CodeAnalysis> {
    let analysis = match ext_type {
        ExtensionType::ClaudeCode => analyze_claude_conversations_with_mode(data, mode)?,
        ExtensionType::Codex => {
            let logs: Vec<CodexLog> = data
                .into_iter()
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            analyze_codex_conversations_with_mode(&logs, mode)?
        }
        ExtensionType::Copilot => {
            let session: CopilotSession = serde_json::from_value(data[0].clone())?;
            analyze_copilot_conversations_with_mode(session, mode)?
        }
        ExtensionType::Gemini => analyze_gemini_conversations_with_mode(data, mode)?,
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
