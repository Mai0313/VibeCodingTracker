//! Mutable accumulator shared by every per-provider session parser.
//!
//! Each provider parser ([`crate::session::claude`], [`crate::session::codex`],
//! …) walks its own JSONL shape but funnels the file-operation facts it
//! extracts through a single [`SessionParseState`], which tallies line /
//! character counts and (in [`ParseMode::Full`]) accumulates the per-op detail
//! records. Once the file is consumed the state is converted into one
//! [`CodeAnalysisRecord`] via [`SessionParseState::into_record`].
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
    /// Retain everything that ends up in the JSON output, including file
    /// bodies and old/new edit strings.
    Full,
    /// Skip per-detail allocations; keep only counts and totals.
    UsageOnly,
}

/// Common parse state shared by all per-provider session parsers.
///
/// Construct with [`SessionParseState::new`] (full detail) or
/// [`SessionParseState::with_mode`], populate the metadata fields
/// (`folder_path`, `git_remote`, `task_id`) directly as the provider header
/// is read, feed file operations through the `add_*` helpers, then call
/// [`SessionParseState::into_record`] to produce the final
/// [`CodeAnalysisRecord`]. Not thread-safe; one instance parses one file on
/// one thread.
pub struct SessionParseState {
    /// Detail-retention level chosen at construction.
    pub mode: ParseMode,
    /// Per-`Write` detail records (empty in [`ParseMode::UsageOnly`]).
    pub write_details: Vec<CodeAnalysisWriteDetail>,
    /// Per-`Read` detail records (empty in [`ParseMode::UsageOnly`]).
    pub read_details: Vec<CodeAnalysisReadDetail>,
    /// Per-`Edit`/diff detail records (empty in [`ParseMode::UsageOnly`]).
    pub edit_details: Vec<CodeAnalysisApplyDiffDetail>,
    /// Per-`Bash`/run-command detail records (empty in [`ParseMode::UsageOnly`]).
    pub run_details: Vec<CodeAnalysisRunCommandDetail>,
    /// Running per-tool call counts (always tallied, both modes).
    pub tool_counts: CodeAnalysisToolCalls,
    /// Distinct normalized file paths touched (only populated in [`ParseMode::Full`]).
    pub unique_files: HashSet<String>,
    /// Sum of lines written across all `Write` operations.
    pub total_write_lines: usize,
    /// Sum of lines read across all `Read` operations.
    pub total_read_lines: usize,
    /// Sum of new-content lines across all `Edit` operations.
    pub total_edit_lines: usize,
    /// Sum of characters written across all `Write` operations.
    pub total_write_characters: usize,
    /// Sum of characters read across all `Read` operations.
    pub total_read_characters: usize,
    /// Sum of new-content characters across all `Edit` operations.
    pub total_edit_characters: usize,
    /// Session working directory; used to resolve relative paths to absolute.
    pub folder_path: String,
    /// Git remote URL for the session's repository, when known.
    pub git_remote: String,
    /// Provider-specific session identifier.
    pub task_id: String,
    /// Latest event timestamp seen, in epoch milliseconds.
    pub last_ts: i64,
}

impl SessionParseState {
    /// Creates an empty parse state in [`ParseMode::Full`].
    ///
    /// # Examples
    ///
    /// ```
    /// use vibe_coding_tracker::session::state::SessionParseState;
    ///
    /// let mut state = SessionParseState::new();
    /// state.folder_path = "/repo".to_string();
    /// state.add_read_detail("src/main.rs", "fn main() {}\n", 0);
    /// assert_eq!(state.total_read_lines, 1);
    /// assert_eq!(state.tool_counts.read, 1);
    /// ```
    pub fn new() -> Self {
        Self::with_mode(ParseMode::Full)
    }

    /// Creates an empty parse state with the given detail-retention `mode`.
    ///
    /// In [`ParseMode::Full`] the detail `Vec`s and `unique_files` set are
    /// pre-sized to typical session sizes; in [`ParseMode::UsageOnly`] they
    /// start empty since they stay empty for the whole parse.
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

    /// Records a `Read` operation against `path` with the read `content`.
    ///
    /// Trailing newlines are stripped before counting. No-op (and no count
    /// bump) when the trimmed content is empty or when `path` normalizes to
    /// an empty string. The detail record and `unique_files` entry are only
    /// stored in [`ParseMode::Full`]; counts accrue in both modes.
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

    /// Records a `Write` operation against `path` with the written `content`.
    ///
    /// Trailing newlines are stripped before counting. No-op when `path`
    /// normalizes to an empty string (an empty body is still counted as a
    /// zero-line write). The full detail (including `content`) and
    /// `unique_files` entry are stored only in [`ParseMode::Full`].
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

    /// Records an `Edit` operation against `path`, replacing `old` with `new`.
    ///
    /// When `old` is empty but `new` is not, the edit is reclassified as a
    /// `Write` (a new-file creation expressed as a diff) and forwarded to
    /// [`SessionParseState::add_write_detail`]. Otherwise the line/character
    /// tally is taken from the trimmed `new` content. No-op when `path`
    /// normalizes to an empty string; full detail stored only in
    /// [`ParseMode::Full`].
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

    /// Records a `Bash`/run-command invocation with its `command` text.
    ///
    /// No-op (and no count bump) when `command` is empty after trimming. The
    /// detail record (attributed to `folder_path`) is stored only in
    /// [`ParseMode::Full`]; the `bash` count accrues in both modes.
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

    /// Resolves `path` to an absolute path, joining it onto `folder_path`.
    ///
    /// Returns the input unchanged when it is already absolute, when it is
    /// empty, or when `folder_path` is empty (nothing to join against).
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

    /// Consumes the state into a finished [`CodeAnalysisRecord`].
    ///
    /// `conversation_usage` is the per-model token map the provider parser
    /// accumulated separately; it is folded into the record verbatim.
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
    /// Equivalent to [`SessionParseState::new`] ([`ParseMode::Full`]).
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_parse_state_new() {
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
