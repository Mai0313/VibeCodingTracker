use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Claude Code log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeCodeLog {
    pub parent_uuid: Option<String>,
    pub is_sidechain: bool,
    pub user_type: String,
    pub cwd: String,
    pub session_id: String,
    pub version: String,
    pub git_branch: String,
    #[serde(rename = "type")]
    pub log_type: String,
    pub uuid: String,
    pub timestamp: String,
    pub message: Option<Value>,
    pub tool_use_result: Option<Value>,
}
