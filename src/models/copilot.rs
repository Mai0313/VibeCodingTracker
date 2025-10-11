use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Top-level structure for Copilot CLI session file
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotSession {
    pub session_id: String,
    pub start_time: String,
    #[serde(default)]
    pub chat_messages: Vec<Value>,
    #[serde(default)]
    pub timeline: Vec<TimelineEvent>,
}

/// Individual event in the timeline
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEvent {
    pub id: String,
    pub timestamp: String,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intention_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

/// Helper struct for parsing str_replace_editor arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrReplaceEditorArgs {
    pub command: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_range: Option<Vec<i64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_str: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_str: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_text: Option<String>,
}

/// Helper struct for parsing bash arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashArgs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}
