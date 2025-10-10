use crate::display::usage::averages::build_usage_summary;
use crate::models::DateUsageResult;
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use std::collections::HashMap;

/// Displays token usage data as plain text (Date > model: cost format)
pub fn display_usage_text(usage_data: &DateUsageResult) {
    if usage_data.is_empty() {
        println!("No usage data found");
        return;
    }

    // Fetch pricing data
    let pricing_map =
        fetch_model_pricing().unwrap_or_else(|_| ModelPricingMap::new(HashMap::new()));

    // Note: Removed pricing_cache - ModelPricingMap uses global MATCH_CACHE internally
    let summary = build_usage_summary(usage_data, &pricing_map);

    if summary.rows.is_empty() {
        println!("No usage data found");
        return;
    }

    for row in &summary.rows {
        println!("{} > {}: ${:.6}", row.date, row.display_model, row.cost);
    }
}
