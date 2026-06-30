//! Interactive (TUI) renderer for the `analysis` view.
//!
//! Runs a ratatui draw loop that periodically re-aggregates the session
//! directories, highlighting rows whose metrics changed since the last tick and
//! redrawing on terminal resize without re-aggregating.

use crate::analysis::{AnalysisData, PerProviderAnalysisRows};
use crate::display::analysis::averages::{
    AnalysisProviderTotals, AnalysisRow, build_analysis_provider_rows,
    calculate_analysis_provider_totals_from_per_provider, convert_to_analysis_rows,
};
use crate::display::common::table::{
    create_controls, create_provider_row, create_ratatui_table, create_summary, main_layout,
    render_scrollable_table, render_too_small, styled_row,
};
use crate::display::common::tui::{
    InputAction, RefreshState, ScrollState, UpdateTracker, handle_input, restore_terminal,
    set_mouse_capture, setup_terminal,
};
use crate::utils::format_compact;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::Constraint,
    style::{Color as RatatuiColor, Style, Stylize},
    widgets::Row as RatatuiRow,
};
use std::io;
use sysinfo::{Pid, System};

/// Seconds between automatic re-aggregation refreshes of the table.
const ANALYSIS_REFRESH_SECS: u64 = 10;
/// Upper bound on the number of rows tracked for the "recently updated"
/// highlight, capping the tracker's memory footprint.
const MAX_TRACKED_ANALYSIS_ROWS: usize = 100;

/// Hard minimum terminal width/height; below this only a notice is drawn. The
/// analysis table is wider (9 columns) so it needs more width than usage.
const ANALYSIS_MIN_W: u16 = 84;
const ANALYSIS_MIN_H: u16 = 14;
/// At or above this height the per-provider band is shown; below it the band is
/// dropped so the scrollable table keeps a usable height.
const ANALYSIS_PANELS_MIN_H: u16 = 18;

/// Render the `analysis` view as an interactive, auto-refreshing TUI.
///
/// Takes over the terminal and runs a draw loop until the user quits. Every
/// `ANALYSIS_REFRESH_SECS` it re-aggregates the session directories for
/// `time_range`, highlights rows whose counters changed, and updates the
/// process-memory readout. `initial_data` only gates the empty-state shortcut;
/// the loop always re-fetches its own data. If a refresh fails the error is
/// logged and the loop continues with empty data rather than tearing down the
/// TUI. Returns immediately after printing a message if `initial_data` is empty.
///
/// # Errors
///
/// Returns an error if the terminal cannot be put into / restored from raw
/// alternate-screen mode, or if drawing a frame or polling for input fails.
///
/// # Panics
///
/// Panics if the current process ID cannot be obtained for memory monitoring.
///
/// # Examples
///
/// ```no_run
/// use vibe_coding_tracker::analysis::aggregate_sessions_by_model;
/// use vibe_coding_tracker::display::analysis::display_analysis_interactive;
/// use vibe_coding_tracker::TimeRange;
///
/// let data = aggregate_sessions_by_model(TimeRange::All)?;
/// display_analysis_interactive(&data, TimeRange::All)?;
/// # Ok::<(), anyhow::Error>(())
/// ```
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

    // Scroll/selection state for the model table, plus the live mouse-capture
    // flag toggled by the `m` key.
    let mut scroll = ScrollState::new();
    let mut mouse_enabled = true;

    // Latest rendered display state, kept across refresh cycles so a terminal
    // resize can redraw at the new size immediately without re-aggregating the
    // session directories.
    let mut rows_data: Vec<AnalysisRow> = Vec::new();
    let mut totals = AnalysisRow::default();
    let mut provider_totals = AnalysisProviderTotals::default();

    loop {
        if refresh_state.should_refresh() {
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

            // Remember the selected model so the highlight follows it across a
            // refresh even when rows are reordered or added/removed.
            let prev_model = scroll
                .table
                .selected()
                .and_then(|i| rows_data.get(i))
                .map(|row| row.model.clone());

            // Calculate totals and extract display data
            totals = AnalysisRow::default();
            rows_data = convert_to_analysis_rows(&current_data.rows);
            // Hide models with no recorded operations in this range; an all-zero
            // row carries no information. Totals are summed from the remaining
            // rows below, and zero rows would add nothing anyway.
            rows_data.retain(|row| {
                row.edit_lines != 0
                    || row.read_lines != 0
                    || row.write_lines != 0
                    || row.bash_count != 0
                    || row.edit_count != 0
                    || row.read_count != 0
                    || row.todo_write_count != 0
                    || row.write_count != 0
            });
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
            let current_row_keys: Vec<String> =
                rows_data.iter().map(|row| row.model.clone()).collect();
            scroll.sync(prev_model.as_deref(), &current_row_keys);
            update_tracker.cleanup(current_row_keys);

            // Compute per-provider totals directly from the per-provider
            // aggregated rows produced by the batch analyzer, so Copilot sessions
            // cannot be mis-attributed to Claude Code based on their (now real)
            // model name.
            provider_totals =
                calculate_analysis_provider_totals_from_per_provider(&per_provider, &provider_days);

            render_analysis_frame(
                &mut terminal,
                &rows_data,
                &totals,
                &provider_totals,
                &update_tracker,
                &sys,
                pid,
                &mut scroll,
            )?;

            // Return arena free lists to the OS — see `release_freed_heap` docs.
            crate::utils::release_freed_heap();
        }

        // Handle input with timeout
        let action = handle_input()?;
        match action {
            InputAction::Quit => break,
            InputAction::Refresh => refresh_state.force(),
            InputAction::ToggleMouse => {
                mouse_enabled = !mouse_enabled;
                set_mouse_capture(&mut terminal, mouse_enabled)?;
            }
            // Move the selection / scroll, then repaint without re-aggregating.
            InputAction::Navigate(nav) => {
                scroll.apply(nav, rows_data.len());
                render_analysis_frame(
                    &mut terminal,
                    &rows_data,
                    &totals,
                    &provider_totals,
                    &update_tracker,
                    &sys,
                    pid,
                    &mut scroll,
                )?;
            }
            // Redraw the cached frame at the new terminal size without
            // re-aggregating, so resize tracks the drag instead of waiting
            // for the next refresh tick.
            InputAction::Resize => render_analysis_frame(
                &mut terminal,
                &rows_data,
                &totals,
                &provider_totals,
                &update_tracker,
                &sys,
                pid,
                &mut scroll,
            )?,
            InputAction::Continue => {}
        }
    }

    // Restore terminal
    restore_terminal(&mut terminal)?;
    Ok(())
}

/// Render a single analysis frame from already-aggregated display state.
///
/// Shared by the periodic refresh and resize redraw; `provider_rows` is
/// rebuilt here (cheap) rather than cached, since it borrows from
/// `provider_totals`.
///
/// # Errors
///
/// Returns an error if the terminal draw call fails.
#[allow(clippy::too_many_arguments)]
fn render_analysis_frame(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    rows_data: &[AnalysisRow],
    totals: &AnalysisRow,
    provider_totals: &AnalysisProviderTotals,
    update_tracker: &UpdateTracker,
    sys: &System,
    pid: Pid,
    scroll: &mut ScrollState,
) -> anyhow::Result<()> {
    let provider_rows = build_analysis_provider_rows(provider_totals);

    terminal.draw(|f| {
        let area = f.area();
        if area.width < ANALYSIS_MIN_W || area.height < ANALYSIS_MIN_H {
            render_too_small(f, ANALYSIS_MIN_W, ANALYSIS_MIN_H);
            return;
        }

        let totals_height = (provider_rows.len() as u16).saturating_add(4).max(4);
        let panels_height = (area.height >= ANALYSIS_PANELS_MIN_H).then_some(totals_height);
        let chunks = main_layout(area, panels_height);

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

        // One selectable row per model; the grand total lives only in the
        // summary bar below. Compact K/M/B numbers keep long counts in-column.
        let rows: Vec<RatatuiRow> = rows_data
            .iter()
            .map(|row| {
                let style = if update_tracker.is_recently_updated(&row.model) {
                    Style::default().bg(RatatuiColor::Rgb(60, 80, 60)).bold()
                } else {
                    Style::default()
                };
                styled_row(
                    vec![
                        row.model.clone(),
                        format_compact(row.edit_lines as i64),
                        format_compact(row.read_lines as i64),
                        format_compact(row.write_lines as i64),
                        format_compact(row.bash_count as i64),
                        format_compact(row.edit_count as i64),
                        format_compact(row.read_count as i64),
                        format_compact(row.todo_write_count as i64),
                        format_compact(row.write_count as i64),
                    ],
                    style,
                    1,
                )
            })
            .collect();

        let widths = [
            Constraint::Min(16),    // Model
            Constraint::Length(11), // Edit Lines
            Constraint::Length(11), // Read Lines
            Constraint::Length(11), // Write Lines
            Constraint::Length(7),  // Bash
            Constraint::Length(7),  // Edit
            Constraint::Length(7),  // Read
            Constraint::Length(10), // TodoWrite
            Constraint::Length(7),  // Write
        ];

        let row_count = rows.len();
        render_scrollable_table(
            f,
            chunks.table,
            header,
            rows,
            &widths,
            RatatuiColor::Green,
            row_count,
            scroll,
        );

        if let Some(panel_area) = chunks.panels {
            // Drop the "All Providers" aggregate; the summary bar already
            // carries the grand totals.
            let mut totals_rows: Vec<RatatuiRow> = provider_rows
                .iter()
                .filter(|row| row.label != "All Providers")
                .map(|row| {
                    create_provider_row(
                        vec![
                            row.label.to_string(),
                            format_compact(row.stats.total_edit_lines as i64),
                            format_compact(row.stats.total_read_lines as i64),
                            format_compact(row.stats.total_write_lines as i64),
                            format_compact(row.stats.total_bash_count as i64),
                            format_compact(row.stats.total_edit_count as i64),
                            format_compact(row.stats.total_read_count as i64),
                            format_compact(row.stats.total_todo_write_count as i64),
                            format_compact(row.stats.total_write_count as i64),
                            format_compact(row.stats.days_count as i64),
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
            f.render_widget(totals_table, panel_area);
        }

        // Summary
        let total_lines_str =
            format_compact((totals.edit_lines + totals.read_lines + totals.write_lines) as i64);
        let total_tools_str = format_compact(
            (totals.bash_count
                + totals.edit_count
                + totals.read_count
                + totals.todo_write_count
                + totals.write_count) as i64,
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

        let summary = create_summary(summary_items, sys, pid);
        f.render_widget(summary, chunks.summary);

        f.render_widget(create_controls(), chunks.controls);
    })?;

    Ok(())
}
