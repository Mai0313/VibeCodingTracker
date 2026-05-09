use crate::display::common::ProviderTotal;
use crate::models::{PerProviderUsage, Provider, ProviderActiveDays, UsageResult};
use serde_json::Value;
use std::borrow::Cow;

/// Data structure for a usage row.
///
/// `output_tokens` is the user-visible response the model emitted;
/// `reasoning_tokens` is the separately-billed "thinking" budget (Gemini
/// `thoughts_tokens`, Codex `reasoning_output_tokens`, Copilot
/// `reasoningTokens`). Display layers typically present their sum in a
/// single "Output" column via `output_with_reasoning()` so the per-row
/// numbers reconcile with `total`, while `cost` is computed against the
/// per-token reasoning rate (when the model publishes one) via
/// `calculate_cost`.
#[derive(Default)]
pub struct UsageRow {
    pub model: String,         // 原始模型名稱
    pub display_model: String, // 可能含 fuzzy match 提示的顯示名稱
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub total: i64,
    pub cost: f64,
}

impl UsageRow {
    /// Sum of output and reasoning tokens — the "total model-emitted
    /// tokens" figure most display tables want to show in an Output
    /// column so the row adds up to `total`.
    #[inline]
    pub fn output_with_reasoning(&self) -> i64 {
        self.output_tokens + self.reasoning_tokens
    }
}

/// Totals for all usage rows
#[derive(Default)]
pub struct UsageTotals {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub total: i64,
    pub cost: f64,
}

impl UsageTotals {
    pub fn accumulate(&mut self, row: &UsageRow) {
        self.input_tokens += row.input_tokens;
        self.output_tokens += row.output_tokens;
        self.reasoning_tokens += row.reasoning_tokens;
        self.cache_read += row.cache_read;
        self.cache_creation += row.cache_creation;
        self.total += row.total;
        self.cost += row.cost;
    }

    /// Same helper as [`UsageRow::output_with_reasoning`] for totals.
    #[inline]
    pub fn output_with_reasoning(&self) -> i64 {
        self.output_tokens + self.reasoning_tokens
    }
}

/// Per-provider totals for usage. `days_count` records how many distinct
/// days contributed to these totals so the display layer can show readers
/// the spread without computing a rate.
#[derive(Default, Clone)]
pub struct ProviderStats {
    pub total_tokens: i64,
    pub total_cost: f64,
    pub days_count: usize,
}

impl ProviderStats {
    fn accumulate_row(&mut self, row: &UsageRow) {
        self.total_tokens += row.total;
        self.total_cost += row.cost;
    }
}

/// Type alias for usage totals grouped by provider.
pub type UsageProviderTotals = crate::display::common::ProviderTotals<ProviderStats>;

/// Summary of usage data
#[derive(Default)]
pub struct UsageSummary {
    pub rows: Vec<UsageRow>,
    pub totals: UsageTotals,
    pub provider_totals: UsageProviderTotals,
}

/// Calculate per-provider totals using **source-directory** attribution.
///
/// Token aggregation is fed directly from the `per_provider` map that
/// `usage::calculator` populates from each session's source directory, so the
/// provider assignment is exact regardless of what model name the session
/// happens to carry. The previous "averages" variant divided by
/// `provider_days` to render a per-day rate; the structure is otherwise
/// identical.
pub fn calculate_provider_totals_from_per_provider(
    per_provider: &PerProviderUsage,
    provider_days: &ProviderActiveDays,
    pricing_map: &crate::pricing::ModelPricingMap,
) -> UsageProviderTotals {
    let mut totals = UsageProviderTotals::default();

    totals.claude.days_count = provider_days.claude;
    totals.codex.days_count = provider_days.codex;
    totals.copilot.days_count = provider_days.copilot;
    totals.gemini.days_count = provider_days.gemini;
    totals.overall.days_count = provider_days.total;

    accumulate_provider(&mut totals.claude, &per_provider.claude, pricing_map);
    accumulate_provider(&mut totals.codex, &per_provider.codex, pricing_map);
    accumulate_provider(&mut totals.copilot, &per_provider.copilot, pricing_map);
    accumulate_provider(&mut totals.gemini, &per_provider.gemini, pricing_map);

    // "All Providers" row sums every provider's totals directly rather
    // than reusing the cross-provider merged `UsageData.models` map.
    // That merged map de-duplicates a shared model like `claude-sonnet-4-6`
    // (used by both Claude Code and Copilot CLI) into a single row, so
    // the underlying tokens are *not* double-counted — but we lose the
    // provider attribution needed to populate per-provider cost columns
    // on the same table, and the single merged row would price with one
    // model-lookup where summing per-provider already-priced stats keeps
    // cost consistent with each provider's own row above.
    totals.overall.total_tokens = totals.claude.total_tokens
        + totals.codex.total_tokens
        + totals.copilot.total_tokens
        + totals.gemini.total_tokens;
    totals.overall.total_cost = totals.claude.total_cost
        + totals.codex.total_cost
        + totals.copilot.total_cost
        + totals.gemini.total_cost;

    totals
}

fn accumulate_provider(
    stats: &mut ProviderStats,
    usage: &UsageResult,
    pricing_map: &crate::pricing::ModelPricingMap,
) {
    for (model, raw_usage) in usage {
        let row = extract_usage_row(model, raw_usage, pricing_map);
        stats.accumulate_row(&row);
    }
}

/// Build provider total rows for display.
pub fn build_provider_total_rows(
    totals: &UsageProviderTotals,
) -> Vec<ProviderTotal<'_, ProviderStats>> {
    let mut rows = Vec::with_capacity(5); // max 4 providers + overall

    if totals.claude.days_count > 0 {
        rows.push(ProviderTotal::new(
            Provider::ClaudeCode,
            &totals.claude,
            false,
        ));
    }

    if totals.codex.days_count > 0 {
        rows.push(ProviderTotal::new(Provider::Codex, &totals.codex, false));
    }

    if totals.copilot.days_count > 0 {
        rows.push(ProviderTotal::new(
            Provider::Copilot,
            &totals.copilot,
            false,
        ));
    }

    if totals.gemini.days_count > 0 {
        rows.push(ProviderTotal::new(Provider::Gemini, &totals.gemini, false));
    }

    if totals.overall.days_count > 0 || rows.is_empty() {
        rows.push(ProviderTotal::new_overall(&totals.overall));
    }

    rows
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

    summary.provider_totals =
        calculate_provider_totals_from_per_provider(per_provider, provider_days, pricing_map);
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
        reasoning_tokens: counts.reasoning_tokens,
        cache_read: counts.cache_read,
        cache_creation: counts.cache_creation,
        total: counts.total,
        cost,
    }
}
