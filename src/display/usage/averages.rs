use crate::display::common::ProviderTotal;
use crate::models::Provider;
use crate::usage::{CostSource, UsageData};
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
#[derive(Default, Clone)]
pub struct UsageRow {
    /// Raw model name as reported by the session (the pricing-lookup key).
    pub model: String, // 原始模型名稱
    /// Name shown in the table; appends the fuzzy-matched pricing model in
    /// parentheses when the lookup was not exact.
    pub display_model: String, // 可能含 fuzzy match 提示的顯示名稱
    /// Prompt (input) tokens.
    pub input_tokens: i64,
    /// User-visible response tokens, excluding reasoning.
    pub output_tokens: i64,
    /// Separately-billed "thinking" tokens.
    pub reasoning_tokens: i64,
    /// Tokens served from the prompt cache.
    pub cache_read: i64,
    /// Tokens written to the prompt cache.
    pub cache_creation: i64,
    /// Sum of all token buckets for this model.
    pub total: i64,
    /// LiteLLM-priced cost in USD for this model's tokens.
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

/// Column-wise totals across every [`UsageRow`] in a summary.
#[derive(Default)]
pub struct UsageTotals {
    /// Summed prompt (input) tokens.
    pub input_tokens: i64,
    /// Summed response tokens, excluding reasoning.
    pub output_tokens: i64,
    /// Summed reasoning ("thinking") tokens.
    pub reasoning_tokens: i64,
    /// Summed cache-read tokens.
    pub cache_read: i64,
    /// Summed cache-creation tokens.
    pub cache_creation: i64,
    /// Summed total tokens.
    pub total: i64,
    /// Summed cost in USD.
    pub cost: f64,
}

impl UsageTotals {
    /// Adds every token bucket and the cost of `row` into these totals.
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
    /// Total tokens attributed to this provider.
    pub total_tokens: i64,
    /// Total cost in USD attributed to this provider.
    pub total_cost: f64,
    /// Number of distinct days that contributed to these totals.
    pub days_count: usize,
}

/// Type alias for usage totals grouped by provider.
pub type UsageProviderTotals = crate::display::common::ProviderTotals<ProviderStats>;

/// Fully priced usage view shared by every output mode.
#[derive(Default)]
pub struct UsageSummary {
    /// Per-model rows, sorted by ascending cost (model name as tie-break).
    pub rows: Vec<UsageRow>,
    /// Column-wise totals across all rows.
    pub totals: UsageTotals,
    /// Per-provider totals for the summary footer.
    pub provider_totals: UsageProviderTotals,
}

/// Calculates provider totals from the request-level pricing ledger retained
/// in [`UsageData`]. Tokens retain source-directory attribution, while costs
/// are resolved independently for every provider request.
pub fn calculate_provider_totals_from_usage_data(
    usage_data: &UsageData,
    pricing_map: &crate::pricing::ModelPricingMap,
) -> UsageProviderTotals {
    let mut totals = UsageProviderTotals::default();
    let provider_days = &usage_data.provider_days;
    totals.claude.days_count = provider_days.claude;
    totals.codex.days_count = provider_days.codex;
    totals.copilot.days_count = provider_days.copilot;
    totals.gemini.days_count = provider_days.gemini;
    totals.grok.days_count = provider_days.grok;
    totals.opencode.days_count = provider_days.opencode;
    totals.cursor.days_count = provider_days.cursor;
    totals.hermes.days_count = provider_days.hermes;
    totals.overall.days_count = provider_days.total;

    accumulate_provider_tokens(&mut totals.claude, &usage_data.per_provider.claude);
    accumulate_provider_tokens(&mut totals.codex, &usage_data.per_provider.codex);
    accumulate_provider_tokens(&mut totals.copilot, &usage_data.per_provider.copilot);
    accumulate_provider_tokens(&mut totals.gemini, &usage_data.per_provider.gemini);
    accumulate_provider_tokens(&mut totals.grok, &usage_data.per_provider.grok);
    accumulate_provider_tokens(&mut totals.opencode, &usage_data.per_provider.opencode);
    accumulate_provider_tokens(&mut totals.cursor, &usage_data.per_provider.cursor);
    accumulate_provider_tokens(&mut totals.hermes, &usage_data.per_provider.hermes);

    totals.claude.total_cost = provider_cost(
        usage_data,
        crate::models::ExtensionType::ClaudeCode,
        pricing_map,
    );
    totals.codex.total_cost =
        provider_cost(usage_data, crate::models::ExtensionType::Codex, pricing_map);
    totals.copilot.total_cost = provider_cost(
        usage_data,
        crate::models::ExtensionType::Copilot,
        pricing_map,
    );
    totals.gemini.total_cost = provider_cost(
        usage_data,
        crate::models::ExtensionType::Gemini,
        pricing_map,
    );
    totals.grok.total_cost =
        provider_cost(usage_data, crate::models::ExtensionType::Grok, pricing_map);
    totals.opencode.total_cost = provider_cost(
        usage_data,
        crate::models::ExtensionType::OpenCode,
        pricing_map,
    );
    totals.cursor.total_cost = provider_cost(
        usage_data,
        crate::models::ExtensionType::Cursor,
        pricing_map,
    );
    totals.hermes.total_cost = provider_cost(
        usage_data,
        crate::models::ExtensionType::Hermes,
        pricing_map,
    );
    totals.overall.total_tokens = totals.claude.total_tokens
        + totals.codex.total_tokens
        + totals.copilot.total_tokens
        + totals.gemini.total_tokens
        + totals.grok.total_tokens
        + totals.opencode.total_tokens
        + totals.cursor.total_tokens
        + totals.hermes.total_tokens;
    totals.overall.total_cost = totals.claude.total_cost
        + totals.codex.total_cost
        + totals.copilot.total_cost
        + totals.gemini.total_cost
        + totals.grok.total_cost
        + totals.opencode.total_cost
        + totals.cursor.total_cost
        + totals.hermes.total_cost;
    totals
}

fn accumulate_provider_tokens(stats: &mut ProviderStats, usage: &crate::models::UsageResult) {
    stats.total_tokens = usage
        .values()
        .map(crate::utils::extract_token_counts)
        .map(|counts| counts.total)
        .sum();
}

fn provider_cost(
    usage_data: &UsageData,
    provider: crate::models::ExtensionType,
    pricing_map: &crate::pricing::ModelPricingMap,
) -> f64 {
    let usage = match provider {
        crate::models::ExtensionType::ClaudeCode => &usage_data.per_provider.claude,
        crate::models::ExtensionType::Codex => &usage_data.per_provider.codex,
        crate::models::ExtensionType::Copilot => &usage_data.per_provider.copilot,
        crate::models::ExtensionType::Gemini => &usage_data.per_provider.gemini,
        crate::models::ExtensionType::Grok => &usage_data.per_provider.grok,
        crate::models::ExtensionType::OpenCode => &usage_data.per_provider.opencode,
        crate::models::ExtensionType::Cursor => &usage_data.per_provider.cursor,
        crate::models::ExtensionType::Hermes => &usage_data.per_provider.hermes,
    };
    usage
        .keys()
        .filter_map(|model| {
            usage_data
                .price_provider_model(provider, model, pricing_map)
                .map(|(cost, _)| cost)
        })
        .sum()
}

/// Build provider total rows for display.
pub fn build_provider_total_rows(
    totals: &UsageProviderTotals,
) -> Vec<ProviderTotal<'_, ProviderStats>> {
    let mut rows = Vec::with_capacity(9); // max 8 providers + overall

    if provider_stats_have_activity(&totals.claude) {
        rows.push(ProviderTotal::new(
            Provider::ClaudeCode,
            &totals.claude,
            false,
        ));
    }

    if provider_stats_have_activity(&totals.codex) {
        rows.push(ProviderTotal::new(Provider::Codex, &totals.codex, false));
    }

    if provider_stats_have_activity(&totals.copilot) {
        rows.push(ProviderTotal::new(
            Provider::Copilot,
            &totals.copilot,
            false,
        ));
    }

    if provider_stats_have_activity(&totals.gemini) {
        rows.push(ProviderTotal::new(Provider::Gemini, &totals.gemini, false));
    }

    if provider_stats_have_activity(&totals.grok) {
        rows.push(ProviderTotal::new(Provider::Grok, &totals.grok, false));
    }

    if provider_stats_have_activity(&totals.opencode) {
        rows.push(ProviderTotal::new(
            Provider::OpenCode,
            &totals.opencode,
            false,
        ));
    }

    if provider_stats_have_activity(&totals.cursor) {
        rows.push(ProviderTotal::new(Provider::Cursor, &totals.cursor, false));
    }

    if provider_stats_have_activity(&totals.hermes) {
        rows.push(ProviderTotal::new(Provider::Hermes, &totals.hermes, false));
    }

    if provider_stats_have_activity(&totals.overall) || rows.is_empty() {
        rows.push(ProviderTotal::new_overall(&totals.overall));
    }

    rows
}

fn provider_stats_have_activity(stats: &ProviderStats) -> bool {
    stats.days_count > 0 || stats.total_tokens != 0 || stats.total_cost != 0.0
}

/// Builds the fully priced summary from request-level accounting facts.
pub fn build_usage_summary(
    usage_data: &UsageData,
    pricing_map: &crate::pricing::ModelPricingMap,
) -> UsageSummary {
    if usage_data.models.is_empty() {
        return UsageSummary::default();
    }

    let mut summary = UsageSummary::default();
    summary.rows.reserve(usage_data.models.len());
    for (model, usage) in &usage_data.models {
        let (cost, matched_model) = usage_data
            .price_merged_model(model, pricing_map)
            .unwrap_or_else(|| price_usage(model, usage, pricing_map, CostSource::Litellm));
        summary
            .rows
            .push(build_usage_row(model, usage, cost, matched_model));
    }
    summary.rows.sort_by(|a, b| {
        a.cost
            .partial_cmp(&b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model.cmp(&b.model))
    });
    for row in &summary.rows {
        summary.totals.accumulate(row);
    }
    summary.provider_totals = calculate_provider_totals_from_usage_data(usage_data, pricing_map);
    summary
}

/// Compatibility alias for [`build_usage_summary`].
pub fn build_usage_summary_from_data(
    usage_data: &UsageData,
    pricing_map: &crate::pricing::ModelPricingMap,
) -> UsageSummary {
    build_usage_summary(usage_data, pricing_map)
}

/// Prices one raw usage value under `source`.
fn price_usage(
    model: &str,
    usage: &Value,
    pricing_map: &crate::pricing::ModelPricingMap,
    source: CostSource,
) -> (f64, Option<String>) {
    use crate::usage::resolve_model_cost;
    use crate::utils::extract_token_counts;

    let counts = extract_token_counts(usage);
    resolve_model_cost(model, &counts, pricing_map, source)
}

/// Builds one display row using an already-resolved cost.
fn build_usage_row(
    model: &str,
    usage: &Value,
    cost: f64,
    matched_model: Option<String>,
) -> UsageRow {
    use crate::utils::extract_token_counts;

    let counts = extract_token_counts(usage);

    // Use Cow<str> for display_model to avoid allocation when no annotation
    let display_model = if let Some(matched) = &matched_model {
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

/// Returns the base model key used to merge rows across provider-routing
/// prefixes: everything after the first `/`, or the whole name when there is no
/// `/`. Unlike [`normalize_model_name`](crate::pricing::normalize_model_name)
/// this does **not** strip version/date suffixes, so `gpt-5.5` and `gpt-5.4`
/// stay distinct.
fn base_model_key(model: &str) -> &str {
    model.split_once('/').map(|(_, rest)| rest).unwrap_or(model)
}

/// Collapses rows by [`base_model_key`], summing every token bucket and the
/// already-resolved cost, and shows every row under its bare base name.
///
/// Costs are summed verbatim: each input row was already priced against its
/// full model name, so a merged `gpt-5.5` correctly adds the differently-priced
/// `openai/gpt-5.5`, `azure/gpt-5.5`, and bare `gpt-5.5` pieces — re-pricing the
/// merged token bucket under a single name would be wrong. Every output row is
/// labeled with just the base name (the provider prefix is dropped even when a
/// model has no duplicate, e.g. `opencode/big-pickle` -> `big-pickle`) with no
/// count suffix, so the merged view reads uniformly. The result is re-sorted by
/// ascending cost, tie-broken by model name, matching [`build_usage_summary`].
pub fn merge_rows_by_base_model(rows: &[UsageRow]) -> Vec<UsageRow> {
    use std::collections::HashMap;

    let mut groups: HashMap<&str, Vec<&UsageRow>> = HashMap::new();
    for row in rows {
        groups
            .entry(base_model_key(&row.model))
            .or_default()
            .push(row);
    }

    let mut merged: Vec<UsageRow> = Vec::with_capacity(groups.len());
    for (key, members) in groups {
        let mut acc = UsageRow {
            model: key.to_string(),
            display_model: key.to_string(),
            ..UsageRow::default()
        };
        for m in members {
            acc.input_tokens += m.input_tokens;
            acc.output_tokens += m.output_tokens;
            acc.reasoning_tokens += m.reasoning_tokens;
            acc.cache_read += m.cache_read;
            acc.cache_creation += m.cache_creation;
            acc.total += m.total;
            acc.cost += m.cost;
        }
        merged.push(acc);
    }

    // Same ordering as build_usage_summary so the merged view reads identically.
    merged.sort_by(|a, b| {
        a.cost
            .partial_cmp(&b.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model.cmp(&b.model))
    });
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_rows_keep_activity_without_a_timestamped_day() {
        let mut totals = UsageProviderTotals::default();
        totals.grok.total_tokens = 42;
        totals.overall.total_tokens = 42;

        let rows = build_provider_total_rows(&totals);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "Grok");
        assert_eq!(rows[1].label, "All Providers");
    }

    fn row(model: &str, input: i64, total: i64, cost: f64) -> UsageRow {
        UsageRow {
            model: model.to_string(),
            display_model: model.to_string(),
            input_tokens: input,
            total,
            cost,
            ..UsageRow::default()
        }
    }

    #[test]
    fn merge_collapses_prefixed_and_bare_names_and_sums() {
        let rows = vec![
            row("openai/gpt-5.5", 100, 100, 0.20),
            row("azure/gpt-5.5", 200, 200, 3.00),
            row("gpt-5.5", 300, 300, 5.00),
        ];

        let merged = merge_rows_by_base_model(&rows);

        assert_eq!(merged.len(), 1);
        let m = &merged[0];
        assert_eq!(m.model, "gpt-5.5");
        assert_eq!(m.display_model, "gpt-5.5");
        assert_eq!(m.input_tokens, 600);
        assert_eq!(m.total, 600);
        assert!((m.cost - 8.20).abs() < 1e-9);
    }

    #[test]
    fn merge_keeps_different_versions_apart() {
        // gpt-5.5 has two provider variants (they merge); gpt-5.4 has one. The
        // key check: the 5.4 tokens never fold into the 5.5 row.
        let rows = vec![
            row("openai/gpt-5.5", 10, 10, 1.0),
            row("azure/gpt-5.5", 30, 30, 3.0),
            row("openai/gpt-5.4", 20, 20, 2.0),
        ];

        let merged = merge_rows_by_base_model(&rows);

        assert_eq!(merged.len(), 2);
        // The two gpt-5.5 rows collapse to one base row; gpt-5.4 stays separate.
        let five_five = merged.iter().find(|r| r.model == "gpt-5.5").unwrap();
        assert_eq!(five_five.display_model, "gpt-5.5");
        assert_eq!(five_five.total, 40);
        // The lone 5.4 also shows under its bare base name, keeping its tokens.
        let five_four = merged.iter().find(|r| r.model == "gpt-5.4").unwrap();
        assert_eq!(five_four.display_model, "gpt-5.4");
        assert_eq!(five_four.total, 20);
    }

    #[test]
    fn merge_strips_prefix_from_single_row() {
        // Even a model with no duplicate drops its provider prefix (and any
        // fuzzy-match hint) so the merged view reads uniformly.
        let mut only = row("deepseek/deepseek-v4-pro", 5, 5, 1.5);
        only.display_model = "deepseek/deepseek-v4-pro (deepseek-v4)".to_string();

        let merged = merge_rows_by_base_model(std::slice::from_ref(&only));

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].model, "deepseek-v4-pro");
        assert_eq!(merged[0].display_model, "deepseek-v4-pro");
        assert_eq!(merged[0].total, 5);
    }

    #[test]
    fn merge_only_strips_first_slash_segment() {
        assert_eq!(base_model_key("a/b/c"), "b/c");
        assert_eq!(base_model_key("gpt-5.5"), "gpt-5.5");
        assert_eq!(base_model_key("openai/gpt-5.5"), "gpt-5.5");
    }

    #[test]
    fn merge_reorders_by_ascending_cost() {
        let rows = vec![
            row("openai/gpt-5.5", 1, 1, 9.0),
            row("azure/gpt-5.5", 1, 1, 9.0),
            row("cheap-model", 1, 1, 0.01),
        ];

        let merged = merge_rows_by_base_model(&rows);

        assert_eq!(merged.len(), 2);
        // cheap-model (0.01) sorts before the merged gpt-5.5 row (18.0).
        assert_eq!(merged[0].model, "cheap-model");
        assert_eq!(merged[1].model, "gpt-5.5");
        assert!((merged[1].cost - 18.0).abs() < 1e-9);
    }
}
