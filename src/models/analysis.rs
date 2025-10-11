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
