//! Grok CLI `signals.json` and sibling session-history parser.

use crate::constants::{FastHashMap, buffer};
use crate::models::{CodeAnalysis, GrokSignals, GrokSummary};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::parser::trim_ascii_whitespace;
use crate::session::state::{CountedJsonString, ParseMode, SessionParseState};
use crate::utils::parse_iso_timestamp;
use anyhow::{Context, Result, bail};
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use std::borrow::Cow;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Debug)]
struct PendingToolCall {
    name: String,
    input: Value,
}

#[derive(Deserialize)]
struct BorrowedGrokUpdateLine<'a> {
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default, borrow)]
    params: Option<BorrowedGrokParams<'a>>,
}

#[derive(Deserialize)]
struct BorrowedGrokParams<'a> {
    #[serde(default, borrow)]
    update: Option<BorrowedGrokUpdate<'a>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BorrowedGrokUpdate<'a> {
    #[serde(default, borrow)]
    session_update: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    tool_call_id: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    title: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    status: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    raw_input: Option<&'a RawValue>,
    #[serde(default, borrow)]
    raw_output: Option<&'a RawValue>,
    #[serde(default, borrow, rename = "_meta")]
    meta: Option<BorrowedGrokMeta<'a>>,
}

#[derive(Deserialize)]
struct BorrowedGrokMeta<'a> {
    #[serde(default, borrow, rename = "x.ai/tool")]
    xai_tool: Option<BorrowedGrokToolMeta<'a>>,
}

#[derive(Deserialize)]
struct BorrowedGrokToolMeta<'a> {
    #[serde(default, borrow)]
    name: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct BorrowedGrokReadOutput<'a> {
    #[serde(default, borrow, rename = "FileContent")]
    file_content: Option<BorrowedGrokFileContent<'a>>,
}

#[derive(Deserialize)]
struct BorrowedGrokFileContent<'a> {
    #[serde(default, borrow)]
    absolute_path: Option<Cow<'a, str>>,
    #[serde(default)]
    content: Option<CountedJsonString>,
}

/// Returns whether a JSON value carries Grok's aggregate signals envelope.
pub(crate) fn is_grok_signals(value: &Value) -> bool {
    value.get("primaryModelId").is_some()
        && value.get("contextTokensUsed").is_some()
        && (value.get("contextWindowTokens").is_some() || value.get("toolsUsed").is_some())
}

/// Parses a Grok `signals.json` file for a known provider, returning `Ok(None)` if empty.
pub(crate) fn parse_grok_session_known(
    path: &Path,
    mode: ParseMode,
) -> Result<Option<ParsedAnalysis>> {
    let bytes = read_signals_bytes(path)?;
    if trim_ascii_whitespace(&bytes).is_empty() {
        return Ok(None);
    }
    parse_grok_signals_bytes(path, &bytes, mode).map(Some)
}

/// Parses a Grok `signals.json` file and its optional session-history siblings.
pub(crate) fn parse_grok_session(path: &Path, mode: ParseMode) -> Result<ParsedAnalysis> {
    let bytes = read_signals_bytes(path)?;
    parse_grok_signals_bytes(path, &bytes, mode)
}

fn read_signals_bytes(path: &Path) -> Result<Vec<u8>> {
    let mut file =
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut bytes = Vec::with_capacity(
        file.metadata()
            .ok()
            .and_then(|meta| usize::try_from(meta.len()).ok())
            .unwrap_or(512),
    );
    file.read_to_end(&mut bytes)
        .with_context(|| format!("Failed to read Grok signals: {}", path.display()))?;
    Ok(bytes)
}

fn parse_grok_signals_bytes(path: &Path, bytes: &[u8], mode: ParseMode) -> Result<ParsedAnalysis> {
    let signals_value: Value = serde_json::from_slice(bytes)
        .with_context(|| format!("Failed to parse Grok signals: {}", path.display()))?;

    if !is_grok_signals(&signals_value) {
        bail!("{} is not a recognized Grok signals file", path.display());
    }

    let signals: GrokSignals = serde_json::from_value(signals_value)
        .with_context(|| format!("Failed to normalize Grok signals: {}", path.display()))?;
    let mut diagnostics = ParseDiagnostics::default();
    diagnostics.record_recognized_source();
    diagnostics.record_relevant(true);

    let summary = read_summary(path, &mut diagnostics)?;
    let model = resolved_model(&signals, summary.as_ref());
    if model.is_empty() {
        bail!("Grok signals file {} has no model id", path.display());
    }

    let mut state = SessionParseState::with_mode(mode);
    state.note_model(&model);
    apply_metadata(&mut state, path, summary.as_ref())?;
    parse_updates(path, &mut state, &mut diagnostics)?;

    // `contextTokensUsed` is a context gauge rather than billed usage; store it in `cache_read_input_tokens` so `CostSource::GrokGauge` prices it at the cache-read rate.
    let estimated_tokens = i64::try_from(signals.context_tokens_used).unwrap_or(i64::MAX);
    let mut usage = FastHashMap::default();
    usage.insert(
        model,
        json!({
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_input_tokens": estimated_tokens,
            "cache_creation_input_tokens": 0
        }),
    );

    Ok(ParsedAnalysis::new(
        CodeAnalysis {
            user: String::new(),
            extension_name: String::new(),
            insights_version: String::new(),
            machine_id: String::new(),
            records: vec![state.into_record(usage)],
        },
        diagnostics,
    ))
}

fn read_summary(
    signals_path: &Path,
    diagnostics: &mut ParseDiagnostics,
) -> Result<Option<GrokSummary>> {
    let path = signals_path.with_file_name("summary.json");
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to open Grok summary: {}", path.display()));
        }
    };
    parse_summary_reader(BufReader::new(file), &path, diagnostics)
}

fn parse_summary_reader<R: Read>(
    reader: R,
    path: &Path,
    diagnostics: &mut ParseDiagnostics,
) -> Result<Option<GrokSummary>> {
    match serde_json::from_reader(reader) {
        Ok(summary) => Ok(Some(summary)),
        Err(error) if error.is_io() => {
            Err(error).with_context(|| format!("Failed to read Grok summary: {}", path.display()))
        }
        Err(_) => {
            diagnostics.record_malformed();
            Ok(None)
        }
    }
}

fn resolved_model(signals: &GrokSignals, summary: Option<&GrokSummary>) -> String {
    let primary = signals.primary_model_id.trim();
    if !primary.is_empty() {
        return primary.to_string();
    }
    if let Some(model) = signals
        .models_used
        .iter()
        .find(|model| !model.trim().is_empty())
    {
        return model.trim().to_string();
    }
    summary
        .map(|summary| summary.current_model_id.trim().to_string())
        .unwrap_or_default()
}

fn apply_metadata(
    state: &mut SessionParseState,
    path: &Path,
    summary: Option<&GrokSummary>,
) -> Result<()> {
    if let Some(summary) = summary {
        state.task_id.clone_from(&summary.info.id);
        state.folder_path.clone_from(&summary.info.cwd);
        state.git_remote = summary.git_remotes.first().cloned().unwrap_or_default();
        state.last_ts = parse_iso_timestamp(&summary.updated_at)
            .max(parse_iso_timestamp(&summary.last_active_at));
    }

    if state.task_id.is_empty() {
        state.task_id = path
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
    }
    if state.folder_path.is_empty() {
        state.folder_path = read_cwd_marker(path)?
            .or_else(|| decode_workspace_dir(path))
            .unwrap_or_default();
    }
    if state.last_ts == 0 {
        state.last_ts = file_modified_millis(path);
    }
    Ok(())
}

fn read_cwd_marker(signals_path: &Path) -> Result<Option<String>> {
    let Some(workspace_dir) = signals_path.parent().and_then(Path::parent) else {
        return Ok(None);
    };
    let path = workspace_dir.join(".cwd");
    let cwd = match fs::read_to_string(&path) {
        Ok(cwd) => cwd,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Failed to read Grok cwd marker: {}", path.display()));
        }
    };
    let cwd = cwd.trim();
    Ok((!cwd.is_empty()).then(|| cwd.to_string()))
}

fn decode_workspace_dir(signals_path: &Path) -> Option<String> {
    let encoded = signals_path.parent()?.parent()?.file_name()?.to_str()?;
    let decoded = percent_decode_str(encoded).decode_utf8().ok()?;
    let decoded = decoded.trim();
    (!decoded.is_empty() && decoded != encoded && Path::new(decoded).is_absolute())
        .then(|| decoded.to_string())
}

fn file_modified_millis(path: &Path) -> i64 {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn parse_updates(
    signals_path: &Path,
    state: &mut SessionParseState,
    diagnostics: &mut ParseDiagnostics,
) -> Result<()> {
    let path = signals_path.with_file_name("updates.jsonl");
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Failed to open Grok updates: {}", path.display()));
        }
    };

    let mut calls = FastHashMap::<String, PendingToolCall>::default();
    let mut reader = BufReader::with_capacity(buffer::FILE_READ_BUFFER, file);
    let mut line = Vec::with_capacity(buffer::AVG_JSONL_LINE_SIZE);
    let mut line_number = 0_usize;

    loop {
        line.clear();
        line_number += 1;
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "Failed to read Grok update line {}: {}",
                        line_number,
                        path.display()
                    )
                });
            }
        }
        let text = std::str::from_utf8(&line)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
            .with_context(|| {
                format!(
                    "Failed to read Grok update line {}: {}",
                    line_number,
                    path.display()
                )
            })?;
        let bytes = trim_ascii_whitespace(text.as_bytes());
        if bytes.is_empty() {
            continue;
        }
        if bytes.starts_with(b"{")
            && let Ok(parsed) = serde_json::from_slice::<BorrowedGrokUpdateLine<'_>>(bytes)
        {
            handle_borrowed_update(parsed, &mut calls, state, diagnostics);
            continue;
        }
        match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => handle_value_update(&value, &mut calls, state, diagnostics),
            Err(_) => diagnostics.record_malformed(),
        }
    }
    Ok(())
}

fn handle_borrowed_update(
    parsed: BorrowedGrokUpdateLine<'_>,
    calls: &mut FastHashMap<String, PendingToolCall>,
    state: &mut SessionParseState,
    diagnostics: &mut ParseDiagnostics,
) {
    let Some(update) = parsed.params.and_then(|params| params.update) else {
        return;
    };
    let Some(kind) = update.session_update.as_deref() else {
        return;
    };
    let Some(id) = update.tool_call_id.as_deref() else {
        return;
    };

    match kind {
        "tool_call" => {
            let name = update
                .meta
                .and_then(|meta| meta.xai_tool)
                .and_then(|tool| tool.name)
                .or(update.title)
                .unwrap_or(Cow::Borrowed(""));
            let input = if is_tracked_tool(name.as_ref()) {
                update
                    .raw_input
                    .and_then(|raw| serde_json::from_str::<Value>(raw.get()).ok())
                    .unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            calls.insert(
                id.to_string(),
                PendingToolCall {
                    name: name.into_owned(),
                    input,
                },
            );
        }
        "tool_call_update" => {
            let Some(status) = update.status.as_deref() else {
                return;
            };
            if status != "completed" && status != "failed" {
                return;
            }
            let Some(call) = calls.remove(id) else {
                return;
            };
            let tracked = is_tracked_tool(&call.name);
            if !tracked && !is_known_unmodeled_tool(&call.name) {
                return;
            }
            diagnostics.record_recognized_source();
            if !tracked {
                // Recognized above to avoid schema drift, but mapped to no metric.
                return;
            }
            if status != "completed" {
                diagnostics.record_relevant(true);
                return;
            }
            let timestamp = parsed.timestamp.unwrap_or_default().saturating_mul(1_000);
            state.last_ts = state.last_ts.max(timestamp);

            // Count `read_file` bodies directly from raw JSON in `UsageOnly` without allocating the file text.
            if matches!(state.mode, ParseMode::UsageOnly)
                && call.name == "read_file"
                && let Some(raw_output) = update.raw_output
                && raw_output.get().starts_with('{')
                && let Ok(read_out) =
                    serde_json::from_str::<BorrowedGrokReadOutput<'_>>(raw_output.get())
            {
                let path = read_out
                    .file_content
                    .as_ref()
                    .and_then(|fc| fc.absolute_path.as_deref())
                    .or_else(|| call.input.get("target_file").and_then(Value::as_str))
                    .filter(|path| !path.is_empty());
                let normalized = match path {
                    Some(path) => {
                        let metrics = read_out
                            .file_content
                            .as_ref()
                            .and_then(|fc| fc.content)
                            .map(|c| c.0)
                            .unwrap_or_default();
                        if metrics.is_empty() {
                            state.tool_counts.read += 1;
                            state.add_non_text_read_path(path);
                        } else {
                            state.add_read_counts(path, metrics.lines, metrics.chars);
                        }
                        true
                    }
                    None => false,
                };
                diagnostics.record_relevant(normalized);
                return;
            }

            let output = update
                .raw_output
                .and_then(|raw| serde_json::from_str::<Value>(raw.get()).ok())
                .unwrap_or(Value::Null);
            let normalized = apply_completed_tool(state, &call, timestamp, &output);
            diagnostics.record_relevant(normalized);
        }
        _ => {}
    }
}

fn handle_value_update(
    value: &Value,
    calls: &mut FastHashMap<String, PendingToolCall>,
    state: &mut SessionParseState,
    diagnostics: &mut ParseDiagnostics,
) {
    let Some(update) = value.pointer("/params/update") else {
        return;
    };
    let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
        return;
    };
    let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
        return;
    };

    match kind {
        "tool_call" => {
            let name = update
                .pointer("/_meta/x.ai~1tool/name")
                .and_then(Value::as_str)
                .or_else(|| update.get("title").and_then(Value::as_str))
                .unwrap_or_default()
                .to_string();
            calls.insert(
                id.to_string(),
                PendingToolCall {
                    name,
                    input: update.get("rawInput").cloned().unwrap_or(Value::Null),
                },
            );
        }
        "tool_call_update" => {
            let Some(status) = update.get("status").and_then(Value::as_str) else {
                return;
            };
            if status != "completed" && status != "failed" {
                return;
            }
            let Some(call) = calls.remove(id) else {
                return;
            };
            let tracked = is_tracked_tool(&call.name);
            if !tracked && !is_known_unmodeled_tool(&call.name) {
                return;
            }
            diagnostics.record_recognized_source();
            if !tracked {
                return;
            }
            if status != "completed" {
                diagnostics.record_relevant(true);
                return;
            }
            let timestamp = value
                .get("timestamp")
                .and_then(Value::as_i64)
                .unwrap_or_default()
                .saturating_mul(1_000);
            state.last_ts = state.last_ts.max(timestamp);
            let output = update.get("rawOutput").unwrap_or(&Value::Null);
            let normalized = apply_completed_tool(state, &call, timestamp, output);
            diagnostics.record_relevant(normalized);
        }
        _ => {}
    }
}

fn is_tracked_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "grep" | "write" | "search_replace" | "run_terminal_command" | "todo_write"
    )
}

/// Returns whether `name` is a recognized Grok orchestration tool intentionally excluded from metrics.
fn is_known_unmodeled_tool(name: &str) -> bool {
    matches!(name, "spawn_subagent" | "get_command_or_subagent_output")
}

fn apply_completed_tool(
    state: &mut SessionParseState,
    call: &PendingToolCall,
    timestamp: i64,
    output: &Value,
) -> bool {
    match call.name.as_str() {
        "read_file" => {
            let Some(path) = output
                .pointer("/FileContent/absolute_path")
                .and_then(Value::as_str)
                .or_else(|| call.input.get("target_file").and_then(Value::as_str))
                .filter(|path| !path.is_empty())
            else {
                return false;
            };
            let content = output
                .pointer("/FileContent/content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // `add_read_detail` ignores content that trims to zero lines, so newline-only reads take the empty branch to keep the invocation count.
            if content.trim_end_matches('\n').is_empty() {
                state.tool_counts.read += 1;
                state.add_non_text_read_path(path);
            } else {
                state.add_read_detail(path, content, timestamp);
            }
            true
        }
        "grep" => {
            let Some(exit_code) = output.get("exit_code").and_then(Value::as_i64) else {
                return false;
            };
            // `grep` exit code 1 means "no matches" (still a completed Read); other non-zero codes are failures.
            if matches!(exit_code, 0 | 1) {
                state.tool_counts.read += 1;
            }
            true
        }
        "write" => {
            let Some(path) = mutation_output_path(output)
                .or_else(|| call.input.get("file_path").and_then(Value::as_str))
                .filter(|path| !path.is_empty())
            else {
                return false;
            };
            let Some(content) = call.input.get("content").and_then(Value::as_str) else {
                return false;
            };
            state.add_write_detail(path, content, timestamp);
            true
        }
        "search_replace" => {
            let Some(path) = mutation_output_path(output)
                .or_else(|| call.input.get("file_path").and_then(Value::as_str))
                .filter(|path| !path.is_empty())
            else {
                return false;
            };
            if let Some(details) = search_replace_details(output) {
                // Count one Edit invocation per tool lifecycle regardless of hunk count.
                let edit_calls = state.tool_counts.edit;
                for detail in details {
                    if let Some((old, new)) = search_replace_detail_strings(detail) {
                        state.add_edit_detail_raw(path, old, new, timestamp);
                    }
                }
                state.tool_counts.edit = edit_calls.saturating_add(1);
                return true;
            }
            let Some(old) = call.input.get("old_string").and_then(Value::as_str) else {
                return false;
            };
            let Some(new) = call.input.get("new_string").and_then(Value::as_str) else {
                return false;
            };
            state.add_edit_detail_raw(path, old, new, timestamp);
            true
        }
        "run_terminal_command" => {
            let Some(command) = call
                .input
                .get("command")
                .and_then(Value::as_str)
                .filter(|command| !command.trim().is_empty())
            else {
                return false;
            };
            let description = call
                .input
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            state.add_run_command(command, description, timestamp);
            true
        }
        "todo_write" => {
            state.tool_counts.todo_write += 1;
            true
        }
        _ => false,
    }
}

fn mutation_output_path(output: &Value) -> Option<&str> {
    output
        .pointer("/EditsApplied/absolute_path")
        .and_then(Value::as_str)
}

fn search_replace_details(output: &Value) -> Option<&[Value]> {
    let details = output.pointer("/EditsApplied/edits/details")?.as_array()?;
    (!details.is_empty()
        && details
            .iter()
            .all(|detail| search_replace_detail_strings(detail).is_some()))
    .then_some(details)
}

fn search_replace_detail_strings(detail: &Value) -> Option<(&str, &str)> {
    Some((
        detail.get("old_string")?.as_str()?,
        detail.get("new_string")?.as_str()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    struct FailingSummaryReader {
        bytes: &'static [u8],
        offset: usize,
    }

    impl Read for FailingSummaryReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.offset == self.bytes.len() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "transient summary read failure",
                ));
            }
            let count = buffer.len().min(self.bytes.len() - self.offset);
            buffer[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
            self.offset += count;
            Ok(count)
        }
    }

    #[test]
    fn detects_only_distinctive_grok_signals() {
        assert!(is_grok_signals(&json!({
            "primaryModelId": "grok-test",
            "contextTokensUsed": 42,
            "contextWindowTokens": 100
        })));
        assert!(!is_grok_signals(&json!({
            "primaryModelId": "another-provider",
            "contextTokensUsed": 42
        })));
    }

    #[test]
    fn primary_model_wins_over_model_list_and_alias() {
        let signals = GrokSignals {
            primary_model_id: "grok-4.5".to_string(),
            models_used: vec!["grok-other".to_string()],
            context_tokens_used: 0,
        };
        let summary = GrokSummary {
            current_model_id: "grok".to_string(),
            ..GrokSummary::default()
        };
        assert_eq!(resolved_model(&signals, Some(&summary)), "grok-4.5");
    }

    #[test]
    fn summary_io_failures_remain_retryable() {
        let mut diagnostics = ParseDiagnostics::default();
        let reader = FailingSummaryReader {
            bytes: br#"{"info":{"id":"session""#,
            offset: 0,
        };

        let error =
            parse_summary_reader(reader, Path::new("summary.json"), &mut diagnostics).unwrap_err();

        assert!(error.to_string().contains("Failed to read Grok summary"));
        assert_eq!(diagnostics, ParseDiagnostics::default());
    }

    #[test]
    fn unreadable_updates_surface_a_retryable_source_error() {
        let temp = tempfile::tempdir().unwrap();
        let session = temp.path().join("workspace").join("session-id");
        std::fs::create_dir_all(&session).unwrap();
        let signals = session.join("signals.json");
        std::fs::write(
            &signals,
            json!({
                "primaryModelId": "grok-test",
                "modelsUsed": ["grok-test"],
                "contextTokensUsed": 42,
                "contextWindowTokens": 200_000
            })
            .to_string(),
        )
        .unwrap();
        std::fs::create_dir(session.join("updates.jsonl")).unwrap();

        let error = parse_grok_session(&signals, ParseMode::UsageOnly).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Failed to read Grok update line")
        );
    }

    #[test]
    fn replace_all_uses_each_completed_edit_detail_once() {
        let mut state = SessionParseState::new();
        state.folder_path = "/wrong/workspace".to_string();
        let call = PendingToolCall {
            name: "search_replace".to_string(),
            input: json!({
                "file_path": "src/lib.rs",
                "old_string": "request old",
                "new_string": "request new",
                "replace_all": true
            }),
        };
        let output = json!({
            "type": "SearchReplace",
            "EditsApplied": {
                "absolute_path": "/workspace/demo/src/lib.rs",
                "edits": {
                    "details": [
                        {"old_string": "matched one", "new_string": "first\nline\n"},
                        {"old_string": "matched two", "new_string": "second"}
                    ]
                }
            }
        });

        assert!(apply_completed_tool(&mut state, &call, 123, &output));
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.total_edit_lines, 3);
        assert_eq!(state.edit_details.len(), 2);
        assert_eq!(
            state.edit_details[0].base.file_path,
            "/workspace/demo/src/lib.rs"
        );
        assert_eq!(state.edit_details[0].old_string, "matched one");
        assert_eq!(state.edit_details[1].old_string, "matched two");
    }

    #[test]
    fn newline_only_read_still_counts_the_invocation() {
        let mut state = SessionParseState::new();
        let call = PendingToolCall {
            name: "read_file".to_string(),
            input: json!({"target_file": "/workspace/blank.txt"}),
        };
        let output = json!({
            "type": "Read",
            "FileContent": {"absolute_path": "/workspace/blank.txt", "content": "\n\n"}
        });

        assert!(apply_completed_tool(&mut state, &call, 123, &output));
        assert_eq!(state.tool_counts.read, 1);
        assert_eq!(state.total_read_lines, 0);
        assert!(state.unique_files.contains("/workspace/blank.txt"));
    }

    #[test]
    fn write_prefers_completed_absolute_path() {
        let mut state = SessionParseState::new();
        state.folder_path = "/wrong/workspace".to_string();
        let call = PendingToolCall {
            name: "write".to_string(),
            input: json!({"file_path": "src/new.rs", "content": "new file\n"}),
        };
        let output = json!({
            "type": "SearchReplace",
            "EditsApplied": {"absolute_path": "/workspace/demo/src/new.rs"}
        });

        assert!(apply_completed_tool(&mut state, &call, 123, &output));
        assert_eq!(state.tool_counts.write, 1);
        assert_eq!(state.write_details.len(), 1);
        assert_eq!(
            state.write_details[0].base.file_path,
            "/workspace/demo/src/new.rs"
        );
    }

    #[test]
    fn known_unmodeled_orchestration_tools_are_recognized_and_ignored() {
        for tool in ["spawn_subagent", "get_command_or_subagent_output"] {
            let temp = tempfile::tempdir().unwrap();
            let signals = temp.path().join("signals.json");
            let call = json!({
                "method": "session/update",
                "params": {"update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "orch-1",
                    "title": tool,
                    "rawInput": {"task": "investigate"},
                    "_meta": {"x.ai/tool": {"name": tool}}
                }},
                "timestamp": 1_767_225_600
            });
            let update = json!({
                "method": "session/update",
                "params": {"update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "orch-1",
                    "status": "completed",
                    "rawOutput": {"type": "TaskOutput", "text": "done"}
                }},
                "timestamp": 1_767_225_601
            });
            std::fs::write(
                temp.path().join("updates.jsonl"),
                format!("{call}\n{update}\n"),
            )
            .unwrap();

            let mut state = SessionParseState::new();
            let mut diagnostics = ParseDiagnostics::default();
            parse_updates(&signals, &mut state, &mut diagnostics).unwrap();

            assert_eq!(state.tool_counts.read, 0);
            assert_eq!(state.tool_counts.write, 0);
            assert_eq!(state.tool_counts.edit, 0);
            assert_eq!(state.tool_counts.bash, 0);
            assert_eq!(state.tool_counts.todo_write, 0);
            assert_eq!(diagnostics.partial_failure_count(), 0);
            assert_eq!(diagnostics.recognized_records, 1);
        }
    }

    #[test]
    fn encoded_workspace_name_recovers_missing_summary_cwd() {
        use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

        let temp = tempfile::tempdir().unwrap();
        let expected = temp.path().join("project");
        let encoded =
            utf8_percent_encode(expected.to_string_lossy().as_ref(), NON_ALPHANUMERIC).to_string();
        let session = temp.path().join(encoded).join("session-id");
        std::fs::create_dir_all(&session).unwrap();
        let signals = session.join("signals.json");
        std::fs::write(&signals, "").unwrap();

        let mut state = SessionParseState::new();
        apply_metadata(&mut state, &signals, None).unwrap();

        assert_eq!(state.task_id, "session-id");
        assert_eq!(state.folder_path, expected.to_string_lossy());
    }

    #[test]
    fn known_grok_empty_signals_returns_none_and_usage_only_matches_full() {
        let temp = tempfile::tempdir().unwrap();
        let empty = temp.path().join("signals.json");
        std::fs::write(&empty, "  \n").unwrap();
        assert!(
            parse_grok_session_known(&empty, ParseMode::UsageOnly)
                .unwrap()
                .is_none()
        );

        let fixture = vct_test_support::fixture("sessions/grok/signals.json");
        let full = parse_grok_session_known(&fixture, ParseMode::Full)
            .unwrap()
            .unwrap();
        let usage_only = parse_grok_session_known(&fixture, ParseMode::UsageOnly)
            .unwrap()
            .unwrap();
        assert_eq!(usage_only.diagnostics, full.diagnostics);
        let f = &full.analysis.records[0];
        let u = &usage_only.analysis.records[0];
        assert_eq!(u.total_unique_files, f.total_unique_files);
        assert_eq!(u.total_read_lines, f.total_read_lines);
        assert_eq!(u.total_read_characters, f.total_read_characters);
        assert_eq!(u.total_write_lines, f.total_write_lines);
        assert_eq!(u.total_write_characters, f.total_write_characters);
        assert_eq!(u.total_edit_lines, f.total_edit_lines);
        assert_eq!(u.total_edit_characters, f.total_edit_characters);
        assert_eq!(u.conversation_usage, f.conversation_usage);
    }
}
