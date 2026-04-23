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

/// Typed entry point used by the global file cache and any caller that wants
/// to read aggregate fields without paying for a `Value` round-trip.
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

    if let Some(analysis) = stream_analyze_jsonl(path, mode)? {
        return Ok(analysis);
    }

    // Fallback: pretty-printed single-object JSON (Gemini/Copilot) or anything
    // the streaming path could not peek. Keeps legacy behaviour intact.
    let data = match read_jsonl(path) {
        Ok(data) => data,
        Err(_) => read_json(path)?,
    };

    if data.is_empty() {
        return Ok(CodeAnalysis {
            user: String::new(),
            extension_name: String::new(),
            insights_version: String::new(),
            machine_id: String::new(),
            records: Vec::new(),
        });
    }

    let ext_type = detect_extension_type(&data)?;
    dispatch_by_vec(data, ext_type, mode)
}

/// Peeks the first JSON record to detect format, then routes to a type-driven
/// streaming analyzer. Returns `Ok(None)` when the input cannot be parsed as
/// JSONL (e.g. pretty-printed JSON) so the caller can fall back.
fn stream_analyze_jsonl(path: &Path, mode: AnalysisMode) -> Result<Option<CodeAnalysis>> {
    let file = File::open(path)
        .with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);

    let first_line = match peek_first_non_empty_line(&mut reader)? {
        Some(line) => line,
        None => return Ok(None), // empty file
    };

    let first_value: Value = match serde_json::from_str(first_line.trim()) {
        Ok(v) => v,
        Err(_) => return Ok(None), // not JSONL (likely pretty-printed JSON)
    };

    let ext = detect_from_first_value(&first_value);
    let analysis = dispatch_streaming(ext, first_value, reader, mode)?;
    Ok(Some(finalize(analysis, ext)))
}

/// Reads lines from `reader` until it finds a non-empty one. Returns `Ok(None)`
/// at EOF.
fn peek_first_non_empty_line<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .context("Failed to read first line")?;
        if n == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            return Ok(Some(line));
        }
    }
}

/// Derives the extension type from a single parsed record. This mirrors
/// [`detect_extension_type`] but without needing the full `Vec<Value>`.
fn detect_from_first_value(v: &Value) -> ExtensionType {
    if let Some(obj) = v.as_object() {
        if obj.contains_key("sessionId")
            && obj.contains_key("projectHash")
            && obj.contains_key("messages")
        {
            return ExtensionType::Gemini;
        }
        if obj.contains_key("sessionId")
            && obj.contains_key("startTime")
            && obj.contains_key("timeline")
        {
            return ExtensionType::Copilot;
        }
        if obj.contains_key("parentUuid") {
            return ExtensionType::ClaudeCode;
        }
    }
    ExtensionType::Codex
}

/// Streams the rest of the file, parsing each line directly into the lean
/// typed shape for the detected provider.
fn dispatch_streaming(
    ext: ExtensionType,
    first_value: Value,
    mut reader: BufReader<File>,
    mode: AnalysisMode,
) -> Result<CodeAnalysis> {
    match ext {
        ExtensionType::ClaudeCode => {
            // Match the legacy `filter_map(..., Ok).ok()` behaviour: skip a
            // malformed leading record instead of failing the whole file.
            let first_iter = serde_json::from_value::<ClaudeCodeLog>(first_value)
                .ok()
                .into_iter();
            let rest = iter_jsonl_typed::<ClaudeCodeLog>(&mut reader);
            analyze_claude_logs(first_iter.chain(rest), mode)
        }
        ExtensionType::Codex => {
            let mut logs: Vec<CodexLog> = Vec::with_capacity(64);
            if let Ok(first) = serde_json::from_value::<CodexLog>(first_value) {
                logs.push(first);
            }
            for log in iter_jsonl_typed::<CodexLog>(&mut reader) {
                logs.push(log);
            }
            analyze_codex_conversations_with_mode(&logs, mode)
        }
        ExtensionType::Copilot => {
            let session: CopilotSession = serde_json::from_value(first_value)
                .context("Failed to parse Copilot session")?;
            analyze_copilot_conversations_with_mode(session, mode)
        }
        ExtensionType::Gemini => {
            let session: GeminiSession = serde_json::from_value(first_value)
                .context("Failed to parse Gemini session")?;
            analyze_gemini_session(session, mode)
        }
    }
}

/// Iterator that yields `T` values, one per non-empty line in the reader.
/// Lines that fail to deserialise into `T` are silently skipped, matching the
/// legacy `from_value(...).ok()` behaviour the analyzers already tolerate.
fn iter_jsonl_typed<'a, T>(
    reader: &'a mut BufReader<File>,
) -> impl Iterator<Item = T> + 'a
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

/// Attaches runtime metadata (user, machine, version) expected in the output.
fn finalize(mut analysis: CodeAnalysis, ext_type: ExtensionType) -> CodeAnalysis {
    analysis.user = get_current_user();
    analysis.extension_name = ext_type.to_string();
    analysis.machine_id = get_machine_id().to_string();
    analysis.insights_version = VERSION.to_string();
    analysis
}
