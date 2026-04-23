use crate::constants::FastHashMap;
use crate::models::Provider;

/// Token usage data aggregated by model (across all dates)
///
/// Structure: Model Name -> Usage Metrics
/// - Uses FastHashMap (ahash) for better performance than std HashMap
/// - Usage format varies by provider:
///   * Claude/Gemini: `{ input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens }`
///   * Codex: `{ total_token_usage: { input_tokens, output_tokens } }`
pub type UsageResult = FastHashMap<String, serde_json::Value>;

/// Tracks the number of active days per AI provider
///
/// Used for calculating daily averages when data is aggregated by model only.
/// Day counts are derived from file modification dates during processing.
#[derive(Debug, Clone, Default)]
pub struct ProviderActiveDays {
    pub claude: usize,
    pub codex: usize,
    pub copilot: usize,
    pub gemini: usize,
    pub total: usize,
}

/// Per-provider usage data, keyed by source directory rather than by model name.
///
/// The top-level `UsageResult` in `UsageData` intentionally merges same-named
/// models across providers (so the per-model table shows one row for
/// `claude-sonnet-4-6` regardless of whether Claude Code, Copilot CLI, or
/// both invoked it). That merge loses the *source* information though, which
/// matters for the per-provider summary: once Copilot CLI stopped writing
/// the `copilot` sentinel and started recording real model names, the old
/// "classify each row by model-name prefix" logic mis-attributed every
/// Copilot session to Claude Code.
///
/// This struct keeps a separate `UsageResult` per provider so the display
/// layer can sum tokens and cost by source directory directly, with no
/// prefix heuristics involved. It is populated in `usage::calculator` at
/// the same time the global merged map is built.
#[derive(Debug, Default, Clone)]
pub struct PerProviderUsage {
    pub claude: UsageResult,
    pub codex: UsageResult,
    pub copilot: UsageResult,
    pub gemini: UsageResult,
}

impl PerProviderUsage {
    pub fn get(&self, provider: Provider) -> Option<&UsageResult> {
        match provider {
            Provider::ClaudeCode => Some(&self.claude),
            Provider::Codex => Some(&self.codex),
            Provider::Copilot => Some(&self.copilot),
            Provider::Gemini => Some(&self.gemini),
            Provider::Unknown => None,
        }
    }

    pub fn get_mut(&mut self, provider: Provider) -> Option<&mut UsageResult> {
        match provider {
            Provider::ClaudeCode => Some(&mut self.claude),
            Provider::Codex => Some(&mut self.codex),
            Provider::Copilot => Some(&mut self.copilot),
            Provider::Gemini => Some(&mut self.gemini),
            Provider::Unknown => None,
        }
    }
}
