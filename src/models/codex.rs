use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Single log entry from Codex/OpenAI session file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexLog {
    pub timestamp: String,
    #[serde(rename = "type")]
    pub log_type: String,
    pub payload: CodexPayload,
}

/// Payload data containing event-specific information within a Codex log entry
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

/// Message content block in Codex format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: Option<String>,
}

/// Git repository metadata captured during a Codex session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexGitInfo {
    pub commit_hash: Option<String>,
    pub branch: Option<String>,
    pub repository_url: Option<String>,
}

/// Shell command arguments structure (legacy `name == "shell"` function call).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexShellArguments {
    pub command: Vec<String>,
}

/// Arguments for the current `name == "exec_command"` function call.
///
/// Codex CLI replaced the legacy `shell` function (whose arguments were a
/// `["bash", "-lc", "<script>"]` array) with a flat `{cmd, workdir, ...}`
/// object. The analyzer normalises both into the same `CodexShellCall`
/// downstream so the patch / sed / cat detection can stay shared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexExecCommandArguments {
    pub cmd: String,
    #[serde(default)]
    pub workdir: String,
}

/// Shell command execution result including output and metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexShellOutput {
    pub output: String,
    pub metadata: Option<CodexShellMetadata>,
}

/// Execution metadata for shell commands
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexShellMetadata {
    pub exit_code: i32,
    pub duration_seconds: f64,
}
