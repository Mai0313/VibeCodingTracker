//! Static-table renderer for the usage view (per-model table + per-provider footer).

use crate::display::common::table::{
    add_totals_row, create_comfy_table, create_metric_cell, create_provider_cell,
};
use crate::display::usage::averages::{
    build_provider_total_rows, build_usage_summary, merge_rows_by_base_model,
};
use comfy_table::{Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use owo_colors::OwoColorize;
use std::collections::HashMap;
use std::io::{self, Write};
use vct_core::pricing::{ModelPricingMap, fetch_model_pricing};
use vct_core::usage::UsageData;
use vct_core::utils::format_number;

/// Prints `usage_data` to stdout as a colored per-model table and a per-provider totals footer.
///
/// The Output column includes `reasoning_tokens` to match Total Tokens, while cost uses the separated buckets. Prints a no-data notice when empty. On pricing fetch failure, warns on stderr and prices LiteLLM-backed models at `$0.00` while keeping provider-stored costs. When `merge` is `true`, collapses rows sharing a base model name across provider prefixes (for example, `openai/gpt-5.5` and `azure/gpt-5.5`).
pub fn display_usage_table(usage_data: &UsageData, merge: bool) {
    if usage_data.models.is_empty() {
        println!("No usage data found in enabled provider sessions");
        return;
    }

    let (pricing_map, pricing_error) = match fetch_model_pricing() {
        Ok(map) => (map, None),
        Err(e) => {
            log::warn!("failed to fetch pricing data: {e}; costs shown as $0.00");
            (ModelPricingMap::new(HashMap::new()), Some(e.to_string()))
        }
    };

    display_usage_table_with_pricing(usage_data, &pricing_map, pricing_error.as_deref(), merge);
}

/// Prints `usage_data` to stdout as a colored per-model table and per-provider totals footer using a pre-fetched `pricing_map`.
///
/// When `pricing_error` is `Some` and `usage_data` is non-empty, warns on stderr that costs are shown as `$0.00`. See [`display_usage_table`] for column and `merge` semantics.
pub fn display_usage_table_with_pricing(
    usage_data: &UsageData,
    pricing_map: &ModelPricingMap,
    pricing_error: Option<&str>,
    merge: bool,
) {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let _ = write_usage_table_with_pricing(
        usage_data,
        pricing_map,
        pricing_error,
        merge,
        &mut stdout,
        &mut stderr,
    );
}

fn write_usage_table_with_pricing(
    usage_data: &UsageData,
    pricing_map: &ModelPricingMap,
    pricing_error: Option<&str>,
    merge: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<()> {
    if usage_data.models.is_empty() {
        writeln!(out, "No usage data found in enabled provider sessions")?;
        return Ok(());
    }

    writeln!(out, "{}", "Token Usage Statistics".bright_cyan().bold())?;
    writeln!(out)?;

    if let Some(error) = pricing_error {
        writeln!(err, "Warning: Failed to fetch pricing data: {}", error)?;
        writeln!(err, "Costs will be shown as $0.00")?;
    }

    let mut summary = build_usage_summary(
        &usage_data.models,
        &usage_data.per_provider,
        &usage_data.provider_days,
        pricing_map,
        &usage_data.stored_costs,
    );

    if summary.rows.is_empty() {
        writeln!(out, "No usage data found in enabled provider sessions")?;
        return Ok(());
    }

    // Totals are a row-wise sum, so they are identical merged or not.
    if merge {
        summary.rows = merge_rows_by_base_model(&summary.rows);
    }

    let rows = &summary.rows;
    let totals = &summary.totals;

    let mut table = create_comfy_table(
        vec![
            "Model",
            "Input",
            "Output",
            "Cache Read",
            "Cache Write",
            "Total Tokens",
            "Cost (USD)",
        ],
        Color::Yellow,
    );

    for row in rows {
        table.add_row(vec![
            Cell::new(&row.model)
                .fg(Color::Green)
                .set_alignment(CellAlignment::Left),
            Cell::new(format_number(row.input_tokens))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.output_with_reasoning()))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.cache_read))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.cache_creation))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.total))
                .fg(Color::Magenta)
                .set_alignment(CellAlignment::Right),
            Cell::new(format!("${:.2}", row.cost))
                .fg(Color::Cyan)
                .set_alignment(CellAlignment::Right),
        ]);
    }

    add_totals_row(
        &mut table,
        vec![
            "TOTAL".to_string(),
            format_number(totals.input_tokens),
            format_number(totals.output_with_reasoning()),
            format_number(totals.cache_read),
            format_number(totals.cache_creation),
            format_number(totals.total),
            format!("${:.2}", totals.cost),
        ],
        Color::Red,
    );

    writeln!(out, "{table}")?;
    writeln!(out)?;

    let provider_rows = build_provider_total_rows(&summary.provider_totals);

    writeln!(out, "{}", "Totals (by Provider)".bright_magenta().bold())?;
    writeln!(out)?;

    let mut totals_table = Table::new();
    totals_table.load_style(UTF8_FULL).set_header(vec![
        Cell::new("Provider")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Left),
        Cell::new("Tokens")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Cost")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
    ]);

    for row in &provider_rows {
        let name_cell = create_provider_cell(row.label.to_string(), row.table_color, row.emphasize);
        let tokens_cell = create_metric_cell(
            format_number(row.stats.total_tokens),
            row.table_color,
            row.emphasize,
        );
        let cost_cell = create_metric_cell(
            format!("${:.2}", row.stats.total_cost),
            row.table_color,
            row.emphasize,
        );

        totals_table.add_row(vec![name_cell, tokens_cell, cost_cell]);
    }

    writeln!(out, "{totals_table}")?;
    writeln!(out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vct_core::pricing::ModelPricing;

    fn empty_usage_data() -> UsageData {
        UsageData {
            models: Default::default(),
            per_provider: Default::default(),
            provider_days: Default::default(),
            stored_costs: Default::default(),
        }
    }

    fn sample_usage_data() -> UsageData {
        let mut usage = empty_usage_data();
        usage.models.insert(
            "claude-sonnet-4".to_string(),
            serde_json::json!({
                "input_tokens": 1_000_000,
                "output_tokens": 0,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0
            }),
        );
        usage
    }

    #[test]
    fn empty_usage_skips_pricing_warning() {
        let empty = empty_usage_data();
        let pricing = ModelPricingMap::new(HashMap::new());
        let mut out = Vec::new();
        let mut err = Vec::new();

        write_usage_table_with_pricing(&empty, &pricing, Some("boom"), false, &mut out, &mut err)
            .unwrap();

        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("No usage data found")
        );
        assert!(err.is_empty());
    }

    #[test]
    fn nonempty_usage_reuses_passed_pricing_and_warns_only_on_error() {
        let usage = sample_usage_data();
        let mut map = HashMap::new();
        map.insert(
            "claude-sonnet-4".to_string(),
            ModelPricing {
                input_cost_per_token: 3e-6,
                ..ModelPricing::default()
            },
        );
        let priced = ModelPricingMap::new(map);

        let mut out = Vec::new();
        let mut err = Vec::new();
        write_usage_table_with_pricing(&usage, &priced, None, false, &mut out, &mut err).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("$3.00"));
        assert!(err.is_empty());

        let empty_pricing = ModelPricingMap::new(HashMap::new());
        let mut out = Vec::new();
        let mut err = Vec::new();
        write_usage_table_with_pricing(
            &usage,
            &empty_pricing,
            Some("network down"),
            false,
            &mut out,
            &mut err,
        )
        .unwrap();
        assert!(String::from_utf8(out).unwrap().contains("$0.00"));
        let err = String::from_utf8(err).unwrap();
        assert!(err.contains("Warning: Failed to fetch pricing data: network down"));
        assert!(err.contains("Costs will be shown as $0.00"));
    }
}
