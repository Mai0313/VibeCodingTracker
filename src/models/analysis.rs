use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Base detail model for code analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisDetailBase {
    pub file_path: String,
    pub line_count: usize,
    pub character_count: usize,
    pub timestamp: i64,
}

/// Write file details
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisWriteDetail {
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
    pub content: String,
}

/// Read file details
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisReadDetail {
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
}

/// Edit file details (diff)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisApplyDiffDetail {
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
    pub old_string: String,
    pub new_string: String,
}

/// Run command details
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysisRunCommandDetail {
    #[serde(flatten)]
    pub base: CodeAnalysisDetailBase,
    pub command: String,
    pub description: String,
}

/// Tool call counters
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct CodeAnalysisToolCalls {
    pub read: usize,
    pub write: usize,
    pub edit: usize,
    pub todo_write: usize,
    pub bash: usize,
}

/// Aggregated analysis record
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
    pub conversation_usage: HashMap<String, serde_json::Value>,
    pub task_id: String,
    pub timestamp: i64,
    pub folder_path: String,
    pub git_remote_url: String,
}

/// Top-level analysis payload
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeAnalysis {
    pub user: String,
    pub extension_name: String,
    pub insights_version: String,
    pub machine_id: String,
    pub records: Vec<CodeAnalysisRecord>,
}

/// Extension type enum
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionType {
    ClaudeCode,
    Codex,
}

impl std::fmt::Display for ExtensionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtensionType::ClaudeCode => write!(f, "Claude-Code"),
            ExtensionType::Codex => write!(f, "Codex"),
        }
    }
}
