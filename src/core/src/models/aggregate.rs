//! Per-provider totals container shared across usage and analysis roll-ups.

/// Per-provider totals alongside an `overall` sum across all providers.
///
/// Parameterized by the per-provider statistics type `S`.
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
    /// Totals for Antigravity sessions.
    pub antigravity: S,
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
            antigravity: S::default(),
            overall: S::default(),
        }
    }
}
