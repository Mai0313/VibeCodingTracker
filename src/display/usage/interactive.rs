//! Auto-refreshing TUI for the usage view.
//!
//! Runs a render loop that re-aggregates the session directories every
//! [`USAGE_REFRESH_SECS`] seconds, repriced from a pricing map rebuilt at most
//! every [`PRICING_REFRESH_SECS`], and highlights rows whose tokens changed
//! since the last tick. The loop holds only the small per-model display state
//! between frames so a resize repaints instantly without re-aggregating; memory
//! is trimmed back to the OS after each refresh.

use crate::display::common::table::{
    create_controls, create_provider_row, create_ratatui_table, create_summary, main_layout,
    render_scrollable_table, render_too_small, styled_row,
};
use crate::display::common::tui::{
    InputAction, RefreshState, ScrollState, UpdateTracker, handle_input, restore_terminal,
    set_mouse_capture, setup_terminal,
};
use crate::display::usage::averages::{
    UsageProviderTotals, UsageRow, UsageTotals, build_provider_total_rows, build_usage_summary,
};
use crate::models::{
    ClaudeRateLimitsCache, CodexQuotaSnapshot, PerProviderUsage, ProviderActiveDays, QuotaSource,
    QuotaWindow, UsageResult,
};
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use crate::quota::{load_claude_rate_limits, load_codex_cache, spawn_codex_quota_worker};
use crate::utils::{format_compact, format_cost, format_duration_until};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout as RatatuiLayout, Rect},
    style::{Color as RatatuiColor, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Row as RatatuiRow},
};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::{Pid, System};

/// Minimum height for the bottom quota panels (border + 2 gauges + labels).
const QUOTA_PANEL_MIN_HEIGHT: u16 = 7;
/// Claude brand color for the quota panel border.
const CLAUDE_COLOR: RatatuiColor = RatatuiColor::Rgb(190, 116, 87);
/// Codex brand color for the quota panel border.
const CODEX_COLOR: RatatuiColor = RatatuiColor::Rgb(118, 127, 198);

/// How often the loop re-aggregates the session directories and repaints.
const USAGE_REFRESH_SECS: u64 = 10;
/// How often to rebuild the LiteLLM pricing map. The underlying data only
/// changes when the upstream JSON is updated (daily at most), so rebuilding
/// a fresh ~500 KB `HashMap<Rc<str>, ModelPricing>` every 10 s just churned
/// the allocator and left heap fragmentation behind on long sessions.
const PRICING_REFRESH_SECS: u64 = 300;
/// Upper bound on rows the [`UpdateTracker`] remembers for change highlighting.
const MAX_TRACKED_ROWS: usize = 100;

/// Hard minimum terminal width/height; below this only a notice is drawn.
const USAGE_MIN_W: u16 = 74;
const USAGE_MIN_H: u16 = 14;
/// At or above this height the provider/quota band is shown; below it the band
/// is dropped so the scrollable table keeps a usable height.
const USAGE_PANELS_MIN_H: u16 = 22;

/// Displays token usage data in an interactive TUI with auto-refresh.
///
/// Runs until the user quits; `time_range` filters which sessions are scanned.
///
/// Features:
/// - Auto-refresh every 10 seconds (usage + pricing)
/// - Real-time memory monitoring
/// - Provider-grouped totals
/// - Scrollable model table (arrow keys / `PgUp`/`PgDn` / `g`/`G` / mouse wheel)
/// - Keyboard controls: `q`, `Esc`, or `Ctrl+C` to exit, `r` to refresh,
///   `m` to toggle mouse capture
///
/// # Errors
///
/// Returns an error if the terminal cannot be set up or restored, if reading a
/// terminal input event fails, or if a frame fails to draw. A failure to
/// aggregate usage or fetch pricing within the loop is logged and the previous
/// data is kept, not propagated.
///
/// # Panics
///
/// Panics if the current process ID cannot be obtained for memory monitoring.
pub fn display_usage_interactive(time_range: crate::cli::TimeRange) -> anyhow::Result<()> {
    let mut terminal = setup_terminal()?;
    let mut refresh_state = RefreshState::new(USAGE_REFRESH_SECS);

    // Codex quota is fetched on a background thread so the blocking HTTP call
    // never stalls the render loop. The panel is seeded from the last-known
    // cache so it shows immediately on launch.
    let codex_shared = Arc::new(Mutex::new(load_codex_cache().unwrap_or_default()));
    let codex_shutdown = Arc::new(AtomicBool::new(false));
    let _codex_worker =
        spawn_codex_quota_worker(Arc::clone(&codex_shared), Arc::clone(&codex_shutdown));

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
    let mut opencode_costs: crate::constants::FastHashMap<String, f64> = Default::default();
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

    // Scroll/selection state for the model table, plus the live mouse-capture
    // flag toggled by the `m` key.
    let mut scroll = ScrollState::new();
    let mut mouse_enabled = true;

    // Latest rendered display state, kept across refresh cycles so a terminal
    // resize can redraw at the new size immediately without re-aggregating the
    // session directories. These are small per-model summaries, not the heavy
    // parse buffers, so holding onto them between refreshes is cheap.
    let mut rows_data: Vec<UsageRow> = Vec::new();
    let mut totals = UsageTotals::default();
    let mut provider_totals = UsageProviderTotals::default();
    // Quota panel state, cached across frames so a resize repaints without
    // re-reading the cache / shared snapshot.
    let mut claude_rl: Option<ClaudeRateLimitsCache> = None;
    let mut codex_snapshot = CodexQuotaSnapshot::default();

    loop {
        if refresh_state.should_refresh() {
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
                    opencode_costs = data.opencode_costs;
                    has_usage_data = true;
                }
                Err(e) => {
                    log::warn!("Failed to get usage data: {}", e);
                    if !has_usage_data {
                        usage_data.clear();
                        per_provider_usage = PerProviderUsage::default();
                        opencode_costs = Default::default();
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
                &opencode_costs,
            );

            // Remember which model was selected so the highlight can follow it
            // across a refresh even if rows are reordered or added/removed.
            let prev_model = scroll
                .table
                .selected()
                .and_then(|i| rows_data.get(i))
                .map(|row| row.model.clone());

            // Cache the rendered display state so a resize can redraw without
            // re-aggregating. These per-model summaries are small; the heavy
            // raw usage buffers are cleared right below.
            rows_data = summary.rows;
            totals = summary.totals;
            provider_totals = summary.provider_totals;

            let model_names: Vec<String> = rows_data.iter().map(|row| row.model.clone()).collect();
            scroll.sync(prev_model.as_deref(), &model_names);

            // Refresh quota panels: Claude from the tiny local ingest cache,
            // Codex from the background worker's latest snapshot.
            claude_rl = load_claude_rate_limits();
            codex_snapshot = codex_shared.lock().map(|g| g.clone()).unwrap_or_default();

            // Clear raw usage data immediately after processing to free memory.
            // Per-provider map is reset on the next refresh when new data arrives.
            usage_data.clear();
            per_provider_usage = PerProviderUsage::default();

            // NOTE: we intentionally do NOT clear the global file cache or the
            // pricing cache here. The usage path already bypasses the file cache
            // (runs in `ParseMode::UsageOnly` and drops each analysis after
            // extraction), so wiping it would only nuke entries populated by
            // other commands. The pricing cache is a single sub-MB hashmap
            // backed by a dated on-disk file — clearing it just forces another
            // file-parse on the next refresh.

            // Track updates
            let current_row_keys: Vec<String> =
                rows_data.iter().map(|row| row.model.clone()).collect();

            update_tracker.cleanup(current_row_keys);

            for row in &rows_data {
                let row_key = row.model.clone();
                // Include reasoning in the change fingerprint so Gemini
                // sessions whose only delta lands in `thoughts_tokens` still
                // trigger a highlight; otherwise the row would look idle
                // while its cost silently grew.
                let current_data = (
                    row.input_tokens,
                    row.output_with_reasoning(),
                    row.cache_read,
                    row.cache_creation,
                );
                update_tracker.track_update(row_key, &current_data);
            }

            render_usage_frame(
                &mut terminal,
                &rows_data,
                &totals,
                &provider_totals,
                &update_tracker,
                &sys,
                pid,
                claude_rl.as_ref(),
                &codex_snapshot,
                &mut scroll,
            )?;

            // Hand any arena-held free pages back to the OS. The refresh cycle
            // just allocated and dropped a lot of small objects (per-file parse
            // buffers, per-model hashmaps, ratatui row vectors); without this
            // call glibc keeps them as internal free lists and RSS climbs by
            // ~6 MB every refresh on a 219-session directory.
            crate::utils::release_freed_heap();
        }

        let action = handle_input()?;
        match action {
            InputAction::Quit => {
                // Signal the detached worker to stop; the OS reclaims it on exit.
                codex_shutdown.store(true, Ordering::Relaxed);
                break;
            }
            InputAction::Refresh => refresh_state.force(),
            InputAction::ToggleMouse => {
                mouse_enabled = !mouse_enabled;
                set_mouse_capture(&mut terminal, mouse_enabled)?;
            }
            // Move the selection / scroll, then repaint the cached frame
            // without re-aggregating.
            InputAction::Navigate(nav) => {
                scroll.apply(nav, rows_data.len());
                render_usage_frame(
                    &mut terminal,
                    &rows_data,
                    &totals,
                    &provider_totals,
                    &update_tracker,
                    &sys,
                    pid,
                    claude_rl.as_ref(),
                    &codex_snapshot,
                    &mut scroll,
                )?;
            }
            // Redraw the cached frame at the new terminal size without
            // re-aggregating, so resize tracks the drag instead of waiting
            // for the next refresh tick.
            InputAction::Resize => render_usage_frame(
                &mut terminal,
                &rows_data,
                &totals,
                &provider_totals,
                &update_tracker,
                &sys,
                pid,
                claude_rl.as_ref(),
                &codex_snapshot,
                &mut scroll,
            )?,
            InputAction::Continue => {}
        }
    }

    restore_terminal(&mut terminal)?;
    Ok(())
}

/// Render a single usage frame from already-aggregated display state.
///
/// Kept separate from the refresh loop so both the periodic refresh and a
/// terminal resize can paint the latest data; `provider_rows` is rebuilt here
/// (cheap, at most five borrow wrappers) rather than cached, since it borrows
/// from `provider_totals`.
///
/// # Errors
///
/// Returns an error if the underlying terminal draw call fails.
#[allow(clippy::too_many_arguments)]
fn render_usage_frame(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    rows_data: &[UsageRow],
    totals: &UsageTotals,
    provider_totals: &UsageProviderTotals,
    update_tracker: &UpdateTracker,
    sys: &System,
    pid: Pid,
    claude_rl: Option<&ClaudeRateLimitsCache>,
    codex: &CodexQuotaSnapshot,
    scroll: &mut ScrollState,
) -> anyhow::Result<()> {
    let provider_rows = build_provider_total_rows(provider_totals);

    terminal.draw(|f| {
        let area = f.area();
        if area.width < USAGE_MIN_W || area.height < USAGE_MIN_H {
            render_too_small(f, USAGE_MIN_W, USAGE_MIN_H);
            return;
        }

        // Drop the provider/quota band on short terminals so the scrollable
        // table keeps a usable height.
        let totals_height = (provider_rows.len() as u16)
            .saturating_add(4)
            .max(QUOTA_PANEL_MIN_HEIGHT);
        let panels_height = (area.height >= USAGE_PANELS_MIN_H).then_some(totals_height);
        let chunks = main_layout(area, panels_height);

        let header = vec![
            "Model",
            "Input",
            "Output",
            "Cache Read",
            "Cache Write",
            "Total",
            "Cost (USD)",
        ];

        // Model rows (selectable) followed by a pinned TOTAL row excluded from
        // selection. Compact K/M/B numbers keep cells inside the columns.
        let mut rows: Vec<RatatuiRow> = rows_data
            .iter()
            .map(|row| {
                let style = if update_tracker.is_recently_updated(&row.model) {
                    Style::default().bg(RatatuiColor::Rgb(60, 80, 60)).bold()
                } else {
                    Style::default()
                };
                styled_row(
                    vec![
                        row.display_model.clone(),
                        format_compact(row.input_tokens),
                        format_compact(row.output_with_reasoning()),
                        format_compact(row.cache_read),
                        format_compact(row.cache_creation),
                        format_compact(row.total),
                        format_cost(row.cost),
                    ],
                    style,
                    1,
                )
            })
            .collect();

        rows.push(styled_row(
            vec![
                "TOTAL".to_string(),
                format_compact(totals.input_tokens),
                format_compact(totals.output_with_reasoning()),
                format_compact(totals.cache_read),
                format_compact(totals.cache_creation),
                format_compact(totals.total),
                format_cost(totals.cost),
            ],
            Style::default()
                .fg(RatatuiColor::Yellow)
                .bold()
                .bg(RatatuiColor::DarkGray),
            1,
        ));

        let widths = [
            Constraint::Min(16),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(11),
            Constraint::Length(11),
            Constraint::Length(9),
            Constraint::Length(12),
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
            let mut totals_rows: Vec<RatatuiRow> = provider_rows
                .iter()
                .map(|row| {
                    create_provider_row(
                        vec![
                            row.label.to_string(),
                            format_compact(row.stats.total_tokens),
                            format_cost(row.stats.total_cost),
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
                    ])
                    .style(Style::default().fg(RatatuiColor::DarkGray)),
                );
            }

            let totals_header = vec!["Provider", "Tokens", "Cost", "Active Days"];
            let totals_widths = [
                Constraint::Min(20),
                Constraint::Length(16),
                Constraint::Length(14),
                Constraint::Length(14),
            ];

            let totals_table = create_ratatui_table(
                totals_rows,
                totals_header,
                &totals_widths,
                RatatuiColor::Magenta,
            );

            // Split the band into provider stats (left) + Claude / Codex quota
            // panels (right).
            let now = chrono::Local::now().timestamp();
            let bottom = RatatuiLayout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(46),
                    Constraint::Percentage(27),
                    Constraint::Percentage(27),
                ])
                .split(panel_area);
            f.render_widget(totals_table, bottom[0]);
            render_claude_quota(f, bottom[1], claude_rl, now);
            render_codex_quota(f, bottom[2], codex, now);
        }

        let total_cost_str = format_cost(totals.cost);
        let total_tokens_str = format_compact(totals.total);
        let entries_str = format!("{}", rows_data.len());

        let summary_items = vec![
            ("Total Cost:", total_cost_str.as_str(), RatatuiColor::Yellow),
            (
                "Total Tokens:",
                total_tokens_str.as_str(),
                RatatuiColor::Cyan,
            ),
            ("Models:", entries_str.as_str(), RatatuiColor::Blue),
        ];

        let summary = create_summary(summary_items, sys, pid);
        f.render_widget(summary, chunks.summary);

        f.render_widget(create_controls(), chunks.controls);
    })?;

    Ok(())
}

/// Maps a usage percentage to a traffic-light color (green/yellow/red).
fn gauge_color(pct: f64) -> RatatuiColor {
    if pct >= 90.0 {
        RatatuiColor::Red
    } else if pct >= 70.0 {
        RatatuiColor::Yellow
    } else {
        RatatuiColor::Green
    }
}

/// Renders a 5-segment mini bar like `▰▰▱▱▱` (any usage shows one block).
fn mini_bar(pct: f64) -> String {
    let filled = ((pct / 20.0).ceil() as i64).clamp(0, 5) as usize;
    (0..5).map(|i| if i < filled { '▰' } else { '▱' }).collect()
}

/// Builds one gauge line: `5h ▰▰▱▱▱  27%  ↻4h13m`.
fn quota_gauge_line(label: &str, w: &QuotaWindow, now: i64) -> Line<'static> {
    let pct = w.used_percent;
    let color = gauge_color(pct);
    let mut spans = vec![
        Span::styled(format!("{label} "), Style::default().fg(RatatuiColor::Gray)),
        Span::styled(mini_bar(pct), Style::default().fg(color)),
        Span::styled(format!(" {pct:>3.0}%"), Style::default().fg(color)),
    ];
    if let Some(reset) = w.resets_at_unix {
        spans.push(Span::styled(
            format!("  ↻{}", format_duration_until(reset, now)),
            Style::default().fg(RatatuiColor::DarkGray),
        ));
    }
    Line::from(spans)
}

/// Builds the "updated Xm ago" staleness line for the Claude panel.
///
/// The Claude cache only refreshes when Claude Code repaints its statusLine, so
/// staleness is expected; it is dimmed, escalating to yellow past 1h and red
/// past 6h.
fn staleness_line(fetched_at: i64, now: i64) -> Line<'static> {
    if fetched_at <= 0 {
        return dim_line("updated: never");
    }
    let age = (now - fetched_at).max(0);
    let color = if age > 6 * 3600 {
        RatatuiColor::Red
    } else if age > 3600 {
        RatatuiColor::Yellow
    } else {
        RatatuiColor::DarkGray
    };
    let ago = format_duration_until(now, fetched_at);
    let text = if ago == "now" {
        "updated just now".to_string()
    } else {
        format!("updated {ago} ago")
    };
    Line::from(Span::styled(text, Style::default().fg(color)))
}

/// A dim gray line for placeholder / hint text.
fn dim_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(RatatuiColor::DarkGray),
    ))
}

/// Renders the Claude quota panel (5h / 7d gauges + staleness).
fn render_claude_quota(f: &mut Frame, area: Rect, rl: Option<&ClaudeRateLimitsCache>, now: i64) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Claude ")
        .border_style(Style::default().fg(CLAUDE_COLOR));

    let lines: Vec<Line> = match rl {
        Some(rl) => {
            let mut v = Vec::new();
            if let Some(w) = &rl.five_hour {
                v.push(quota_gauge_line("5h", w, now));
            }
            if let Some(w) = &rl.seven_day {
                v.push(quota_gauge_line("7d", w, now));
            }
            if v.is_empty() {
                v.push(dim_line("no rate-limit data"));
            }
            v.push(staleness_line(rl.fetched_at, now));
            v
        }
        None => vec![
            dim_line("no statusLine data"),
            dim_line("add: vct statusline ingest"),
        ],
    };

    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Renders the Codex quota panel (plan, 5h / 7d gauges, credits).
fn render_codex_quota(f: &mut Frame, area: Rect, codex: &CodexQuotaSnapshot, now: i64) {
    let title = match codex.source {
        QuotaSource::Api => " Codex ",
        QuotaSource::SessionFallback => " Codex (session) ",
        QuotaSource::None => " Codex ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(CODEX_COLOR));

    let lines: Vec<Line> = if codex.source == QuotaSource::None {
        vec![
            dim_line("no Codex quota"),
            dim_line("(no auth.json / sessions)"),
        ]
    } else {
        let mut v = Vec::new();

        let mut plan_spans = vec![Span::styled(
            format!("Plan: {}", codex.plan_type.as_deref().unwrap_or("?")),
            Style::default().fg(RatatuiColor::Gray),
        )];
        if codex.limit_reached == Some(true) {
            plan_spans.push(Span::styled(
                "  LIMIT",
                Style::default()
                    .fg(RatatuiColor::Red)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        v.push(Line::from(plan_spans));

        if let Some(w) = &codex.primary {
            v.push(quota_gauge_line("5h", w, now));
        }
        if let Some(w) = &codex.secondary {
            v.push(quota_gauge_line("7d", w, now));
        }
        v.push(credits_line(codex));
        v
    };

    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Builds the credits line for the Codex panel.
fn credits_line(codex: &CodexQuotaSnapshot) -> Line<'static> {
    let mut s = String::from("Credits: ");
    if codex.unlimited == Some(true) {
        s.push_str("unlimited");
    } else if let Some(bal) = &codex.credits_balance {
        s.push_str(bal);
    } else {
        s.push('-');
    }
    if let Some(n) = codex.reset_credits_available
        && n > 0
    {
        s.push_str(&format!("  +{n} reset"));
    }
    Line::from(Span::styled(s, Style::default().fg(RatatuiColor::Gray)))
}
