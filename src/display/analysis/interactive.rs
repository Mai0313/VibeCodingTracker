use crate::analysis::AggregatedAnalysisRow;
use crate::display::analysis::averages::{
    AnalysisRow, build_analysis_provider_rows, calculate_analysis_daily_averages,
    convert_to_analysis_rows, format_lines_per_day,
};
use crate::display::common::table::{
    create_controls, create_provider_row, create_ratatui_table, create_summary, create_title,
};
use crate::display::common::tui::{
    InputAction, RefreshState, UpdateTracker, handle_input, restore_terminal, setup_terminal,
};
use crate::utils::{format_number, get_current_date};
use ratatui::{
    layout::{Constraint, Direction, Layout as RatatuiLayout},
    style::{Color as RatatuiColor, Style, Stylize},
    widgets::Row as RatatuiRow,
};
use sysinfo::System;

const ANALYSIS_REFRESH_SECS: u64 = 10;
const MAX_TRACKED_ANALYSIS_ROWS: usize = 100;

// Type alias for analysis data snapshot: (edit_lines, read_lines, write_lines, bash_count, edit_count, read_count, todo_write_count, write_count)
type AnalysisDataSnapshot = (usize, usize, usize, usize, usize, usize, usize, usize);

/// Display analysis data as an interactive table
pub fn display_analysis_interactive(data: &[AggregatedAnalysisRow]) -> anyhow::Result<()> {
    if data.is_empty() {
        println!("⚠️  No analysis data found");
        return Ok(());
    }

    // Setup terminal
    let mut terminal = setup_terminal()?;
    let mut refresh_state = RefreshState::new(ANALYSIS_REFRESH_SECS);

    // Initialize system for memory monitoring
    let mut sys = System::new_all();
    let pid =
        sysinfo::get_current_pid().expect("Failed to get current process ID for memory monitoring");

    // Track updates
    let mut update_tracker: UpdateTracker<AnalysisDataSnapshot> =
        UpdateTracker::new(MAX_TRACKED_ANALYSIS_ROWS, 1000);
    let mut current_data = data.to_vec();

    loop {
        if !refresh_state.should_refresh() {
            match handle_input()? {
                InputAction::Quit => break,
                InputAction::Refresh => refresh_state.force(),
                InputAction::Continue => continue,
            }
            continue;
        }

        refresh_state.mark_refreshed();

        // Update system information
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
        sys.refresh_cpu_all();

        // Fetch fresh data with error logging
        match crate::analysis::analyze_all_sessions() {
            Ok(data) => {
                current_data = data;
            }
            Err(e) => {
                log::warn!("Failed to analyze sessions: {}", e);
            }
        }

        // Calculate totals
        let mut totals = AnalysisRow::default();
        let rows_data = convert_to_analysis_rows(&current_data);

        let today = get_current_date();

        // Track updates
        for row in &rows_data {
            let row_key = format!("{}:{}", row.date, row.model);
            let current_tuple = (
                row.edit_lines,
                row.read_lines,
                row.write_lines,
                row.bash_count,
                row.edit_count,
                row.read_count,
                row.todo_write_count,
                row.write_count,
            );

            update_tracker.track_update(row_key, current_tuple);

            totals.edit_lines += row.edit_lines;
            totals.read_lines += row.read_lines;
            totals.write_lines += row.write_lines;
            totals.bash_count += row.bash_count;
            totals.edit_count += row.edit_count;
            totals.read_count += row.read_count;
            totals.todo_write_count += row.todo_write_count;
            totals.write_count += row.write_count;
        }

        // Cleanup old entries
        let current_row_keys: Vec<String> = rows_data
            .iter()
            .map(|row| format!("{}:{}", row.date, row.model))
            .collect();
        update_tracker.cleanup(current_row_keys);

        // Calculate daily averages
        let daily_averages = calculate_analysis_daily_averages(&rows_data);
        let provider_rows = build_analysis_provider_rows(&daily_averages);

        // Render
        terminal.draw(|f| {
            let avg_height = (provider_rows.len() as u16).saturating_add(4).max(4);
            let chunks = RatatuiLayout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),          // Title
                    Constraint::Min(10),            // Table
                    Constraint::Length(avg_height), // Daily Averages
                    Constraint::Length(3),          // Summary
                    Constraint::Length(2),          // Controls
                ])
                .split(f.area());

            // Title
            let title = create_title("Analysis Statistics", "🔍", RatatuiColor::Cyan);
            f.render_widget(title, chunks[0]);

            // Table
            let header = vec![
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
            ];

            let mut rows: Vec<RatatuiRow> = rows_data
                .iter()
                .map(|row| {
                    let row_key = format!("{}:{}", row.date, row.model);

                    // Check if this row was recently updated
                    let is_recently_updated = update_tracker.is_recently_updated(&row_key);

                    let style = if is_recently_updated {
                        Style::default().bg(RatatuiColor::Rgb(60, 80, 60)).bold()
                    } else if row.date == today {
                        Style::default().bg(RatatuiColor::Rgb(32, 32, 32))
                    } else {
                        Style::default()
                    };

                    RatatuiRow::new(vec![
                        row.date.clone(),
                        row.model.clone(),
                        format_number(row.edit_lines),
                        format_number(row.read_lines),
                        format_number(row.write_lines),
                        format_number(row.bash_count),
                        format_number(row.edit_count),
                        format_number(row.read_count),
                        format_number(row.todo_write_count),
                        format_number(row.write_count),
                    ])
                    .style(style)
                })
                .collect();

            // Add totals row
            rows.push(
                RatatuiRow::new(vec![
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
                ])
                .style(
                    Style::default()
                        .fg(RatatuiColor::Yellow)
                        .bold()
                        .bg(RatatuiColor::DarkGray),
                ),
            );

            let widths = [
                Constraint::Length(12), // Date
                Constraint::Min(20),    // Model
                Constraint::Length(12), // Edit Lines
                Constraint::Length(12), // Read Lines
                Constraint::Length(12), // Write Lines
                Constraint::Length(8),  // Bash
                Constraint::Length(8),  // Edit
                Constraint::Length(8),  // Read
                Constraint::Length(12), // TodoWrite
                Constraint::Length(8),  // Write
            ];

            let table = create_ratatui_table(rows, header, &widths, RatatuiColor::Green);
            f.render_widget(table, chunks[1]);

            // Daily Averages Table
            let mut avg_rows: Vec<RatatuiRow> = provider_rows
                .iter()
                .map(|row| {
                    create_provider_row(
                        vec![
                            format!("{} {}", row.icon, row.label),
                            format_lines_per_day(row.stats.avg_edit_lines()),
                            format_lines_per_day(row.stats.avg_read_lines()),
                            format_lines_per_day(row.stats.avg_write_lines()),
                            format!("{:.1}", row.stats.avg_bash_count()),
                            format!("{:.1}", row.stats.avg_edit_count()),
                            format!("{:.1}", row.stats.avg_read_count()),
                            format!("{:.1}", row.stats.avg_todo_write_count()),
                            format!("{:.1}", row.stats.avg_write_count()),
                            format_number(row.stats.days_count as i64),
                        ],
                        row.tui_color,
                        row.emphasize,
                    )
                })
                .collect();

            if avg_rows.is_empty() {
                avg_rows.push(
                    RatatuiRow::new(vec![
                        "No provider data yet".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                        "-".to_string(),
                    ])
                    .style(Style::default().fg(RatatuiColor::DarkGray)),
                );
            }

            let avg_header = vec![
                "Provider",
                "EditL/Day",
                "ReadL/Day",
                "WriteL/Day",
                "Bash/Day",
                "Edit/Day",
                "Read/Day",
                "Todo/Day",
                "Write/Day",
                "Days",
            ];

            let avg_widths = [
                Constraint::Min(15),    // Provider
                Constraint::Length(10), // Edit/Day
                Constraint::Length(10), // Read/Day
                Constraint::Length(10), // Write/Day
                Constraint::Length(10), // Bash/Day
                Constraint::Length(10), // Edit/Day
                Constraint::Length(10), // Read/Day
                Constraint::Length(10), // Todo/Day
                Constraint::Length(10), // Write/Day
                Constraint::Length(8),  // Days
            ];

            let average_table =
                create_ratatui_table(avg_rows, avg_header, &avg_widths, RatatuiColor::Magenta);
            f.render_widget(average_table, chunks[2]);

            // Summary
            let total_lines_str =
                format_number(totals.edit_lines + totals.read_lines + totals.write_lines);
            let total_tools_str = format_number(
                totals.bash_count
                    + totals.edit_count
                    + totals.read_count
                    + totals.todo_write_count
                    + totals.write_count,
            );
            let entries_str = format!("{}", rows_data.len());

            let summary_items = vec![
                (
                    "📝 Total Lines:",
                    total_lines_str.as_str(),
                    RatatuiColor::Yellow,
                ),
                (
                    "🔧 Total Tools:",
                    total_tools_str.as_str(),
                    RatatuiColor::Cyan,
                ),
                ("📅 Entries:", entries_str.as_str(), RatatuiColor::Blue),
            ];

            let summary = create_summary(summary_items, &sys, pid);
            f.render_widget(summary, chunks[3]);

            // Controls
            let controls = create_controls();
            f.render_widget(controls, chunks[4]);
        })?;

        // Handle input with timeout
        match handle_input()? {
            InputAction::Quit => break,
            InputAction::Refresh => refresh_state.force(),
            InputAction::Continue => {}
        }
    }

    // Restore terminal
    restore_terminal(&mut terminal)?;
    Ok(())
}
