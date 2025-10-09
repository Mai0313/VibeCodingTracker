use crate::models::Provider;
use std::collections::{BTreeMap, HashSet};

/// Trait for rows that can provide date and model information
pub trait DailyAverageRow {
    fn date(&self) -> &str;
    fn model(&self) -> &str;
}

/// Generic provider statistics that can accumulate values
/// The Row type parameter allows type-safe accumulation
pub trait ProviderStatistics<Row: DailyAverageRow>: Default {
    /// Accumulate values from a row for a specific provider
    fn accumulate(&mut self, row: &Row, provider: Provider);

    /// Set the number of days for this provider
    fn set_days(&mut self, days: usize);
}

/// Calculate daily averages grouped by provider (generic implementation)
/// This eliminates the 100+ lines of duplicated code between usage and analysis
pub fn calculate_daily_averages<R, S>(rows: &[R]) -> DailyAverages<R, S>
where
    R: DailyAverageRow,
    S: ProviderStatistics<R>,
{
    let mut averages: DailyAverages<R, S> = DailyAverages::default();

    // Use BTreeMap for date storage (already sorted, no String cloning for keys)
    let mut date_provider_map: BTreeMap<&str, HashSet<Provider>> = BTreeMap::new();

    // Group by date and provider to count unique days per provider
    for row in rows {
        let provider = Provider::from_model_name(row.model());
        date_provider_map
            .entry(row.date())
            .or_insert_with(|| HashSet::with_capacity(3)) // Max 3 providers
            .insert(provider);
    }

    // Count days per provider
    let (claude_days, codex_days, gemini_days, total_days) =
        count_provider_days(&date_provider_map);

    averages.claude.set_days(claude_days);
    averages.codex.set_days(codex_days);
    averages.gemini.set_days(gemini_days);
    averages.overall.set_days(total_days);

    // Accumulate totals
    for row in rows {
        let provider = Provider::from_model_name(row.model());

        match provider {
            Provider::ClaudeCode => averages.claude.accumulate(row, provider),
            Provider::Codex => averages.codex.accumulate(row, provider),
            Provider::Gemini => averages.gemini.accumulate(row, provider),
            Provider::Unknown => {}
        }

        // Always accumulate to overall
        averages.overall.accumulate(row, Provider::Unknown);
    }

    averages
}

/// Count days per provider from the date-provider map
fn count_provider_days(
    date_provider_map: &BTreeMap<&str, HashSet<Provider>>,
) -> (usize, usize, usize, usize) {
    let mut claude_days = 0;
    let mut codex_days = 0;
    let mut gemini_days = 0;

    for providers in date_provider_map.values() {
        if providers.contains(&Provider::ClaudeCode) {
            claude_days += 1;
        }
        if providers.contains(&Provider::Codex) {
            codex_days += 1;
        }
        if providers.contains(&Provider::Gemini) {
            gemini_days += 1;
        }
    }

    (claude_days, codex_days, gemini_days, date_provider_map.len())
}

/// Generic daily averages structure
pub struct DailyAverages<R: DailyAverageRow, S: ProviderStatistics<R>> {
    pub claude: S,
    pub codex: S,
    pub gemini: S,
    pub overall: S,
    _phantom: std::marker::PhantomData<R>,
}

impl<R: DailyAverageRow, S: ProviderStatistics<R>> Default for DailyAverages<R, S> {
    fn default() -> Self {
        Self {
            claude: S::default(),
            codex: S::default(),
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
            Provider::Gemini => &self.gemini,
            Provider::Unknown => &self.overall,
        }
    }

    /// Get mutable stats for a specific provider
    pub fn get_stats_mut(&mut self, provider: Provider) -> &mut S {
        match provider {
            Provider::ClaudeCode => &mut self.claude,
            Provider::Codex => &mut self.codex,
            Provider::Gemini => &mut self.gemini,
            Provider::Unknown => &mut self.overall,
        }
    }
}
