use crate::display::common::ProviderAverage;
use crate::models::Provider;
use crate::utils::format_number;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

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

/// Provider-specific statistics
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

/// Daily averages grouped by provider
#[derive(Default)]
pub struct DailyAverages {
    pub claude: ProviderStats,
    pub codex: ProviderStats,
    pub gemini: ProviderStats,
    pub overall: ProviderStats,
}

/// Summary of usage data
#[derive(Default)]
pub struct UsageSummary {
    pub rows: Vec<UsageRow>,
    pub totals: UsageTotals,
    pub daily_averages: DailyAverages,
}

/// Calculate daily averages grouped by provider (optimized with BTreeMap)
pub fn calculate_daily_averages(rows: &[UsageRow]) -> DailyAverages {
    let mut averages = DailyAverages::default();

    // Use BTreeMap for date storage (already sorted, no String cloning for keys)
    let mut date_provider_map: BTreeMap<&str, HashSet<Provider>> = BTreeMap::new();

    // Group by date and provider to count unique days per provider
    for row in rows {
        let provider = Provider::from_model_name(&row.model);
        date_provider_map
            .entry(&row.date)
            .or_insert_with(|| HashSet::with_capacity(3)) // Max 3 providers
            .insert(provider);
    }

    // Count days per provider using BTreeMap (avoids HashSet cloning)
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

    averages.claude.days_count = claude_days;
    averages.codex.days_count = codex_days;
    averages.gemini.days_count = gemini_days;
    averages.overall.days_count = date_provider_map.len();

    // Accumulate totals
    for row in rows {
        let provider = Provider::from_model_name(&row.model);
        match provider {
            Provider::ClaudeCode => {
                averages.claude.total_tokens += row.total;
                averages.claude.total_cost += row.cost;
            }
            Provider::Codex => {
                averages.codex.total_tokens += row.total;
                averages.codex.total_cost += row.cost;
            }
            Provider::Gemini => {
                averages.gemini.total_tokens += row.total;
                averages.gemini.total_cost += row.cost;
            }
            Provider::Unknown => {}
        }
        averages.overall.total_tokens += row.total;
        averages.overall.total_cost += row.cost;
    }

    averages
}

/// Build provider average rows for display
pub fn build_provider_average_rows(
    averages: &DailyAverages,
) -> Vec<ProviderAverage<'_, ProviderStats>> {
    let mut rows = Vec::with_capacity(4); // Pre-allocate: max 3 providers + overall

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
pub fn build_usage_summary(
    usage_data: &crate::models::DateUsageResult,
    pricing_map: &crate::pricing::ModelPricingMap,
    pricing_cache: &mut HashMap<String, crate::pricing::ModelPricingResult>,
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
            let row = extract_usage_row(date, model, usage, pricing_map, pricing_cache);
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
    pricing_cache: &mut HashMap<String, crate::pricing::ModelPricingResult>,
) -> UsageRow {
    use crate::pricing::calculate_cost;
    use crate::utils::extract_token_counts;

    // Extract token counts using utility function
    let counts = extract_token_counts(usage);

    // Calculate cost with fuzzy matching (using entry API to avoid double lookup)
    let pricing_result = pricing_cache
        .entry(model.to_string())
        .or_insert_with(|| pricing_map.get(model));

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
