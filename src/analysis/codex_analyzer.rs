use crate::models::*;
use crate::utils::{count_lines, get_git_remote_url, parse_iso_timestamp};
use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Analyze Codex conversations
pub fn analyze_codex_conversations(logs: &[CodexLog]) -> Result<CodeAnalysis> {
    let mut state = CodexAnalysisState::new();
    let mut conversation_usage: HashMap<String, Value> = HashMap::new();
    let mut current_model = String::new();
    let mut shell_calls: HashMap<String, CodexShellCall> = HashMap::new();

    for entry in logs {
        let ts = parse_iso_timestamp(&entry.timestamp);
        if ts > state.last_ts {
            state.last_ts = ts;
        }

        match entry.log_type.as_str() {
            "session_meta" => {
                if state.folder_path.is_empty() {
                    if let Some(cwd) = &entry.payload.cwd {
                        state.folder_path = cwd.clone();
                    }
                }
                if state.task_id.is_empty() {
                    if let Some(id) = &entry.payload.id {
                        state.task_id = id.clone();
                    }
                }
                if state.git_remote.is_empty() {
                    if let Some(git) = &entry.payload.git {
                        if let Some(url) = &git.repository_url {
                            state.git_remote = url.clone();
                        }
                    }
                }
            }
            "turn_context" => {
                if state.folder_path.is_empty() {
                    if let Some(cwd) = &entry.payload.cwd {
                        state.folder_path = cwd.clone();
                    }
                }
                if let Some(model) = &entry.payload.model {
                    current_model = model.clone();
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

/// Process Codex usage data
fn process_codex_usage(conversation_usage: &mut HashMap<String, Value>, model: &str, info: &Value) {
    let info_obj = match info.as_object() {
        Some(obj) => obj,
        None => return,
    };

    let existing = conversation_usage
        .entry(model.to_string())
        .or_insert_with(|| {
            serde_json::json!({
                "total_token_usage": {},
                "last_token_usage": {},
                "model_context_window": null
            })
        });

    let existing_obj = existing.as_object_mut().unwrap();

    // Process total_token_usage
    if let Some(total_usage) = info_obj
        .get("total_token_usage")
        .and_then(|v| v.as_object())
    {
        let existing_total = existing_obj
            .entry("total_token_usage".to_string())
            .or_insert_with(|| serde_json::json!({}));

        if let Some(existing_total_obj) = existing_total.as_object_mut() {
            for (key, value) in total_usage {
                if let Some(v) = value.as_i64() {
                    let current = existing_total_obj
                        .get(key)
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    existing_total_obj.insert(key.clone(), (current + v).into());
                }
            }
        }
    }

    // Process last_token_usage
    if let Some(last_usage) = info_obj.get("last_token_usage") {
        existing_obj.insert("last_token_usage".to_string(), last_usage.clone());
    }

    // Handle model_context_window
    if let Some(context_window) = info_obj.get("model_context_window") {
        existing_obj.insert("model_context_window".to_string(), context_window.clone());
    }
}

struct CodexAnalysisState {
    write_details: Vec<CodeAnalysisWriteDetail>,
    read_details: Vec<CodeAnalysisReadDetail>,
    edit_details: Vec<CodeAnalysisApplyDiffDetail>,
    run_details: Vec<CodeAnalysisRunCommandDetail>,
    tool_counts: CodeAnalysisToolCalls,
    unique_files: HashSet<String>,
    total_write_lines: usize,
    total_read_lines: usize,
    total_edit_lines: usize,
    total_write_characters: usize,
    total_read_characters: usize,
    total_edit_characters: usize,
    folder_path: String,
    git_remote: String,
    task_id: String,
    last_ts: i64,
}

impl CodexAnalysisState {
    fn new() -> Self {
        Self {
            write_details: Vec::new(),
            read_details: Vec::new(),
            edit_details: Vec::new(),
            run_details: Vec::new(),
            tool_counts: CodeAnalysisToolCalls::default(),
            unique_files: HashSet::new(),
            total_write_lines: 0,
            total_read_lines: 0,
            total_edit_lines: 0,
            total_write_characters: 0,
            total_read_characters: 0,
            total_edit_characters: 0,
            folder_path: String::new(),
            git_remote: String::new(),
            task_id: String::new(),
            last_ts: 0,
        }
    }

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

    fn add_read_detail(&mut self, path: &str, content: &str, ts: i64) {
        let trimmed = content.trim_end_matches('\n');
        if trimmed.is_empty() {
            return;
        }

        let line_count = count_lines(trimmed);
        let char_count = trimmed.chars().count();
        let resolved = self.normalize_path(path);

        if resolved.is_empty() {
            return;
        }

        self.read_details.push(CodeAnalysisReadDetail {
            base: CodeAnalysisDetailBase {
                file_path: resolved.clone(),
                line_count,
                character_count: char_count,
                timestamp: ts,
            },
        });

        self.unique_files.insert(resolved);
        self.total_read_lines += line_count;
        self.total_read_characters += char_count;
        self.tool_counts.read += 1;
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
        self.unique_files.insert(resolved.clone());

        match patch.action.as_str() {
            "add" => {
                let content = new_str.trim_end_matches('\n');
                let line_count = count_lines(content);
                let char_count = content.chars().count();

                self.write_details.push(CodeAnalysisWriteDetail {
                    base: CodeAnalysisDetailBase {
                        file_path: resolved,
                        line_count,
                        character_count: char_count,
                        timestamp: ts,
                    },
                    content: content.to_string(),
                });

                self.tool_counts.write += 1;
                self.total_write_lines += line_count;
                self.total_write_characters += char_count;
            }
            "delete" => {
                let content = old_str.trim_end_matches('\n');
                if content.is_empty() {
                    return;
                }

                let line_count = count_lines(content);
                let char_count = content.chars().count();

                self.edit_details.push(CodeAnalysisApplyDiffDetail {
                    base: CodeAnalysisDetailBase {
                        file_path: resolved,
                        line_count,
                        character_count: char_count,
                        timestamp: ts,
                    },
                    old_string: content.to_string(),
                    new_string: String::new(),
                });

                self.tool_counts.edit += 1;
                self.total_edit_lines += line_count;
                self.total_edit_characters += char_count;
            }
            _ => {
                let content = new_str.trim_end_matches('\n');
                let line_count = count_lines(content);
                let char_count = content.chars().count();

                let trimmed_old = old_str.trim_end_matches('\n');

                if trimmed_old.is_empty() && !content.is_empty() {
                    // New file creation
                    self.write_details.push(CodeAnalysisWriteDetail {
                        base: CodeAnalysisDetailBase {
                            file_path: resolved,
                            line_count,
                            character_count: char_count,
                            timestamp: ts,
                        },
                        content: content.to_string(),
                    });

                    self.tool_counts.write += 1;
                    self.total_write_lines += line_count;
                    self.total_write_characters += char_count;
                } else {
                    // File modification
                    self.edit_details.push(CodeAnalysisApplyDiffDetail {
                        base: CodeAnalysisDetailBase {
                            file_path: resolved,
                            line_count,
                            character_count: char_count,
                            timestamp: ts,
                        },
                        old_string: trimmed_old.to_string(),
                        new_string: content.to_string(),
                    });

                    self.tool_counts.edit += 1;
                    self.total_edit_lines += line_count;
                    self.total_edit_characters += char_count;
                }
            }
        }
    }

    fn record_run_command(&mut self, call: CodexShellCall) {
        let command_str = if call.full_command.is_empty() {
            call.script.trim().to_string()
        } else {
            call.full_command.join(" ").trim().to_string()
        };

        if command_str.is_empty() {
            return;
        }

        let command_chars = command_str.chars().count();

        self.run_details.push(CodeAnalysisRunCommandDetail {
            base: CodeAnalysisDetailBase {
                file_path: self.folder_path.clone(),
                line_count: 0,
                character_count: command_chars,
                timestamp: call.timestamp,
            },
            command: command_str,
            description: String::new(),
        });

        self.tool_counts.bash += 1;
    }

    fn normalize_path(&self, path: &str) -> String {
        if path.is_empty() {
            return String::new();
        }

        let path_buf = std::path::PathBuf::from(path);
        if path_buf.is_absolute() {
            return path.to_string();
        }

        if self.folder_path.is_empty() {
            return path.to_string();
        }

        std::path::PathBuf::from(&self.folder_path)
            .join(path)
            .to_string_lossy()
            .to_string()
    }

    fn into_record(self, conversation_usage: HashMap<String, Value>) -> CodeAnalysisRecord {
        CodeAnalysisRecord {
            total_unique_files: self.unique_files.len(),
            total_write_lines: self.total_write_lines,
            total_read_lines: self.total_read_lines,
            total_edit_lines: self.total_edit_lines,
            total_write_characters: self.total_write_characters,
            total_read_characters: self.total_read_characters,
            total_edit_characters: self.total_edit_characters,
            write_file_details: self.write_details,
            read_file_details: self.read_details,
            edit_file_details: self.edit_details,
            run_command_details: self.run_details,
            tool_call_counts: self.tool_counts,
            conversation_usage,
            task_id: self.task_id,
            timestamp: self.last_ts,
            folder_path: self.folder_path,
            git_remote_url: self.git_remote,
        }
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
    let mut patches = Vec::new();
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
                lines: Vec::new(),
            });
        } else if line.starts_with("*** Add File:") {
            if let Some(patch) = current.take() {
                patches.push(patch);
            }
            let file_path = line.trim_start_matches("*** Add File:").trim().to_string();
            current = Some(CodexPatch {
                action: "add".to_string(),
                file_path,
                lines: Vec::new(),
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
                lines: Vec::new(),
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
    let mut old_str = String::new();
    let mut new_str = String::new();

    for line in lines {
        if line.is_empty() {
            continue;
        }

        if line.len() > 1 && line.starts_with("@@") {
            continue;
        }

        let first_char = line.chars().next().unwrap();
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

    (
        old_str.trim_end_matches('\n').to_string(),
        new_str.trim_end_matches('\n').to_string(),
    )
}

fn extract_sed_file_path(script: &str) -> Option<String> {
    let re = Regex::new(r"sed\s+-n\s+'[^']*'\s+([^\s]+)").ok()?;
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

        let mut clean_output = output.to_string();
        if let Some(idx) = clean_output.find("\n---") {
            clean_output = clean_output[..idx].to_string();
        }
        clean_output = clean_output.trim_end_matches('\n').to_string();

        return Some((path.to_string(), clean_output));
    }

    None
}
