use crate::constants::FastHashMap;
use crate::models::*;
use crate::utils::count_lines;
use serde_json::Value;
use std::collections::HashSet;

/// Controls how much per-operation detail the session parser retains.
///
/// `Full` keeps everything that ends up in the public JSON output
/// (file contents on `Write`, old/new strings on `Edit`, command text on
/// `Bash`). `UsageOnly` skips those allocations — counts and totals are
/// still accurate, but the per-detail `Vec`s stay empty. Callers that only
/// consume `conversation_usage` / `tool_call_counts` / `total_*_lines`
/// (the `usage` command, the aggregated `analysis` path) should pick
/// `UsageOnly` to avoid pulling entire file bodies into memory on every
/// session parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    Full,
    UsageOnly,
}

/// Common parse state shared by all per-provider session parsers.
pub struct SessionParseState {
    pub mode: ParseMode,
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

impl SessionParseState {
    pub fn new() -> Self {
        Self::with_mode(ParseMode::Full)
    }

    pub fn with_mode(mode: ParseMode) -> Self {
        // Pre-allocate Vecs with reasonable capacity estimates based on
        // typical session sizes. In `UsageOnly` mode we skip the
        // pre-allocation because the vecs stay empty.
        let pre = matches!(mode, ParseMode::Full);
        Self {
            mode,
            write_details: if pre {
                Vec::with_capacity(10)
            } else {
                Vec::new()
            },
            read_details: if pre {
                Vec::with_capacity(20)
            } else {
                Vec::new()
            },
            edit_details: if pre {
                Vec::with_capacity(15)
            } else {
                Vec::new()
            },
            run_details: if pre {
                Vec::with_capacity(10)
            } else {
                Vec::new()
            },
            tool_counts: CodeAnalysisToolCalls::default(),
            unique_files: HashSet::with_capacity(if pre { 20 } else { 0 }),
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

        if matches!(self.mode, ParseMode::Full) {
            self.read_details.push(CodeAnalysisReadDetail {
                base: CodeAnalysisDetailBase {
                    file_path: resolved.clone(),
                    line_count,
                    character_count: char_count,
                    timestamp: ts,
                },
            });
            self.unique_files.insert(resolved);
        }

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

        if matches!(self.mode, ParseMode::Full) {
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
        }

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

        if matches!(self.mode, ParseMode::Full) {
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
        }

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

        if matches!(self.mode, ParseMode::Full) {
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
        }

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
    pub fn into_record(self, conversation_usage: FastHashMap<String, Value>) -> CodeAnalysisRecord {
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

impl Default for SessionParseState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analysis_state_new() {
        // Test creating a new SessionParseState
        let state = SessionParseState::new();

        assert_eq!(state.total_write_lines, 0);
        assert_eq!(state.total_read_lines, 0);
        assert_eq!(state.total_edit_lines, 0);
        assert_eq!(state.write_details.len(), 0);
        assert_eq!(state.read_details.len(), 0);
        assert_eq!(state.edit_details.len(), 0);
        assert_eq!(state.unique_files.len(), 0);
        assert!(state.folder_path.is_empty());
    }

    #[test]
    fn test_add_read_detail() {
        // Test adding a read operation
        let mut state = SessionParseState::new();
        state.folder_path = "/test/folder".to_string();

        state.add_read_detail("test.rs", "line1\nline2\nline3", 1234567890);

        assert_eq!(state.read_details.len(), 1);
        assert_eq!(state.total_read_lines, 3);
        assert_eq!(state.tool_counts.read, 1);
        assert!(state.unique_files.contains("/test/folder/test.rs"));
    }

    #[test]
    fn test_add_read_detail_ignores_empty() {
        // Test that empty content is ignored
        let mut state = SessionParseState::new();

        state.add_read_detail("test.rs", "", 1234567890);

        assert_eq!(state.read_details.len(), 0);
        assert_eq!(state.total_read_lines, 0);
        assert_eq!(state.tool_counts.read, 0);
    }

    #[test]
    fn test_add_write_detail() {
        // Test adding a write operation
        let mut state = SessionParseState::new();
        state.folder_path = "/test/folder".to_string();

        state.add_write_detail("output.txt", "content line 1\ncontent line 2", 1234567890);

        assert_eq!(state.write_details.len(), 1);
        assert_eq!(state.total_write_lines, 2);
        assert_eq!(state.tool_counts.write, 1);
        assert!(state.unique_files.contains("/test/folder/output.txt"));
    }

    #[test]
    fn test_add_edit_detail() {
        // Test adding an edit operation
        let mut state = SessionParseState::new();
        state.folder_path = "/test".to_string();

        state.add_edit_detail(
            "file.rs",
            "old content\nold line 2",
            "new content\nnew line 2\nnew line 3",
            1234567890,
        );

        assert_eq!(state.edit_details.len(), 1);
        assert_eq!(state.total_edit_lines, 3);
        assert_eq!(state.tool_counts.edit, 1);
        assert!(state.unique_files.contains("/test/file.rs"));
    }

    #[test]
    fn test_add_edit_detail_empty_old_becomes_write() {
        // Test that edit with empty old content becomes a write
        let mut state = SessionParseState::new();
        state.folder_path = "/test".to_string();

        state.add_edit_detail("new_file.rs", "", "new content", 1234567890);

        // Should be recorded as write, not edit
        assert_eq!(state.write_details.len(), 1);
        assert_eq!(state.edit_details.len(), 0);
        assert_eq!(state.tool_counts.write, 1);
        assert_eq!(state.tool_counts.edit, 0);
    }

    #[test]
    fn test_add_run_command() {
        // Test adding a run command
        let mut state = SessionParseState::new();
        state.folder_path = "/workspace".to_string();

        state.add_run_command("cargo test", "Running tests", 1234567890);

        assert_eq!(state.run_details.len(), 1);
        assert_eq!(state.tool_counts.bash, 1);
        assert_eq!(state.run_details[0].command, "cargo test");
    }

    #[test]
    fn test_add_run_command_ignores_empty() {
        // Test that empty commands are ignored
        let mut state = SessionParseState::new();

        state.add_run_command("", "description", 1234567890);
        state.add_run_command("   ", "description", 1234567890);

        assert_eq!(state.run_details.len(), 0);
        assert_eq!(state.tool_counts.bash, 0);
    }

    #[test]
    fn test_normalize_path_absolute() {
        // Test normalizing absolute paths
        let mut state = SessionParseState::new();
        state.folder_path = "/workspace".to_string();

        let result = state.normalize_path("/absolute/path/file.rs");
        assert_eq!(result, "/absolute/path/file.rs");
    }

    #[test]
    fn test_normalize_path_relative() {
        // Test normalizing relative paths
        let mut state = SessionParseState::new();
        state.folder_path = "/workspace".to_string();

        let result = state.normalize_path("relative/file.rs");
        assert_eq!(result, "/workspace/relative/file.rs");
    }

    #[test]
    fn test_normalize_path_empty_folder() {
        // Test normalizing when folder_path is empty
        let state = SessionParseState::new();

        let result = state.normalize_path("file.rs");
        assert_eq!(result, "file.rs");
    }

    #[test]
    fn test_normalize_path_empty_input() {
        // Test normalizing empty path
        let mut state = SessionParseState::new();
        state.folder_path = "/workspace".to_string();

        let result = state.normalize_path("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_unique_files_tracking() {
        // Test that unique files are tracked correctly
        let mut state = SessionParseState::new();
        state.folder_path = "/project".to_string();

        // Add operations on same file
        state.add_read_detail("file1.rs", "content", 1);
        state.add_write_detail("file1.rs", "content", 2);
        state.add_edit_detail("file1.rs", "old", "new", 3);

        // Add operations on different file
        state.add_read_detail("file2.rs", "content", 4);

        assert_eq!(state.unique_files.len(), 2);
        assert!(state.unique_files.contains("/project/file1.rs"));
        assert!(state.unique_files.contains("/project/file2.rs"));
    }

    #[test]
    fn test_character_counting() {
        // Test that character counts are correct
        let mut state = SessionParseState::new();

        state.add_read_detail("file.txt", "hello", 1);
        assert_eq!(state.total_read_characters, 5);

        state.add_write_detail("file2.txt", "world!", 2);
        assert_eq!(state.total_write_characters, 6);

        state.add_edit_detail("file3.txt", "old", "new content", 3);
        assert_eq!(state.total_edit_characters, 11);
    }

    #[test]
    fn test_into_record() {
        // Test converting state into a record
        let mut state = SessionParseState::new();
        state.folder_path = "/test".to_string();
        state.git_remote = "https://github.com/test/repo".to_string();
        state.task_id = "task-123".to_string();
        state.last_ts = 9999999999;

        state.add_read_detail("file.rs", "line1\nline2", 1);
        state.add_write_detail("output.rs", "content", 2);

        let usage = FastHashMap::default();
        let record = state.into_record(usage);

        assert_eq!(record.total_unique_files, 2);
        assert_eq!(record.total_read_lines, 2);
        assert_eq!(record.total_write_lines, 1);
        assert_eq!(record.folder_path, "/test");
        assert_eq!(record.git_remote_url, "https://github.com/test/repo");
        assert_eq!(record.task_id, "task-123");
        assert_eq!(record.timestamp, 9999999999);
    }

    #[test]
    fn test_default_trait() {
        // Test Default trait implementation
        let state = SessionParseState::default();

        assert_eq!(state.total_write_lines, 0);
        assert_eq!(state.total_read_lines, 0);
        assert_eq!(state.total_edit_lines, 0);
    }

    #[test]
    fn test_multiple_operations() {
        // Test handling multiple operations
        let mut state = SessionParseState::new();
        state.folder_path = "/workspace".to_string();

        // Multiple reads
        state.add_read_detail("a.rs", "line1", 1);
        state.add_read_detail("b.rs", "line1\nline2", 2);
        state.add_read_detail("c.rs", "line1\nline2\nline3", 3);

        // Multiple writes
        state.add_write_detail("out1.txt", "content1", 4);
        state.add_write_detail("out2.txt", "content2", 5);

        // Multiple edits
        state.add_edit_detail("edit1.rs", "old", "new", 6);

        // Multiple commands
        state.add_run_command("ls", "list files", 7);
        state.add_run_command("pwd", "print dir", 8);

        assert_eq!(state.read_details.len(), 3);
        assert_eq!(state.write_details.len(), 2);
        assert_eq!(state.edit_details.len(), 1);
        assert_eq!(state.run_details.len(), 2);
        assert_eq!(state.total_read_lines, 6); // 1 + 2 + 3
        assert_eq!(state.tool_counts.read, 3);
        assert_eq!(state.tool_counts.write, 2);
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.tool_counts.bash, 2);
    }
}
