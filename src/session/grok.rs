//! Grok CLI `signals.json` and sibling session-history parser.

use crate::constants::FastHashMap;
use crate::models::{CodeAnalysis, GrokSignals, GrokSummary};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::parse_iso_timestamp;
use anyhow::{Context, Result, bail};
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Debug)]
struct PendingToolCall {
    name: String,
    input: Value,
}

/// Returns whether a JSON value carries Grok's aggregate signals envelope.
pub(crate) fn is_grok_signals(value: &Value) -> bool {
    value.get("primaryModelId").is_some()
        && value.get("contextTokensUsed").is_some()
        && (value.get("contextWindowTokens").is_some() || value.get("toolsUsed").is_some())
}

/// Parses a Grok `signals.json` file and its optional session-history siblings.
pub(crate) fn parse_grok_session(path: &Path, mode: ParseMode) -> Result<ParsedAnalysis> {
    let signals_value: Value = serde_json::from_reader(
        File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?,
    )
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
    apply_metadata(&mut state, path, summary.as_ref())?;
    parse_updates(path, &mut state, &mut diagnostics)?;

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
    parse_summary_reader(file, &path, diagnostics)
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

    let mut calls = HashMap::<String, PendingToolCall>::new();

    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "Failed to read Grok update line {}: {}",
                        index + 1,
                        path.display()
                    )
                });
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                diagnostics.record_malformed();
                continue;
            }
        };
        let Some(update) = value.pointer("/params/update") else {
            continue;
        };
        let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
            continue;
        };
        let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
            continue;
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
                    continue;
                };
                if status != "completed" && status != "failed" {
                    continue;
                }
                let Some(call) = calls.remove(id) else {
                    continue;
                };
                let tracked = is_tracked_tool(&call.name);
                if !tracked && !is_known_unmodeled_tool(&call.name) {
                    continue;
                }
                diagnostics.record_recognized_source();
                if !tracked {
                    // spawn_subagent / get_command_or_subagent_output orchestrate
                    // other tools instead of touching files: recognized so they
                    // never look like schema drift, but mapped to no metric.
                    continue;
                }
                if status != "completed" {
                    diagnostics.record_relevant(true);
                    continue;
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
    Ok(())
}

fn is_tracked_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "grep" | "write" | "search_replace" | "run_terminal_command" | "todo_write"
    )
}

/// Grok tools we recognize but deliberately do not model. They orchestrate
/// other tools rather than touching files, so counting them would invent
/// metrics; recognizing them keeps them from registering as schema drift.
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
            if content.is_empty() {
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
            // Recognized (not schema drift), just not modeled.
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
}
