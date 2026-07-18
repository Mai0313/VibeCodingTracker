//! Interactive (TUI) renderer for the `analysis` view.
//!
//! Runs a ratatui draw loop that periodically re-aggregates the session
//! directories, highlighting rows whose metrics changed since the last tick and
//! redrawing on terminal resize without re-aggregating.

use crate::analysis::AnalysisData;
use crate::config::ProvidersConfig;
use crate::display::analysis::averages::{
    AnalysisProviderTotals, AnalysisRow, build_analysis_provider_rows,
    calculate_analysis_provider_totals_from_per_provider, convert_to_analysis_rows,
};
use crate::display::common::table::{
    create_controls_with_status, create_provider_row, create_ratatui_table, create_summary,
    init_process_metrics, main_layout, refresh_process_metrics, render_scrollable_table,
    render_too_small, styled_row,
};
use crate::display::common::tui::{
    InputAction, RefreshWorker, RefreshWorkerError, ScrollState, TerminalSession, UpdateTracker,
    handle_input, overlay_repo_hyperlink, refresh_status, render_loading_frame,
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
use std::sync::Arc;
use std::time::{Duration, Instant};
use sysinfo::{Pid, System};

/// Upper bound on the number of rows tracked for the "recently updated"
/// highlight, capping the tracker's memory footprint.
const MAX_TRACKED_ANALYSIS_ROWS: usize = 100;

/// Hard minimum terminal width/height; below this only a notice is drawn. The
/// analysis table is wider (9 columns) so it needs more width than usage.
const ANALYSIS_MIN_W: u16 = 84;
const ANALYSIS_MIN_H: u16 = 14;
/// Rows that must fit *below* the provider band before it is worth showing: the
/// scrollable table (`main_layout` gives it `Min(6)` ≈ 2 body rows after the
/// border + header + margin) plus the summary bar (3) and controls line (1).
const ANALYSIS_BELOW_BAND_MIN_H: u16 = 10;

/// Height of the provider band, or `None` when the terminal is too short to
/// show it without squeezing the table / summary / controls beneath it.
///
/// The band is `provider_row_count + 4` rows tall (its own border + header),
/// floored at 4. Because it scales with the number of providers, gating on a
/// fixed height would either hide it needlessly (few providers) or render it
/// truncated (all provider rows + overall). We instead require room for the
/// band *and* everything below it, so it only appears when it fits in full.
fn analysis_panels_height(area_height: u16, provider_row_count: usize) -> Option<u16> {
    let totals_height = (provider_row_count as u16).saturating_add(4).max(4);
    (area_height >= totals_height.saturating_add(ANALYSIS_BELOW_BAND_MIN_H))
        .then_some(totals_height)
}

struct AnalysisUiState {
    rows: Vec<AnalysisRow>,
    totals: AnalysisRow,
    provider_totals: AnalysisProviderTotals,
    update_tracker: UpdateTracker,
    scroll: ScrollState,
}

impl AnalysisUiState {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            totals: AnalysisRow::default(),
            provider_totals: AnalysisProviderTotals::default(),
            update_tracker: UpdateTracker::new(MAX_TRACKED_ANALYSIS_ROWS, 1000),
            scroll: ScrollState::new(),
        }
    }

    fn apply(&mut self, data: AnalysisData) {
        let previous = self
            .scroll
            .table
            .selected()
            .and_then(|index| self.rows.get(index))
            .map(|row| row.model.clone());
        self.rows = convert_to_analysis_rows(&data.rows);
        self.rows.retain(|row| {
            row.edit_lines != 0
                || row.read_lines != 0
                || row.write_lines != 0
                || row.bash_count != 0
                || row.edit_count != 0
                || row.read_count != 0
                || row.todo_write_count != 0
                || row.write_count != 0
        });
        self.totals = AnalysisRow::default();
        let fingerprints: Vec<_> = self
            .rows
            .iter()
            .map(|row| {
                (
                    row.model.clone(),
                    (
                        row.edit_lines,
                        row.read_lines,
                        row.write_lines,
                        row.bash_count,
                        row.edit_count,
                        row.read_count,
                        row.todo_write_count,
                        row.write_count,
                    ),
                )
            })
            .collect();
        for row in &self.rows {
            self.totals.edit_lines += row.edit_lines;
            self.totals.read_lines += row.read_lines;
            self.totals.write_lines += row.write_lines;
            self.totals.bash_count += row.bash_count;
            self.totals.edit_count += row.edit_count;
            self.totals.read_count += row.read_count;
            self.totals.todo_write_count += row.todo_write_count;
            self.totals.write_count += row.write_count;
        }
        let models: Vec<_> = fingerprints
            .iter()
            .map(|(model, _)| model.clone())
            .collect();
        self.scroll.sync(previous.as_deref(), &models);
        self.update_tracker.cleanup(models);
        for (model, fingerprint) in fingerprints {
            self.update_tracker.track_update(model, &fingerprint);
        }
        self.provider_totals = calculate_analysis_provider_totals_from_per_provider(
            &data.per_provider,
            &data.provider_days,
        );
    }

    fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        sys: &System,
        pid: Pid,
        status: Option<&str>,
    ) -> anyhow::Result<()> {
        render_analysis_frame_with_status(
            terminal,
            &self.rows,
            &self.totals,
            &self.provider_totals,
            &self.update_tracker,
            sys,
            pid,
            &mut self.scroll,
            status,
        )
    }
}

/// Starts the CLI analysis TUI with its initial scan in the background.
pub fn display_analysis_interactive_loading(
    time_range: crate::models::TimeRange,
    providers: ProvidersConfig,
    refresh_secs: u64,
) -> anyhow::Result<()> {
    let threads = crate::config::PerformanceConfig::default().resolved_scan_threads();
    let pool = Arc::new(crate::scan::build_scan_pool(threads)?);
    display_analysis_interactive_loading_with_pool(time_range, providers, refresh_secs, pool)
}

/// [`display_analysis_interactive_loading`] with a caller-owned scan pool.
pub fn display_analysis_interactive_loading_with_pool(
    time_range: crate::models::TimeRange,
    providers: ProvidersConfig,
    refresh_secs: u64,
    scan_pool: Arc<rayon::ThreadPool>,
) -> anyhow::Result<()> {
    run_analysis_interactive(None, time_range, providers, refresh_secs, scan_pool)
}

fn run_analysis_interactive(
    initial_data: Option<AnalysisData>,
    time_range: crate::models::TimeRange,
    providers: ProvidersConfig,
    refresh_secs: u64,
    scan_pool: Arc<rayon::ThreadPool>,
) -> anyhow::Result<()> {
    let mut terminal = TerminalSession::new()?;
    let mut show_no_data = false;
    let result = (|| -> anyhow::Result<()> {
        let mut spinner_index = 0usize;
        let mut loaded = initial_data.is_some();
        if !loaded {
            render_loading_frame(terminal.terminal_mut(), spinner_index)?;
        }

        let paths = crate::utils::resolve_paths()?;
        let worker_paths = paths.clone();
        let worker_pool = Arc::clone(&scan_pool);
        let mut worker = RefreshWorker::new_with_init(refresh_secs, move || {
            let mut cache = crate::summary_cache::SummaryScanCache::new();
            move || {
                let aggregation = worker_pool.install(|| {
                    crate::analysis::aggregate_sessions_by_model_from_paths_with_cache(
                        &worker_paths,
                        time_range,
                        providers,
                        &mut cache,
                    )
                })?;
                if aggregation.diagnostics.all_failed() {
                    let first = aggregation
                        .diagnostics
                        .failures
                        .first()
                        .map(|failure| failure.error.as_str())
                        .unwrap_or("unknown source failure");
                    anyhow::bail!(
                        "failed to parse all {} analysis sources: {first}",
                        aggregation.diagnostics.candidates
                    );
                }
                if aggregation.diagnostics.partially_failed() {
                    log::warn!(
                        "analysis refresh kept partial data after {} source failures",
                        aggregation.diagnostics.failures.len()
                    );
                }
                Ok(aggregation.data)
            }
        });

        let pid = sysinfo::get_current_pid()
            .expect("Failed to get current process ID for memory monitoring");
        let mut sys = System::new();
        init_process_metrics(&mut sys, pid);
        let metrics_interval = Duration::from_millis(crate::constants::refresh::METRICS_REFRESH_MS);
        let mut last_metrics = Instant::now();
        let mut last_spinner = Instant::now();
        let mut state = AnalysisUiState::new();
        let mut failure_until = None;

        if let Some(data) = initial_data {
            state.apply(data);
            state.render(terminal.terminal_mut(), &sys, pid, None)?;
            worker.defer_until_interval();
        } else {
            worker.request();
        }

        loop {
            if let Some(result) = worker.try_result() {
                match result {
                    Ok(data) => {
                        if !loaded && data.rows.is_empty() {
                            show_no_data = true;
                            return Ok(());
                        }
                        state.apply(data);
                        loaded = true;
                        failure_until = None;
                        refresh_process_metrics(&mut sys, pid);
                        state.render(
                            terminal.terminal_mut(),
                            &sys,
                            pid,
                            refresh_status(worker.is_active(), failure_until),
                        )?;
                        crate::utils::release_freed_heap();
                        last_metrics = Instant::now();
                    }
                    Err(RefreshWorkerError::Disconnected) => {
                        return Err(anyhow::anyhow!("refresh worker disconnected"));
                    }
                    Err(error) if !loaded => {
                        return Err(anyhow::anyhow!("initial analysis load failed: {error}"));
                    }
                    Err(error) => {
                        log::warn!("analysis refresh failed: {error}");
                        failure_until = Some(Instant::now() + Duration::from_secs(3));
                        state.render(
                            terminal.terminal_mut(),
                            &sys,
                            pid,
                            refresh_status(worker.is_active(), failure_until),
                        )?;
                    }
                }
            }

            let auto_refresh_started = worker.request_if_due();
            if !loaded && last_spinner.elapsed() >= Duration::from_millis(100) {
                spinner_index = spinner_index.wrapping_add(1);
                last_spinner = Instant::now();
                render_loading_frame(terminal.terminal_mut(), spinner_index)?;
            } else if loaded && (auto_refresh_started || last_metrics.elapsed() >= metrics_interval)
            {
                if last_metrics.elapsed() >= metrics_interval {
                    last_metrics = Instant::now();
                    refresh_process_metrics(&mut sys, pid);
                }
                let status = refresh_status(worker.is_active(), failure_until);
                state.render(terminal.terminal_mut(), &sys, pid, status)?;
            }

            match handle_input()? {
                InputAction::Quit => break,
                InputAction::Refresh => {
                    worker.request();
                    if loaded {
                        state.render(
                            terminal.terminal_mut(),
                            &sys,
                            pid,
                            refresh_status(worker.is_active(), failure_until),
                        )?;
                    }
                }
                InputAction::ToggleMerge => {}
                InputAction::Navigate(delta) if loaded => {
                    state.scroll.apply(delta, state.rows.len());
                    state.render(
                        terminal.terminal_mut(),
                        &sys,
                        pid,
                        refresh_status(worker.is_active(), failure_until),
                    )?;
                }
                InputAction::Resize if loaded => {
                    state.render(
                        terminal.terminal_mut(),
                        &sys,
                        pid,
                        refresh_status(worker.is_active(), failure_until),
                    )?;
                }
                InputAction::Resize => {
                    render_loading_frame(terminal.terminal_mut(), spinner_index)?;
                }
                InputAction::Navigate(_) | InputAction::Continue => {}
            }
        }

        Ok(())
    })();
    let finished = terminal.finish(result);
    if finished.is_ok() && show_no_data {
        println!("No analysis data found");
    }
    finished
}

/// Render the `analysis` view as an interactive, auto-refreshing TUI.
///
/// Takes over the terminal and runs a draw loop until the user quits. Every
/// `refresh_secs` a background worker incrementally re-aggregates the session
/// directories for `time_range`, highlights changed counters, and keeps the
/// previous rows visible if a refresh fails. `initial_data` renders immediately
/// for library callers; the CLI-specific loading entry point starts with a
/// spinner instead. Returns immediately after printing a message if
/// `initial_data` is empty.
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
/// display_analysis_interactive(data, TimeRange::All, Default::default(), 10)?;
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn display_analysis_interactive(
    initial_data: AnalysisData,
    time_range: crate::models::TimeRange,
    providers: ProvidersConfig,
    refresh_secs: u64,
) -> anyhow::Result<()> {
    if initial_data.rows.is_empty() {
        println!("No analysis data found");
        return Ok(());
    }
    let threads = crate::config::PerformanceConfig::default().resolved_scan_threads();
    let pool = Arc::new(crate::scan::build_scan_pool(threads)?);
    run_analysis_interactive(
        Some(initial_data),
        time_range,
        providers,
        refresh_secs,
        pool,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_analysis_frame_with_status(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    rows_data: &[AnalysisRow],
    totals: &AnalysisRow,
    provider_totals: &AnalysisProviderTotals,
    update_tracker: &UpdateTracker,
    sys: &System,
    pid: Pid,
    scroll: &mut ScrollState,
    status: Option<&str>,
) -> anyhow::Result<()> {
    let provider_rows = build_analysis_provider_rows(provider_totals);

    let completed = terminal.draw(|f| {
        let area = f.area();
        if area.width < ANALYSIS_MIN_W || area.height < ANALYSIS_MIN_H {
            render_too_small(f, ANALYSIS_MIN_W, ANALYSIS_MIN_H);
            return;
        }

        let panels_height = analysis_panels_height(area.height, provider_rows.len());
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

        let summary = create_summary(summary_items, sys, pid, chunks.summary.width);
        f.render_widget(summary, chunks.summary);

        f.render_widget(create_controls_with_status(&[], status), chunks.controls);
    })?;

    // ratatui can't embed the OSC 8 escape itself, so hyperlink the repo label
    // it just drew (a no-op on terminals without hyperlink support).
    overlay_repo_hyperlink(completed.buffer)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_shown_only_when_it_fits_in_full() {
        // Five sample providers + the overall row = 6 band rows -> 10 tall, so the
        // band needs at least 20 rows of terminal to also fit table+summary+
        // controls. Below that it is hidden (previously it rendered truncated).
        assert_eq!(analysis_panels_height(18, 6), None);
        assert_eq!(analysis_panels_height(19, 6), None);
        assert_eq!(analysis_panels_height(20, 6), Some(10));

        // The threshold scales down with fewer providers instead of a fixed 18.
        assert_eq!(analysis_panels_height(14, 1), None);
        assert_eq!(analysis_panels_height(15, 1), Some(5));

        // No providers still floors the band height at 4 (needs 14 rows).
        assert_eq!(analysis_panels_height(13, 0), None);
        assert_eq!(analysis_panels_height(14, 0), Some(4));
    }
}
