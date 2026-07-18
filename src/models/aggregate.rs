//! Neutral per-provider totals container shared by the `usage` and `analysis`
//! roll-ups (and their display summaries).
//!
//! `ProviderTotals<S>` is pure data over [`Provider`] — no feature or
//! presentation knowledge — so both core summaries and the display layer build
//! and read it without either depending on the other.

use crate::models::Provider;

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
            overall: S::default(),
        }
    }
}

impl<S> ProviderTotals<S> {
    /// Borrows the stats bucket for `provider`.
    ///
    /// [`Provider::Unknown`] maps to the `overall` bucket, since there is no
    /// dedicated slot for unclassified providers.
    pub fn get_stats(&self, provider: Provider) -> &S {
        match provider {
            Provider::ClaudeCode => &self.claude,
            Provider::Codex => &self.codex,
            Provider::Copilot => &self.copilot,
            Provider::Gemini => &self.gemini,
            Provider::OpenCode => &self.opencode,
            Provider::Cursor => &self.cursor,
            Provider::Hermes => &self.hermes,
            Provider::Grok => &self.grok,
            Provider::Unknown => &self.overall,
        }
    }

    /// Mutably borrows the stats bucket for `provider`.
    ///
    /// [`Provider::Unknown`] maps to the `overall` bucket, since there is no
    /// dedicated slot for unclassified providers.
    pub fn get_stats_mut(&mut self, provider: Provider) -> &mut S {
        match provider {
            Provider::ClaudeCode => &mut self.claude,
            Provider::Codex => &mut self.codex,
            Provider::Copilot => &mut self.copilot,
            Provider::Gemini => &mut self.gemini,
            Provider::OpenCode => &mut self.opencode,
            Provider::Cursor => &mut self.cursor,
            Provider::Hermes => &mut self.hermes,
            Provider::Grok => &mut self.grok,
            Provider::Unknown => &mut self.overall,
        }
    }
}
