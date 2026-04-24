use crate::constants::FastHashMap;
use serde::{Deserialize, Serialize};

/// Base metadata for file operations captured during analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisDetailBase {
    pub file_path: String,
    pub line_count: usize,
    pub character_count: usize,
    pub timestamp: i64,
}

/// Details of a file write operation including full content
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisWriteDetail {
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
    pub content: String,
}

/// Details of a file read operation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisReadDetail {
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
}

/// Details of a file edit operation showing before and after content
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisApplyDiffDetail {
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
    pub old_string: String,
    pub new_string: String,
}

/// Details of a shell command execution
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisRunCommandDetail {
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
    pub command: String,
    pub description: String,
}

/// Counters for each type of tool call made during a coding session
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CodeAnalysisToolCalls {
    pub read: usize,
    pub write: usize,
    pub edit: usize,
    pub todo_write: usize,
    pub bash: usize,
}

/// Aggregated metrics and details for a single coding session
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisRecord {
    pub total_unique_files: usize,
    pub total_write_lines: usize,
    pub total_read_lines: usize,
    pub total_edit_lines: usize,
    pub total_write_characters: usize,
    pub total_read_characters: usize,
    pub total_edit_characters: usize,
    pub write_file_details: Vec<CodeAnalysisWriteDetail>,
    pub read_file_details: Vec<CodeAnalysisReadDetail>,
    pub edit_file_details: Vec<CodeAnalysisApplyDiffDetail>,
    pub run_command_details: Vec<CodeAnalysisRunCommandDetail>,
    pub tool_call_counts: CodeAnalysisToolCalls,
    pub conversation_usage: FastHashMap<String, serde_json::Value>,
    pub task_id: String,
    pub timestamp: i64,
    pub folder_path: String,
    pub git_remote_url: String,
}

/// Top-level analysis result containing metadata and session records
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysis {
    pub user: String,
    pub extension_name: String,
    pub insights_version: String,
    pub machine_id: String,
    pub records: Vec<CodeAnalysisRecord>,
}

/// AI coding assistant extension types (Claude Code, Codex, Copilot, or Gemini)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionType {
    ClaudeCode,
    Codex,
    Copilot,
    Gemini,
}

impl std::fmt::Display for ExtensionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtensionType::ClaudeCode => write!(f, "Claude-Code"),
            ExtensionType::Codex => write!(f, "Codex"),
            ExtensionType::Copilot => write!(f, "Copilot-CLI"),
            ExtensionType::Gemini => write!(f, "Gemini"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
