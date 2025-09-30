use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Claude usage data
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClaudeUsage {
    pub input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub cache_creation: HashMap<String, i64>,
    pub output_tokens: i64,
    pub service_tier: String,
}

/// Codex usage data
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodexUsage {
    pub total_token_usage: HashMap<String, i64>,
    pub last_token_usage: HashMap<String, i64>,
    pub model_context_window: Option<serde_json::Value>,
}

/// Usage result with tool calls and conversation usage
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageResult {
    pub tool_call_counts: HashMap<String, usize>,
    pub conversation_usage: HashMap<String, serde_json::Value>,
}

/// Date-based usage result
pub type DateUsageResult = HashMap<String, HashMap<String, serde_json::Value>>;
