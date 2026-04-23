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
use crate::models::{PerProviderUsage, ProviderActiveDays, UsageResult};
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use crate::utils::format_number;
use ratatui::{
    layout::{Constraint, Direction, Layout as RatatuiLayout},
    style::{Color as RatatuiColor, Style, Stylize},
    widgets::Row as RatatuiRow,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use sysinfo::System;

const USAGE_REFRESH_SECS: u64 = 10;
/// How often to rebuild the LiteLLM pricing map. The underlying data only
/// changes when the upstream JSON is updated (daily at most), so rebuilding
/// a fresh ~500 KB `HashMap<Rc<str>, ModelPricing>` every 10 s just churned
/// the allocator and left heap fragmentation behind on long sessions.
const PRICING_REFRESH_SECS: u64 = 300;
const MAX_TRACKED_ROWS: usize = 100;

/// Displays token usage data in an interactive TUI with auto-refresh
///
/// Features:
/// - Auto-refresh every 10 seconds (usage + pricing)
/// - Real-time memory monitoring
/// - Provider-grouped daily averages
/// - Keyboard controls: `q`, `Esc`, or `Ctrl+C` to exit
pub fn display_usage_interactive(time_range: crate::cli::TimeRange) -> anyhow::Result<()> {
    let mut terminal = setup_terminal()?;
    let mut refresh_state = RefreshState::new(USAGE_REFRESH_SECS);

    let pid =
        sysinfo::get_current_pid().expect("Failed to get current process ID for memory monitoring");
    // `System::new_all` would load every process, disk and network on the
    // machine (tens of MB on a busy host). We only read our own process
    // stats, so start from an empty `System` and populate it with just our
    // pid on every refresh. `remove_dead_processes: true` ensures no stale
    // entries linger across refreshes.
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);

    let mut usage_data = UsageResult::default();
    let mut per_provider_usage = PerProviderUsage::default();
    let mut provider_days = ProviderActiveDays::default();
    let mut has_usage_data = false;

    // Pricing map is large (~500 KB / ~400 models) but changes at most once
    // per day upstream, so build it once and reuse across refresh cycles.
    // We only rebuild when `PRICING_REFRESH_SECS` has elapsed — otherwise a
    // 10 s refresh interval would allocate and drop a fresh hashmap six
    // times a minute, leaving the glibc heap fragmented on long sessions.
    let mut pricing_map = match fetch_model_pricing() {
        Ok(map) => map,
        Err(e) => {
            log::warn!("Failed to fetch initial pricing: {}", e);
            ModelPricingMap::new(HashMap::new())
        }
    };
    let mut last_pricing_refresh = Instant::now();

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

        // Only refresh our own process entry and prune any that have died.
        // Per-process CPU usage is updated as part of `refresh_processes`, so
        // the former `refresh_cpu_all()` (which scans every CPU system-wide)
        // is not needed here.
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);

        match crate::usage::get_usage_from_directories(time_range) {
            Ok(data) => {
                usage_data = data.models;
                per_provider_usage = data.per_provider;
                provider_days = data.provider_days;
                has_usage_data = true;
            }
            Err(e) => {
                log::warn!("Failed to get usage data: {}", e);
                if !has_usage_data {
                    usage_data.clear();
                    per_provider_usage = PerProviderUsage::default();
                }
            }
        }

        // Refresh the pricing map at most once every `PRICING_REFRESH_SECS`.
        if last_pricing_refresh.elapsed() >= Duration::from_secs(PRICING_REFRESH_SECS)
            || pricing_map.is_empty()
        {
            match fetch_model_pricing() {
                Ok(map) => {
                    pricing_map = map;
                    last_pricing_refresh = Instant::now();
                }
                Err(e) => log::warn!("Failed to refresh pricing: {}", e),
            }
        }

        let summary = build_usage_summary(
            &usage_data,
            &per_provider_usage,
            &provider_days,
            &pricing_map,
        );

        // Extract only the data needed for rendering to minimize memory usage
        let rows_data = summary.rows;
        let totals = summary.totals;
        let daily_averages = summary.daily_averages;

        // Clear raw usage data immediately after processing to free memory.
        // Per-provider map is reset on the next refresh when new data arrives.
        usage_data.clear();
        per_provider_usage = PerProviderUsage::default();

        // NOTE: we intentionally do NOT clear the global file cache or the
        // pricing cache here. The usage path already bypasses the file cache
        // (runs in `AnalysisMode::UsageOnly` and drops each analysis after
        // extraction), so wiping it would only nuke entries populated by
        // other commands. The pricing cache is a single sub-MB hashmap
        // backed by a dated on-disk file — clearing it just forces another
        // file-parse on the next refresh.

        let provider_rows = build_provider_average_rows(&daily_averages);

        // Track updates
        let current_row_keys: Vec<String> = rows_data.iter().map(|row| row.model.clone()).collect();

        update_tracker.cleanup(current_row_keys.clone());

        for row in &rows_data {
            let row_key = row.model.clone();
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
                "Model",
                "Input",
                "Output",
                "Cache Read",
                "Cache Create",
                "Total",
                "Cost (USD)",
            ];

            let mut rows: Vec<RatatuiRow> = rows_data
                .iter()
                .map(|row| {
                    let row_key = row.model.clone();

                    let is_recently_updated = update_tracker.is_recently_updated(&row_key);

                    let style = if is_recently_updated {
                        Style::default().bg(RatatuiColor::Rgb(60, 80, 60)).bold()
                    } else {
                        Style::default()
                    };

                    RatatuiRow::new(vec![
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
                ("📊 Models:", entries_str.as_str(), RatatuiColor::Blue),
            ];

            let summary = create_summary(summary_items, &sys, pid);
            f.render_widget(summary, chunks[3]);

            let controls = create_controls();
            f.render_widget(controls, chunks[4]);

            let star_hint = create_star_hint();
            f.render_widget(star_hint, chunks[5]);
        })?;

        // Drop heavy data structures after rendering to free memory immediately
        drop(rows_data);
        drop(provider_rows);

        // Hand any arena-held free pages back to the OS. The refresh cycle
        // just allocated and dropped a lot of small objects (per-file parse
        // buffers, per-model hashmaps, ratatui row vectors); without this
        // call glibc keeps them as internal free lists and RSS climbs by
        // ~6 MB every refresh on a 219-session directory.
        crate::utils::release_freed_heap();

        match handle_input()? {
            InputAction::Quit => break,
            InputAction::Refresh => refresh_state.force(),
            InputAction::Continue => {}
        }
    }

    restore_terminal(&mut terminal)?;
    Ok(())
}
