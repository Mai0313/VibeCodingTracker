use crate::models::{Provider, ProviderActiveDays};

/// Trait for data rows that provide model information for provider grouping
pub trait DailyAverageRow {
    fn model(&self) -> &str;
}

/// Trait for provider-specific statistics that can accumulate metrics
pub trait ProviderStatistics<Row: DailyAverageRow>: Default {
    /// Accumulates metrics from a row into provider statistics
    fn accumulate(&mut self, row: &Row, provider: Provider);

    /// Sets the number of active days for this provider
    fn set_days(&mut self, days: usize);
}

/// Calculates daily averages grouped by AI provider
///
/// Groups rows by provider, uses externally-provided day counts, and accumulates metrics.
pub fn calculate_daily_averages<R, S>(
    rows: &[R],
    provider_days: &ProviderActiveDays,
) -> DailyAverages<R, S>
where
    R: DailyAverageRow,
    S: ProviderStatistics<R>,
{
    let mut averages: DailyAverages<R, S> = DailyAverages::default();

    averages.claude.set_days(provider_days.claude);
    averages.codex.set_days(provider_days.codex);
    averages.copilot.set_days(provider_days.copilot);
    averages.gemini.set_days(provider_days.gemini);
    averages.overall.set_days(provider_days.total);

    // Accumulate totals
    for row in rows {
        let provider = Provider::from_model_name(row.model());

        match provider {
            Provider::ClaudeCode => averages.claude.accumulate(row, provider),
            Provider::Codex => averages.codex.accumulate(row, provider),
            Provider::Copilot => averages.copilot.accumulate(row, provider),
            Provider::Gemini => averages.gemini.accumulate(row, provider),
            Provider::Unknown => {}
        }

        // Always accumulate to overall
        averages.overall.accumulate(row, Provider::Unknown);
    }

    averages
}

/// Daily averages organized by provider with generic statistics type
pub struct DailyAverages<R: DailyAverageRow, S: ProviderStatistics<R>> {
    pub claude: S,
    pub codex: S,
    pub copilot: S,
    pub gemini: S,
    pub overall: S,
    _phantom: std::marker::PhantomData<R>,
}

impl<R: DailyAverageRow, S: ProviderStatistics<R>> Default for DailyAverages<R, S> {
    fn default() -> Self {
        Self {
            claude: S::default(),
            codex: S::default(),
            copilot: S::default(),
            gemini: S::default(),
            overall: S::default(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<R: DailyAverageRow, S: ProviderStatistics<R>> DailyAverages<R, S> {
    /// Get stats for a specific provider
    pub fn get_stats(&self, provider: Provider) -> &S {
        match provider {
            Provider::ClaudeCode => &self.claude,
            Provider::Codex => &self.codex,
            Provider::Copilot => &self.copilot,
            Provider::Gemini => &self.gemini,
            Provider::Unknown => &self.overall,
        }
    }

    /// Get mutable stats for a specific provider
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
