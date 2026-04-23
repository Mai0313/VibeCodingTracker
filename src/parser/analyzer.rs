use crate::VERSION;
use crate::constants::buffer;
use crate::models::{
    ClaudeCodeLog, CodeAnalysis, CodexLog, CopilotSession, ExtensionType, GeminiSession,
};
use crate::parser::claude_analyzer::analyze_claude_logs;
use crate::parser::codex_analyzer::analyze_codex_conversations_with_mode;
use crate::parser::common_state::ParseMode;
use crate::parser::copilot_analyzer::analyze_copilot_conversations_with_mode;
use crate::parser::gemini_analyzer::{
    analyze_gemini_conversations_with_mode, analyze_gemini_session,
};
use crate::utils::{get_current_user, get_machine_id, read_json, read_jsonl};
use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Typed entry point when the caller already knows the provider.
///
/// Directory scanners use this so that provider classification follows
/// the source path — `~/.claude/projects` is always
/// [`ExtensionType::ClaudeCode`], `~/.codex/sessions` is always
/// [`ExtensionType::Codex`], and so on. This eliminates the whole class
/// of bug where a metadata sentinel record at the top of a session
/// (`permission-mode`, `file-history-snapshot`) would have led a content-
/// based detector to mis-file the log under another provider and silently
/// drop its usage totals.
pub fn analyze_session_file_typed_as<P: AsRef<Path>>(
    path: P,
    provider: ExtensionType,
    mode: ParseMode,
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

/// Fallback dispatch for the pretty-printed JSON path. Operates on an
/// already-materialised `Vec<Value>` — avoided in the hot path, but needed
/// for Gemini/Copilot dumps that span multiple lines.
fn dispatch_by_vec(
    data: Vec<Value>,
    ext_type: ExtensionType,
    mode: ParseMode,
) -> Result<CodeAnalysis> {
    let analysis = match ext_type {
        ExtensionType::ClaudeCode => {
            let logs_iter = data
                .into_iter()
                .filter_map(|v| serde_json::from_value::<ClaudeCodeLog>(v).ok());
            analyze_claude_logs(logs_iter, mode)?
        }
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
