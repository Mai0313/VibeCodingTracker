use crate::models::Provider;

/// Per-provider totals organized by AI provider.
///
/// Replaces the previous `DailyAverages<R, S>` generic — the display layer
/// no longer renders per-day rates, so this just keeps each provider's
/// running totals alongside an `overall` "All Providers" bucket. The `S`
/// parameter is the per-provider stats type each command supplies (e.g.
/// `ProviderStats` for usage, `AnalysisProviderStats` for analysis).
pub struct ProviderTotals<S> {
    pub claude: S,
    pub codex: S,
    pub copilot: S,
    pub gemini: S,
    pub overall: S,
}

impl<S: Default> Default for ProviderTotals<S> {
    fn default() -> Self {
        Self {
            claude: S::default(),
            codex: S::default(),
            copilot: S::default(),
            gemini: S::default(),
            overall: S::default(),
        }
    }
}

impl<S> ProviderTotals<S> {
    pub fn get_stats(&self, provider: Provider) -> &S {
        match provider {
            Provider::ClaudeCode => &self.claude,
            Provider::Codex => &self.codex,
            Provider::Copilot => &self.copilot,
            Provider::Gemini => &self.gemini,
            Provider::Unknown => &self.overall,
        }
    }

    pub fn get_stats_mut(&mut self, provider: Provider) -> &mut S {
        match provider {
            Provider::ClaudeCode => &mut self.claude,
            Provider::Codex => &mut self.codex,
            Provider::Copilot => &mut self.copilot,
            Provider::Gemini => &mut self.gemini,
            Provider::Unknown => &mut self.overall,
        }
    }
}
