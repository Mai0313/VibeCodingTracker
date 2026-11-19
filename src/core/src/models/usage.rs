//! Token-usage aggregation types.

use crate::constants::FastHashMap;
use serde::Serialize;

/// Token usage aggregated by model name across dates.
///
/// Values retain each provider's wire shape: flat token keys (`input_tokens`, `output_tokens`, ...) for most providers, or nested under `total_token_usage` for Codex. Gemini reports reasoning as `thoughts_tokens` where other providers use `reasoning_output_tokens`.
pub type UsageResult = FastHashMap<String, serde_json::Value>;

/// Distinct active day counts per provider and overall.
///
/// Active days come from the file modification date for file-backed providers and from stored row timestamps for SQLite-backed providers.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProviderActiveDays {
    /// Distinct active days observed for Claude Code.
    pub claude: usize,
    /// Distinct active days observed for Codex.
    pub codex: usize,
    /// Distinct active days observed for Copilot CLI.
    pub copilot: usize,
    /// Distinct active days observed for Gemini CLI.
    pub gemini: usize,
    /// Distinct active days observed for OpenCode.
    pub opencode: usize,
    /// Distinct active days observed for Cursor.
    pub cursor: usize,
    /// Distinct active days observed for Hermes.
    pub hermes: usize,
    /// Distinct active days observed for Grok CLI.
    pub grok: usize,
    /// Distinct active days observed for DeepSeek Harness.
    pub deepseek: usize,
    /// Distinct active days observed for Antigravity.
    pub antigravity: usize,
    /// Distinct active days across all providers combined.
    pub total: usize,
}

/// Per-model token usage bucketed by the originating provider.
///
/// Preserves source attribution when multiple providers invoke the same model name. Holds one [`UsageResult`] per [`crate::models::ExtensionType`] variant and deliberately omits a [`crate::models::Provider`]-keyed lookup, as [`crate::models::Provider::Unknown`] has no bucket.
#[derive(Debug, Default, Clone, Serialize)]
pub struct PerProviderUsage {
    /// Per-model usage attributed to Claude Code.
    pub claude: UsageResult,
    /// Per-model usage attributed to Codex.
    pub codex: UsageResult,
    /// Per-model usage attributed to Copilot CLI.
    pub copilot: UsageResult,
    /// Per-model usage attributed to Gemini CLI.
    pub gemini: UsageResult,
    /// Per-model usage attributed to OpenCode.
    pub opencode: UsageResult,
    /// Per-model usage attributed to Cursor.
    pub cursor: UsageResult,
    /// Per-model usage attributed to Hermes.
    pub hermes: UsageResult,
    /// Per-model usage attributed to Grok CLI.
    pub grok: UsageResult,
    /// Per-model usage attributed to DeepSeek Harness.
    pub deepseek: UsageResult,
    /// Per-model usage attributed to Antigravity.
    pub antigravity: UsageResult,
}
