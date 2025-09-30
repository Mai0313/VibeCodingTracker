use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Codex log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexLog {
    pub timestamp: String,
    #[serde(rename = "type")]
    pub log_type: String,
    pub payload: CodexPayload,
}

/// Codex payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexPayload {
    #[serde(rename = "type")]
    pub payload_type: Option<String>,
    pub role: Option<String>,
    pub content: Option<Vec<CodexContent>>,
    pub name: Option<String>,
    pub arguments: Option<String>,
    pub call_id: Option<String>,
    pub output: Option<String>,
    pub message: Option<String>,
    pub info: Option<Value>,
    pub cwd: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox_policy: Option<Value>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub summary: Option<String>,
    pub id: Option<String>,
    pub originator: Option<String>,
    pub git: Option<CodexGitInfo>,
}

/// Codex content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: Option<String>,
}

/// Codex git information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexGitInfo {
    pub commit_hash: Option<String>,
    pub branch: Option<String>,
    pub repository_url: Option<String>,
}

/// Codex shell arguments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexShellArguments {
    pub command: Vec<String>,
}

/// Codex shell output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexShellOutput {
    pub output: String,
    pub metadata: Option<CodexShellMetadata>,
}

/// Codex shell metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexShellMetadata {
    pub exit_code: i32,
    pub duration_seconds: f64,
}
