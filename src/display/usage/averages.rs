use crate::display::common::{DailyAverageRow, ProviderAverage, ProviderStatistics};
use crate::models::{PerProviderUsage, Provider, ProviderActiveDays, UsageResult};
use crate::utils::format_number;
use serde_json::Value;
use std::borrow::Cow;

/// Data structure for a usage row
#[derive(Default)]
pub struct UsageRow {
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

/// Calculate daily averages grouped by provider, using **source-directory**
/// attribution instead of model-name heuristics.
///
/// The previous implementation ran `Provider::from_model_name(row.model())`
/// over each merged per-model row, which misattributed every Copilot
/// session to Claude Code the moment the Copilot parser started emitting
/// real model names (e.g. `claude-sonnet-4-6`) instead of the historical
/// sentinel string `"copilot"`. Token aggregation is now fed directly
/// from the `per_provider` map that `usage::calculator` populates from
/// each session's source directory, so the provider assignment is exact
/// regardless of what model name the session happens to carry.
pub fn calculate_daily_averages_from_per_provider(
    per_provider: &PerProviderUsage,
    provider_days: &ProviderActiveDays,
    pricing_map: &crate::pricing::ModelPricingMap,
) -> DailyAverages {
    let mut averages = DailyAverages::default();

    averages.claude.set_days(provider_days.claude);
    averages.codex.set_days(provider_days.codex);
    averages.copilot.set_days(provider_days.copilot);
    averages.gemini.set_days(provider_days.gemini);
    averages.overall.set_days(provider_days.total);

    accumulate_provider(&mut averages.claude, &per_provider.claude, pricing_map);
    accumulate_provider(&mut averages.codex, &per_provider.codex, pricing_map);
    accumulate_provider(&mut averages.copilot, &per_provider.copilot, pricing_map);
    accumulate_provider(&mut averages.gemini, &per_provider.gemini, pricing_map);

    // "All Providers" row sums every provider's totals directly rather
    // than reusing the cross-provider merged `UsageData.models` map.
    // That merged map de-duplicates a shared model like `claude-sonnet-4-6`
    // (used by both Claude Code and Copilot CLI) into a single row, so
    // the underlying tokens are *not* double-counted — but we lose the
    // provider attribution needed to populate per-provider cost columns
    // on the same table, and the single merged row would price with one
    // model-lookup where summing per-provider already-priced stats keeps
    // cost consistent with each provider's own row above.
    averages.overall.total_tokens = averages.claude.total_tokens
        + averages.codex.total_tokens
        + averages.copilot.total_tokens
        + averages.gemini.total_tokens;
    averages.overall.total_cost = averages.claude.total_cost
        + averages.codex.total_cost
        + averages.copilot.total_cost
        + averages.gemini.total_cost;

    averages
}

fn accumulate_provider(
    stats: &mut ProviderStats,
    usage: &UsageResult,
    pricing_map: &crate::pricing::ModelPricingMap,
) {
    for (model, raw_usage) in usage {
        let row = extract_usage_row(model, raw_usage, pricing_map);
        // Provider is ignored by the usage impl of `accumulate`, but we
        // still pass a value to satisfy the trait contract.
        stats.accumulate(&row, Provider::Unknown);
    }
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

/// Build a summary from raw usage data.
///
/// `usage_data` is the cross-provider merged map (drives the per-model
/// table); `per_provider` is the source-directory-scoped map (drives the
/// per-provider footer). Keeping the two aggregations independent is what
/// lets Copilot-originated Claude tokens stay attributed to Copilot even
/// though they share a row with Claude Code tokens in the main table.
pub fn build_usage_summary(
    usage_data: &UsageResult,
    per_provider: &PerProviderUsage,
    provider_days: &ProviderActiveDays,
    pricing_map: &crate::pricing::ModelPricingMap,
) -> UsageSummary {
    if usage_data.is_empty() {
        return UsageSummary::default();
    }

    let mut summary = UsageSummary::default();

    // Pre-allocate rows vector
    summary.rows.reserve(usage_data.len());

    // Extract rows first so we can sort by cost
    for (model, usage) in usage_data.iter() {
        let row = extract_usage_row(model, usage, pricing_map);
        summary.rows.push(row);
    }

    // Sort by cost ascending (higher cost at the bottom); tie-break by model name for stability
    summary.rows.sort_by(|a, b| {
        a.cost
            .partial_cmp(&b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model.cmp(&b.model))
    });

    for row in &summary.rows {
        summary.totals.accumulate(row);
    }

    summary.daily_averages =
        calculate_daily_averages_from_per_provider(per_provider, provider_days, pricing_map);
    summary
}

fn extract_usage_row(
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
        counts.reasoning_tokens,
        counts.cache_read,
        counts.cache_creation_5m,
        counts.cache_creation_1h,
        &pricing_result.pricing,
    );

    // Use Cow<str> for display_model to avoid allocation when no fuzzy match
    let display_model = if let Some(matched) = &pricing_result.matched_model {
        Cow::Owned(format!("{} ({})", model, matched))
    } else {
        Cow::Borrowed(model)
    };

    UsageRow {
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
