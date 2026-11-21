//! Normalized, cross-provider analysis result types.
//!
//! Every provider parser emits a [`CodeAnalysis`]. The `serde` field names and flattening attributes define the persisted JSON wire format.

use crate::constants::FastHashMap;
use serde::{Deserialize, Serialize, Serializer, ser::SerializeMap};

/// Serializes a model-keyed usage map in lexical key order for deterministic JSON output.
fn serialize_conversation_usage<S>(
    usage: &FastHashMap<String, serde_json::Value>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let mut entries: Vec<_> = usage.iter().collect();
    entries.sort_unstable_by_key(|(model, _)| *model);

    let mut map = serializer.serialize_map(Some(entries.len()))?;
    for (model, value) in entries {
        map.serialize_entry(model, value)?;
    }
    map.end()
}

/// Shared metadata for per-operation analysis detail records.
///
/// `line_count` and `character_count` measure the trimmed payload, with `character_count` counting Unicode scalar values (`str::chars`). Serializes with camelCase keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisDetailBase {
    /// Absolute path of the file the operation targeted.
    pub file_path: String,
    /// Number of lines in the operation payload.
    pub line_count: usize,
    /// Number of Unicode scalar values in the operation payload.
    pub character_count: usize,
    /// Unix epoch timestamp (milliseconds) when the operation occurred.
    pub timestamp: i64,
}

/// Single file-write operation with its full written content.
///
/// Flattens [`Self::base`] into the parent JSON object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisWriteDetail {
    /// Shared path / line / character / timestamp metadata, flattened inline.
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
    /// Full content written to the file.
    pub content: String,
}

/// Single file-read operation; retains only [`CodeAnalysisDetailBase`] metadata without the file body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisReadDetail {
    /// Shared path / line / character / timestamp metadata, flattened inline.
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
}

/// Single file-edit operation recording the replaced and replacement text.
///
/// [`Self::base`] is flattened inline, with `line_count` and `character_count` measuring `new_string`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisApplyDiffDetail {
    /// Shared path / line / character / timestamp metadata, flattened inline.
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
    /// Text that was replaced.
    pub old_string: String,
    /// Text that replaced `old_string`.
    pub new_string: String,
}

/// Single shell-command execution.
///
/// `base.file_path` holds the session working directory and `base.line_count` is `0`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisRunCommandDetail {
    /// Shared metadata; `file_path` is the working directory, `line_count` is 0.
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
    /// The command line that was executed.
    pub command: String,
    /// Human-readable description of the command, when the assistant supplied one.
    pub description: String,
}

/// Per-session tool-call counters, serialized with PascalCase keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CodeAnalysisToolCalls {
    /// Number of file-read tool calls.
    pub read: usize,
    /// Number of file-write tool calls.
    pub write: usize,
    /// Number of file-edit tool calls.
    pub edit: usize,
    /// Number of todo-list update tool calls.
    pub todo_write: usize,
    /// Number of shell-command tool calls.
    pub bash: usize,
}

/// Aggregated metrics and per-operation details for a single coding session.
///
/// When parsed in usage-only mode, the `*_file_details` and `run_command_details` vectors remain empty while the `total_*` counters and `conversation_usage` are still populated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisRecord {
    /// Count of distinct files touched (read, written, or edited) in the session.
    pub total_unique_files: usize,
    /// Sum of lines written across all write operations.
    pub total_write_lines: usize,
    /// Sum of lines read across all read operations.
    pub total_read_lines: usize,
    /// Sum of replacement lines across all edit operations.
    pub total_edit_lines: usize,
    /// Sum of characters written across all write operations.
    pub total_write_characters: usize,
    /// Sum of characters read across all read operations.
    pub total_read_characters: usize,
    /// Sum of replacement characters across all edit operations.
    pub total_edit_characters: usize,
    /// Per-operation write records (empty in usage-only mode).
    pub write_file_details: Vec<CodeAnalysisWriteDetail>,
    /// Per-operation read records (empty in usage-only mode).
    pub read_file_details: Vec<CodeAnalysisReadDetail>,
    /// Per-operation edit records (empty in usage-only mode).
    pub edit_file_details: Vec<CodeAnalysisApplyDiffDetail>,
    /// Per-command shell execution records (empty in usage-only mode).
    pub run_command_details: Vec<CodeAnalysisRunCommandDetail>,
    /// Tool-call counters for the session.
    pub tool_call_counts: CodeAnalysisToolCalls,
    /// Token-usage payloads keyed by model name; shape varies by provider (see [`crate::models::UsageResult`]).
    #[serde(serialize_with = "serialize_conversation_usage")]
    pub conversation_usage: FastHashMap<String, serde_json::Value>,
    /// Token usage from non-tool requests (Claude Code `advisor_message` iterations keyed by the advisor model, and Antigravity compaction requests keyed by the active model or `unknown`).
    ///
    /// Kept separate from `conversation_usage` and skipped during serialization so usage roll-ups bill these tokens without attributing the session's tool counts to them in analysis.
    #[serde(skip)]
    pub advisor_usage: FastHashMap<String, serde_json::Value>,
    /// Per-model file-operation and tool-call breakdown for multi-model sessions.
    ///
    /// Skipped during serialization so single-file JSON output stays unchanged; when non-empty, analysis roll-ups emit these rows directly instead of duplicating session-wide totals across every key in `conversation_usage`.
    #[serde(skip)]
    pub per_model_analysis: FastHashMap<String, AggregatedAnalysisRow>,
    /// Session / task identifier from the source log.
    pub task_id: String,
    /// Unix epoch timestamp (milliseconds) of the session's last activity.
    pub timestamp: i64,
    /// Working directory the session ran in.
    pub folder_path: String,
    /// Git remote URL of the project, when one was detected.
    pub git_remote_url: String,
}

/// Top-level session analysis output: host metadata plus one record per session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysis {
    /// OS user name that owned the session.
    pub user: String,
    /// Source assistant label (for example `Claude-Code`), matching [`ExtensionType`]'s [`std::fmt::Display`] output.
    pub extension_name: String,
    /// Version string of the analyzer that produced this result.
    pub insights_version: String,
    /// Stable machine identifier for the host.
    pub machine_id: String,
    /// One [`CodeAnalysisRecord`] per parsed session.
    pub records: Vec<CodeAnalysisRecord>,
}

/// Aggregated file-operation line counts and tool-call counts for a single model across sessions.
///
/// Serializes with camelCase field names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregatedAnalysisRow {
    /// Model name the metrics are grouped under.
    pub model: String,
    /// Total lines changed by `Edit` operations.
    pub edit_lines: usize,
    /// Total lines returned by `Read` operations.
    pub read_lines: usize,
    /// Total lines emitted by `Write` operations.
    pub write_lines: usize,
    /// Number of `Bash` tool calls.
    pub bash_count: usize,
    /// Number of `Edit` tool calls.
    pub edit_count: usize,
    /// Number of `Read` tool calls.
    pub read_count: usize,
    /// Number of `TodoWrite` tool calls.
    pub todo_write_count: usize,
    /// Number of `Write` tool calls.
    pub write_count: usize,
}

impl AggregatedAnalysisRow {
    /// Returns a zeroed analysis row for `model`.
    pub(crate) fn empty(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            edit_lines: 0,
            read_lines: 0,
            write_lines: 0,
            bash_count: 0,
            edit_count: 0,
            read_count: 0,
            todo_write_count: 0,
            write_count: 0,
        }
    }

    /// Adds all line and tool-call counts from `other` into `self`.
    pub(crate) fn add_assign(&mut self, other: &Self) {
        self.edit_lines += other.edit_lines;
        self.read_lines += other.read_lines;
        self.write_lines += other.write_lines;
        self.bash_count += other.bash_count;
        self.edit_count += other.edit_count;
        self.read_count += other.read_count;
        self.todo_write_count += other.todo_write_count;
        self.write_count += other.write_count;
    }
}

/// Concrete AI coding assistant a session came from.
///
/// Unlike [`crate::models::Provider`], there is no `Unknown` variant; [`std::fmt::Display`] formats the `extension_name` label stored in [`CodeAnalysis`].
///
/// # Examples
///
/// ```
/// use vct_core::models::ExtensionType;
///
/// assert_eq!(ExtensionType::Copilot.to_string(), "Copilot-CLI");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExtensionType {
    /// Anthropic Claude Code.
    ClaudeCode,
    /// OpenAI Codex CLI.
    Codex,
    /// GitHub Copilot CLI.
    Copilot,
    /// Google Gemini CLI.
    Gemini,
    /// OpenCode.
    OpenCode,
    /// Cursor CLI / IDE.
    Cursor,
    /// Hermes.
    Hermes,
    /// xAI Grok CLI.
    Grok,
    /// DeepSeek Harness (`dsh`).
    DeepSeek,
    /// Google Antigravity: the `agy` CLI, the Desktop app and other builds.
    Antigravity,
}

impl ExtensionType {
    /// Returns the canonical provider rank used to order scan diagnostics deterministically.
    ///
    /// Independent of the scanner's provider fan-out table; new providers must be added to both.
    pub fn scan_rank(self) -> u8 {
        match self {
            ExtensionType::ClaudeCode => 0,
            ExtensionType::Codex => 1,
            ExtensionType::Copilot => 2,
            ExtensionType::Gemini => 3,
            ExtensionType::Grok => 4,
            ExtensionType::DeepSeek => 5,
            ExtensionType::Antigravity => 6,
            ExtensionType::OpenCode => 7,
            ExtensionType::Cursor => 8,
            ExtensionType::Hermes => 9,
        }
    }
}

impl std::fmt::Display for ExtensionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtensionType::ClaudeCode => write!(f, "Claude-Code"),
            ExtensionType::Codex => write!(f, "Codex"),
            ExtensionType::Copilot => write!(f, "Copilot-CLI"),
            ExtensionType::Gemini => write!(f, "Gemini"),
            ExtensionType::OpenCode => write!(f, "OpenCode"),
            ExtensionType::Cursor => write!(f, "Cursor"),
            ExtensionType::Hermes => write!(f, "Hermes"),
            ExtensionType::Grok => write!(f, "Grok"),
            ExtensionType::DeepSeek => write!(f, "DeepSeek-Harness"),
            ExtensionType::Antigravity => write!(f, "Antigravity"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_analysis_literal_has_no_parser_diagnostic_field() {
        let analysis = CodeAnalysis {
            user: String::new(),
            extension_name: String::new(),
            insights_version: String::new(),
            machine_id: String::new(),
            records: Vec::new(),
        };

        assert_eq!(
            serde_json::to_value(analysis).unwrap()["records"],
            serde_json::json!([])
        );
    }

    #[test]
    fn test_code_analysis_tool_calls_serialization() {
        let tool_calls = CodeAnalysisToolCalls {
            read: 10,
            write: 5,
            edit: 3,
            todo_write: 2,
            bash: 1,
        };

        let json = serde_json::to_string(&tool_calls).unwrap();
        let deserialized: CodeAnalysisToolCalls = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.read, 10);
        assert_eq!(deserialized.write, 5);
        assert_eq!(deserialized.edit, 3);
        assert_eq!(deserialized.todo_write, 2);
        assert_eq!(deserialized.bash, 1);
    }

    #[test]
    fn test_code_analysis_tool_calls_default() {
        let tool_calls = CodeAnalysisToolCalls::default();

        assert_eq!(tool_calls.read, 0);
        assert_eq!(tool_calls.write, 0);
        assert_eq!(tool_calls.edit, 0);
        assert_eq!(tool_calls.todo_write, 0);
        assert_eq!(tool_calls.bash, 0);
    }

    #[test]
    fn test_code_analysis_detail_base_serialization() {
        let detail = CodeAnalysisDetailBase {
            file_path: "/path/to/file.rs".to_string(),
            line_count: 100,
            character_count: 2500,
            timestamp: 1234567890,
        };

        let json = serde_json::to_string(&detail).unwrap();
        let deserialized: CodeAnalysisDetailBase = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.file_path, "/path/to/file.rs");
        assert_eq!(deserialized.line_count, 100);
        assert_eq!(deserialized.character_count, 2500);
        assert_eq!(deserialized.timestamp, 1234567890);
    }

    #[test]
    fn test_code_analysis_write_detail_serialization() {
        let write_detail = CodeAnalysisWriteDetail {
            base: CodeAnalysisDetailBase {
                file_path: "/test/file.rs".to_string(),
                line_count: 10,
                character_count: 250,
                timestamp: 1234567890,
            },
            content: "fn main() {\n    println!(\"Hello\");\n}".to_string(),
        };

        let json = serde_json::to_string(&write_detail).unwrap();
        let deserialized: CodeAnalysisWriteDetail = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.base.file_path, "/test/file.rs");
        assert_eq!(deserialized.base.line_count, 10);
        assert!(deserialized.content.contains("main"));
    }

    #[test]
    fn test_code_analysis_read_detail_serialization() {
        let read_detail = CodeAnalysisReadDetail {
            base: CodeAnalysisDetailBase {
                file_path: "/test/input.txt".to_string(),
                line_count: 50,
                character_count: 1200,
                timestamp: 1234567890,
            },
        };

        let json = serde_json::to_string(&read_detail).unwrap();
        let deserialized: CodeAnalysisReadDetail = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.base.file_path, "/test/input.txt");
        assert_eq!(deserialized.base.line_count, 50);
        assert_eq!(deserialized.base.character_count, 1200);
    }

    #[test]
    fn test_code_analysis_apply_diff_detail_serialization() {
        let edit_detail = CodeAnalysisApplyDiffDetail {
            base: CodeAnalysisDetailBase {
                file_path: "/test/edit.rs".to_string(),
                line_count: 5,
                character_count: 100,
                timestamp: 1234567890,
            },
            old_string: "old content".to_string(),
            new_string: "new content".to_string(),
        };

        let json = serde_json::to_string(&edit_detail).unwrap();
        let deserialized: CodeAnalysisApplyDiffDetail = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.base.file_path, "/test/edit.rs");
        assert_eq!(deserialized.old_string, "old content");
        assert_eq!(deserialized.new_string, "new content");
    }

    #[test]
    fn test_code_analysis_run_command_detail_serialization() {
        let run_detail = CodeAnalysisRunCommandDetail {
            base: CodeAnalysisDetailBase {
                file_path: "/workspace".to_string(),
                line_count: 0,
                character_count: 10,
                timestamp: 1234567890,
            },
            command: "cargo test".to_string(),
            description: "Running tests".to_string(),
        };

        let json = serde_json::to_string(&run_detail).unwrap();
        let deserialized: CodeAnalysisRunCommandDetail = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.command, "cargo test");
        assert_eq!(deserialized.description, "Running tests");
    }

    #[test]
    fn test_extension_type_equality() {
        assert_eq!(ExtensionType::ClaudeCode, ExtensionType::ClaudeCode);
        assert_eq!(ExtensionType::Codex, ExtensionType::Codex);
        assert_eq!(ExtensionType::Copilot, ExtensionType::Copilot);
        assert_eq!(ExtensionType::Gemini, ExtensionType::Gemini);
        assert_eq!(ExtensionType::Grok.to_string(), "Grok");

        assert_ne!(ExtensionType::ClaudeCode, ExtensionType::Codex);
        assert_ne!(ExtensionType::Copilot, ExtensionType::Gemini);
    }

    #[test]
    fn test_extension_type_clone() {
        let ext1 = ExtensionType::ClaudeCode;
        let ext2 = ext1;

        assert_eq!(ext1, ext2);
    }

    #[test]
    fn test_extension_type_debug() {
        let ext = ExtensionType::ClaudeCode;
        let debug_str = format!("{:?}", ext);

        assert!(debug_str.contains("ClaudeCode"));
    }

    #[test]
    fn test_code_analysis_tool_calls_clone() {
        let tool_calls1 = CodeAnalysisToolCalls {
            read: 5,
            write: 3,
            edit: 2,
            todo_write: 1,
            bash: 4,
        };

        let tool_calls2 = tool_calls1.clone();

        assert_eq!(tool_calls1.read, tool_calls2.read);
        assert_eq!(tool_calls1.write, tool_calls2.write);
    }

    #[test]
    fn test_camel_case_serialization() {
        let detail = CodeAnalysisDetailBase {
            file_path: "/test".to_string(),
            line_count: 10,
            character_count: 100,
            timestamp: 123,
        };

        let json = serde_json::to_value(&detail).unwrap();

        assert!(json["filePath"].is_string());
        assert!(json["lineCount"].is_number());
        assert!(json["characterCount"].is_number());
        assert!(json["timestamp"].is_number());
    }

    #[test]
    fn test_pascal_case_tool_calls() {
        let tool_calls = CodeAnalysisToolCalls {
            read: 1,
            write: 2,
            edit: 3,
            todo_write: 4,
            bash: 5,
        };

        let json = serde_json::to_value(&tool_calls).unwrap();

        assert!(json["Read"].is_number());
        assert!(json["Write"].is_number());
        assert!(json["Edit"].is_number());
    }

    #[test]
    fn test_code_analysis_record_serialization() {
        let record = CodeAnalysisRecord {
            total_unique_files: 5,
            total_write_lines: 100,
            total_read_lines: 200,
            total_edit_lines: 50,
            total_write_characters: 2500,
            total_read_characters: 5000,
            total_edit_characters: 1250,
            write_file_details: vec![],
            read_file_details: vec![],
            edit_file_details: vec![],
            run_command_details: vec![],
            tool_call_counts: CodeAnalysisToolCalls::default(),
            conversation_usage: FastHashMap::default(),
            advisor_usage: FastHashMap::default(),
            per_model_analysis: FastHashMap::default(),
            task_id: "task-123".to_string(),
            timestamp: 1234567890,
            folder_path: "/workspace".to_string(),
            git_remote_url: "https://github.com/test/repo".to_string(),
        };

        let json = serde_json::to_string(&record).unwrap();
        let deserialized: CodeAnalysisRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.total_unique_files, 5);
        assert_eq!(deserialized.total_write_lines, 100);
        assert_eq!(deserialized.task_id, "task-123");
        assert_eq!(deserialized.folder_path, "/workspace");
    }

    #[test]
    fn test_empty_details_serialization() {
        let record = CodeAnalysisRecord {
            total_unique_files: 0,
            total_write_lines: 0,
            total_read_lines: 0,
            total_edit_lines: 0,
            total_write_characters: 0,
            total_read_characters: 0,
            total_edit_characters: 0,
            write_file_details: vec![],
            read_file_details: vec![],
            edit_file_details: vec![],
            run_command_details: vec![],
            tool_call_counts: CodeAnalysisToolCalls::default(),
            conversation_usage: FastHashMap::default(),
            advisor_usage: FastHashMap::default(),
            per_model_analysis: FastHashMap::default(),
            task_id: String::new(),
            timestamp: 0,
            folder_path: String::new(),
            git_remote_url: String::new(),
        };

        let json = serde_json::to_string(&record).unwrap();
        let deserialized: CodeAnalysisRecord = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.write_file_details.len(), 0);
        assert_eq!(deserialized.read_file_details.len(), 0);
        assert_eq!(deserialized.edit_file_details.len(), 0);
        assert_eq!(deserialized.run_command_details.len(), 0);
    }
}
