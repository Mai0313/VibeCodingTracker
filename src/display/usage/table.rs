use crate::display::common::table::{
    add_totals_row, create_comfy_table, create_metric_cell, create_provider_cell,
};
use crate::display::usage::averages::{build_provider_total_rows, build_usage_summary};
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use crate::usage::UsageData;
use crate::utils::format_number;
use comfy_table::{Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use owo_colors::OwoColorize;
use std::collections::HashMap;

/// Displays token usage data as a static table
pub fn display_usage_table(usage_data: &UsageData) {
    if usage_data.models.is_empty() {
        println!("No usage data found in Claude Code, Codex, Copilot, or Gemini sessions");
        return;
    }

    println!("{}", "Token Usage Statistics".bright_cyan().bold());
    println!();

    // Fetch pricing data
    let pricing_map = match fetch_model_pricing() {
        Ok(map) => map,
        Err(e) => {
            eprintln!("Warning: Failed to fetch pricing data: {}", e);
            eprintln!("Costs will be shown as $0.00");
            ModelPricingMap::new(HashMap::new())
        }
    };

    let summary = build_usage_summary(
        &usage_data.models,
        &usage_data.per_provider,
        &usage_data.provider_days,
        &pricing_map,
    );

    if summary.rows.is_empty() {
        println!("No usage data found in Claude Code, Codex, Copilot, or Gemini sessions");
        return;
    }

    let rows = &summary.rows;
    let totals = &summary.totals;

    // Create table
    let mut table = create_comfy_table(
        vec![
            "Model",
            "Input",
            "Output",
            "Cache Read",
            "Cache Creation",
            "Total Tokens",
            "Cost (USD)",
        ],
        Color::Yellow,
    );

    // Add data rows. The "Output" column folds `reasoning_tokens` back
    // into the displayed number so each row still adds up to `Total`
    // — costs are already calculated against the separated buckets via
    // `calculate_cost`.
    for row in rows {
        table.add_row(vec![
            Cell::new(&row.display_model)
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

    // Add totals row
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

    println!("{table}");
    println!();

    // Display per-provider totals (the active-day count comes along so
    // readers can see how many days these totals span without converting
    // back to a daily average).
    let provider_rows = build_provider_total_rows(&summary.provider_totals);

    println!("{}", "Totals (by Provider)".bright_magenta().bold());
    println!();

    let mut totals_table = Table::new();
    totals_table.load_preset(UTF8_FULL).set_header(vec![
        Cell::new("Provider")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Left),
        Cell::new("Tokens")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Cost")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Active Days")
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
        let days_cell = create_metric_cell(
            format_number(row.stats.days_count as i64),
            row.table_color,
            row.emphasize,
        );

        totals_table.add_row(vec![name_cell, tokens_cell, cost_cell, days_cell]);
    }

    println!("{totals_table}");
    println!();
}
