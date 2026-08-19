//! Neutral per-provider totals container shared by the `usage` and `analysis`
//! roll-ups (and their display summaries).
//!
//! `ProviderTotals<S>` carries no feature or presentation knowledge, which is
//! what lets it live in `models`: `usage` and `analysis` each build one without
//! importing the other, and the display layer re-exports this type instead of
//! defining a parallel container.

/// Per-provider totals organized by AI provider.
///
/// Keeps each provider's running totals alongside an `overall` "All Providers"
/// bucket. The `S` parameter is the per-provider stats type each feature
/// supplies (e.g. `ProviderStats` for usage, `AnalysisProviderStats` for
/// analysis).
pub struct ProviderTotals<S> {
    /// Totals for Claude Code sessions.
    pub claude: S,
    /// Totals for OpenAI Codex sessions.
    pub codex: S,
    /// Totals for GitHub Copilot CLI sessions.
    pub copilot: S,
    /// Totals for Gemini CLI sessions.
    pub gemini: S,
    /// Totals for OpenCode sessions.
    pub opencode: S,
    /// Totals for Cursor sessions.
    pub cursor: S,
    /// Totals for Hermes sessions.
    pub hermes: S,
    /// Totals for Grok CLI sessions.
    pub grok: S,
    /// Totals for DeepSeek Harness sessions.
    pub deepseek: S,
    /// Sum across every provider (the "All Providers" bucket).
    pub overall: S,
}

impl<S: Default> Default for ProviderTotals<S> {
    fn default() -> Self {
        Self {
            claude: S::default(),
            codex: S::default(),
            copilot: S::default(),
            gemini: S::default(),
            opencode: S::default(),
            cursor: S::default(),
            hermes: S::default(),
            grok: S::default(),
            deepseek: S::default(),
            overall: S::default(),
        }
    }
}
