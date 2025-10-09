use crate::analysis::common_state::AnalysisState;
use crate::models::*;
use crate::utils::{get_git_remote_url, parse_iso_timestamp, process_codex_usage};
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;

/// Analyze Codex conversations
pub fn analyze_codex_conversations(logs: &[CodexLog]) -> Result<CodeAnalysis> {
    let mut state = AnalysisState::new();
    let mut conversation_usage: HashMap<String, Value> = HashMap::with_capacity(5);
    let mut current_model = String::new();
    let mut shell_calls: HashMap<String, CodexShellCall> = HashMap::with_capacity(50);

    for entry in logs {
        let ts = parse_iso_timestamp(&entry.timestamp);
        if ts > state.last_ts {
            state.last_ts = ts;
        }

        match entry.log_type.as_str() {
            "session_meta" => {
                if state.folder_path.is_empty() {
                    if let Some(cwd) = &entry.payload.cwd {
                        state.folder_path.clone_from(cwd);  // More efficient than clone()
                    }
                }
                if state.task_id.is_empty() {
                    if let Some(id) = &entry.payload.id {
                        state.task_id.clone_from(id);
                    }
                }
                if state.git_remote.is_empty() {
                    if let Some(git) = &entry.payload.git {
                        if let Some(url) = &git.repository_url {
                            state.git_remote.clone_from(url);
                        }
                    }
                }
            }
            "turn_context" => {
                if state.folder_path.is_empty() {
                    if let Some(cwd) = &entry.payload.cwd {
                        state.folder_path.clone_from(cwd);
                    }
                }
                if let Some(model) = &entry.payload.model {
                    current_model.clone_from(model);  // Reuse existing allocation
                }
            }
            "event_msg" => {
                if let Some(payload_type) = &entry.payload.payload_type {
                    if payload_type == "token_count" && !current_model.is_empty() {
                        if let Some(info) = &entry.payload.info {
                            process_codex_usage(&mut conversation_usage, &current_model, info);
                        }
                    }
                }
            }
            "response_item" => {
                if let Some(payload_type) = &entry.payload.payload_type {
                    match payload_type.as_str() {
                        "function_call" => {
                            if let Some(name) = &entry.payload.name {
                                if name == "shell" {
                                    if let Some(args_str) = &entry.payload.arguments {
                                        if let Ok(args) =
                                            serde_json::from_str::<CodexShellArguments>(args_str)
                                        {
                                            let script =
                                                args.command.last().cloned().unwrap_or_default();
                                            if let Some(call_id) = &entry.payload.call_id {
                                                shell_calls.insert(
                                                    call_id.clone(),
                                                    CodexShellCall {
                                                        timestamp: ts,
                                                        script: script.clone(),
                                                        full_command: args.command,
                                                    },
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        "function_call_output" => {
                            if let Some(call_id) = &entry.payload.call_id {
                                if let Some(call) = shell_calls.remove(call_id) {
                                    let output = if let Some(output_str) = &entry.payload.output {
                                        serde_json::from_str::<CodexShellOutput>(output_str)
                                            .unwrap_or_else(|_| CodexShellOutput {
                                                output: output_str.clone(),
                                                metadata: None,
                                            })
                                    } else {
                                        CodexShellOutput {
                                            output: String::new(),
                                            metadata: None,
                                        }
                                    };
                                    state.handle_shell_call(call, output);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    if state.git_remote.is_empty() {
        state.git_remote = get_git_remote_url(&state.folder_path);
    }

    let record = state.into_record(conversation_usage);

    Ok(CodeAnalysis {
        user: String::new(),
        extension_name: String::new(),
        insights_version: String::new(),
        machine_id: String::new(),
        records: vec![record],
    })
}

// Codex-specific extension methods for AnalysisState
trait CodexAnalysisExt {
    fn handle_shell_call(&mut self, call: CodexShellCall, output: CodexShellOutput);
    fn handle_patch(&mut self, patch: CodexPatch, ts: i64);
    fn record_run_command(&mut self, call: CodexShellCall);
}

impl CodexAnalysisExt for AnalysisState {
    fn handle_shell_call(&mut self, call: CodexShellCall, output: CodexShellOutput) {
        // Check for applypatch script
        if call.script.contains("applypatch") {
            let patches = parse_apply_patch_script(&call.script);
            for patch in patches {
                self.handle_patch(patch, call.timestamp);
            }
            return;
        }

        // Check for sed command
        if let Some(path) = extract_sed_file_path(&call.script) {
            self.add_read_detail(&path, &output.output, call.timestamp);
            return;
        }

        // Check for cat command
        if let Some((path, content)) = extract_cat_read(&call.script, &output.output) {
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

struct CodexShellCall {
    timestamp: i64,
    script: String,
    full_command: Vec<String>,
}

struct CodexPatch {
    action: String,
    file_path: String,
    lines: Vec<String>,
}

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
