//! Plain-text renderer for the usage view: one `model: $cost` line per model.

use crate::display::usage::averages::{build_usage_summary, merge_rows_by_base_model};
use std::collections::HashMap;
use std::io::{self, Write};
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

    display_usage_text_with_pricing(usage_data, &pricing_map, merge);
}

/// Prints `usage_data` to stdout as one `model: $cost` line per model using a pre-fetched `pricing_map`.
///
/// See [`display_usage_text`] for ordering, empty-state, and `merge` semantics.
pub fn display_usage_text_with_pricing(
    usage_data: &UsageData,
    pricing_map: &ModelPricingMap,
    merge: bool,
) {
    let mut stdout = io::stdout().lock();
    let _ = write_usage_text_with_pricing(usage_data, pricing_map, merge, &mut stdout);
}

fn write_usage_text_with_pricing(
    usage_data: &UsageData,
    pricing_map: &ModelPricingMap,
    merge: bool,
    out: &mut impl Write,
) -> io::Result<()> {
    if usage_data.models.is_empty() {
        writeln!(out, "No usage data found")?;
        return Ok(());
    }

    let mut summary = build_usage_summary(
        &usage_data.models,
        &usage_data.per_provider,
        &usage_data.provider_days,
        pricing_map,
        &usage_data.stored_costs,
    );

    if summary.rows.is_empty() {
        writeln!(out, "No usage data found")?;
        return Ok(());
    }

    if merge {
        summary.rows = merge_rows_by_base_model(&summary.rows);
    }

    for row in &summary.rows {
        writeln!(out, "{}: ${:.6}", row.model, row.cost)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vct_core::pricing::ModelPricing;

    #[test]
    fn text_renderer_reuses_passed_pricing_map() {
        let mut usage = UsageData {
            models: Default::default(),
            per_provider: Default::default(),
            provider_days: Default::default(),
            stored_costs: Default::default(),
        };
        usage.models.insert(
            "claude-sonnet-4".to_string(),
            serde_json::json!({
                "input_tokens": 1_000_000,
                "output_tokens": 0,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0
            }),
        );
        let mut map = HashMap::new();
        map.insert(
            "claude-sonnet-4".to_string(),
            ModelPricing {
                input_cost_per_token: 3e-6,
                ..ModelPricing::default()
            },
        );
        let pricing = ModelPricingMap::new(map);

        let mut out = Vec::new();
        write_usage_text_with_pricing(&usage, &pricing, false, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "claude-sonnet-4: $3.000000\n"
        );
    }
}
