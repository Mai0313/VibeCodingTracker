use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Usage result with tool calls and conversation usage
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageResult {
    pub tool_call_counts: HashMap<String, usize>,
    pub conversation_usage: HashMap<String, serde_json::Value>,
}

/// Date-based usage result
pub type DateUsageResult = HashMap<String, HashMap<String, serde_json::Value>>;
