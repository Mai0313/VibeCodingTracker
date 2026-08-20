//! Token-usage aggregation types shared between the usage calculator and the
//! display layer.

use crate::constants::FastHashMap;
use serde::Serialize;

/// Token usage data aggregated by model name (across all dates).
///
/// The value keeps the provider's own token shape, of which there are two:
/// flat token keys (`input_tokens`, `output_tokens`, …) for every provider
/// except Codex, which nests its counters under `total_token_usage`. Which
/// keys a flat value carries still varies — Gemini's reasoning arrives as
/// `thoughts_tokens` where the others use `reasoning_output_tokens`.
/// `extract_token_counts` is what reads both shapes into disjoint buckets.
pub type UsageResult = FastHashMap<String, serde_json::Value>;

/// Tracks the number of active days per AI provider.
///
/// Used for calculating daily averages when data is aggregated by model only.
/// A day is counted from the session's own date: the file modification date
/// for the file-backed providers, the stored row timestamp for the
/// SQLite-backed ones.
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
    /// Distinct active days across all providers combined.
    pub total: usize,
}

/// Per-provider usage data, bucketed by the provider that produced it rather
/// than by model name.
///
/// The top-level `UsageResult` in `UsageData` intentionally merges same-named
/// models across providers (so the per-model table shows one row for
/// `claude-sonnet-4-6` regardless of whether Claude Code, Copilot CLI, or
/// both invoked it). That merge loses the *source* information though, which
/// matters for the per-provider summary: once Copilot CLI stopped writing the
/// `copilot` sentinel and started recording real model names, the old
/// "classify each row by model-name prefix" logic mis-attributed every Copilot
/// session to Claude Code. Keeping one `UsageResult` per provider lets the
/// display layer sum tokens and cost by source with no prefix heuristics. It
/// is populated at the same time as the global merged map.
///
/// There is deliberately no keyed accessor on this type. Its nine buckets are
/// one per [`ExtensionType`](crate::models::ExtensionType) variant, so a
/// [`Provider`](crate::models::Provider)-keyed lookup would carry a tenth
/// `Unknown` naming no bucket here. And nothing ever arrives holding a single
/// key to look up: the summary and pricing passes each need all nine under a
/// per-provider rule, one aggregation path names all nine fields directly, and
/// the one that does key its write keys it on `ExtensionType`.
// A `Provider`-keyed `get` / `get_mut` pair, reachable only through the equally
// uncalled `UsageData::provider_usage`, was removed in #237.
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
}
