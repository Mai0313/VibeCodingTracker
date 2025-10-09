use crate::models::*;
use crate::utils::count_lines;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Common analysis state shared by all analyzers (Claude, Codex, Gemini)
pub struct AnalysisState {
    pub write_details: Vec<CodeAnalysisWriteDetail>,
    pub read_details: Vec<CodeAnalysisReadDetail>,
    pub edit_details: Vec<CodeAnalysisApplyDiffDetail>,
    pub run_details: Vec<CodeAnalysisRunCommandDetail>,
    pub tool_counts: CodeAnalysisToolCalls,
    pub unique_files: HashSet<String>,
    pub total_write_lines: usize,
    pub total_read_lines: usize,
    pub total_edit_lines: usize,
    pub total_write_characters: usize,
    pub total_read_characters: usize,
    pub total_edit_characters: usize,
    pub folder_path: String,
    pub git_remote: String,
    pub task_id: String,
    pub last_ts: i64,
}

impl AnalysisState {
    pub fn new() -> Self {
        // Pre-allocate Vecs with reasonable capacity estimates based on typical session sizes
        // This significantly reduces allocations and memory fragmentation
        Self {
            write_details: Vec::with_capacity(10), // typical: 5-15 write operations
            read_details: Vec::with_capacity(20),  // typical: 10-30 read operations
            edit_details: Vec::with_capacity(15),  // typical: 10-20 edit operations
            run_details: Vec::with_capacity(10),   // typical: 5-15 bash commands
            tool_counts: CodeAnalysisToolCalls::default(),
            unique_files: HashSet::with_capacity(20), // typical: 10-30 unique files
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

    /// Add a read operation detail
    pub fn add_read_detail(&mut self, path: &str, content: &str, ts: i64) {
        let trimmed = content.trim_end_matches('\n');
        let line_count = count_lines(trimmed);

        if line_count == 0 {
            return;
        }

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

    /// Add a write operation detail
    pub fn add_write_detail(&mut self, path: &str, content: &str, ts: i64) {
        let trimmed = content.trim_end_matches('\n');
        let line_count = count_lines(trimmed);
        let char_count = trimmed.chars().count();
        let resolved = self.normalize_path(path);

        if resolved.is_empty() {
            return;
        }

        self.write_details.push(CodeAnalysisWriteDetail {
            base: CodeAnalysisDetailBase {
                file_path: resolved.clone(),
                line_count,
                character_count: char_count,
                timestamp: ts,
            },
            content: trimmed.to_string(),
        });

        self.unique_files.insert(resolved);
        self.total_write_lines += line_count;
        self.total_write_characters += char_count;
        self.tool_counts.write += 1;
    }

    /// Add an edit operation detail
    pub fn add_edit_detail(&mut self, path: &str, old: &str, new: &str, ts: i64) {
        let trimmed_new = new.trim_end_matches('\n');
        let trimmed_old = old.trim_end_matches('\n');

        // If old is empty and new has content, treat as write
        if trimmed_old.is_empty() && !trimmed_new.is_empty() {
            self.add_write_detail(path, new, ts);
            return;
        }

        let line_count = count_lines(trimmed_new);
        let char_count = trimmed_new.chars().count();
        let resolved = self.normalize_path(path);

        if resolved.is_empty() {
            return;
        }

        self.edit_details.push(CodeAnalysisApplyDiffDetail {
            base: CodeAnalysisDetailBase {
                file_path: resolved.clone(),
                line_count,
                character_count: char_count,
                timestamp: ts,
            },
            old_string: trimmed_old.to_string(),
            new_string: trimmed_new.to_string(),
        });

        self.unique_files.insert(resolved);
        self.total_edit_lines += line_count;
        self.total_edit_characters += char_count;
        self.tool_counts.edit += 1;
    }

    /// Add a run command detail
    pub fn add_run_command(&mut self, command: &str, description: &str, ts: i64) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }

        let command_chars = command.chars().count();

        self.run_details.push(CodeAnalysisRunCommandDetail {
            base: CodeAnalysisDetailBase {
                file_path: self.folder_path.clone(),
                line_count: 0,
                character_count: command_chars,
                timestamp: ts,
            },
            command: command.to_string(),
            description: description.to_string(),
        });

        self.tool_counts.bash += 1;
    }

    /// Normalize a file path (convert relative to absolute if needed)
    pub fn normalize_path(&self, path: &str) -> String {
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

    /// Convert state into a CodeAnalysisRecord
    pub fn into_record(self, conversation_usage: HashMap<String, Value>) -> CodeAnalysisRecord {
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

impl Default for AnalysisState {
    fn default() -> Self {
        Self::new()
    }
}
