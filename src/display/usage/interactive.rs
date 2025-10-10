use crate::display::common::table::{
    create_controls, create_provider_row, create_ratatui_table, create_star_hint, create_summary,
    create_title,
};
use crate::display::common::tui::{
    InputAction, RefreshState, UpdateTracker, handle_input, restore_terminal, setup_terminal,
};
use crate::display::usage::averages::{
    build_provider_average_rows, build_usage_summary, format_tokens_per_day,
};
use crate::models::DateUsageResult;
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use crate::utils::{format_number, get_current_date};
use ratatui::{
    layout::{Constraint, Direction, Layout as RatatuiLayout},
    style::{Color as RatatuiColor, Style, Stylize},
    widgets::Row as RatatuiRow,
};
use std::collections::HashMap;
use std::time::Duration;
use sysinfo::System;

const USAGE_REFRESH_SECS: u64 = 5;
const PRICING_REFRESH_SECS: u64 = 300;
const MAX_TRACKED_ROWS: usize = 100;

/// Displays token usage data in an interactive TUI with auto-refresh
///
/// Features:
/// - Auto-refresh every 5 seconds (usage data) and 5 minutes (pricing)
/// - Real-time memory monitoring
/// - Provider-grouped daily averages
/// - Keyboard controls: `q`, `Esc`, or `Ctrl+C` to exit
pub fn display_usage_interactive() -> anyhow::Result<()> {
    let mut terminal = setup_terminal()?;
    let mut refresh_state = RefreshState::new(USAGE_REFRESH_SECS);

    let mut sys = System::new_all();
    let pid =
        sysinfo::get_current_pid().expect("Failed to get current process ID for memory monitoring");

    let mut pricing_map = match fetch_model_pricing() {
        Ok(map) => map,
        Err(e) => {
            log::warn!("Failed to fetch pricing: {}", e);
            ModelPricingMap::new(HashMap::new())
        }
    };
    // Note: Removed pricing_lookup_cache - ModelPricingMap uses global MATCH_CACHE internally
    let mut last_pricing_refresh = std::time::Instant::now();
    if pricing_map.raw().is_empty() {
        last_pricing_refresh =
            std::time::Instant::now() - Duration::from_secs(PRICING_REFRESH_SECS);
    }

    let mut usage_data = DateUsageResult::new();
    let mut has_usage_data = false;

    let mut update_tracker = UpdateTracker::new(MAX_TRACKED_ROWS, 1000);

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

        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
        sys.refresh_cpu_all();

        if last_pricing_refresh.elapsed() >= Duration::from_secs(PRICING_REFRESH_SECS)
            || pricing_map.raw().is_empty()
        {
            match fetch_model_pricing() {
                Ok(map) => {
                    pricing_map = map;
                    // No need to clear local cache - we're using global MATCH_CACHE
                    last_pricing_refresh = std::time::Instant::now();
                }
                Err(e) => {
                    log::warn!("Failed to fetch pricing: {}", e);
                    if pricing_map.raw().is_empty() {
                        last_pricing_refresh =
                            std::time::Instant::now() - Duration::from_secs(PRICING_REFRESH_SECS);
                    }
                }
            }
        }

        match crate::usage::get_usage_from_directories() {
            Ok(data) => {
                usage_data = data;
                has_usage_data = true;
            }
            Err(e) => {
                log::warn!("Failed to get usage data: {}", e);
                if !has_usage_data {
                    usage_data.clear();
                }
            }
        }

        let summary = build_usage_summary(&usage_data, &pricing_map);
        let rows_data = &summary.rows;
        let totals = &summary.totals;
        let daily_averages = &summary.daily_averages;
        let provider_rows = build_provider_average_rows(daily_averages);

        // Track updates
        let current_row_keys: Vec<String> = rows_data
            .iter()
            .map(|row| format!("{}:{}", row.date, row.model))
            .collect();

        update_tracker.cleanup(current_row_keys.clone());

        for row in rows_data {
            let row_key = format!("{}:{}", row.date, row.model);
            let current_data = (
                row.input_tokens,
                row.output_tokens,
                row.cache_read,
                row.cache_creation,
            );
            update_tracker.track_update(row_key, &current_data);
        }

        terminal.draw(|f| {
            let avg_height = (provider_rows.len() as u16).saturating_add(4).max(4);
            let chunks = RatatuiLayout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(10),
                    Constraint::Length(avg_height),
                    Constraint::Length(3),
                    Constraint::Length(2),
                    Constraint::Length(1),
                ])
                .split(f.area());

            let title = create_title("Token Usage Statistics", "📊", RatatuiColor::Cyan);
            f.render_widget(title, chunks[0]);

            let header = vec![
                "Date",
                "Model",
                "Input",
                "Output",
                "Cache Read",
                "Cache Create",
                "Total",
                "Cost (USD)",
            ];

            let today = get_current_date();

            let mut rows: Vec<RatatuiRow> = rows_data
                .iter()
                .map(|row| {
                    let row_key = format!("{}:{}", row.date, row.model);

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
                        row.display_model.clone(),
                        format_number(row.input_tokens),
                        format_number(row.output_tokens),
                        format_number(row.cache_read),
                        format_number(row.cache_creation),
                        format_number(row.total),
                        format!("${:.2}", row.cost),
                    ])
                    .style(style)
                })
                .collect();

            rows.push(
                RatatuiRow::new(vec![
                    "".to_string(),
                    "TOTAL".to_string(),
                    format_number(totals.input_tokens),
                    format_number(totals.output_tokens),
                    format_number(totals.cache_read),
                    format_number(totals.cache_creation),
                    format_number(totals.total),
                    format!("${:.2}", totals.cost),
                ])
                .style(
                    Style::default()
                        .fg(RatatuiColor::Yellow)
                        .bold()
                        .bg(RatatuiColor::DarkGray),
                ),
            );

            let widths = [
                Constraint::Length(12),
                Constraint::Min(20),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(14),
                Constraint::Length(12),
                Constraint::Length(12),
            ];

            let table = create_ratatui_table(rows, header, &widths, RatatuiColor::Green);
            f.render_widget(table, chunks[1]);

            let mut avg_rows: Vec<RatatuiRow> = provider_rows
                .iter()
                .map(|row| {
                    create_provider_row(
                        vec![
                            format!("{} {}", row.icon, row.label),
                            format_tokens_per_day(row.stats.avg_tokens()),
                            format!("${:.2}", row.stats.avg_cost()),
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
                    ])
                    .style(Style::default().fg(RatatuiColor::DarkGray)),
                );
            }

            let avg_header = vec!["Provider", "Tokens / Day", "Cost / Day", "Active Days"];
            let avg_widths = [
                Constraint::Min(20),
                Constraint::Length(16),
                Constraint::Length(14),
                Constraint::Length(14),
            ];

            let average_table =
                create_ratatui_table(avg_rows, avg_header, &avg_widths, RatatuiColor::Magenta);
            f.render_widget(average_table, chunks[2]);

            let total_cost_str = format!("${:.2}", totals.cost);
            let total_tokens_str = format_number(totals.total);
            let entries_str = format!("{}", rows_data.len());

            let summary_items = vec![
                (
                    "💰 Total Cost:",
                    total_cost_str.as_str(),
                    RatatuiColor::Yellow,
                ),
                (
                    "🔢 Total Tokens:",
                    total_tokens_str.as_str(),
                    RatatuiColor::Cyan,
                ),
                ("📅 Entries:", entries_str.as_str(), RatatuiColor::Blue),
            ];

            let summary = create_summary(summary_items, &sys, pid);
            f.render_widget(summary, chunks[3]);

            let controls = create_controls();
            f.render_widget(controls, chunks[4]);

            let star_hint = create_star_hint();
            f.render_widget(star_hint, chunks[5]);
        })?;

        match handle_input()? {
            InputAction::Quit => break,
            InputAction::Refresh => refresh_state.force(),
            InputAction::Continue => {}
        }
    }

    restore_terminal(&mut terminal)?;
    Ok(())
}
