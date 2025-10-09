use crate::analysis::AggregatedAnalysisRow;
use crate::display::analysis::averages::{
    AnalysisRow, build_analysis_provider_rows, calculate_analysis_daily_averages,
    convert_to_analysis_rows, format_lines_per_day,
};
use crate::display::common::table::{
    add_totals_row, create_comfy_table, create_metric_cell, create_provider_cell,
};
use crate::utils::format_number;
use comfy_table::{Cell, CellAlignment, Color, Table, presets::UTF8_FULL};
use owo_colors::OwoColorize;

/// Display analysis data as a static table
pub fn display_analysis_table(data: &[AggregatedAnalysisRow]) {
    if data.is_empty() {
        println!("⚠️  No analysis data found");
        return;
    }

    println!("{}", "🔍 Analysis Statistics".bright_cyan().bold());
    println!();

    let mut table = create_comfy_table(
        vec![
            "Date",
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
            Cell::new(&row.date)
                .fg(Color::Cyan)
                .set_alignment(CellAlignment::Left),
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
            "".to_string(),
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

    // Calculate and display daily averages
    let rows_for_averages = convert_to_analysis_rows(data);
    let daily_averages = calculate_analysis_daily_averages(&rows_for_averages);
    let provider_rows = build_analysis_provider_rows(&daily_averages);

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
        Cell::new("EditL/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("ReadL/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("WriteL/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Bash/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Edit/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Read/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Todo/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Write/Day")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
        Cell::new("Days")
            .fg(Color::Magenta)
            .set_alignment(CellAlignment::Right),
    ]);

    for row in &provider_rows {
        let name = format!("{} {}", row.icon, row.label);

        avg_table.add_row(vec![
            create_provider_cell(name, row.table_color, row.emphasize),
            create_metric_cell(
                format_lines_per_day(row.stats.avg_edit_lines()),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format_lines_per_day(row.stats.avg_read_lines()),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format_lines_per_day(row.stats.avg_write_lines()),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format!("{:.1}", row.stats.avg_bash_count()),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format!("{:.1}", row.stats.avg_edit_count()),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format!("{:.1}", row.stats.avg_read_count()),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format!("{:.1}", row.stats.avg_todo_write_count()),
                row.table_color,
                row.emphasize,
            ),
            create_metric_cell(
                format!("{:.1}", row.stats.avg_write_count()),
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

    println!("{avg_table}");
    println!();
}
