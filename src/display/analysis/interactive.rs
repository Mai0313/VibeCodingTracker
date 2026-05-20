use crate::analysis::{AnalysisData, PerProviderAnalysisRows};
use crate::display::analysis::averages::{
    AnalysisRow, build_analysis_provider_rows,
    calculate_analysis_provider_totals_from_per_provider, convert_to_analysis_rows,
};
use crate::display::common::table::{
    create_controls, create_provider_row, create_ratatui_table, create_star_hint, create_summary,
    create_title,
};
use crate::display::common::tui::{
    InputAction, RefreshState, UpdateTracker, handle_input, restore_terminal, setup_terminal,
};
use crate::utils::format_number;
use ratatui::{
    layout::{Constraint, Direction, Layout as RatatuiLayout},
    style::{Color as RatatuiColor, Style, Stylize},
    widgets::Row as RatatuiRow,
};
use sysinfo::System;

const ANALYSIS_REFRESH_SECS: u64 = 10;
const MAX_TRACKED_ANALYSIS_ROWS: usize = 100;

/// Display analysis data as an interactive table
pub fn display_analysis_interactive(
    initial_data: &AnalysisData,
    time_range: crate::cli::TimeRange,
) -> anyhow::Result<()> {
    if initial_data.rows.is_empty() {
        println!("No analysis data found");
        return Ok(());
    }

    // Setup terminal
    let mut terminal = setup_terminal()?;
    let mut refresh_state = RefreshState::new(ANALYSIS_REFRESH_SECS);

    // Initialize system for memory monitoring. We only read our own process
    // stats, so start from an empty `System` to avoid loading the machine's
    // entire process table, disks, and network adapters.
    let pid =
        sysinfo::get_current_pid().expect("Failed to get current process ID for memory monitoring");
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);

    // Track updates
    let mut update_tracker = UpdateTracker::new(MAX_TRACKED_ANALYSIS_ROWS, 1000);

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

        // Refresh only our own process; `remove_dead_processes: true` keeps
        // the `System` from accumulating state for PIDs that come and go on
        // the host over long TUI sessions.
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);

        // Fetch fresh data with error logging
        let current_data = match crate::analysis::aggregate_sessions_by_model(time_range) {
            Ok(data) => data,
            Err(e) => {
                log::warn!("Failed to analyze sessions: {}", e);
                AnalysisData {
                    rows: Vec::new(),
                    per_provider: PerProviderAnalysisRows::default(),
                    provider_days: Default::default(),
                }
            }
        };

        // Calculate totals and extract display data
        let mut totals = AnalysisRow::default();
        let rows_data = convert_to_analysis_rows(&current_data.rows);
        let provider_days = current_data.provider_days.clone();
        let per_provider = current_data.per_provider.clone();

        // Drop current_data immediately after conversion to free memory
        drop(current_data);

        // `aggregate_sessions_by_model` now bypasses the file cache for
        // aggregated metrics (runs each file in `ParseMode::UsageOnly` and
        // drops immediately), so there is nothing useful to clear here.

        // Track updates
        for row in &rows_data {
            let row_key = row.model.clone();
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

            update_tracker.track_update(row_key, &current_tuple);

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
        let current_row_keys: Vec<String> = rows_data.iter().map(|row| row.model.clone()).collect();
        update_tracker.cleanup(current_row_keys);

        // Compute per-provider totals directly from the per-provider
        // aggregated rows produced by the batch analyzer, so Copilot sessions
        // cannot be mis-attributed to Claude Code based on their (now real)
        // model name.
        let provider_totals =
            calculate_analysis_provider_totals_from_per_provider(&per_provider, &provider_days);
        let provider_rows = build_analysis_provider_rows(&provider_totals);

        // Render
        terminal.draw(|f| {
            let totals_height = (provider_rows.len() as u16).saturating_add(4).max(4);
            let chunks = RatatuiLayout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),             // Title
                    Constraint::Min(10),               // Table
                    Constraint::Length(totals_height), // Per-provider totals
                    Constraint::Length(3),             // Summary
                    Constraint::Length(2),             // Controls
                    Constraint::Length(1),             // Star Hint
                ])
                .split(f.area());

            // Title
            let title = create_title("Analysis Statistics", RatatuiColor::Cyan);
            f.render_widget(title, chunks[0]);

            // Table
            let header = vec![
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
                    let row_key = row.model.clone();

                    // Check if this row was recently updated
                    let is_recently_updated = update_tracker.is_recently_updated(&row_key);

                    let style = if is_recently_updated {
                        Style::default().bg(RatatuiColor::Rgb(60, 80, 60)).bold()
                    } else {
                        Style::default()
                    };

                    RatatuiRow::new(vec![
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

            // Per-provider totals table
            let mut totals_rows: Vec<RatatuiRow> = provider_rows
                .iter()
                .map(|row| {
                    create_provider_row(
                        vec![
                            row.label.to_string(),
                            format_number(row.stats.total_edit_lines as i64),
                            format_number(row.stats.total_read_lines as i64),
                            format_number(row.stats.total_write_lines as i64),
                            format_number(row.stats.total_bash_count as i64),
                            format_number(row.stats.total_edit_count as i64),
                            format_number(row.stats.total_read_count as i64),
                            format_number(row.stats.total_todo_write_count as i64),
                            format_number(row.stats.total_write_count as i64),
                            format_number(row.stats.days_count as i64),
                        ],
                        row.tui_color,
                        row.emphasize,
                    )
                })
                .collect();

            if totals_rows.is_empty() {
                totals_rows.push(
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

            let totals_header = vec![
                "Provider",
                "Edit Lines",
                "Read Lines",
                "Write Lines",
                "Bash",
                "Edit",
                "Read",
                "TodoWrite",
                "Write",
                "Days",
            ];

            let totals_widths = [
                Constraint::Min(15),    // Provider
                Constraint::Length(11), // Edit Lines
                Constraint::Length(11), // Read Lines
                Constraint::Length(12), // Write Lines
                Constraint::Length(8),  // Bash
                Constraint::Length(8),  // Edit
                Constraint::Length(8),  // Read
                Constraint::Length(11), // TodoWrite
                Constraint::Length(8),  // Write
                Constraint::Length(8),  // Days
            ];

            let totals_table = create_ratatui_table(
                totals_rows,
                totals_header,
                &totals_widths,
                RatatuiColor::Magenta,
            );
            f.render_widget(totals_table, chunks[2]);

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
                    "Total Lines:",
                    total_lines_str.as_str(),
                    RatatuiColor::Yellow,
                ),
                ("Total Tools:", total_tools_str.as_str(), RatatuiColor::Cyan),
                ("Models:", entries_str.as_str(), RatatuiColor::Blue),
            ];

            let summary = create_summary(summary_items, &sys, pid);
            f.render_widget(summary, chunks[3]);

            // Controls
            let controls = create_controls();
            f.render_widget(controls, chunks[4]);

            // Star Hint
            let star_hint = create_star_hint();
            f.render_widget(star_hint, chunks[5]);
        })?;

        // Drop heavy data structures after rendering to free memory immediately
        drop(rows_data);
        drop(provider_rows);

        // Return arena free lists to the OS — see `release_freed_heap` docs.
        crate::utils::release_freed_heap();

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
