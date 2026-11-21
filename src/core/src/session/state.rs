//! Shared per-session accumulator for file-operation metrics and detail records.
//!
//! Provider parsers feed operations into [`SessionParseState`] and finalize it into a [`CodeAnalysisRecord`] with [`SessionParseState::into_record`].
use crate::constants::{FastHashMap, FastHashSet};
use crate::models::*;
use crate::utils::count_lines;
use serde_json::Value;
use std::borrow::Cow;
use std::path::Path;

/// Precomputed line and Unicode scalar counts after stripping trailing `\n` characters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TextMetrics {
    pub(crate) lines: usize,
    pub(crate) chars: usize,
}

impl TextMetrics {
    /// Computes trimmed line and Unicode scalar counts from `content`.
    pub(crate) fn from_str(content: &str) -> Self {
        let trimmed = content.trim_end_matches('\n');
        Self {
            lines: count_lines(trimmed),
            chars: bytecount::num_chars(trimmed.as_bytes()),
        }
    }
}

/// Controls how much per-operation detail a session parser retains.
///
/// Both variants produce identical scalar counts, line/character totals, and unique-file counts; only per-operation detail vectors differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseMode {
    /// Retains full per-operation detail records, including file contents, edit diffs, and command text.
    Full,
    /// Omits per-operation detail records and retains only scalar counts and totals.
    UsageOnly,
}

/// Accumulator for a single session's file operations, tool counts, and metadata.
///
/// Populate `folder_path`, `git_remote`, and `task_id` from provider metadata, record operations via the `add_*` methods, and call [`SessionParseState::into_record`] to build a [`CodeAnalysisRecord`].
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
    /// Distinct normalized file paths touched (populated in both parse modes).
    pub unique_files: FastHashSet<String>,
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
    /// use vct_core::session::state::SessionParseState;
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

    /// Returns an empty parse state configured with `mode`.
    pub fn with_mode(mode: ParseMode) -> Self {
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
            unique_files: FastHashSet::with_capacity(20),
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

    /// Records a `Read` operation for `path` with `content` at epoch-millisecond timestamp `ts`.
    ///
    /// Strips trailing `\n` characters before counting lines and characters. Does nothing if the trimmed content is empty or `path` normalizes to an empty string. Stores a detail record only in [`ParseMode::Full`].
    pub fn add_read_detail(&mut self, path: &str, content: &str, ts: i64) {
        if path.is_empty() {
            return;
        }
        let metrics = TextMetrics::from_str(content);
        if metrics.lines == 0 {
            return;
        }
        if matches!(self.mode, ParseMode::UsageOnly) {
            self.add_read_counts(path, metrics.lines, metrics.chars);
            return;
        }

        let file_path = self.record_unique_file_owned(path);
        self.read_details.push(CodeAnalysisReadDetail {
            base: CodeAnalysisDetailBase {
                file_path,
                line_count: metrics.lines,
                character_count: metrics.chars,
                timestamp: ts,
            },
        });
        self.total_read_lines += metrics.lines;
        self.total_read_characters += metrics.chars;
        self.tool_counts.read += 1;
    }

    /// Records a `Read` operation in [`ParseMode::UsageOnly`] from precomputed `(line_count, char_count)`.
    pub(crate) fn add_read_counts(&mut self, path: &str, line_count: usize, char_count: usize) {
        debug_assert_eq!(self.mode, ParseMode::UsageOnly);
        if line_count == 0 || path.is_empty() {
            return;
        }
        self.record_unique_file_usage_only(path);
        self.total_read_lines += line_count;
        self.total_read_characters += char_count;
        self.tool_counts.read += 1;
    }

    /// Records the path of a successful non-text read without inventing lines.
    pub(crate) fn add_non_text_read_path(&mut self, path: &str) {
        if !path.is_empty() {
            self.record_unique_file_usage_only(path);
        }
    }

    /// Records a `Write` operation for `path` with `content` at epoch-millisecond timestamp `ts`.
    ///
    /// Strips trailing `\n` characters before counting; empty `content` is recorded as a zero-line write. Does nothing if `path` normalizes to an empty string. Stores a detail record only in [`ParseMode::Full`].
    pub fn add_write_detail(&mut self, path: &str, content: &str, ts: i64) {
        if path.is_empty() {
            return;
        }
        let trimmed = content.trim_end_matches('\n');
        let line_count = count_lines(trimmed);
        let char_count = bytecount::num_chars(trimmed.as_bytes());
        if matches!(self.mode, ParseMode::UsageOnly) {
            self.add_write_counts(path, line_count, char_count);
            return;
        }

        let file_path = self.record_unique_file_owned(path);
        self.write_details.push(CodeAnalysisWriteDetail {
            base: CodeAnalysisDetailBase {
                file_path,
                line_count,
                character_count: char_count,
                timestamp: ts,
            },
            content: trimmed.to_string(),
        });
        self.total_write_lines += line_count;
        self.total_write_characters += char_count;
        self.tool_counts.write += 1;
    }

    /// Records a `Write` operation in [`ParseMode::UsageOnly`] from precomputed `(line_count, char_count)`.
    pub(crate) fn add_write_counts(&mut self, path: &str, line_count: usize, char_count: usize) {
        debug_assert_eq!(self.mode, ParseMode::UsageOnly);
        if path.is_empty() {
            return;
        }
        self.record_unique_file_usage_only(path);
        self.total_write_lines += line_count;
        self.total_write_characters += char_count;
        self.tool_counts.write += 1;
    }

    /// Records an `Edit` operation for `path`, replacing `old` with `new` at epoch-millisecond timestamp `ts`.
    ///
    /// Reclassifies the operation as a `Write` via [`SessionParseState::add_write_detail`] when `old` is empty and `new` is non-empty after stripping trailing `\n`; otherwise forwards to [`SessionParseState::add_edit_detail_raw`].
    pub fn add_edit_detail(&mut self, path: &str, old: &str, new: &str, ts: i64) {
        if path.is_empty() {
            return;
        }
        if matches!(self.mode, ParseMode::UsageOnly) {
            let old_is_empty = old.trim_end_matches('\n').is_empty();
            let metrics = TextMetrics::from_str(new);
            self.add_edit_counts(path, old_is_empty, metrics.lines, metrics.chars);
            return;
        }
        if old.trim_end_matches('\n').is_empty() && !new.trim_end_matches('\n').is_empty() {
            self.add_write_detail(path, new, ts);
            return;
        }

        self.add_edit_detail_raw(path, old, new, ts);
    }

    /// Records an `Edit` operation in [`ParseMode::UsageOnly`], reclassifying empty `old` with non-empty `new` as a `Write`.
    pub(crate) fn add_edit_counts(
        &mut self,
        path: &str,
        old_is_empty: bool,
        new_lines: usize,
        new_chars: usize,
    ) {
        debug_assert_eq!(self.mode, ParseMode::UsageOnly);
        if old_is_empty && (new_lines > 0 || new_chars > 0) {
            self.add_write_counts(path, new_lines, new_chars);
            return;
        }
        self.add_edit_counts_raw(path, new_lines, new_chars);
    }

    /// Records an `Edit` operation for `path` without reclassifying empty `old` as a `Write`.
    ///
    /// Counts lines and characters from `new` after stripping trailing `\n`. Does nothing if `path` normalizes to an empty string. Stores a detail record only in [`ParseMode::Full`].
    pub fn add_edit_detail_raw(&mut self, path: &str, old: &str, new: &str, ts: i64) {
        if path.is_empty() {
            return;
        }
        let trimmed_new = new.trim_end_matches('\n');
        let line_count = count_lines(trimmed_new);
        let char_count = bytecount::num_chars(trimmed_new.as_bytes());
        if matches!(self.mode, ParseMode::UsageOnly) {
            self.add_edit_counts_raw(path, line_count, char_count);
            return;
        }

        let trimmed_old = old.trim_end_matches('\n');
        let file_path = self.record_unique_file_owned(path);
        self.edit_details.push(CodeAnalysisApplyDiffDetail {
            base: CodeAnalysisDetailBase {
                file_path,
                line_count,
                character_count: char_count,
                timestamp: ts,
            },
            old_string: trimmed_old.to_string(),
            new_string: trimmed_new.to_string(),
        });
        self.total_edit_lines += line_count;
        self.total_edit_characters += char_count;
        self.tool_counts.edit += 1;
    }

    /// Records an `Edit` operation in [`ParseMode::UsageOnly`] from precomputed `(line_count, char_count)` without `Write` reclassification.
    pub(crate) fn add_edit_counts_raw(&mut self, path: &str, line_count: usize, char_count: usize) {
        debug_assert_eq!(self.mode, ParseMode::UsageOnly);
        if path.is_empty() {
            return;
        }
        self.record_unique_file_usage_only(path);
        self.total_edit_lines += line_count;
        self.total_edit_characters += char_count;
        self.tool_counts.edit += 1;
    }

    /// Records a shell command invocation with `command` and `description` at epoch-millisecond timestamp `ts`.
    ///
    /// Does nothing if `command` is empty after trimming whitespace. Stores a detail record (attributed to `folder_path`) only in [`ParseMode::Full`].
    pub fn add_run_command(&mut self, command: &str, description: &str, ts: i64) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }

        if matches!(self.mode, ParseMode::Full) {
            let command_chars = bytecount::num_chars(command.as_bytes());
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

    /// Resolves `path` against `folder_path`, borrowing `path` when empty, absolute, or `folder_path` is empty.
    fn resolve_path<'a>(&self, path: &'a str) -> Cow<'a, str> {
        if path.is_empty() || Path::new(path).is_absolute() || self.folder_path.is_empty() {
            return Cow::Borrowed(path);
        }

        let joined = Path::new(&self.folder_path).join(path);
        match joined.into_os_string().into_string() {
            Ok(s) => Cow::Owned(s),
            Err(os) => Cow::Owned(os.to_string_lossy().into_owned()),
        }
    }

    fn record_unique_file_usage_only(&mut self, path: &str) {
        let resolved = self.resolve_path(path);
        if !self.unique_files.contains(resolved.as_ref()) {
            self.unique_files.insert(resolved.into_owned());
        }
    }

    fn record_unique_file_owned(&mut self, path: &str) -> String {
        let file_path = self.resolve_path(path).into_owned();
        if !self.unique_files.contains(&file_path) {
            self.unique_files.insert(file_path.clone());
        }
        file_path
    }

    /// Returns `path` joined onto `folder_path`, or `path` unchanged if it is empty, already absolute, or `folder_path` is empty.
    pub fn normalize_path(&self, path: &str) -> String {
        self.resolve_path(path).into_owned()
    }

    /// Combines `other` into `self`, summing counts and totals, taking the maximum `last_ts`, and filling empty metadata fields from `other`.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `self.mode != other.mode`.
    pub fn merge(&mut self, mut other: Self) {
        debug_assert_eq!(self.mode, other.mode);
        self.write_details.append(&mut other.write_details);
        self.read_details.append(&mut other.read_details);
        self.edit_details.append(&mut other.edit_details);
        self.run_details.append(&mut other.run_details);

        self.tool_counts.read += other.tool_counts.read;
        self.tool_counts.write += other.tool_counts.write;
        self.tool_counts.edit += other.tool_counts.edit;
        self.tool_counts.todo_write += other.tool_counts.todo_write;
        self.tool_counts.bash += other.tool_counts.bash;
        self.unique_files.extend(other.unique_files);

        self.total_write_lines += other.total_write_lines;
        self.total_read_lines += other.total_read_lines;
        self.total_edit_lines += other.total_edit_lines;
        self.total_write_characters += other.total_write_characters;
        self.total_read_characters += other.total_read_characters;
        self.total_edit_characters += other.total_edit_characters;
        self.last_ts = self.last_ts.max(other.last_ts);

        if self.folder_path.is_empty() {
            self.folder_path = other.folder_path;
        }
        if self.git_remote.is_empty() {
            self.git_remote = other.git_remote;
        }
        if self.task_id.is_empty() {
            self.task_id = other.task_id;
        }
    }

    /// Consumes this state and returns a [`CodeAnalysisRecord`] carrying `conversation_usage`.
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
            advisor_usage: FastHashMap::default(),
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
        let mut state = SessionParseState::new();

        state.add_read_detail("test.rs", "", 1234567890);

        assert_eq!(state.read_details.len(), 0);
        assert_eq!(state.total_read_lines, 0);
        assert_eq!(state.tool_counts.read, 0);
    }

    #[test]
    fn test_add_write_detail() {
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
        let mut state = SessionParseState::new();
        state.folder_path = "/test".to_string();

        state.add_edit_detail("new_file.rs", "", "new content", 1234567890);

        assert_eq!(state.write_details.len(), 1);
        assert_eq!(state.edit_details.len(), 0);
        assert_eq!(state.tool_counts.write, 1);
        assert_eq!(state.tool_counts.edit, 0);
    }

    #[test]
    fn test_add_run_command() {
        let mut state = SessionParseState::new();
        state.folder_path = "/workspace".to_string();

        state.add_run_command("cargo test", "Running tests", 1234567890);

        assert_eq!(state.run_details.len(), 1);
        assert_eq!(state.tool_counts.bash, 1);
        assert_eq!(state.run_details[0].command, "cargo test");
    }

    #[test]
    fn test_add_run_command_ignores_empty() {
        let mut state = SessionParseState::new();

        state.add_run_command("", "description", 1234567890);
        state.add_run_command("   ", "description", 1234567890);

        assert_eq!(state.run_details.len(), 0);
        assert_eq!(state.tool_counts.bash, 0);
    }

    #[test]
    fn test_normalize_path_absolute() {
        let mut state = SessionParseState::new();
        state.folder_path = "/workspace".to_string();

        let result = state.normalize_path("/absolute/path/file.rs");
        assert_eq!(result, "/absolute/path/file.rs");
    }

    #[test]
    fn test_normalize_path_relative() {
        let mut state = SessionParseState::new();
        state.folder_path = "/workspace".to_string();

        let result = state.normalize_path("relative/file.rs");
        assert_eq!(result, "/workspace/relative/file.rs");
    }

    #[test]
    fn test_normalize_path_empty_folder() {
        let state = SessionParseState::new();

        let result = state.normalize_path("file.rs");
        assert_eq!(result, "file.rs");
    }

    #[test]
    fn test_normalize_path_empty_input() {
        let mut state = SessionParseState::new();
        state.folder_path = "/workspace".to_string();

        let result = state.normalize_path("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_unique_files_tracking() {
        let mut state = SessionParseState::new();
        state.folder_path = "/project".to_string();

        state.add_read_detail("file1.rs", "content", 1);
        state.add_write_detail("file1.rs", "content", 2);
        state.add_edit_detail("file1.rs", "old", "new", 3);

        state.add_read_detail("file2.rs", "content", 4);

        assert_eq!(state.unique_files.len(), 2);
        assert!(state.unique_files.contains("/project/file1.rs"));
        assert!(state.unique_files.contains("/project/file2.rs"));
    }

    #[test]
    fn test_character_counting() {
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
        let state = SessionParseState::default();

        assert_eq!(state.total_write_lines, 0);
        assert_eq!(state.total_read_lines, 0);
        assert_eq!(state.total_edit_lines, 0);
    }

    #[test]
    fn test_multiple_operations() {
        let mut state = SessionParseState::new();
        state.folder_path = "/workspace".to_string();

        state.add_read_detail("a.rs", "line1", 1);
        state.add_read_detail("b.rs", "line1\nline2", 2);
        state.add_read_detail("c.rs", "line1\nline2\nline3", 3);

        state.add_write_detail("out1.txt", "content1", 4);
        state.add_write_detail("out2.txt", "content2", 5);

        state.add_edit_detail("edit1.rs", "old", "new", 6);

        state.add_run_command("ls", "list files", 7);
        state.add_run_command("pwd", "print dir", 8);

        assert_eq!(state.read_details.len(), 3);
        assert_eq!(state.write_details.len(), 2);
        assert_eq!(state.edit_details.len(), 1);
        assert_eq!(state.run_details.len(), 2);
        assert_eq!(state.total_read_lines, 6);
        assert_eq!(state.tool_counts.read, 3);
        assert_eq!(state.tool_counts.write, 2);
        assert_eq!(state.tool_counts.edit, 1);
        assert_eq!(state.tool_counts.bash, 2);
    }

    #[test]
    fn simd_char_counting_matches_chars_count_for_cjk_emoji_and_crlf() {
        let samples = [
            "",
            "\n\n",
            "ascii",
            "繁體中文測試\n第二行\n",
            "café\r\n日本語🚀🎉\r\n",
            "a\nb\n\nc\n\n",
            "🦀 Rust 與 Unicode 👨‍👩‍👧‍👦",
        ];

        let mut full = SessionParseState::with_mode(ParseMode::Full);
        let mut usage_only = SessionParseState::with_mode(ParseMode::UsageOnly);
        full.folder_path = "/workspace".to_string();
        usage_only.folder_path = "/workspace".to_string();

        let mut expected_lines = 0;
        let mut expected_chars = 0;

        for (i, sample) in samples.iter().enumerate() {
            let trimmed = sample.trim_end_matches('\n');
            let lines = count_lines(trimmed);
            let chars = trimmed.chars().count();
            assert_eq!(TextMetrics::from_str(sample), TextMetrics { lines, chars });

            let path = format!("/abs/file_{i}.txt");
            full.add_read_detail(&path, sample, i as i64);
            usage_only.add_read_detail(&path, sample, i as i64);
            full.add_write_detail(&path, sample, i as i64);
            usage_only.add_write_detail(&path, sample, i as i64);
            full.add_edit_detail(&path, "old", sample, i as i64);
            usage_only.add_edit_detail(&path, "old", sample, i as i64);
            full.add_run_command(sample, "desc", i as i64);
            usage_only.add_run_command(sample, "desc", i as i64);

            expected_lines += lines;
            expected_chars += chars;
        }

        assert_eq!(full.total_read_lines, expected_lines);
        assert_eq!(full.total_read_characters, expected_chars);
        assert_eq!(full.total_write_lines, expected_lines);
        assert_eq!(full.total_write_characters, expected_chars);
        assert_eq!(full.total_edit_lines, expected_lines);
        assert_eq!(full.total_edit_characters, expected_chars);

        assert_eq!(usage_only.total_read_lines, full.total_read_lines);
        assert_eq!(usage_only.total_read_characters, full.total_read_characters);
        assert_eq!(usage_only.total_write_lines, full.total_write_lines);
        assert_eq!(
            usage_only.total_write_characters,
            full.total_write_characters
        );
        assert_eq!(usage_only.total_edit_lines, full.total_edit_lines);
        assert_eq!(usage_only.total_edit_characters, full.total_edit_characters);
        assert_eq!(usage_only.unique_files, full.unique_files);
        assert_eq!(usage_only.tool_counts.read, full.tool_counts.read);
        assert_eq!(usage_only.tool_counts.write, full.tool_counts.write);
        assert_eq!(usage_only.tool_counts.edit, full.tool_counts.edit);
        assert_eq!(usage_only.tool_counts.bash, full.tool_counts.bash);
        assert!(usage_only.read_details.is_empty());
        assert!(usage_only.write_details.is_empty());
        assert!(usage_only.edit_details.is_empty());
        assert!(usage_only.run_details.is_empty());
    }

    #[test]
    fn usage_only_counts_helpers_match_detail_methods() {
        let mut full = SessionParseState::with_mode(ParseMode::Full);
        let mut counted = SessionParseState::with_mode(ParseMode::UsageOnly);
        full.folder_path = "/repo".to_string();
        counted.folder_path = "/repo".to_string();

        full.add_read_detail("a.rs", "你好\n世界\n", 1);
        let m = TextMetrics::from_str("你好\n世界\n");
        counted.add_read_counts("a.rs", m.lines, m.chars);

        full.add_write_detail("/abs/b.rs", "", 2);
        counted.add_write_counts("/abs/b.rs", 0, 0);

        full.add_edit_detail("/abs/b.rs", "\n\n", "🚀\nline2", 3);
        let m2 = TextMetrics::from_str("🚀\nline2");
        counted.add_edit_counts("/abs/b.rs", true, m2.lines, m2.chars);

        full.add_edit_detail_raw("c.rs", "", "x\ny", 4);
        let m3 = TextMetrics::from_str("x\ny");
        counted.add_edit_counts_raw("c.rs", m3.lines, m3.chars);

        assert_eq!(counted.total_read_lines, full.total_read_lines);
        assert_eq!(counted.total_read_characters, full.total_read_characters);
        assert_eq!(counted.total_write_lines, full.total_write_lines);
        assert_eq!(counted.total_write_characters, full.total_write_characters);
        assert_eq!(counted.total_edit_lines, full.total_edit_lines);
        assert_eq!(counted.total_edit_characters, full.total_edit_characters);
        assert_eq!(counted.unique_files, full.unique_files);
        assert_eq!(counted.tool_counts.read, full.tool_counts.read);
        assert_eq!(counted.tool_counts.write, full.tool_counts.write);
        assert_eq!(counted.tool_counts.edit, full.tool_counts.edit);
    }
}
