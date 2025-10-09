use crate::display::usage::averages::build_usage_summary;
use crate::models::DateUsageResult;
use crate::pricing::{ModelPricingMap, ModelPricingResult, fetch_model_pricing};
use std::collections::HashMap;

/// Display usage data as plain text
pub fn display_usage_text(usage_data: &DateUsageResult) {
    if usage_data.is_empty() {
        println!("No usage data found");
        return;
    }

    // Fetch pricing data
    let pricing_map =
        fetch_model_pricing().unwrap_or_else(|_| ModelPricingMap::new(HashMap::new()));
    let mut pricing_cache: HashMap<String, ModelPricingResult> = HashMap::new();

    let summary = build_usage_summary(usage_data, &pricing_map, &mut pricing_cache);

    if summary.rows.is_empty() {
        println!("No usage data found");
        return;
    }

    for row in &summary.rows {
        println!("{} > {}: ${:.6}", row.date, row.display_model, row.cost);
    }
}
