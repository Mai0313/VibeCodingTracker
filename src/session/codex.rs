//! Parser for OpenAI Codex rollout logs (`~/.codex/sessions/**/*.jsonl`).
//!
//! Codex records carry a `type` discriminator (`session_meta`,
//! `turn_context`, `event_msg`, `response_item`). File operations are not
//! first-class: Codex records them as legacy `function_call` pairs or current
//! `custom_tool_call` pairs. This parser joins each call to its output by
//! `call_id`. Legacy shell calls retain command-level analysis. A top-level
//! custom `exec` is counted as one run command because its JavaScript source
//! does not prove which nested tools actually ran. Direct custom patches are
//! applied only when their paired output explicitly reports success.
use crate::constants::FastHashMap;
use crate::models::*;
use crate::pricing::{TierClassifier, TierThresholds};
use crate::session::diagnostics::{ParseDiagnostics, ParsedAnalysis};
use crate::session::state::{ParseMode, SessionParseState};
use crate::utils::{
    CodexTokenTotals, get_git_remote_url, parse_iso_timestamp, process_codex_usage,
};
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::borrow::Borrow;
use std::collections::HashSet;

/// Parse Codex session records from a slice of pre-typed logs.
///
/// Walks the records in order, threading function and custom-tool calls to
/// their outputs and folding token usage from `token_count` events. Returns a
/// single-record [`CodeAnalysis`] with the runtime metadata fields blank
/// (the caller's `finalize` step fills them).
///
/// # Errors
///
/// Returns `anyhow::Result` for parity with the other provider parsers, but
/// has no fallible step: malformed arguments and outputs are tolerated
/// (skipped or substituted), so it returns `Ok` for any slice.
pub fn parse_codex_logs(logs: &[CodexLog], mode: ParseMode) -> Result<CodeAnalysis> {
    parse_codex_log_iter(logs, mode)
}

/// Streaming Codex parser used by JSONL readers that already own an iterator.
pub(crate) fn parse_codex_log_iter<I>(logs: I, mode: ParseMode) -> Result<CodeAnalysis>
where
    I: IntoIterator,
    I::Item: Borrow<CodexLog>,
{
    Ok(parse_codex_log_iter_with_diagnostics(logs, mode, None)?.analysis)
}

/// Streaming Codex parser with parser-only schema diagnostics.
pub(crate) fn parse_codex_log_iter_with_diagnostics<I>(
    logs: I,
    mode: ParseMode,
    tiers: Option<&TierThresholds>,
) -> Result<ParsedAnalysis>
where
    I: IntoIterator,
    I::Item: Borrow<CodexLog>,
{
    let mut classifier = tiers.map(TierClassifier::new);
    let mut state = SessionParseState::with_mode(mode);
    let mut conversation_usage: FastHashMap<String, Value> = FastHashMap::with_capacity(5);
    let mut current_model = String::new();
    // Codex publishes whole-session cumulative counters on every token_count
    // event; each model is billed only the delta since the previous snapshot.
    // Pre-context snapshots (a resumed session's replayed totals) advance the
    // snapshot without attribution, replacing the old replay-baseline hack.
    let mut prev_totals: Option<CodexTokenTotals> = None;
    let mut shell_calls: FastHashMap<String, PendingCodexShellCall> =
        FastHashMap::with_capacity(50);
    let mut custom_calls: FastHashMap<String, CodexCustomCall> = FastHashMap::with_capacity(32);
    // Call ids of direct `apply_patch` custom_tool_calls, so a paired
    // `patch_apply_end` event is not double counted (see the event_msg arm).
    let mut apply_patch_call_ids: HashSet<String> = HashSet::new();
    let mut diagnostics = ParseDiagnostics::default();

    for entry in logs {
        let entry = entry.borrow();
        let recognized = matches!(
            entry.log_type.as_str(),
            "session_meta"
                | "turn_context"
                | "event_msg"
                | "response_item"
                | "inter_agent_communication_metadata"
                | "world_state"
                | "compacted"
        );
        if recognized {
            diagnostics.record_recognized_source();
        } else {
            diagnostics.record_unrecognized();
        }
        let ts = parse_iso_timestamp(&entry.timestamp);
        if ts > state.last_ts {
            state.last_ts = ts;
        }

        match entry.log_type.as_str() {
            "session_meta" => {
                if state.folder_path.is_empty()
                    && let Some(cwd) = &entry.payload.cwd
                {
                    state.folder_path.clone_from(cwd); // More efficient than clone()
                }
                if state.task_id.is_empty()
                    && let Some(id) = &entry.payload.id
                {
                    state.task_id.clone_from(id);
                }
                if state.git_remote.is_empty()
                    && let Some(git) = &entry.payload.git
                    && let Some(url) = &git.repository_url
                {
                    state.git_remote.clone_from(url);
                }
            }
            "turn_context" => {
                if state.folder_path.is_empty()
                    && let Some(cwd) = &entry.payload.cwd
                {
                    state.folder_path.clone_from(cwd);
                }
                if let Some(model) = entry
                    .payload
                    .model
                    .as_ref()
                    .filter(|model| !model.is_empty())
                {
                    current_model.clone_from(model); // Reuse existing allocation
                }
            }
            "event_msg" => {
                if let Some(payload_type) = &entry.payload.payload_type
                    && payload_type == "token_count"
                    && let Some(info) = entry.payload.info.as_ref().filter(|info| !info.is_null())
                {
                    if !is_supported_codex_usage(info) {
                        diagnostics.record_relevant(false);
                    } else {
                        diagnostics.record_relevant(true);
                        let total = info.get("total_token_usage").and_then(Value::as_object);
                        if current_model.is_empty() {
                            // Replayed pre-context totals: advance the snapshot
                            // without attributing tokens to a guessed model.
                            if let Some(total) = total {
                                prev_totals = Some(CodexTokenTotals::from_total_object(total));
                            }
                        } else {
                            let delta = total
                                .map(|total| {
                                    CodexTokenTotals::delta_fields(total, prev_totals.as_ref())
                                })
                                .unwrap_or_default();
                            // One token_count is one turn; its request context
                            // is the turn's own full prompt (cached included),
                            // published as last_token_usage.input_tokens. Fall
                            // back to the delta input when absent.
                            let above = classifier.as_mut().is_some_and(|classifier| {
                                let request_context = info
                                    .get("last_token_usage")
                                    .and_then(|last| last.get("input_tokens"))
                                    .and_then(Value::as_i64)
                                    .filter(|tokens| *tokens > 0)
                                    .or_else(|| delta.get("input_tokens").and_then(Value::as_i64))
                                    .unwrap_or(0);
                                classifier.is_above(&current_model, request_context)
                            });
                            process_codex_usage(
                                &mut conversation_usage,
                                &current_model,
                                &delta,
                                info,
                                above,
                            );
                            if let Some(total) = total {
                                prev_totals = Some(CodexTokenTotals::from_total_object(total));
                            }
                        }
                    }
                }

                // Modern Codex applies most edits inside opaque `exec` cells and
                // records the resulting file changes in a `patch_apply_end` event.
                // A successful one is the authoritative source of those file ops.
                if entry.payload.payload_type.as_deref() == Some("patch_apply_end") {
                    // Skip the completion of a direct apply_patch custom_tool_call;
                    // that path already counts it. Otherwise this event is the only
                    // record of the change and must be folded in.
                    let already_counted = entry
                        .payload
                        .call_id
                        .as_deref()
                        .is_some_and(|id| apply_patch_call_ids.contains(id));
                    if matches!(entry.payload.success, Some(true)) && !already_counted {
                        match parse_patch_changes(entry.payload.changes.as_ref()) {
                            Some(patches) => {
                                for patch in patches {
                                    state.handle_patch(patch, ts);
                                }
                                diagnostics.record_relevant(true);
                            }
                            None => diagnostics.record_relevant(false),
                        }
                    }
                    // A failed or deduped event is recognized but creates no file ops.
                }
            }
            "response_item" => {
                if let Some(payload_type) = &entry.payload.payload_type {
                    match payload_type.as_str() {
                        "function_call" => {
                            if let Some(name) = entry.payload.name.as_deref()
                                && matches!(name, "shell" | "exec_command")
                            {
                                let call = entry
                                    .payload
                                    .arguments
                                    .as_deref()
                                    .and_then(|args| parse_function_call(name, args, ts));
                                if let Some(call_id) = entry.payload.call_id.as_deref() {
                                    if let Some(call) = call {
                                        diagnostics.record_relevant(true);
                                        shell_calls.insert(
                                            call_id.to_string(),
                                            PendingCodexShellCall::Parsed(call),
                                        );
                                    } else {
                                        // Defer the schema verdict until the paired output. Codex
                                        // persists model-generated argument errors as ordinary
                                        // lifecycle records even though no command ran.
                                        shell_calls.insert(
                                            call_id.to_string(),
                                            PendingCodexShellCall::InvalidArguments,
                                        );
                                    }
                                } else {
                                    diagnostics.record_relevant(false);
                                }
                            }
                        }
                        "function_call_output" => {
                            if let Some(call_id) = &entry.payload.call_id
                                && let Some(call) = shell_calls.remove(call_id)
                            {
                                match call {
                                    PendingCodexShellCall::Parsed(call) => {
                                        diagnostics.record_relevant(true);
                                        let output = shell_output(entry.payload.output.as_deref());
                                        state.handle_shell_call(call, output);
                                    }
                                    PendingCodexShellCall::InvalidArguments => {
                                        diagnostics.record_relevant(output_reports_argument_error(
                                            entry.payload.output.as_deref(),
                                        ));
                                    }
                                }
                            }
                        }
                        "custom_tool_call" => {
                            if let Some(name) = entry.payload.name.as_deref()
                                && matches!(name, "exec" | "apply_patch")
                            {
                                // Remember every direct apply_patch call id up front so a
                                // later patch_apply_end for the same id is skipped, whichever
                                // order the call output and the event arrive in.
                                if name == "apply_patch"
                                    && let Some(call_id) = entry.payload.call_id.as_deref()
                                {
                                    apply_patch_call_ids.insert(call_id.to_string());
                                }
                                let call = entry
                                    .payload
                                    .arguments
                                    .as_deref()
                                    .and_then(|input| parse_custom_call(name, input, ts, mode));
                                let normalized = call.is_some() && entry.payload.call_id.is_some();
                                diagnostics.record_relevant(normalized);
                                if let (Some(call), Some(call_id)) =
                                    (call, entry.payload.call_id.as_deref())
                                {
                                    custom_calls.insert(call_id.to_string(), call);
                                }
                            }
                        }
                        "custom_tool_call_output" => {
                            if let Some(call_id) = entry.payload.call_id.as_deref()
                                && let Some(call) = custom_calls.remove(call_id)
                            {
                                let normalized = dispatch_custom_call(
                                    &mut state,
                                    call,
                                    entry.payload.output.as_deref(),
                                );
                                diagnostics.record_relevant(normalized);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    for call in shell_calls.into_values() {
        if matches!(call, PendingCodexShellCall::InvalidArguments) {
            diagnostics.record_relevant(false);
        }
    }
    if state.git_remote.is_empty() {
        state.git_remote = get_git_remote_url(&state.folder_path);
    }

    let record = state.into_record(conversation_usage);

    let analysis = CodeAnalysis {
        user: String::new(),
        extension_name: String::new(),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    };
    Ok(ParsedAnalysis::new(analysis, diagnostics))
}

fn is_supported_codex_usage(info: &Value) -> bool {
    let Some(info) = info.as_object() else {
        return false;
    };
    let mut recognized = false;
    for key in ["total_token_usage", "last_token_usage"] {
        let Some(usage) = info.get(key) else {
            continue;
        };
        if !is_supported_codex_token_usage(usage) {
            return false;
        }
        recognized = true;
    }
    if let Some(context_window) = info.get("model_context_window") {
        if context_window.is_null() {
            // Older Codex records retain the field with a null value while
            // still carrying valid token objects.
        } else if context_window.as_i64().is_none() {
            return false;
        } else {
            recognized = true;
        }
    }
    recognized
}

fn is_supported_codex_token_usage(usage: &Value) -> bool {
    let Some(usage) = usage.as_object() else {
        return false;
    };
    if usage.is_empty() {
        return true;
    }

    let mut recognized = false;
    for key in [
        "input_tokens",
        "cached_input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
        "total_tokens",
    ] {
        if let Some(value) = usage.get(key) {
            if value.as_i64().is_none() {
                return false;
            }
            recognized = true;
        }
    }
    recognized
}

enum PendingCodexShellCall {
    Parsed(CodexShellCall),
    InvalidArguments,
}

fn output_reports_argument_error(output: Option<&str>) -> bool {
    let output = shell_output(output).output;
    let output = strip_exec_command_metadata_prefix(&output)
        .trim_start()
        .to_ascii_lowercase();
    (output.starts_with("error") || output.starts_with("failed"))
        && (output.contains("failed to parse")
            || output.contains("missing field")
            || output.contains("missing required")
            || output.contains("invalid argument"))
}

/// Strip the metadata header that current Codex (`exec_command`) prepends
/// to every shell output. Format observed in the wild:
///
/// ```text
/// Chunk ID: <hex>
/// Wall time: <num> seconds
/// Process exited with code <num>
/// Original token count: <num>
/// Output:
/// <actual stdout>
/// ```
///
/// Without stripping, sed/cat read-detail extraction over-counts the file
/// by 5 lines (the prefix). Legacy `shell` outputs do not include the
/// header, so the search returns the original slice unchanged when no
/// `\nOutput:\n` marker is found.
fn strip_exec_command_metadata_prefix(output: &str) -> &str {
    const MARKER: &str = "\nOutput:\n";
    if let Some(idx) = output.find(MARKER) {
        &output[idx + MARKER.len()..]
    } else if let Some(rest) = output.strip_prefix("Output:\n") {
        rest
    } else {
        output
    }
}

/// Normalizes legacy strings and structured outputs serialized at the model boundary.
fn shell_output(output: Option<&str>) -> CodexShellOutput {
    let Some(output) = output else {
        return CodexShellOutput {
            output: String::new(),
            metadata: None,
        };
    };

    serde_json::from_str::<CodexShellOutput>(output).unwrap_or_else(|_| CodexShellOutput {
        output: serde_json::from_str::<Value>(output)
            .ok()
            .and_then(|value| value.get("output")?.as_str().map(str::to_string))
            .unwrap_or_else(|| output.to_string()),
        metadata: None,
    })
}

/// Build a `CodexShellCall` from either the legacy `shell` function or the
/// current `exec_command` function.
///
/// Returns `None` when the function name is unrelated (e.g. `update_plan`,
/// MCP tool calls) or when the arguments fail to deserialize. Both shapes
/// collapse to the same downstream representation so the patch / sed / cat
/// dispatch in `handle_shell_call` does not need to branch on the source
/// function name.
fn parse_function_call(name: &str, args_str: &str, ts: i64) -> Option<CodexShellCall> {
    match name {
        "shell" => {
            let args = serde_json::from_str::<CodexShellArguments>(args_str).ok()?;
            let script = args.command.last().cloned().unwrap_or_default();
            Some(CodexShellCall {
                timestamp: ts,
                script,
                full_command: args.command,
            })
        }
        "exec_command" => {
            let args = serde_json::from_str::<CodexExecCommandArguments>(args_str).ok()?;
            let cmd = args.cmd;
            Some(CodexShellCall {
                timestamp: ts,
                script: cmd.clone(),
                full_command: vec![cmd],
            })
        }
        _ => None,
    }
}

enum CodexCustomCall {
    Exec {
        source: Option<String>,
        timestamp: i64,
    },
    ApplyPatch {
        patches: Vec<CodexPatch>,
        timestamp: i64,
    },
}

fn parse_custom_call(
    name: &str,
    input: &str,
    timestamp: i64,
    mode: ParseMode,
) -> Option<CodexCustomCall> {
    match name {
        "exec" if !input.trim().is_empty() => Some(CodexCustomCall::Exec {
            source: matches!(mode, ParseMode::Full).then(|| input.to_string()),
            timestamp,
        }),
        "apply_patch" => {
            let patches: Vec<_> = parse_apply_patch_script(input)
                .into_iter()
                .filter(|patch| !patch.file_path.is_empty())
                .collect();
            (!patches.is_empty()).then_some(CodexCustomCall::ApplyPatch { patches, timestamp })
        }
        _ => None,
    }
}

fn dispatch_custom_call(
    state: &mut SessionParseState,
    call: CodexCustomCall,
    output: Option<&str>,
) -> bool {
    match call {
        CodexCustomCall::Exec { source, timestamp } => {
            // The JavaScript wrapper can contain branches, loops, retries, or
            // tool calls whose execution cannot be proven from source text.
            // Treat the completed cell itself as the only observable action.
            if let Some(source) = source {
                state.add_run_command(&source, "", timestamp);
            } else {
                state.tool_counts.bash += 1;
            }
            true
        }
        CodexCustomCall::ApplyPatch { patches, timestamp } => {
            match custom_apply_patch_result(output) {
                CustomApplyPatchResult::Success => {
                    for patch in patches {
                        state.handle_patch(patch, timestamp);
                    }
                    true
                }
                CustomApplyPatchResult::Failure => true,
                CustomApplyPatchResult::Unknown => false,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CustomApplyPatchResult {
    Success,
    Failure,
    Unknown,
}

fn custom_apply_patch_result(output: Option<&str>) -> CustomApplyPatchResult {
    let output = normalize_custom_output(output);
    let output = output.trim();
    if output == "Done!" || output.starts_with("Success. Updated the following files:") {
        CustomApplyPatchResult::Success
    } else if output.starts_with("Failed")
        || output.starts_with("Error")
        || output.starts_with("apply_patch verification failed:")
        || output.starts_with("Invalid patch")
        || output.starts_with("apply_patch handler received")
        || output.starts_with("apply_patch is unavailable")
    {
        CustomApplyPatchResult::Failure
    } else {
        CustomApplyPatchResult::Unknown
    }
}

/// Decodes a custom tool's normalized string or `JSON.stringify` result.
fn normalize_custom_output(output: Option<&str>) -> String {
    let body = strip_exec_command_metadata_prefix(output.unwrap_or_default()).trim();
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(object)) => object
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or(body)
            .to_string(),
        Ok(Value::String(text)) => text,
        _ => body.to_string(),
    }
}

/// Codex-specific dispatch helpers grafted onto [`SessionParseState`].
///
/// Kept as a private extension trait so the generic state type stays free of
/// Codex's `apply_patch`/`sed`/`cat` heuristics.
trait CodexAnalysisExt {
    /// Routes a completed shell call to the read / patch / run-command tally
    /// based on what its `script` did.
    fn handle_shell_call(&mut self, call: CodexShellCall, output: CodexShellOutput);
    /// Applies one parsed `apply_patch` hunk as a write, delete, or edit.
    fn handle_patch(&mut self, patch: CodexPatch, ts: i64);
    /// Records a shell call that was not a file operation as a run command.
    fn record_run_command(&mut self, call: CodexShellCall);
}

impl CodexAnalysisExt for SessionParseState {
    fn handle_shell_call(&mut self, call: CodexShellCall, output: CodexShellOutput) {
        // Patch payloads carry a stable envelope regardless of the launcher name.
        if call.script.contains("*** Begin Patch") {
            let patches = parse_apply_patch_script(&call.script);
            for patch in patches {
                self.handle_patch(patch, call.timestamp);
            }
            return;
        }

        // The legacy `shell` function returned just the raw command output
        // in `output`. The current `exec_command` function wraps that
        // output with a metadata header — strip it so line counting sees
        // only what the model actually saw as the file body.
        let output_body = strip_exec_command_metadata_prefix(&output.output);

        // Check for sed command
        if let Some(path) = extract_sed_file_path(&call.script) {
            self.add_read_detail(&path, output_body, call.timestamp);
            return;
        }

        // Check for cat command
        if let Some((path, content)) = extract_cat_read(&call.script, output_body) {
            self.add_read_detail(&path, &content, call.timestamp);
            return;
        }

        // Record as run command
        self.record_run_command(call);
    }

    fn handle_patch(&mut self, patch: CodexPatch, ts: i64) {
        if patch.file_path.is_empty() {
            return;
        }

        let resolved = self.normalize_path(&patch.file_path);
        if resolved.is_empty() {
            return;
        }

        let (old_str, new_str) = extract_patch_strings(&patch.lines);

        match patch.action.as_str() {
            "add" => {
                self.add_write_detail(&resolved, &new_str, ts);
            }
            "delete" => {
                let content = old_str.trim_end_matches('\n');
                if !content.is_empty() {
                    self.add_edit_detail(&resolved, content, "", ts);
                }
            }
            _ => {
                self.add_edit_detail(&resolved, &old_str, &new_str, ts);
            }
        }
    }

    fn record_run_command(&mut self, call: CodexShellCall) {
        let command_str = if call.full_command.is_empty() {
            call.script.trim()
        } else {
            &call.full_command.join(" ")
        };

        self.add_run_command(command_str, "", call.timestamp);
    }
}

/// A Codex shell invocation awaiting its output, normalized across the
/// legacy `shell` and current `exec_command` function shapes.
struct CodexShellCall {
    /// Event timestamp in epoch milliseconds.
    timestamp: i64,
    /// The command line actually executed (used for `sed`/`cat`/patch sniffing).
    script: String,
    /// The full argv as written by the model, for verbatim run-command display.
    full_command: Vec<String>,
}

/// One file hunk extracted from an `apply_patch` script.
struct CodexPatch {
    /// `"add"`, `"delete"`, or `"update"`.
    action: String,
    /// Target file path as written in the patch header.
    file_path: String,
    /// Raw diff body lines (with their leading `+`/`-`/context markers).
    lines: Vec<String>,
}

/// Parses an `apply_patch` script into its constituent per-file hunks.
///
/// Returns an empty `Vec` when no `*** Begin Patch` marker is present.
fn parse_apply_patch_script(script: &str) -> Vec<CodexPatch> {
    let start = match script.find("*** Begin Patch") {
        Some(idx) => idx,
        None => return Vec::new(),
    };

    let segment = &script[start..];
    let lines: Vec<&str> = segment.lines().collect();
    // Pre-allocate capacity based on typical patch count (1-5 patches)
    let mut patches = Vec::with_capacity(3);
    let mut current: Option<CodexPatch> = None;

    for line in lines {
        let line = line.trim_end_matches('\r');

        if line.starts_with("*** End Patch") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            break;
        } else if line.starts_with("*** Begin Patch") {
            continue;
        } else if line.starts_with("*** Update File:") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            let file_path = line
                .trim_start_matches("*** Update File:")
                .trim()
                .to_string();
            current = Some(CodexPatch {
                action: "update".to_string(),
                file_path,
                lines: Vec::with_capacity(20), // typical: 10-30 lines per patch
            });
        } else if line.starts_with("*** Add File:") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            let file_path = line.trim_start_matches("*** Add File:").trim().to_string();
            current = Some(CodexPatch {
                action: "add".to_string(),
                file_path,
                lines: Vec::with_capacity(20),
            });
        } else if line.starts_with("*** Delete File:") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            let file_path = line
                .trim_start_matches("*** Delete File:")
                .trim()
                .to_string();
            current = Some(CodexPatch {
                action: "delete".to_string(),
                file_path,
                lines: Vec::with_capacity(20),
            });
        } else if let Some(ref mut patch) = current {
            patch.lines.push(line.to_string());
        }
    }

    if let Some(patch) = current {
        patches.push(patch);
    }

    patches
}

/// Normalizes a `patch_apply_end` `changes` map into patch hunks.
///
/// Returns `None` when the payload is missing, not an object, or carries only
/// unrecognizable entries (schema drift). An empty object is a well-formed
/// no-op and yields an empty `Vec`. Entries are visited in sorted-path order
/// (the underlying map is a `BTreeMap`) so the folded detail order is stable.
fn parse_patch_changes(changes: Option<&Value>) -> Option<Vec<CodexPatch>> {
    let obj = changes?.as_object()?;
    let mut patches = Vec::with_capacity(obj.len());
    for (path, entry) in obj {
        if let Some(patch) = parse_patch_change_entry(path, entry) {
            patches.push(patch);
        }
    }
    if !obj.is_empty() && patches.is_empty() {
        return None;
    }
    Some(patches)
}

/// Builds one [`CodexPatch`] from a single `changes` entry.
///
/// The unified diff feeds the same `(old, new)` extraction as the direct
/// apply_patch path, so add / update / delete map to the identical metrics.
fn parse_patch_change_entry(path: &str, entry: &Value) -> Option<CodexPatch> {
    if path.is_empty() {
        return None;
    }
    let entry = entry.as_object()?;
    let action = entry.get("type").and_then(Value::as_str)?;
    if !matches!(action, "add" | "update" | "delete") {
        return None;
    }
    // Real logs omit `unified_diff` on some entries (observed on `add`); the
    // operation still counts, with no invented line/char content.
    let lines = entry
        .get("unified_diff")
        .and_then(Value::as_str)
        .map(|diff| diff.lines().map(str::to_string).collect())
        .unwrap_or_default();
    Some(CodexPatch {
        action: action.to_string(),
        file_path: path.to_string(),
        lines,
    })
}

/// Splits diff `lines` into the joined `(old, new)` text.
///
/// `+`-prefixed lines build the new content, `-`-prefixed lines the old;
/// `@@` hunk headers and `\` no-newline markers are skipped. Both results
/// have their trailing newline trimmed.
fn extract_patch_strings(lines: &[String]) -> (String, String) {
    // Pre-allocate with estimated capacity
    let estimated_size = lines.iter().map(|l| l.len()).sum::<usize>();
    let mut old_str = String::with_capacity(estimated_size / 2);
    let mut new_str = String::with_capacity(estimated_size / 2);

    for line in lines {
        if line.is_empty() {
            continue;
        }

        if line.len() > 1 && line.starts_with("@@") {
            continue;
        }

        let Some(first_char) = line.chars().next() else {
            continue;
        };
        match first_char {
            '+' => {
                new_str.push_str(&line[1..]);
                new_str.push('\n');
            }
            '-' => {
                old_str.push_str(&line[1..]);
                old_str.push('\n');
            }
            '\\' => continue,
            _ => {}
        }
    }

    // Trim in-place instead of allocating new strings
    let old_len = old_str.trim_end_matches('\n').len();
    old_str.truncate(old_len);
    let new_len = new_str.trim_end_matches('\n').len();
    new_str.truncate(new_len);

    (old_str, new_str)
}

/// Extracts the file path read by a `sed -n '<range>' <path>` command.
///
/// Returns `None` when the script is not a recognised `sed -n` read.
fn extract_sed_file_path(script: &str) -> Option<String> {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"sed\s+-n\s+'[^']*'\s+([^\s]+)").unwrap());
    let caps = re.captures(script)?;
    Some(
        caps.get(1)?
            .as_str()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string(),
    )
}

/// Extracts the `(path, content)` read by a `cat <path>` command.
///
/// `output` is the captured stdout (already metadata-stripped); content after
/// a `\n---` separator is dropped. Returns `None` when no `cat` line is found.
fn extract_cat_read(script: &str, output: &str) -> Option<(String, String)> {
    for line in script.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("cat ") {
            continue;
        }

        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }

        let path = fields[1].trim_matches(|c| c == '"' || c == '\'');

        // Optimize: avoid multiple allocations
        let clean_output = if let Some(idx) = output.find("\n---") {
            output[..idx].trim_end_matches('\n').to_string()
        } else {
            output.trim_end_matches('\n').to_string()
        };

        return Some((path.to_string(), clean_output));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_item(payload: Value) -> CodexLog {
        serde_json::from_value(serde_json::json!({
            "timestamp": "2026-07-12T00:00:00Z",
            "type": "response_item",
            "payload": payload
        }))
        .unwrap()
    }

    fn custom_output(call_id: &str, blocks: Value) -> CodexLog {
        response_item(serde_json::json!({
            "type": "custom_tool_call_output",
            "call_id": call_id,
            "output": blocks
        }))
    }

    fn patch_apply_end(call_id: &str, success: bool, changes: Value) -> CodexLog {
        serde_json::from_value(serde_json::json!({
            "timestamp": "2026-07-12T00:00:00Z",
            "type": "event_msg",
            "payload": {
                "type": "patch_apply_end",
                "call_id": call_id,
                "success": success,
                "changes": changes
            }
        }))
        .unwrap()
    }

    fn apply_patch_success_output(file: &str) -> CodexLog {
        let wire = serde_json::json!({
            "output": format!("Success. Updated the following files:\nM {file}")
        })
        .to_string();
        custom_output("shared-call", Value::String(wire))
    }

    #[test]
    fn legacy_shell_function_parses_into_call() {
        // Old schema: arguments = {"command": ["bash", "-lc", "<script>"]}
        let args = r#"{"command":["bash","-lc","ls -la"]}"#;
        let call = parse_function_call("shell", args, 42).expect("shell call should parse");
        assert_eq!(call.timestamp, 42);
        assert_eq!(call.script, "ls -la");
        assert_eq!(call.full_command, vec!["bash", "-lc", "ls -la"]);
    }

    #[test]
    fn current_exec_command_function_parses_into_call() {
        // Current schema: arguments = {"cmd":"...","workdir":"...","yield_time_ms":...}
        let args =
            r#"{"cmd":"sed -n '1,260p' src/main.rs","workdir":"/repo","yield_time_ms":1000}"#;
        let call =
            parse_function_call("exec_command", args, 99).expect("exec_command should parse");
        assert_eq!(call.timestamp, 99);
        assert_eq!(call.script, "sed -n '1,260p' src/main.rs");
        // `full_command` collapses to the single cmd string so
        // `record_run_command`'s `join(" ")` produces the verbatim command.
        assert_eq!(call.full_command, vec!["sed -n '1,260p' src/main.rs"]);
    }

    #[test]
    fn structured_function_arguments_are_normalized_end_to_end() {
        let logs = vec![
            response_item(serde_json::json!({
                "type": "function_call",
                "name": "exec_command",
                "arguments": { "cmd": "pwd", "workdir": "/repo" },
                "call_id": "exec-object"
            })),
            response_item(serde_json::json!({
                "type": "function_call_output",
                "call_id": "exec-object",
                "output": "done"
            })),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.run_command_details[0].command, "pwd");
    }

    #[test]
    fn unrelated_function_names_are_ignored() {
        // MCP tool calls, `update_plan`, etc. must not be treated as shell.
        assert!(parse_function_call("update_plan", "{}", 0).is_none());
        assert!(parse_function_call("_fetch_pr", "{}", 0).is_none());
    }

    #[test]
    fn malformed_arguments_yield_none_instead_of_panicking() {
        assert!(parse_function_call("shell", "not json", 0).is_none());
        assert!(parse_function_call("exec_command", "not json", 0).is_none());
    }

    #[test]
    fn paired_exec_argument_errors_are_supported_metric_free_lifecycles() {
        for arguments in ["{not json", r#"{"command_as_key":""}"#] {
            let logs = vec![
                response_item(serde_json::json!({
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": arguments,
                    "call_id": "bad-exec"
                })),
                response_item(serde_json::json!({
                    "type": "function_call_output",
                    "call_id": "bad-exec",
                    "output": "Error: failed to parse function arguments"
                })),
            ];

            let parsed =
                parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
            assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
            assert!(!parsed.diagnostics.is_complete_failure());
            assert_eq!(parsed.analysis.records[0].tool_call_counts.bash, 0);
            assert!(parsed.analysis.records[0].run_command_details.is_empty());
        }
    }

    #[test]
    fn invalid_exec_arguments_without_an_explicit_error_remain_drift() {
        let logs = vec![
            response_item(serde_json::json!({
                "type": "function_call",
                "name": "exec_command",
                "arguments": { "future_command": "pwd" },
                "call_id": "future-exec"
            })),
            response_item(serde_json::json!({
                "type": "function_call_output",
                "call_id": "future-exec",
                "output": "completed"
            })),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.analysis.records[0].tool_call_counts.bash, 0);
    }

    #[test]
    fn valid_token_count_without_model_context_is_not_schema_failure() {
        let logs: Vec<CodexLog> = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "session_meta",
                "payload": { "id": "compacted-session", "model_provider": "openai" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": { "input_tokens": 1, "output_tokens": 1 },
                        "last_token_usage": { "input_tokens": 1, "output_tokens": 1 },
                        "model_context_window": 200000
                    }
                }
            }),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert_eq!(parsed.diagnostics.relevant_records, 1);
        assert_eq!(parsed.diagnostics.normalized_records, 1);
        assert_eq!(parsed.diagnostics.failed_relevant_records, 0);
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }

    #[test]
    fn resumed_usage_subtracts_the_pre_context_replay_baseline() {
        let logs: Vec<CodexLog> = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "session_meta",
                "payload": { "id": "resumed-session", "model_provider": "openai" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 100,
                            "cached_input_tokens": 20,
                            "output_tokens": 30,
                            "reasoning_output_tokens": 10,
                            "total_tokens": 130
                        },
                        "model_context_window": 200000
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:02Z",
                "type": "turn_context",
                "payload": { "model": "gpt-5.3-codex" }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:03Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 160,
                            "cached_input_tokens": 25,
                            "output_tokens": 50,
                            "reasoning_output_tokens": 15,
                            "total_tokens": 210
                        },
                        "last_token_usage": { "input_tokens": 60, "output_tokens": 20 },
                        "model_context_window": 200000
                    }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:04Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": 190,
                            "cached_input_tokens": 30,
                            "output_tokens": 60,
                            "reasoning_output_tokens": 20,
                            "total_tokens": 250
                        },
                        "last_token_usage": { "input_tokens": 30, "output_tokens": 10 },
                        "model_context_window": 200000
                    }
                }
            }),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert!(!parsed.diagnostics.is_complete_failure());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);

        let usage = &parsed.analysis.records[0].conversation_usage["gpt-5.3-codex"];
        assert_eq!(usage["total_token_usage"]["input_tokens"], 90);
        assert_eq!(usage["total_token_usage"]["cached_input_tokens"], 10);
        assert_eq!(usage["total_token_usage"]["output_tokens"], 30);
        assert_eq!(usage["total_token_usage"]["reasoning_output_tokens"], 10);
        assert_eq!(usage["total_token_usage"]["total_tokens"], 120);
        assert_eq!(usage["last_token_usage"]["output_tokens"], 10);
        assert_eq!(usage["model_context_window"], 200000);
    }

    #[test]
    fn pre_context_usage_is_not_guessed_from_a_later_model() {
        let logs: Vec<CodexLog> = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": { "total_token_usage": { "input_tokens": 10 } }
                }
            }),
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:01Z",
                "type": "turn_context",
                "payload": { "model": "gpt-5.3-codex" }
            }),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }

    #[test]
    fn mid_session_model_switch_bills_each_model_its_own_delta() {
        // The cumulative counter keeps growing across a model switch; the
        // second model must be billed only the post-switch increment, not the
        // whole-session cumulative that already contains the first model.
        let token_count = |input: i64, cached: i64, output: i64, total: i64| {
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": input,
                            "cached_input_tokens": cached,
                            "output_tokens": output,
                            "reasoning_output_tokens": 0,
                            "total_tokens": total
                        },
                        "model_context_window": 200000
                    }
                }
            })
        };
        let turn_context = |model: &str| {
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "turn_context",
                "payload": { "model": model }
            })
        };
        let logs: Vec<CodexLog> = [
            turn_context("gpt-5.6-luna"),
            token_count(20_000, 4_000, 3_289, 23_289),
            turn_context("gpt-5.6-sol"),
            token_count(60_000, 30_000, 10_000, 70_000),
            token_count(90_000, 56_000, 14_576, 104_576),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);

        let usage = &parsed.analysis.records[0].conversation_usage;
        let luna = usage["gpt-5.6-luna"]["total_token_usage"]
            .as_object()
            .unwrap();
        let sol = usage["gpt-5.6-sol"]["total_token_usage"]
            .as_object()
            .unwrap();
        assert_eq!(luna["total_tokens"].as_i64().unwrap(), 23_289);
        assert_eq!(sol["total_tokens"].as_i64().unwrap(), 104_576 - 23_289);
        assert_eq!(
            luna["total_tokens"].as_i64().unwrap() + sol["total_tokens"].as_i64().unwrap(),
            104_576,
            "the two models' attributed totals must reconstruct the session cumulative"
        );
    }

    #[test]
    fn per_turn_tier_classification_uses_the_turns_own_context() {
        // Two turns: the first's own prompt (last_token_usage.input_tokens)
        // is below the 272k threshold, the second's is above. Only the second
        // turn's delta lands in above_tier even though the cumulative total
        // crosses the threshold much earlier.
        let event = |cum_input: i64, cum_out: i64, cum_total: i64, last_input: i64| {
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "event_msg",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": {
                            "input_tokens": cum_input,
                            "cached_input_tokens": 0,
                            "output_tokens": cum_out,
                            "reasoning_output_tokens": 0,
                            "total_tokens": cum_total
                        },
                        "last_token_usage": { "input_tokens": last_input },
                        "model_context_window": 400000
                    }
                }
            })
        };
        let logs: Vec<CodexLog> = [
            serde_json::json!({
                "timestamp": "2026-07-12T00:00:00Z",
                "type": "turn_context",
                "payload": { "model": "gpt-5.4" }
            }),
            event(200_000, 1_000, 201_000, 200_000),
            event(500_000, 2_500, 502_500, 300_000),
        ]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let tiers =
            crate::pricing::TierThresholds::from_entries([("gpt-5.4", 272_000)].into_iter());
        let parsed =
            parse_codex_log_iter_with_diagnostics(&logs, ParseMode::UsageOnly, Some(&tiers))
                .unwrap();
        let usage = &parsed.analysis.records[0].conversation_usage["gpt-5.4"];
        assert_eq!(usage["total_token_usage"]["total_tokens"], 502_500);
        // Only the second turn's delta (input 300k, output 1.5k) is above.
        assert_eq!(usage["above_tier"]["input_tokens"], 300_000);
        assert_eq!(usage["above_tier"]["output_tokens"], 1_500);
        assert!(
            usage["above_tier"].get("cache_read_tokens").is_none(),
            "no cached tokens in this session"
        );
    }

    #[test]
    fn malformed_pre_context_usage_remains_schema_failure() {
        let logs: Vec<CodexLog> = [serde_json::json!({
            "timestamp": "2026-07-12T00:00:00Z",
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": { "total_token_usage": { "input_tokens": "10" } }
            }
        })]
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert!(parsed.diagnostics.is_complete_failure());
        assert!(parsed.analysis.records[0].conversation_usage.is_empty());
    }

    #[test]
    fn usage_validation_rejects_unknown_only_and_wrong_typed_token_payloads() {
        assert!(is_supported_codex_usage(&serde_json::json!({
            "total_token_usage": {}
        })));
        assert!(is_supported_codex_usage(&serde_json::json!({
            "last_token_usage": { "input_tokens": 0, "future_metric": 5 }
        })));
        assert!(is_supported_codex_usage(&serde_json::json!({
            "total_token_usage": { "output_tokens": 0 },
            "model_context_window": null
        })));
        assert!(is_supported_codex_usage(&serde_json::json!({
            "model_context_window": 200000
        })));

        assert!(!is_supported_codex_usage(&serde_json::json!({
            "total_token_usage": { "prompt_tokens": 3 }
        })));
        assert!(!is_supported_codex_usage(&serde_json::json!({
            "last_token_usage": { "output_tokens": "3" }
        })));
        assert!(!is_supported_codex_usage(&serde_json::json!({
            "total_token_usage": { "input_tokens": 3 },
            "last_token_usage": { "completion_tokens": 1 }
        })));
        assert!(!is_supported_codex_usage(&serde_json::json!({
            "model_context_window": "200000"
        })));
    }

    #[test]
    fn exec_command_metadata_prefix_is_stripped() {
        // Real-world Codex `exec_command` output wraps actual stdout with
        // a 5-line header; without stripping, the analyzer over-counts
        // file reads by 5 lines per sed/cat invocation.
        let raw = "Chunk ID: deadbeef\n\
                   Wall time: 0.0000 seconds\n\
                   Process exited with code 0\n\
                   Original token count: 100\n\
                   Output:\n\
                   line one\n\
                   line two\n";
        assert_eq!(
            strip_exec_command_metadata_prefix(raw),
            "line one\nline two\n"
        );
    }

    #[test]
    fn legacy_shell_output_passes_through_unchanged() {
        // Legacy `shell` function output has no metadata header; the
        // helper must leave non-prefixed strings exactly as-is so the
        // existing fixture-based tests keep matching.
        let raw = "line one\nline two\n";
        assert_eq!(strip_exec_command_metadata_prefix(raw), raw);
    }

    #[test]
    fn output_starting_with_marker_handles_no_leading_newline() {
        // Defensive: if a future Codex variant drops the leading newline
        // before the `Output:` marker, still strip it cleanly.
        let raw = "Output:\nthe content";
        assert_eq!(strip_exec_command_metadata_prefix(raw), "the content");
    }

    #[test]
    fn custom_apply_patch_requires_supported_nonempty_file_headers() {
        let drifted = "*** Begin Patch\n*** Future File: src/lib.rs\n+new\n*** End Patch";
        assert!(
            parse_custom_call("apply_patch", drifted, 1, ParseMode::Full).is_none(),
            "an unknown file header must not normalize as a successful patch"
        );

        for patch in [
            "*** Begin Patch\n*** Add File: empty.txt\n*** End Patch",
            "*** Begin Patch\n*** Delete File: empty.txt\n*** End Patch",
        ] {
            let Some(CodexCustomCall::ApplyPatch { patches, .. }) =
                parse_custom_call("apply_patch", patch, 1, ParseMode::Full)
            else {
                panic!("supported empty-body patch was rejected");
            };
            assert_eq!(patches.len(), 1);
            assert_eq!(patches[0].file_path, "empty.txt");
        }

        let empty_path = "*** Begin Patch\n*** Add File:\n+new\n*** End Patch";
        assert!(parse_custom_call("apply_patch", empty_path, 1, ParseMode::Full).is_none());
    }

    #[test]
    fn direct_custom_apply_patch_success_is_paired_and_parsed() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
        let wire_result = serde_json::json!({
            "output": "Success. Updated the following files:\nM src/lib.rs",
            "metadata": { "exit_code": 0, "duration_seconds": 0.01 }
        })
        .to_string();
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "patch-1"
            })),
            custom_output("patch-1", Value::String(wire_result)),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.edit_file_details.len(), 1);
        assert_eq!(record.edit_file_details[0].base.file_path, "src/lib.rs");
    }

    #[test]
    fn direct_object_custom_apply_patch_output_is_paired_and_parsed() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "patch-object"
            })),
            custom_output(
                "patch-object",
                serde_json::json!({
                    "output": "Success. Updated the following files:\nM src/lib.rs",
                    "metadata": { "exit_code": 0, "duration_seconds": 0.01 }
                }),
            ),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.edit_file_details.len(), 1);
    }

    #[test]
    fn failed_direct_custom_apply_patch_is_skipped() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
        let wire_result = serde_json::json!({
            "output": "Failed to find expected lines in src/lib.rs",
            "metadata": { "exit_code": 1, "duration_seconds": 0.01 }
        })
        .to_string();
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "patch-failed"
            })),
            custom_output("patch-failed", Value::String(wire_result)),
        ];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 0);
        assert!(record.edit_file_details.is_empty());
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
    }

    #[test]
    fn unknown_direct_custom_apply_patch_result_is_skipped() {
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch";
        for output in [
            Value::Null,
            serde_json::json!("Patch command finished"),
            serde_json::json!({
                "success": true,
                "updated_files": ["src/lib.rs"]
            }),
        ] {
            let logs = vec![
                response_item(serde_json::json!({
                    "type": "custom_tool_call",
                    "name": "apply_patch",
                    "input": patch,
                    "call_id": "patch-unknown"
                })),
                custom_output("patch-unknown", output),
            ];

            let parsed =
                parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
            let record = &parsed.analysis.records[0];
            assert_eq!(record.tool_call_counts.edit, 0);
            assert!(record.edit_file_details.is_empty());
            assert_eq!(parsed.diagnostics.relevant_records, 2);
            assert_eq!(parsed.diagnostics.normalized_records, 1);
            assert_eq!(parsed.diagnostics.failed_relevant_records, 1);
            assert_eq!(parsed.diagnostics.partial_failure_count(), 1);
            assert!(!parsed.diagnostics.is_complete_failure());
        }
    }

    #[test]
    fn custom_output_envelope_is_stripped_before_json_result() {
        let result = serde_json::json!({
            "output": "Success. Updated the following files:\nM src/lib.rs",
            "metadata": { "exit_code": 0, "duration_seconds": 0.01 }
        })
        .to_string();
        let output = custom_output(
            "patch-array",
            serde_json::json!([
                { "type": "input_text", "text": "Script completed\nWall time: 0.1 seconds\nOutput:\n" },
                { "type": "input_text", "text": result }
            ]),
        )
        .payload
        .output;

        assert_eq!(
            normalize_custom_output(output.as_deref()),
            "Success. Updated the following files:\nM src/lib.rs"
        );
        assert_eq!(
            custom_apply_patch_result(output.as_deref()),
            CustomApplyPatchResult::Success
        );
    }

    #[test]
    fn custom_exec_is_one_bash_cell_not_nested_operations() {
        let input = "const results = await Promise.all([tools.exec_command({cmd: \"sed -n '1,2p' src/lib.rs\"}), tools.apply_patch(`*** Begin Patch\n*** Add File: notes.txt\n+hello\n*** End Patch`)]); text(JSON.stringify(results));";
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "exec",
                "input": input,
                "call_id": "exec-1"
            })),
            custom_output("exec-1", serde_json::json!("Script completed")),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert_eq!(record.tool_call_counts.read, 0);
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.tool_call_counts.edit, 0);
        assert_eq!(record.run_command_details.len(), 1);
        assert_eq!(record.run_command_details[0].command, input);
    }

    #[test]
    fn custom_exec_usage_only_keeps_count_without_source_detail() {
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "exec",
                "input": "await tools.exec_command({cmd: dynamicCommand});",
                "call_id": "exec-dynamic"
            })),
            custom_output("exec-dynamic", serde_json::json!("Script completed")),
        ];

        let analysis = parse_codex_log_iter(logs, ParseMode::UsageOnly).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.bash, 1);
        assert!(record.run_command_details.is_empty());
    }

    #[test]
    fn current_exec_command_apply_patch_marker_is_parsed() {
        let args = serde_json::json!({
            "cmd": "apply_patch <<'PATCH'\n*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-old\n+new\n*** End Patch\nPATCH"
        })
        .to_string();
        let call = parse_function_call("exec_command", &args, 1).unwrap();
        let mut state = SessionParseState::new();
        state.handle_shell_call(
            call,
            CodexShellOutput {
                output: String::new(),
                metadata: None,
            },
        );
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.edit_details.len(), 1);
    }

    #[test]
    fn patch_apply_end_success_counts_add_as_write_and_update_as_edit() {
        let changes = serde_json::json!({
            "/repo/new.rs": {
                "type": "add",
                "unified_diff": "@@ -0,0 +1,2 @@\n+fn main() {}\n+// added\n"
            },
            "/repo/lib.rs": {
                "type": "update",
                "unified_diff": "@@ -1,1 +1,1 @@\n-old\n+new\n"
            }
        });
        let logs = vec![patch_apply_end("call-1", true, changes)];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.total_unique_files, 2);
        assert_eq!(record.write_file_details[0].base.file_path, "/repo/new.rs");
        assert_eq!(record.edit_file_details[0].base.file_path, "/repo/lib.rs");
    }

    #[test]
    fn patch_apply_end_without_unified_diff_still_counts_the_operation() {
        // Observed in real 2026-06 logs: an `add` entry with no unified_diff.
        // The write must count (invocation + unique file, zero line/char
        // content) instead of flagging the record as schema drift.
        let changes = serde_json::json!({
            "/repo/generated.bin": { "type": "add" }
        });
        let logs = vec![patch_apply_end("call-nodiff", true, changes)];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.write, 1);
        assert_eq!(record.total_unique_files, 1);
        assert_eq!(record.total_write_lines, 0);
        assert_eq!(parsed.diagnostics.partial_failure_count(), 0);
    }

    #[test]
    fn patch_apply_end_failure_creates_no_file_ops() {
        let changes = serde_json::json!({
            "/repo/x.rs": { "type": "add", "unified_diff": "@@ -0,0 +1,1 @@\n+nope\n" }
        });
        let logs = vec![patch_apply_end("call-fail", false, changes)];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        let record = &parsed.analysis.records[0];
        assert_eq!(record.tool_call_counts.write, 0);
        assert_eq!(record.total_unique_files, 0);
        assert!(record.write_file_details.is_empty());
        // Recognized event, but a failed apply is not a relevant normalization.
        assert_eq!(parsed.diagnostics.relevant_records, 0);
        assert!(!parsed.diagnostics.is_complete_failure());
    }

    #[test]
    fn patch_apply_end_malformed_changes_is_relevant_failure() {
        // success is true but `changes` is not an object → schema drift.
        let logs = vec![patch_apply_end(
            "call-x",
            true,
            Value::String("nope".into()),
        )];

        let parsed = parse_codex_log_iter_with_diagnostics(&logs, ParseMode::Full, None).unwrap();
        assert_eq!(parsed.analysis.records[0].tool_call_counts.edit, 0);
        assert_eq!(parsed.analysis.records[0].tool_call_counts.write, 0);
        assert!(parsed.diagnostics.is_complete_failure());
    }

    #[test]
    fn patch_apply_end_deduped_against_direct_apply_patch_before_output() {
        let patch = "*** Begin Patch\n*** Update File: lib.rs\n@@\n-old\n+new\n*** End Patch";
        let changes = serde_json::json!({
            "lib.rs": { "type": "update", "unified_diff": "@@ -1,1 +1,1 @@\n-old\n+new\n" }
        });
        // Event arrives between the custom_tool_call and its output.
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "shared-call"
            })),
            patch_apply_end("shared-call", true, changes),
            apply_patch_success_output("lib.rs"),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.edit_file_details.len(), 1);
    }

    #[test]
    fn patch_apply_end_deduped_against_direct_apply_patch_after_output() {
        let patch = "*** Begin Patch\n*** Update File: lib.rs\n@@\n-old\n+new\n*** End Patch";
        let changes = serde_json::json!({
            "lib.rs": { "type": "update", "unified_diff": "@@ -1,1 +1,1 @@\n-old\n+new\n" }
        });
        // Event arrives after the pending call was already consumed by its output.
        let logs = vec![
            response_item(serde_json::json!({
                "type": "custom_tool_call",
                "name": "apply_patch",
                "input": patch,
                "call_id": "shared-call"
            })),
            apply_patch_success_output("lib.rs"),
            patch_apply_end("shared-call", true, changes),
        ];

        let analysis = parse_codex_logs(&logs, ParseMode::Full).unwrap();
        let record = &analysis.records[0];
        assert_eq!(record.tool_call_counts.edit, 1);
        assert_eq!(record.edit_file_details.len(), 1);
    }

    #[test]
    fn patch_apply_end_scalar_counts_match_across_parse_modes() {
        let changes = serde_json::json!({
            "/repo/new.rs": {
                "type": "add",
                "unified_diff": "@@ -0,0 +1,2 @@\n+fn main() {}\n+// added\n"
            },
            "/repo/lib.rs": {
                "type": "update",
                "unified_diff": "@@ -1,1 +1,1 @@\n-old\n+new\n"
            }
        });
        let build = || vec![patch_apply_end("call-1", true, changes.clone())];

        let full = parse_codex_logs(&build(), ParseMode::Full).unwrap();
        let usage = parse_codex_logs(&build(), ParseMode::UsageOnly).unwrap();
        let (f, u) = (&full.records[0], &usage.records[0]);

        assert_eq!(f.tool_call_counts.write, u.tool_call_counts.write);
        assert_eq!(f.tool_call_counts.edit, u.tool_call_counts.edit);
        assert_eq!(f.total_unique_files, u.total_unique_files);
        assert_eq!(f.total_write_lines, u.total_write_lines);
        assert_eq!(f.total_edit_lines, u.total_edit_lines);
        assert_eq!(f.total_write_characters, u.total_write_characters);
        assert_eq!(f.total_edit_characters, u.total_edit_characters);
        // Detail bodies are retained only in Full mode.
        assert!(!f.write_file_details.is_empty());
        assert!(u.write_file_details.is_empty());
    }
}
