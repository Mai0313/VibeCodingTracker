use crate::display::common::table::{
    add_totals_row, create_comfy_table, create_metric_cell, create_provider_cell,
};
use crate::display::usage::averages::{
    build_provider_average_rows, build_usage_summary, format_tokens_per_day,
};
use crate::models::DateUsageResult;
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use crate::utils::format_number;
use comfy_table::{Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use owo_colors::OwoColorize;
use std::collections::HashMap;

/// Displays token usage data as a static table
pub fn display_usage_table(usage_data: &DateUsageResult) {
    if usage_data.is_empty() {
        println!("⚠️  No usage data found in Claude Code or Codex sessions");
        return;
    }

    println!("{}", "📊 Token Usage Statistics".bright_cyan().bold());
    println!();

    // Fetch pricing data
    let pricing_map = match fetch_model_pricing() {
        Ok(map) => map,
        Err(e) => {
            eprintln!("⚠️  Warning: Failed to fetch pricing data: {}", e);
            eprintln!("   Costs will be shown as $0.00");
            ModelPricingMap::new(HashMap::new())
        }
    };

    // Note: Removed pricing_cache - ModelPricingMap uses global MATCH_CACHE internally
    let summary = build_usage_summary(usage_data, &pricing_map);

    if summary.rows.is_empty() {
        println!("⚠️  No usage data found in Claude Code or Codex sessions");
        return;
    }

    let rows = &summary.rows;
    let totals = &summary.totals;

    // Create table
    let mut table = create_comfy_table(
        vec![
            "Date",
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

    // Add data rows
    for row in rows {
        table.add_row(vec![
            Cell::new(&row.date)
                .fg(Color::Cyan)
                .set_alignment(CellAlignment::Left),
            Cell::new(&row.display_model)
                .fg(Color::Green)
                .set_alignment(CellAlignment::Left),
            Cell::new(format_number(row.input_tokens))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.output_tokens))
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
            "".to_string(),
            "TOTAL".to_string(),
            format_number(totals.input_tokens),
            format_number(totals.output_tokens),
            format_number(totals.cache_read),
            format_number(totals.cache_creation),
            format_number(totals.total),
            format!("${:.2}", totals.cost),
        ],
        Color::Red,
    );

    println!("{table}");
    println!();

    // Calculate and display daily averages
    let provider_rows = build_provider_average_rows(&summary.daily_averages);

    println!(
        "{}",
        "📈 Daily Averages (by Provider)".bright_magenta().bold()
    );
    println!();

    let mut avg_table = Table::new();
    avg_table.load_preset(UTF8_FULL).set_header(vec![
        Cell::new("Provider")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Left),
        Cell::new("Tokens/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Cost/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Active Days")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
    ]);

    for row in &provider_rows {
        let name = format!("{} {}", row.icon, row.label);
        let name_cell = create_provider_cell(name, row.table_color, row.emphasize);
        let tokens_cell = create_metric_cell(
            format_tokens_per_day(row.stats.avg_tokens()),
            row.table_color,
            row.emphasize,
        );
        let cost_cell = create_metric_cell(
            format!("${:.2}", row.stats.avg_cost()),
            row.table_color,
            row.emphasize,
        );
        let days_cell = create_metric_cell(
            format_number(row.stats.days_count as i64),
            row.table_color,
            row.emphasize,
        );

        avg_table.add_row(vec![name_cell, tokens_cell, cost_cell, days_cell]);
    }

    println!("{avg_table}");
    println!();
}
