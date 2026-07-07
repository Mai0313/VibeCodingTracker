//! Plain-text renderer for the usage view: one `model: $cost` line per model.

use crate::display::usage::averages::{build_usage_summary, merge_rows_by_base_model};
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use crate::usage::UsageData;
use std::collections::HashMap;

/// Prints token usage to stdout as one `model: $cost` line per model.
///
/// Rows are ordered by ascending cost. Prints `No usage data found` when there
/// is nothing to show. If pricing cannot be fetched, costs fall back to `$0.00`
/// rather than failing. When `merge` is set, rows sharing a base model name
/// across provider prefixes (e.g. `openai/gpt-5.5` + `azure/gpt-5.5`) are
/// collapsed into one.
pub fn display_usage_text(usage_data: &UsageData, merge: bool) {
    if usage_data.models.is_empty() {
        println!("No usage data found");
        return;
    }

    // Fetch pricing data
    let pricing_map =
        fetch_model_pricing().unwrap_or_else(|_| ModelPricingMap::new(HashMap::new()));

    let mut summary = build_usage_summary(
        &usage_data.models,
        &usage_data.per_provider,
        &usage_data.provider_days,
        &pricing_map,
        &usage_data.opencode_costs,
    );

    if summary.rows.is_empty() {
        println!("No usage data found");
        return;
    }

    if merge {
        summary.rows = merge_rows_by_base_model(&summary.rows);
    }

    for row in &summary.rows {
        println!("{}: ${:.6}", row.display_model, row.cost);
    }
}
