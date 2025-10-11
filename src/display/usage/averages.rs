use crate::display::common::{DailyAverageRow, ProviderAverage, ProviderStatistics};
use crate::models::Provider;
use crate::utils::format_number;
use serde_json::Value;
use std::borrow::Cow;

/// Data structure for a usage row
#[derive(Default)]
pub struct UsageRow {
    pub date: String,
    pub model: String,         // 原始模型名稱
    pub display_model: String, // 可能含 fuzzy match 提示的顯示名稱
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub total: i64,
    pub cost: f64,
}

/// Totals for all usage rows
#[derive(Default)]
pub struct UsageTotals {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub total: i64,
    pub cost: f64,
}

impl UsageTotals {
    pub fn accumulate(&mut self, row: &UsageRow) {
        self.input_tokens += row.input_tokens;
        self.output_tokens += row.output_tokens;
        self.cache_read += row.cache_read;
        self.cache_creation += row.cache_creation;
        self.total += row.total;
        self.cost += row.cost;
    }
}

/// Provider-specific statistics for usage
#[derive(Default, Clone)]
pub struct ProviderStats {
    pub total_tokens: i64,
    pub total_cost: f64,
    pub days_count: usize,
}

impl ProviderStats {
    pub fn avg_tokens(&self) -> f64 {
        if self.days_count > 0 {
            self.total_tokens as f64 / self.days_count as f64
        } else {
            0.0
        }
    }

    pub fn avg_cost(&self) -> f64 {
        if self.days_count > 0 {
            self.total_cost / self.days_count as f64
        } else {
            0.0
        }
    }
}

impl ProviderStatistics<UsageRow> for ProviderStats {
    fn accumulate(&mut self, row: &UsageRow, _provider: Provider) {
        self.total_tokens += row.total;
        self.total_cost += row.cost;
    }

    fn set_days(&mut self, days: usize) {
        self.days_count = days;
    }
}

impl DailyAverageRow for UsageRow {
    fn date(&self) -> &str {
        &self.date
    }

    fn model(&self) -> &str {
        &self.model
    }
}

/// Type alias for daily averages with usage statistics
pub type DailyAverages = crate::display::common::DailyAverages<UsageRow, ProviderStats>;

/// Summary of usage data
#[derive(Default)]
pub struct UsageSummary {
    pub rows: Vec<UsageRow>,
    pub totals: UsageTotals,
    pub daily_averages: DailyAverages,
}

/// Calculate daily averages grouped by provider (uses generic implementation)
pub fn calculate_daily_averages(rows: &[UsageRow]) -> DailyAverages {
    crate::display::common::calculate_daily_averages(rows)
}

/// Build provider average rows for display
pub fn build_provider_average_rows(
    averages: &DailyAverages,
) -> Vec<ProviderAverage<'_, ProviderStats>> {
    let mut rows = Vec::with_capacity(5); // Pre-allocate: max 4 providers + overall

    if averages.claude.days_count > 0 {
        rows.push(ProviderAverage::new(
            Provider::ClaudeCode,
            &averages.claude,
            false,
        ));
    }

    if averages.codex.days_count > 0 {
        rows.push(ProviderAverage::new(
            Provider::Codex,
            &averages.codex,
            false,
        ));
    }

    if averages.copilot.days_count > 0 {
        rows.push(ProviderAverage::new(
            Provider::Copilot,
            &averages.copilot,
            false,
        ));
    }

    if averages.gemini.days_count > 0 {
        rows.push(ProviderAverage::new(
            Provider::Gemini,
            &averages.gemini,
            false,
        ));
    }

    if averages.overall.days_count > 0 || rows.is_empty() {
        rows.push(ProviderAverage::new_overall(&averages.overall));
    }

    rows
}

/// Format tokens per day for display
pub fn format_tokens_per_day(value: f64) -> String {
    if value >= 9_999.5 {
        format_number(value.round() as i64)
    } else if value >= 1.0 {
        format!("{:.1}", value)
    } else if value > 0.0 {
        format!("{:.2}", value)
    } else {
        "0".to_string()
    }
}

/// Build a summary from raw usage data
/// Note: Removed pricing_cache parameter - ModelPricingMap uses global MATCH_CACHE internally
pub fn build_usage_summary(
    usage_data: &crate::models::DateUsageResult,
    pricing_map: &crate::pricing::ModelPricingMap,
) -> UsageSummary {
    if usage_data.is_empty() {
        return UsageSummary::default();
    }

    let mut summary = UsageSummary::default();

    // Pre-allocate rows vector with estimated capacity
    let estimated_size: usize = usage_data.values().map(|m| m.len()).sum();
    summary.rows.reserve(estimated_size);

    // Iterate in chronological order (BTreeMap is automatically sorted by date)
    for (date, date_usage) in usage_data.iter() {
        // Collect and sort models
        let mut models: Vec<_> = date_usage.iter().collect();
        models.sort_by_key(|(model, _)| *model);

        for (model, usage) in models {
            let row = extract_usage_row(date, model, usage, pricing_map);
            summary.totals.accumulate(&row);
            summary.rows.push(row);
        }
    }

    summary.daily_averages = calculate_daily_averages(&summary.rows);
    summary
}

fn extract_usage_row(
    date: &str,
    model: &str,
    usage: &Value,
    pricing_map: &crate::pricing::ModelPricingMap,
) -> UsageRow {
    use crate::pricing::calculate_cost;
    use crate::utils::extract_token_counts;

    // Extract token counts using utility function
    let counts = extract_token_counts(usage);

    // Direct call - no local cache needed (uses global MATCH_CACHE)
    let pricing_result = pricing_map.get(model);

    let cost = calculate_cost(
        counts.input_tokens,
        counts.output_tokens,
        counts.cache_read,
        counts.cache_creation,
        &pricing_result.pricing,
    );

    // Use Cow<str> for display_model to avoid allocation when no fuzzy match
    let display_model = if let Some(matched) = &pricing_result.matched_model {
        Cow::Owned(format!("{} ({})", model, matched))
    } else {
        Cow::Borrowed(model)
    };

    UsageRow {
        date: date.to_string(),
        model: model.to_string(),
        display_model: display_model.into_owned(),
        input_tokens: counts.input_tokens,
        output_tokens: counts.output_tokens,
        cache_read: counts.cache_read,
        cache_creation: counts.cache_creation,
        total: counts.total,
        cost,
    }
}
