//! Normalized, cross-provider analysis result types.
//!
//! These structs are the analyzer's output shape: every provider parser in
//! `src/session/` produces a [`CodeAnalysis`] regardless of the source
//! assistant, and the `analysis` / `usage` roll-up layers consume them. The
//! `serde` attributes here also define the JSON layout of the golden fixtures,
//! `vct analysis FILE`, and each element of the batch `vct analysis --json`
//! array.

use crate::constants::FastHashMap;
use serde::{Deserialize, Serialize, Serializer, ser::SerializeMap};

/// Serializes a model-keyed usage map in lexical key order.
///
/// [`FastHashMap`] intentionally randomizes iteration order, but analysis JSON
/// is a persisted public format. Sorting only at this boundary keeps the hot
/// parser path fast while making repeated serialization byte-for-byte stable.
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

/// Fields shared by every per-operation detail record.
///
/// `line_count` and `character_count` are measured on the *trimmed* payload,
/// and `character_count` counts Unicode scalar values (`str::chars`), not
/// bytes. Serialized with camelCase keys (`filePath`, `lineCount`, …).
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

/// A single file-write operation, including the full written content.
///
/// The `base` fields are flattened into the same JSON object (no nested
/// `base` key), so the record serializes as `{filePath, lineCount, …, content}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisWriteDetail {
    /// Shared path / line / character / timestamp metadata, flattened inline.
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
    /// Full content written to the file.
    pub content: String,
}

/// A single file-read operation.
///
/// Carries only the shared [`CodeAnalysisDetailBase`] metadata; the file body
/// itself is not retained.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisReadDetail {
    /// Shared path / line / character / timestamp metadata, flattened inline.
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
}

/// A single file-edit operation, recording the before/after strings.
///
/// `line_count` / `character_count` in `base` describe the new (replacement)
/// text. Serializes with the `base` fields flattened inline.
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

/// A single shell-command execution.
///
/// For run-command records `base.file_path` holds the session's working
/// directory (there is no single target file) and `base.line_count` is `0`.
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

/// Per-session counters for each tool the analyzer tracks.
///
/// Serialized with PascalCase keys (`Read`, `Write`, `Edit`, `TodoWrite`,
/// `Bash`).
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
/// One record corresponds to one session file. When parsed in
/// `ParseMode::UsageOnly` the `*_file_details` / `run_command_details` vectors
/// are left empty to avoid allocating large bodies, while the `total_*`
/// counters and `conversation_usage` are still populated.
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
    /// Per-operation write records (empty in `ParseMode::UsageOnly`).
    pub write_file_details: Vec<CodeAnalysisWriteDetail>,
    /// Per-operation read records (empty in `ParseMode::UsageOnly`).
    pub read_file_details: Vec<CodeAnalysisReadDetail>,
    /// Per-operation edit records (empty in `ParseMode::UsageOnly`).
    pub edit_file_details: Vec<CodeAnalysisApplyDiffDetail>,
    /// Per-command shell execution records (empty in `ParseMode::UsageOnly`).
    pub run_command_details: Vec<CodeAnalysisRunCommandDetail>,
    /// Tool-call counters for the session.
    pub tool_call_counts: CodeAnalysisToolCalls,
    /// Token-usage payloads keyed by model name; shape varies by provider
    /// (see [`crate::models::UsageResult`]).
    #[serde(serialize_with = "serialize_conversation_usage")]
    pub conversation_usage: FastHashMap<String, serde_json::Value>,
    /// Token usage from Claude Code `advisor_message` iterations, keyed by the
    /// advisor's own model. Kept **out** of `conversation_usage` on purpose:
    /// the `analysis` aggregator attributes a record's file-operation / tool
    /// counts to every model in `conversation_usage`, but an advisor model
    /// never executes tools, so adding it there would mis-attribute the main
    /// model's metrics to the advisor. The `usage` path merges this in (priced
    /// at the advisor's own rate); `analysis` ignores it. Not serialized, so
    /// the `analysis` JSON / golden output is unaffected.
    #[serde(skip)]
    pub advisor_usage: FastHashMap<String, serde_json::Value>,
    /// Session / task identifier from the source log.
    pub task_id: String,
    /// Unix epoch timestamp (milliseconds) of the session's last activity.
    pub timestamp: i64,
    /// Working directory the session ran in.
    pub folder_path: String,
    /// Git remote URL of the project, when one was detected.
    pub git_remote_url: String,
}

/// Top-level analysis result: environment metadata plus one record per session.
///
/// This is the shape returned by `parse_session_file_typed`, printed directly
/// by `vct analysis FILE`, and used for every element of the batch
/// `vct analysis --json` array. The `insights_version`, `machine_id`, and
/// `user` fields are environment-specific and are deliberately ignored by the
/// golden fixture tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysis {
    /// OS user name that owned the session.
    pub user: String,
    /// Source assistant label (e.g. `Claude-Code`), as produced by
    /// [`ExtensionType`]'s [`std::fmt::Display`].
    pub extension_name: String,
    /// Version string of the analyzer that produced this result.
    pub insights_version: String,
    /// Stable machine identifier for the host.
    pub machine_id: String,
    /// One [`CodeAnalysisRecord`] per parsed session.
    pub records: Vec<CodeAnalysisRecord>,
}

/// The AI coding assistant a session came from.
///
/// Distinct from [`crate::models::Provider`]: `ExtensionType` is the
/// concrete assistants the detector resolves a session file to (there is no
/// `Unknown` variant), and its [`std::fmt::Display`] produces the hyphenated
/// `extension_name` strings stored in [`CodeAnalysis`].
///
/// # Examples
///
/// ```
/// use vibe_coding_tracker::models::ExtensionType;
///
/// assert_eq!(ExtensionType::Copilot.to_string(), "Copilot-CLI");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        // Test serializing CodeAnalysisToolCalls
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
        // Test default values
        let tool_calls = CodeAnalysisToolCalls::default();

        assert_eq!(tool_calls.read, 0);
        assert_eq!(tool_calls.write, 0);
        assert_eq!(tool_calls.edit, 0);
        assert_eq!(tool_calls.todo_write, 0);
        assert_eq!(tool_calls.bash, 0);
    }

    #[test]
    fn test_code_analysis_detail_base_serialization() {
        // Test serializing CodeAnalysisDetailBase
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
        // Test serializing CodeAnalysisWriteDetail
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
        // Test serializing CodeAnalysisReadDetail
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
        // Test serializing CodeAnalysisApplyDiffDetail
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
        // Test serializing CodeAnalysisRunCommandDetail
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
        // Test ExtensionType equality
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
        // Test ExtensionType can be cloned
        let ext1 = ExtensionType::ClaudeCode;
        let ext2 = ext1;

        assert_eq!(ext1, ext2);
    }

    #[test]
    fn test_extension_type_debug() {
        // Test ExtensionType debug formatting
        let ext = ExtensionType::ClaudeCode;
        let debug_str = format!("{:?}", ext);

        assert!(debug_str.contains("ClaudeCode"));
    }

    #[test]
    fn test_code_analysis_tool_calls_clone() {
        // Test cloning CodeAnalysisToolCalls
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
        // Test that serialization uses camelCase
        let detail = CodeAnalysisDetailBase {
            file_path: "/test".to_string(),
            line_count: 10,
            character_count: 100,
            timestamp: 123,
        };

        let json = serde_json::to_value(&detail).unwrap();

        // Should have camelCase keys
        assert!(json["filePath"].is_string());
        assert!(json["lineCount"].is_number());
        assert!(json["characterCount"].is_number());
        assert!(json["timestamp"].is_number());
    }

    #[test]
    fn test_pascal_case_tool_calls() {
        // Test that tool calls use PascalCase
        let tool_calls = CodeAnalysisToolCalls {
            read: 1,
            write: 2,
            edit: 3,
            todo_write: 4,
            bash: 5,
        };

        let json = serde_json::to_value(&tool_calls).unwrap();

        // Should have PascalCase keys (first letter capitalized)
        assert!(json["Read"].is_number());
        assert!(json["Write"].is_number());
        assert!(json["Edit"].is_number());
    }

    #[test]
    fn test_code_analysis_record_serialization() {
        // Test serializing full CodeAnalysisRecord
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
        // Test serializing empty detail vectors
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
