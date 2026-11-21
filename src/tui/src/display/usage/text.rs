//! Plain-text renderer for the usage view: one `model: $cost` line per model.

use crate::display::usage::averages::{build_usage_summary, merge_rows_by_base_model};
use std::collections::HashMap;
use vct_core::pricing::{ModelPricingMap, fetch_model_pricing};
use vct_core::usage::UsageData;

/// Prints `usage_data` to stdout as one `model: $cost` line per model.
///
/// Rows are ordered by ascending cost. Prints `No usage data found` when empty. Pricing fetch errors are ignored, leaving LiteLLM-backed models at `$0.000000` while keeping provider-stored costs. When `merge` is `true`, collapses rows sharing a base model name across provider prefixes (for example, `openai/gpt-5.5` and `azure/gpt-5.5`).
pub fn display_usage_text(usage_data: &UsageData, merge: bool) {
    if usage_data.models.is_empty() {
        println!("No usage data found");
        return;
    }

    let pricing_map =
        fetch_model_pricing().unwrap_or_else(|_| ModelPricingMap::new(HashMap::new()));

    display_usage_text_with_pricing(usage_data, merge, &pricing_map);
}

/// Prints the plain-text usage report with the supplied model prices.
pub fn display_usage_text_with_pricing(
    usage_data: &UsageData,
    merge: bool,
    pricing_map: &ModelPricingMap,
) {
    let mut summary = build_usage_summary(
        &usage_data.models,
        &usage_data.per_provider,
        &usage_data.provider_days,
        pricing_map,
        &usage_data.stored_costs,
    );

    if summary.rows.is_empty() {
        println!("No usage data found");
        return;
    }

    if merge {
        summary.rows = merge_rows_by_base_model(&summary.rows);
    }

    for row in &summary.rows {
        println!("{}: ${:.6}", row.model, row.cost);
    }
}
