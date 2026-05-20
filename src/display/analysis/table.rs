use crate::analysis::AnalysisData;
use crate::display::analysis::averages::{
    AnalysisRow, build_analysis_provider_rows, calculate_analysis_provider_totals_from_per_provider,
};
use crate::display::common::table::{
    add_totals_row, create_comfy_table, create_metric_cell, create_provider_cell,
};
use crate::utils::format_number;
use comfy_table::{Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use owo_colors::OwoColorize;

/// Display analysis data as a static table
pub fn display_analysis_table(analysis: &AnalysisData) {
    let data = &analysis.rows;
    if data.is_empty() {
        println!("No analysis data found");
        return;
    }

    println!("{}", "Analysis Statistics".bright_cyan().bold());
    println!();

    let mut table = create_comfy_table(
        vec![
            "Model",
            "Edit Lines",
            "Read Lines",
            "Write Lines",
            "Bash",
            "Edit",
            "Read",
            "TodoWrite",
            "Write",
        ],
        Color::Yellow,
    );

    let mut totals = AnalysisRow::default();

    for row in data {
        table.add_row(vec![
            Cell::new(&row.model)
                .fg(Color::Green)
                .set_alignment(CellAlignment::Left),
            Cell::new(format_number(row.edit_lines))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.read_lines))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.write_lines))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.bash_count))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.edit_count))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.read_count))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.todo_write_count))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
            Cell::new(format_number(row.write_count))
                .fg(Color::White)
                .set_alignment(CellAlignment::Right),
        ]);

        totals.edit_lines += row.edit_lines;
        totals.read_lines += row.read_lines;
        totals.write_lines += row.write_lines;
        totals.bash_count += row.bash_count;
        totals.edit_count += row.edit_count;
        totals.read_count += row.read_count;
        totals.todo_write_count += row.todo_write_count;
        totals.write_count += row.write_count;
    }

    // Add totals row
    add_totals_row(
        &mut table,
        vec![
            "TOTAL".to_string(),
            format_number(totals.edit_lines),
            format_number(totals.read_lines),
            format_number(totals.write_lines),
            format_number(totals.bash_count),
            format_number(totals.edit_count),
            format_number(totals.read_count),
            format_number(totals.todo_write_count),
            format_number(totals.write_count),
        ],
        Color::Red,
    );

    println!("{table}");
    println!();

    // Compute per-provider totals directly from the per-provider aggregated
    // rows the batch analyzer produced (no model-name guessing — each row
    // is already scoped to a known source directory).
    let provider_totals = calculate_analysis_provider_totals_from_per_provider(
        &analysis.per_provider,
        &analysis.provider_days,
    );
    let provider_rows = build_analysis_provider_rows(&provider_totals);

    println!("{}", "Totals (by Provider)".bright_magenta().bold());
    println!();

    let mut totals_table = Table::new();
    totals_table.load_preset(UTF8_FULL).set_header(vec![
        Cell::new("Provider")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Left),
        Cell::new("Edit Lines")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Read Lines")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Write Lines")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Bash")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Edit")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Read")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("TodoWrite")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Write")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Days")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
    ]);

    for row in &provider_rows {
        totals_table.add_row(vec![
            create_provider_cell(row.label.to_string(), row.table_color, row.emphasize),
            create_metric_cell(
                format_number(row.stats.total_edit_lines as i64),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format_number(row.stats.total_read_lines as i64),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format_number(row.stats.total_write_lines as i64),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format_number(row.stats.total_bash_count as i64),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format_number(row.stats.total_edit_count as i64),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format_number(row.stats.total_read_count as i64),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format_number(row.stats.total_todo_write_count as i64),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format_number(row.stats.total_write_count as i64),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format_number(row.stats.days_count as i64),
                row.table_color,
                row.emphasize,
            ),
        ]);
    }

    println!("{totals_table}");
    println!();
}
