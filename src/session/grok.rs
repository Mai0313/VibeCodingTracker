//! Grok CLI `signals.json` and sibling session-history parser.

use crate::constants::FastHashMap;
use crate::models::{CodeAnalysis, GrokSignals, GrokSummary};
use crate::session::diagnostics::{
    AnalysisFact, AnalysisFactEffect, AnalysisMetrics, AnalysisStateSnapshot, ParseDiagnostics,
    ParsedAnalysis, PricingGranularity, ToolFactStatus, UsageFact, UsageFactUnit,
};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::parse_iso_timestamp;
use anyhow::{Context, Result, bail};
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

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
    let session_scope = grok_session_scope(path, summary.as_ref());

    let mut state = SessionParseState::with_mode(mode);
    apply_metadata(&mut state, path, summary.as_ref())?;
    let analysis_facts = parse_updates(path, &mut state, &mut diagnostics, &model, &session_scope)?;

    let estimated_tokens = i64::try_from(signals.context_tokens_used).unwrap_or(i64::MAX);
    let mut usage = FastHashMap::default();
    let usage_value = json!({
        "input_tokens": 0,
        "output_tokens": 0,
        "cache_read_input_tokens": estimated_tokens,
        "cache_creation_input_tokens": 0
    });
    usage.insert(model.clone(), usage_value.clone());
    let usage_timestamp = state.last_ts;

    let mut parsed = ParsedAnalysis::new(
        CodeAnalysis {
            user: String::new(),
            extension_name: String::new(),
            insights_version: String::new(),
            machine_id: String::new(),
            records: vec![state.into_record(usage)],
        },
        diagnostics,
    );
    parsed.usage_facts = vec![UsageFact::anonymous(
        usage_timestamp,
        0,
        vec![UsageFactUnit::from_value(
            model,
            &usage_value,
            PricingGranularity::Aggregate,
        )],
    )];
    parsed.analysis_facts = analysis_facts;
    Ok(parsed)
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

fn grok_session_scope(signals_path: &Path, summary: Option<&GrokSummary>) -> String {
    summary
        .map(|summary| summary.info.id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            signals_path
                .parent()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|id| !id.is_empty())
        })
        .unwrap_or_else(|| signals_path.to_string_lossy().into_owned())
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

fn parse_updates(
    signals_path: &Path,
    state: &mut SessionParseState,
    diagnostics: &mut ParseDiagnostics,
    model: &str,
    session_scope: &str,
) -> Result<Vec<AnalysisFact>> {
    let path = signals_path.with_file_name("updates.jsonl");
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Failed to open Grok updates: {}", path.display()));
        }
    };

    let mut calls = HashMap::<String, PendingToolCall>::new();
    let mut tracked_calls = HashSet::<String>::new();
    let mut terminal_calls = HashSet::<String>::new();
    let mut fact_indices = HashMap::<String, usize>::new();
    let mut facts = Vec::<AnalysisFact>::new();

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
        let id = update
            .get("toolCallId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let timestamp = update_timestamp_ms(&value);
        state.last_ts = state.last_ts.max(timestamp);

        match kind {
            "tool_call" => {
                let name = update
                    .pointer("/_meta/x.ai~1tool/name")
                    .and_then(Value::as_str)
                    .or_else(|| update.get("title").and_then(Value::as_str))
                    .unwrap_or_default()
                    .to_string();
                diagnostics.record_recognized_source();
                if is_tracked_tool(&name) {
                    if let Some(id) = id
                        && tracked_calls.insert(id.to_string())
                    {
                        let metrics = record_tool_invocation(state, &name);
                        diagnostics.record_relevant(true);
                        let fact_index = facts.len();
                        facts.push(AnalysisFact {
                            stable_id: Some(format!("grok-tool:{session_scope}:{id}")),
                            timestamp_ms: (timestamp > 0).then_some(timestamp),
                            observed_at_ms: (timestamp > 0).then_some(timestamp),
                            source_order: index,
                            model: model.to_string(),
                            status: ToolFactStatus::Pending,
                            metrics,
                            effect: None,
                        });
                        fact_indices.insert(id.to_string(), fact_index);
                    } else if id.is_none() {
                        let metrics = record_tool_invocation(state, &name);
                        diagnostics.record_relevant(true);
                        diagnostics.record_relevant(false);
                        facts.push(AnalysisFact {
                            stable_id: None,
                            timestamp_ms: (timestamp > 0).then_some(timestamp),
                            observed_at_ms: (timestamp > 0).then_some(timestamp),
                            source_order: index,
                            model: model.to_string(),
                            status: ToolFactStatus::Pending,
                            metrics,
                            effect: None,
                        });
                    }
                } else if is_analysis_like_tool_name(&name) {
                    diagnostics.record_relevant(false);
                }
                if let Some(id) = id
                    && !terminal_calls.contains(id)
                {
                    calls.entry(id.to_string()).or_insert(PendingToolCall {
                        name,
                        input: update.get("rawInput").cloned().unwrap_or(Value::Null),
                    });
                }
            }
            "tool_call_update" => {
                let Some(id) = id else {
                    diagnostics.record_relevant(false);
                    continue;
                };
                let Some(status) = update.get("status").and_then(Value::as_str) else {
                    continue;
                };
                if !matches!(
                    status,
                    "completed" | "failed" | "rejected" | "cancelled" | "canceled"
                ) || !terminal_calls.insert(id.to_string())
                {
                    continue;
                }
                let Some(call) = calls.remove(id) else {
                    continue;
                };
                if !is_tracked_tool(&call.name) {
                    continue;
                }
                diagnostics.record_recognized_source();
                if status != "completed" {
                    diagnostics.record_relevant(true);
                    update_tool_fact(
                        &mut facts,
                        fact_indices.get(id).copied(),
                        ToolFactStatus::Failed,
                        timestamp,
                        AnalysisMetrics::default(),
                        None,
                    );
                    continue;
                }
                let output = update.get("rawOutput").unwrap_or(&Value::Null);
                let effect_before = AnalysisStateSnapshot::capture(state);
                let before = AnalysisMetrics::from_state(state);
                let invocation_counts = state.tool_counts.clone();
                let outcome = apply_completed_tool(state, &call, timestamp, output);
                let effects =
                    effect_metrics(AnalysisMetrics::from_state(state).saturating_sub(before));
                state.tool_counts = invocation_counts;
                diagnostics.record_relevant(outcome != ToolCompletion::Unsupported);
                update_tool_fact(
                    &mut facts,
                    fact_indices.get(id).copied(),
                    if outcome == ToolCompletion::Succeeded {
                        ToolFactStatus::Succeeded
                    } else {
                        ToolFactStatus::Failed
                    },
                    timestamp,
                    effects,
                    (outcome == ToolCompletion::Succeeded)
                        .then(|| effect_before.effect_since(state, Vec::new())),
                );
            }
            _ => {}
        }
    }
    Ok(facts)
}

fn update_timestamp_ms(value: &Value) -> i64 {
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    if timestamp.unsigned_abs() >= 1_000_000_000_000 {
        timestamp
    } else {
        timestamp.saturating_mul(1_000)
    }
}

fn is_tracked_tool(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "grep" | "write" | "search_replace" | "run_terminal_command" | "todo_write"
    )
}

fn record_tool_invocation(state: &mut SessionParseState, name: &str) -> AnalysisMetrics {
    let mut metrics = AnalysisMetrics::default();
    match name {
        "read_file" | "grep" => {
            state.tool_counts.read += 1;
            metrics.read_count = 1;
        }
        "write" => {
            state.tool_counts.write += 1;
            metrics.write_count = 1;
        }
        "search_replace" => {
            state.tool_counts.edit += 1;
            metrics.edit_count = 1;
        }
        "run_terminal_command" => {
            state.tool_counts.bash += 1;
            metrics.bash_count = 1;
        }
        "todo_write" => {
            state.tool_counts.todo_write += 1;
            metrics.todo_write_count = 1;
        }
        _ => {}
    }
    metrics
}

fn is_analysis_like_tool_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if name == "get_command_or_subagent_output" {
        return false;
    }
    [
        "read", "grep", "write", "edit", "replace", "patch", "file", "terminal", "shell",
        "command", "todo",
    ]
    .iter()
    .any(|fragment| name.contains(fragment))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolCompletion {
    Succeeded,
    Failed,
    Unsupported,
}

fn apply_completed_tool(
    state: &mut SessionParseState,
    call: &PendingToolCall,
    timestamp: i64,
    output: &Value,
) -> ToolCompletion {
    match call.name.as_str() {
        "read_file" => {
            let Some(path) = output
                .pointer("/FileContent/absolute_path")
                .and_then(Value::as_str)
                .or_else(|| call.input.get("target_file").and_then(Value::as_str))
                .filter(|path| !path.is_empty())
            else {
                return ToolCompletion::Unsupported;
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
            ToolCompletion::Succeeded
        }
        "grep" => {
            let Some(exit_code) = output.get("exit_code").and_then(Value::as_i64) else {
                return ToolCompletion::Unsupported;
            };
            if matches!(exit_code, 0 | 1) {
                state.tool_counts.read += 1;
                ToolCompletion::Succeeded
            } else {
                ToolCompletion::Failed
            }
        }
        "write" => {
            let Some(path) = mutation_output_path(output)
                .or_else(|| call.input.get("file_path").and_then(Value::as_str))
                .filter(|path| !path.is_empty())
            else {
                return ToolCompletion::Unsupported;
            };
            let Some(content) = call.input.get("content").and_then(Value::as_str) else {
                return ToolCompletion::Unsupported;
            };
            state.add_write_detail(path, content, timestamp);
            ToolCompletion::Succeeded
        }
        "search_replace" => {
            let Some(path) = mutation_output_path(output)
                .or_else(|| call.input.get("file_path").and_then(Value::as_str))
                .filter(|path| !path.is_empty())
            else {
                return ToolCompletion::Unsupported;
            };
            if let Some(details) = search_replace_details(output) {
                let edit_calls = state.tool_counts.edit;
                for detail in details {
                    if let Some((old, new)) = search_replace_detail_strings(detail) {
                        state.add_edit_detail_raw(path, old, new, timestamp);
                    }
                }
                state.tool_counts.edit = edit_calls.saturating_add(1);
                return ToolCompletion::Succeeded;
            }
            let Some(old) = call.input.get("old_string").and_then(Value::as_str) else {
                return ToolCompletion::Unsupported;
            };
            let Some(new) = call.input.get("new_string").and_then(Value::as_str) else {
                return ToolCompletion::Unsupported;
            };
            state.add_edit_detail_raw(path, old, new, timestamp);
            ToolCompletion::Succeeded
        }
        "run_terminal_command" => {
            let Some(command) = call
                .input
                .get("command")
                .and_then(Value::as_str)
                .filter(|command| !command.trim().is_empty())
            else {
                return ToolCompletion::Unsupported;
            };
            let description = call
                .input
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            state.add_run_command(command, description, timestamp);
            ToolCompletion::Succeeded
        }
        "todo_write" => {
            state.tool_counts.todo_write += 1;
            ToolCompletion::Succeeded
        }
        _ => ToolCompletion::Unsupported,
    }
}

fn effect_metrics(metrics: AnalysisMetrics) -> AnalysisMetrics {
    AnalysisMetrics {
        edit_lines: metrics.edit_lines,
        read_lines: metrics.read_lines,
        write_lines: metrics.write_lines,
        ..AnalysisMetrics::default()
    }
}

fn update_tool_fact(
    facts: &mut [AnalysisFact],
    fact_index: Option<usize>,
    status: ToolFactStatus,
    observed_at_ms: i64,
    effects: AnalysisMetrics,
    effect: Option<AnalysisFactEffect>,
) {
    let Some(fact) = fact_index.and_then(|index| facts.get_mut(index)) else {
        return;
    };
    fact.status = status;
    if observed_at_ms > 0 {
        fact.observed_at_ms = Some(observed_at_ms);
    }
    if status == ToolFactStatus::Succeeded {
        fact.metrics.add_assign(effects);
        fact.effect = effect;
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

    fn write_session(
        root: &Path,
        directory_id: &str,
        summary_id: Option<&str>,
        updates: &[Value],
    ) -> std::path::PathBuf {
        let session = root.join("workspace").join(directory_id);
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
        if let Some(summary_id) = summary_id {
            std::fs::write(
                session.join("summary.json"),
                json!({ "info": { "id": summary_id } }).to_string(),
            )
            .unwrap();
        }
        let body = updates
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(session.join("updates.jsonl"), body).unwrap();
        signals
    }

    fn make_session(updates: &[Value]) -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let signals = write_session(temp.path(), "session-id", None, updates);
        (temp, signals)
    }

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
    fn failed_rejected_and_pending_tools_count_without_file_effects() {
        let call = |id: &str, name: &str, input: Value| {
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": id,
                    "title": name,
                    "rawInput": input
                }}
            })
        };
        let terminal = |id: &str, status: &str| {
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": id,
                    "status": status,
                    "rawOutput": { "type": "Text", "text": status }
                }},
                "timestamp": 1_767_225_600
            })
        };
        let (_temp, signals) = make_session(&[
            call(
                "failed-read",
                "read_file",
                json!({ "target_file": "/repo/failed.rs" }),
            ),
            terminal("failed-read", "failed"),
            call(
                "rejected-edit",
                "search_replace",
                json!({
                    "file_path": "/repo/rejected.rs",
                    "old_string": "old",
                    "new_string": "new"
                }),
            ),
            terminal("rejected-edit", "rejected"),
            call(
                "pending-write",
                "write",
                json!({ "file_path": "/repo/pending.rs", "content": "pending" }),
            ),
        ]);

        let parsed = parse_grok_session(&signals, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.total_unique_files, 0);
        assert_eq!(record.total_read_lines, 0);
        assert_eq!(record.total_edit_lines, 0);
        assert_eq!(record.total_write_lines, 0);
        assert!(record.read_file_details.is_empty());
        assert!(record.edit_file_details.is_empty());
        assert!(record.write_file_details.is_empty());

        assert_eq!(parsed.analysis_facts.len(), 3);
        let failed = &parsed.analysis_facts[0];
        assert_eq!(
            failed.stable_id.as_deref(),
            Some("grok-tool:session-id:failed-read")
        );
        assert_eq!(failed.model, "grok-test");
        assert_eq!(failed.status, ToolFactStatus::Failed);
        assert_eq!(failed.metrics.read_count, 1);
        assert_eq!(failed.metrics.read_lines, 0);
        let rejected = &parsed.analysis_facts[1];
        assert_eq!(rejected.status, ToolFactStatus::Failed);
        assert_eq!(rejected.metrics.edit_count, 1);
        assert_eq!(rejected.metrics.edit_lines, 0);
        let pending = &parsed.analysis_facts[2];
        assert_eq!(pending.status, ToolFactStatus::Pending);
        assert_eq!(pending.metrics.write_count, 1);
        assert_eq!(pending.metrics.write_lines, 0);
    }

    #[test]
    fn command_polling_helper_is_not_reported_as_tool_schema_drift() {
        let (_temp, signals) = make_session(&[
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "poll-1",
                    "title": "get_command_or_subagent_output",
                    "rawInput": { "id": "process-1" }
                }},
                "timestamp": 1_767_225_600
            }),
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "poll-1",
                    "status": "completed",
                    "rawOutput": { "type": "Text", "text": "complete" }
                }},
                "timestamp": 1_767_225_601
            }),
        ]);

        let parsed = parse_grok_session(&signals, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];

        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(record.tool_call_counts.read, 0);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.tool_call_counts.edit, 0);
        assert_eq!(record.tool_call_counts.todo_write, 0);
        assert_eq!(record.tool_call_counts.bash, 0);
        assert!(parsed.analysis_facts.is_empty());
    }

    #[test]
    fn tracked_tool_without_correlation_id_is_anonymous_without_effects() {
        let (_temp, signals) = make_session(&[
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call",
                    "title": "write",
                    "rawInput": {
                        "file_path": "/repo/new.rs",
                        "content": "one\ntwo\n"
                    }
                }},
                "timestamp": 1_767_225_600
            }),
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call_update",
                    "status": "completed",
                    "rawOutput": {
                        "EditsApplied": { "absolute_path": "/repo/new.rs" }
                    }
                }},
                "timestamp": 1_767_225_610
            }),
        ]);

        let parsed = parse_grok_session(&signals, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];

        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.total_write_lines, 0);
        assert_eq!(record.total_unique_files, 0);
        assert!(record.write_file_details.is_empty());
        assert_eq!(parsed.analysis_facts.len(), 1);
        let fact = &parsed.analysis_facts[0];
        assert!(fact.stable_id.is_none());
        assert_eq!(fact.model, "grok-test");
        assert_eq!(fact.status, ToolFactStatus::Pending);
        assert_eq!(fact.metrics.write_count, 1);
        assert_eq!(fact.metrics.write_lines, 0);
        assert!(fact.effect.is_none());
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 2);
    }

    #[test]
    fn tool_fact_uses_invocation_timestamp_and_success_effects() {
        let (_temp, signals) = make_session(&[
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "write-1",
                    "title": "write",
                    "rawInput": {
                        "file_path": "/repo/new.rs",
                        "content": "one\ntwo\n"
                    }
                }},
                "timestamp": 1_767_225_600
            }),
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "write-1",
                    "status": "completed",
                    "rawOutput": { "type": "Text", "text": "ok" }
                }},
                "timestamp": 1_767_225_610
            }),
        ]);

        let parsed = parse_grok_session(&signals, ParseMode::Full).unwrap();

        assert_eq!(parsed.analysis_facts.len(), 1);
        let fact = &parsed.analysis_facts[0];
        assert_eq!(
            fact.stable_id.as_deref(),
            Some("grok-tool:session-id:write-1")
        );
        assert_eq!(fact.timestamp_ms, Some(1_767_225_600_000));
        assert_eq!(fact.observed_at_ms, Some(1_767_225_610_000));
        assert_eq!(fact.model, "grok-test");
        assert_eq!(fact.status, ToolFactStatus::Succeeded);
        assert_eq!(fact.metrics.write_count, 1);
        assert_eq!(fact.metrics.write_lines, 2);
        assert_eq!(parsed.analysis.records[0].tool_call_counts.write, 1);
    }

    #[test]
    fn tool_fact_ids_are_scoped_to_the_grok_session() {
        let call = json!({
            "params": { "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "read-1",
                "title": "read_file",
                "rawInput": { "target_file": "/repo/shared.rs" }
            }},
            "timestamp": 1_767_225_600
        });
        let temp = tempfile::tempdir().unwrap();
        let first_signals = write_session(
            temp.path(),
            "directory-one",
            Some("summary-one"),
            std::slice::from_ref(&call),
        );
        let second_signals = write_session(
            temp.path(),
            "directory-two",
            Some("summary-two"),
            std::slice::from_ref(&call),
        );

        let first = parse_grok_session(&first_signals, ParseMode::Full).unwrap();
        let second = parse_grok_session(&second_signals, ParseMode::Full).unwrap();
        let first_id = first.analysis_facts[0].stable_id.as_deref();
        let second_id = second.analysis_facts[0].stable_id.as_deref();

        assert_eq!(first_id, Some("grok-tool:summary-one:read-1"));
        assert_eq!(second_id, Some("grok-tool:summary-two:read-1"));
        assert_ne!(first_id, second_id);
    }

    #[test]
    fn successful_empty_read_keeps_unique_path_in_full_effect() {
        let (_temp, signals) = make_session(&[
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "read-empty",
                    "title": "read_file",
                    "rawInput": { "target_file": "/repo/empty.txt" }
                }},
                "timestamp": 1_767_225_600
            }),
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "read-empty",
                    "status": "completed",
                    "rawOutput": {
                        "FileContent": {
                            "absolute_path": "/repo/empty.txt",
                            "content": ""
                        }
                    }
                }},
                "timestamp": 1_767_225_610
            }),
        ]);

        let parsed = parse_grok_session(&signals, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];
        let fact = &parsed.analysis_facts[0];

        assert_eq!(record.tool_call_counts.read, 1);
        assert_eq!(record.total_unique_files, 1);
        assert_eq!(record.total_read_lines, 0);
        assert!(record.read_file_details.is_empty());
        assert_eq!(fact.status, ToolFactStatus::Succeeded);
        assert_eq!(fact.metrics.read_count, 1);
        assert_eq!(fact.metrics.read_lines, 0);
        assert_eq!(
            fact.effect.as_ref().unwrap().unique_files,
            vec!["/repo/empty.txt"]
        );
    }

    #[test]
    fn duplicate_lifecycle_is_counted_once() {
        let call = json!({
            "params": { "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "write-1",
                "title": "write",
                "rawInput": {
                    "file_path": "/repo/new.rs",
                    "content": "one\ntwo\n"
                }
            }},
            "timestamp": 1_767_225_600
        });
        let result = json!({
            "params": { "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "write-1",
                "status": "completed",
                "rawOutput": { "type": "Text", "text": "ok" }
            }},
            "timestamp": 1_767_225_610
        });
        let (_temp, signals) = make_session(&[call.clone(), call, result.clone(), result]);

        let parsed = parse_grok_session(&signals, ParseMode::Full).unwrap();
        let record = &parsed.analysis.records[0];

        assert_eq!(parsed.analysis_facts.len(), 1);
        assert_eq!(parsed.analysis_facts[0].metrics.write_count, 1);
        assert_eq!(parsed.analysis_facts[0].metrics.write_lines, 2);
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.total_write_lines, 2);
        assert_eq!(record.write_file_details.len(), 1);
    }

    #[test]
    fn completed_grep_with_error_exit_is_a_failed_fact() {
        let (_temp, signals) = make_session(&[
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "grep-error",
                    "title": "grep",
                    "rawInput": { "path": "src", "pattern": "[" }
                }},
                "timestamp": 1_767_225_600
            }),
            json!({
                "params": { "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "grep-error",
                    "status": "completed",
                    "rawOutput": { "type": "GrepSearch", "exit_code": 2 }
                }},
                "timestamp": 1_767_225_601
            }),
        ]);

        let parsed = parse_grok_session(&signals, ParseMode::Full).unwrap();
        let fact = &parsed.analysis_facts[0];

        assert_eq!(fact.status, ToolFactStatus::Failed);
        assert_eq!(fact.metrics.read_count, 1);
        assert_eq!(fact.metrics.read_lines, 0);
        assert_eq!(parsed.analysis.records[0].tool_call_counts.read, 1);
    }

    #[test]
    fn result_only_update_does_not_invent_an_invocation() {
        let (_temp, signals) = make_session(&[json!({
            "params": { "update": {
                "sessionUpdate": "tool_call_update",
                "toolCallId": "missing-call",
                "status": "completed",
                "rawOutput": {
                    "type": "ReadFile",
                    "FileContent": {
                        "absolute_path": "/repo/a.rs",
                        "content": "content"
                    }
                }
            }},
            "timestamp": 1_767_225_601
        })]);

        let parsed = parse_grok_session(&signals, ParseMode::Full).unwrap();

        assert!(parsed.analysis_facts.is_empty());
        assert_eq!(parsed.analysis.records[0].tool_call_counts.read, 0);
        assert_eq!(parsed.analysis.records[0].total_read_lines, 0);
    }

    #[test]
    fn unknown_analysis_tool_reports_schema_drift() {
        let (_temp, signals) = make_session(&[json!({
            "params": { "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": "future-tool",
                "title": "future_file_mutator",
                "rawInput": { "path": "/repo/a.rs" }
            }}
        })]);

        let parsed = parse_grok_session(&signals, ParseMode::Full).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
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

        assert_eq!(
            apply_completed_tool(&mut state, &call, 123, &output),
            ToolCompletion::Succeeded
        );
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

        assert_eq!(
            apply_completed_tool(&mut state, &call, 123, &output),
            ToolCompletion::Succeeded
        );
        assert_eq!(state.tool_counts.write, 1);
        assert_eq!(state.write_details.len(), 1);
        assert_eq!(
            state.write_details[0].base.file_path,
            "/workspace/demo/src/new.rs"
        );
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
