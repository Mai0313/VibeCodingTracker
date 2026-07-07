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
    setup_terminal,
};
use crate::display::usage::averages::{
    UsageProviderTotals, UsageRow, UsageTotals, build_provider_total_rows, build_usage_summary,
};
use crate::models::{
    ClaudeQuotaSnapshot, CodexQuotaSnapshot, CopilotQuotaSnapshot, CursorQuotaSnapshot,
    PerProviderUsage, ProviderActiveDays, QuotaSource, QuotaWindow, UsageResult,
};
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use crate::quota::{
    CLAUDE_LOGIN_HINT, CODEX_LOGIN_HINT, COPILOT_LOGIN_HINT, CURSOR_LOGIN_HINT, ClaudeState,
    CodexState, CopilotState, CursorState, load_claude_cache, load_codex_cache, load_copilot_cache,
    load_cursor_cache, save_claude_cache, save_codex_cache, save_copilot_cache, save_cursor_cache,
    spawn_quota_worker,
};
use crate::utils::{
    format_compact, format_cost, format_duration_until, get_claude_credentials_path,
    get_copilot_config_path, get_cursor_auth_path, resolve_paths,
};
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

/// Minimum height for the bottom quota panels. Sized for the common case
/// (Claude: 5h/7d/scoped/balance/staleness; Codex: plan/5h/7d/credits/staleness)
/// plus the border. A rare overlap (extras line + login hint at once) clips the
/// least-critical bottom line, which `Paragraph` handles without panicking.
const QUOTA_PANEL_MIN_HEIGHT: u16 = 8;
/// Claude brand color for the quota panel border.
const CLAUDE_COLOR: RatatuiColor = RatatuiColor::Rgb(190, 116, 87);
/// Codex brand color for the quota panel border.
const CODEX_COLOR: RatatuiColor = RatatuiColor::Rgb(118, 127, 198);
/// Copilot brand color (GitHub green) for the quota panel border.
const COPILOT_COLOR: RatatuiColor = RatatuiColor::Rgb(46, 160, 67);
/// Cursor brand color (teal) for the quota panel border.
const CURSOR_COLOR: RatatuiColor = RatatuiColor::Rgb(64, 180, 180);

/// Minimum readable width for a single quota panel column (label + bar +
/// percent + reset). Below this a panel's gauge tail may clip.
const PANEL_MIN_W: u16 = 28;
/// Minimum width to keep the (slimmed) Provider Usage table inline in the band.
/// Matches the table's own column widths (Provider 16 + Tokens 16 + Cost 14)
/// so it is only kept when it can render without truncating; otherwise the band
/// drops it and the panels take the full width.
const BAND_TABLE_MIN_W: u16 = 46;
/// Terminal height needed to wrap the panels into a two-row grid.
const PANELS_2ROW_MIN_H: u16 = 26;

/// Which provider quota panels have credentials on this machine.
#[derive(Clone, Copy, Default)]
struct QuotaPresence {
    claude: bool,
    codex: bool,
    copilot: bool,
    cursor: bool,
}

impl QuotaPresence {
    /// Detects presence from each provider's credential file (once at launch).
    fn detect() -> Self {
        let claude = get_claude_credentials_path()
            .map(|p| p.exists())
            .unwrap_or(false);
        let codex = resolve_paths()
            .map(|p| p.codex_dir.join("auth.json").exists() || p.codex_session_dir.exists())
            .unwrap_or(false);
        let copilot = get_copilot_config_path()
            .map(|p| p.exists())
            .unwrap_or(false);
        let cursor = get_cursor_auth_path().map(|p| p.exists()).unwrap_or(false);
        Self {
            claude,
            codex,
            copilot,
            cursor,
        }
    }

    /// Number of provider quota panels present.
    fn count(&self) -> usize {
        self.claude as usize + self.codex as usize + self.copilot as usize + self.cursor as usize
    }
}

/// Borrowed quota state passed to the render frame.
struct QuotaView<'a> {
    claude: &'a ClaudeQuotaSnapshot,
    codex: &'a CodexQuotaSnapshot,
    copilot: &'a CopilotQuotaSnapshot,
    cursor: &'a CursorQuotaSnapshot,
    present: QuotaPresence,
}

/// How often the loop re-aggregates the session directories and repaints.
const USAGE_REFRESH_SECS: u64 = 10;
/// Claude quota worker cadence. Longer than Codex's 10s because the Claude
/// usage endpoint rate-limits frequent polling; quota moves slowly enough that
/// once a minute stays fresh.
const CLAUDE_REFRESH_SECS: u64 = 60;
/// Copilot quota worker cadence. GitHub's API is not tightly rate-limited here,
/// but quota moves slowly so a conservative once-a-minute poll is plenty.
const COPILOT_REFRESH_SECS: u64 = 60;
/// Cursor quota worker cadence. Re-reads `auth.json` each tick, so 60s keeps it
/// fresh without hammering cursor.com.
const CURSOR_REFRESH_SECS: u64 = 60;
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
/// - Scrollable model table (arrow keys / `PgUp`/`PgDn` / `g`/`G`)
/// - Keyboard controls: `q`, `Esc`, or `Ctrl+C` to exit, `r` to refresh
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

    // Each provider's quota is fetched on its own background thread so a
    // blocking (or slow) HTTP call never stalls the render loop or the other
    // providers. Panels are seeded from the last-known cache so they show
    // immediately on launch, and a worker is spawned only for a provider whose
    // credentials are present. All workers share one HTTP client and shutdown
    // flag.
    let present = QuotaPresence::detect();
    let quota_shutdown = Arc::new(AtomicBool::new(false));
    let claude_shared = Arc::new(Mutex::new(
        present
            .claude
            .then(load_claude_cache)
            .flatten()
            .unwrap_or_default(),
    ));
    let codex_shared = Arc::new(Mutex::new(
        present
            .codex
            .then(load_codex_cache)
            .flatten()
            .unwrap_or_default(),
    ));
    let copilot_shared = Arc::new(Mutex::new(
        present
            .copilot
            .then(load_copilot_cache)
            .flatten()
            .unwrap_or_default(),
    ));
    let cursor_shared = Arc::new(Mutex::new(
        present
            .cursor
            .then(load_cursor_cache)
            .flatten()
            .unwrap_or_default(),
    ));
    if present.claude || present.codex || present.copilot || present.cursor {
        match crate::quota::http::build_client() {
            Ok(client) => {
                if present.claude {
                    let (c, sh, shared) = (
                        client.clone(),
                        Arc::clone(&quota_shutdown),
                        Arc::clone(&claude_shared),
                    );
                    let mut state = ClaudeState::default();
                    spawn_quota_worker(
                        "claude",
                        shared,
                        sh,
                        CLAUDE_REFRESH_SECS,
                        move || state.resolve(&c),
                        |s| {
                            let _ = save_claude_cache(s);
                        },
                    );
                }
                if present.codex {
                    let (c, sh, shared) = (
                        client.clone(),
                        Arc::clone(&quota_shutdown),
                        Arc::clone(&codex_shared),
                    );
                    let mut state = CodexState::default();
                    spawn_quota_worker(
                        "codex",
                        shared,
                        sh,
                        crate::quota::provider::REFRESH_SECS,
                        move || state.resolve(&c),
                        |s| {
                            let _ = save_codex_cache(s);
                        },
                    );
                }
                if present.copilot {
                    let (c, sh, shared) = (
                        client.clone(),
                        Arc::clone(&quota_shutdown),
                        Arc::clone(&copilot_shared),
                    );
                    let mut state = CopilotState;
                    spawn_quota_worker(
                        "copilot",
                        shared,
                        sh,
                        COPILOT_REFRESH_SECS,
                        move || state.resolve(&c),
                        |s| {
                            let _ = save_copilot_cache(s);
                        },
                    );
                }
                if present.cursor {
                    let (c, sh, shared) = (
                        client.clone(),
                        Arc::clone(&quota_shutdown),
                        Arc::clone(&cursor_shared),
                    );
                    let mut state = CursorState;
                    spawn_quota_worker(
                        "cursor",
                        shared,
                        sh,
                        CURSOR_REFRESH_SECS,
                        move || state.resolve(&c),
                        |s| {
                            let _ = save_cursor_cache(s);
                        },
                    );
                }
            }
            Err(e) => log::warn!("quota workers disabled: failed to build HTTP client: {e}"),
        }
    }

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

    // Scroll/selection state for the model table (keyboard-driven).
    let mut scroll = ScrollState::new();

    // Latest rendered display state, kept across refresh cycles so a terminal
    // resize can redraw at the new size immediately without re-aggregating the
    // session directories. These are small per-model summaries, not the heavy
    // parse buffers, so holding onto them between refreshes is cheap.
    let mut rows_data: Vec<UsageRow> = Vec::new();
    let mut totals = UsageTotals::default();
    let mut provider_totals = UsageProviderTotals::default();
    // Quota panel state, cached across frames so a resize repaints without
    // re-reading the shared snapshots.
    let mut claude_snapshot = ClaudeQuotaSnapshot::default();
    let mut codex_snapshot = CodexQuotaSnapshot::default();
    let mut copilot_snapshot = CopilotQuotaSnapshot::default();
    let mut cursor_snapshot = CursorQuotaSnapshot::default();

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
            // Hide models that contributed neither tokens nor cost in this
            // range; they only add noise. A model can have zero tokens but a
            // nonzero cost (Claude per-query web search, or an OpenCode model
            // priced from its stored cost or a credit adjustment), so keep any
            // row that carries cost too. Otherwise it vanishes from the table
            // while its cost still counts toward the grand total, leaving the
            // two inconsistent. A negative (credit) cost counts just as much as
            // a positive one, so match on any nonzero value, not just > 0.
            rows_data.retain(|row| row.total != 0 || row.cost != 0.0);
            totals = summary.totals;
            provider_totals = summary.provider_totals;

            let model_names: Vec<String> = rows_data.iter().map(|row| row.model.clone()).collect();
            scroll.sync(prev_model.as_deref(), &model_names);

            // Refresh quota panels from each background worker's latest snapshot.
            claude_snapshot = claude_shared.lock().map(|g| g.clone()).unwrap_or_default();
            codex_snapshot = codex_shared.lock().map(|g| g.clone()).unwrap_or_default();
            copilot_snapshot = copilot_shared.lock().map(|g| g.clone()).unwrap_or_default();
            cursor_snapshot = cursor_shared.lock().map(|g| g.clone()).unwrap_or_default();

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
                &QuotaView {
                    claude: &claude_snapshot,
                    codex: &codex_snapshot,
                    copilot: &copilot_snapshot,
                    cursor: &cursor_snapshot,
                    present,
                },
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
                // Signal the detached workers to stop; the OS reclaims them on exit.
                quota_shutdown.store(true, Ordering::Relaxed);
                break;
            }
            InputAction::Refresh => refresh_state.force(),
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
                    &QuotaView {
                        claude: &claude_snapshot,
                        codex: &codex_snapshot,
                        copilot: &copilot_snapshot,
                        cursor: &cursor_snapshot,
                        present,
                    },
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
                &QuotaView {
                    claude: &claude_snapshot,
                    codex: &codex_snapshot,
                    copilot: &copilot_snapshot,
                    cursor: &cursor_snapshot,
                    present,
                },
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
    quota: &QuotaView,
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
        // Decide how the band arranges the present quota panels (and whether the
        // slimmed Provider Usage table shares the row) from the terminal size,
        // then size the band accordingly. The band spans the full width, so the
        // arrange decision uses `area.width`.
        let n = quota.present.count();
        let arrange = arrange_band(area.width, area.height, n);
        let panels_height =
            (area.height >= USAGE_PANELS_MIN_H).then(|| band_height(&arrange, provider_rows.len()));
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

        // One selectable row per model. The grand total lives only in the
        // summary bar below (it was redundant here and in the provider band).
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
            let grid = split_band(panel_area, &arrange, n);

            // The (slimmed) Provider Usage table is only shown when the band
            // keeps a cell for it; otherwise the panels take the whole band and
            // the scrollable per-model table above carries the per-provider view.
            if let Some(table_area) = grid.table {
                // Drop the "All Providers" aggregate; the summary bar already
                // carries the grand totals.
                let mut totals_rows: Vec<RatatuiRow> = provider_rows
                    .iter()
                    .filter(|row| row.label != "All Providers")
                    .map(|row| {
                        create_provider_row(
                            vec![
                                row.label.to_string(),
                                format_compact(row.stats.total_tokens),
                                format_cost(row.stats.total_cost),
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
                        ])
                        .style(Style::default().fg(RatatuiColor::DarkGray)),
                    );
                }

                let totals_header = vec!["Provider", "Tokens", "Cost"];
                let totals_widths = [
                    Constraint::Min(16),
                    Constraint::Length(16),
                    Constraint::Length(14),
                ];

                let totals_table = create_ratatui_table(
                    totals_rows,
                    totals_header,
                    &totals_widths,
                    RatatuiColor::Magenta,
                );
                f.render_widget(totals_table, table_area);
            }

            // Render present panels in fixed order (Claude → Codex → Copilot →
            // Cursor) into the grid cells; a missing provider consumes no cell.
            let now = chrono::Local::now().timestamp();
            let mut idx = 0;
            if quota.present.claude {
                render_claude_quota(f, grid.panels[idx], quota.claude, now);
                idx += 1;
            }
            if quota.present.codex {
                render_codex_quota(f, grid.panels[idx], quota.codex, now);
                idx += 1;
            }
            if quota.present.copilot {
                render_copilot_quota(f, grid.panels[idx], quota.copilot, now);
                idx += 1;
            }
            if quota.present.cursor {
                render_cursor_quota(f, grid.panels[idx], quota.cursor, now);
            }
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

/// Builds one gauge line: `5h     ▰▰▱▱▱   27%  ↻ 4h13m`.
///
/// The label is padded to a fixed width so the bars line up across `5h` / `7d`
/// and the longer per-model labels ("Opus" / "Sonnet").
fn quota_gauge_line(label: &str, w: &QuotaWindow, now: i64) -> Line<'static> {
    let pct = w.used_percent;
    let color = gauge_color(pct);
    let mut spans = vec![
        Span::styled(
            format!("{label:<6} "),
            Style::default().fg(RatatuiColor::Gray),
        ),
        Span::styled(mini_bar(pct), Style::default().fg(color)),
        Span::styled(format!(" {pct:>3.0}%"), Style::default().fg(color)),
    ];
    if let Some(reset) = w.resets_at_unix {
        spans.push(Span::styled(
            format!("  ↻ {}", format_duration_until(reset, now)),
            Style::default().fg(RatatuiColor::DarkGray),
        ));
    }
    Line::from(spans)
}

/// Like [`quota_gauge_line`] but labels the bar with a caller-supplied value
/// (e.g. `36/1500`) instead of a percentage, and carries no reset marker. `pct`
/// still drives the bar fill and its traffic-light color; used for the Copilot
/// request-count gauge, which shares the `prem` line's reset window.
fn quota_gauge_line_value(label: &str, pct: f64, value: &str) -> Line<'static> {
    let color = gauge_color(pct);
    Line::from(vec![
        Span::styled(
            format!("{label:<6} "),
            Style::default().fg(RatatuiColor::Gray),
        ),
        Span::styled(mini_bar(pct), Style::default().fg(color)),
        Span::styled(format!(" {value}"), Style::default().fg(color)),
    ])
}

/// Builds the "updated Xm ago" staleness line, from the last successful fetch.
///
/// Dimmed by default, escalating to yellow past 1h and red past 6h so a panel
/// stuck on stale data (e.g. persistent auth failure) reads as such.
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

/// A red login-hint line shown when a provider's token needs a re-login.
fn login_hint_line(hint: &str) -> Line<'static> {
    Line::from(Span::styled(
        hint.to_string(),
        Style::default()
            .fg(RatatuiColor::Red)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Resolved band arrangement (pure; independent of the band `Rect`).
enum BandArrange {
    /// One row: an optional Provider Usage table cell, then the panel cells.
    SingleRow { table: bool },
    /// Two rows: `top` panels on the first row, the rest on the second; a
    /// trailing empty cell on the second row is filled with the table.
    TwoRow { top: usize, table_in_hole: bool },
}

/// Decides the band arrangement from band width, terminal height, and the
/// number of present quota panels.
///
/// Preference order: table + all panels in one row → panels-only in one row
/// (table dropped as redundant with the scrollable table) → a two-row grid →
/// a last-resort even split that may clip a gauge tail but never panics.
fn arrange_band(w: u16, area_h: u16, n: usize) -> BandArrange {
    if n == 0 {
        return BandArrange::SingleRow { table: true };
    }
    let panels_w = PANEL_MIN_W.saturating_mul(n as u16);
    if w >= BAND_TABLE_MIN_W + panels_w {
        BandArrange::SingleRow { table: true }
    } else if w >= panels_w {
        BandArrange::SingleRow { table: false }
    } else if area_h >= PANELS_2ROW_MIN_H {
        let top = n.div_ceil(2);
        BandArrange::TwoRow {
            top,
            table_in_hole: top * 2 > n,
        }
    } else {
        BandArrange::SingleRow { table: false }
    }
}

/// The band height fed to `main_layout` (computed before the band `Rect` exists).
fn band_height(arrange: &BandArrange, provider_rows: usize) -> u16 {
    match arrange {
        BandArrange::SingleRow { table: true } => (provider_rows as u16)
            .saturating_add(4)
            .max(QUOTA_PANEL_MIN_HEIGHT),
        BandArrange::SingleRow { table: false } => QUOTA_PANEL_MIN_HEIGHT,
        BandArrange::TwoRow { .. } => QUOTA_PANEL_MIN_HEIGHT.saturating_mul(2),
    }
}

/// The band split into an optional table cell plus one cell per present panel.
struct BandGrid {
    table: Option<Rect>,
    panels: Vec<Rect>,
}

/// Splits a horizontal rect into `k` equal columns.
fn split_even(area: Rect, k: usize) -> Vec<Rect> {
    if k == 0 {
        return Vec::new();
    }
    let cons = vec![Constraint::Ratio(1, k as u32); k];
    RatatuiLayout::default()
        .direction(Direction::Horizontal)
        .constraints(cons)
        .split(area)
        .to_vec()
}

/// Splits the resolved band `Rect` into the table cell + ordered panel cells.
///
/// `panels` always has exactly `n` entries so the render dispatch can index it
/// by present-provider order without bounds concerns.
fn split_band(band: Rect, arrange: &BandArrange, n: usize) -> BandGrid {
    match *arrange {
        BandArrange::SingleRow { table } => {
            let mut cons: Vec<Constraint> = Vec::new();
            if table {
                cons.push(Constraint::Min(BAND_TABLE_MIN_W));
            }
            // Give each panel an equal share of whatever the table leaves.
            let panel_span = if table {
                band.width.saturating_sub(BAND_TABLE_MIN_W)
            } else {
                band.width
            };
            let pw = if n > 0 {
                (panel_span / n as u16).max(PANEL_MIN_W)
            } else {
                PANEL_MIN_W
            };
            for _ in 0..n {
                cons.push(Constraint::Length(pw));
            }
            let cells = RatatuiLayout::default()
                .direction(Direction::Horizontal)
                .constraints(cons)
                .split(band);
            let (table_rect, off) = if table {
                (Some(cells[0]), 1)
            } else {
                (None, 0)
            };
            BandGrid {
                table: table_rect,
                panels: cells[off..off + n].to_vec(),
            }
        }
        BandArrange::TwoRow { top, table_in_hole } => {
            let rows = RatatuiLayout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(QUOTA_PANEL_MIN_HEIGHT),
                    Constraint::Length(QUOTA_PANEL_MIN_HEIGHT),
                ])
                .split(band);
            let bottom = n - top;
            let row0 = split_even(rows[0], top);
            let row1 = split_even(rows[1], bottom + table_in_hole as usize);
            let mut panels = row0;
            panels.extend_from_slice(&row1[..bottom]);
            let table_rect = table_in_hole.then(|| row1[bottom]);
            BandGrid {
                table: table_rect,
                panels,
            }
        }
    }
}

/// Builds a quota-panel block: provider title on the left, plus a red bold
/// `LIMIT` flag right-aligned in the top border when a cap is hit. Shared by
/// both panels so they flag limits identically.
fn quota_block(title: &str, border: RatatuiColor, limit_reached: bool) -> Block<'static> {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title.to_string()))
        .border_style(Style::default().fg(border));
    if limit_reached {
        block = block.title(
            Line::from(Span::styled(
                "LIMIT ",
                Style::default()
                    .fg(RatatuiColor::Red)
                    .add_modifier(Modifier::BOLD),
            ))
            .right_aligned(),
        );
    }
    block
}

/// Renders the Claude quota panel (5h / 7d / scoped gauges + balance +
/// staleness + login hint).
fn render_claude_quota(f: &mut Frame, area: Rect, claude: &ClaudeQuotaSnapshot, now: i64) {
    let block = quota_block(" Claude ", CLAUDE_COLOR, claude.limit_reached);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(plan) = &claude.plan_type {
        lines.push(plan_line(plan));
    }
    // Track windows separately so a lone Plan line does not count as "has data".
    let mut has_data = false;
    if let Some(w) = &claude.five_hour {
        lines.push(quota_gauge_line("5h", w, now));
        has_data = true;
    }
    if let Some(w) = &claude.seven_day {
        lines.push(quota_gauge_line("7d", w, now));
        has_data = true;
    }
    // The per-model weekly cap (Fable today) is volatile on Anthropic's side, so
    // it is only drawn when both the window and its model label are present.
    if let (Some(w), Some(label)) = (&claude.scoped_weekly, &claude.scoped_label) {
        lines.push(quota_gauge_line(label, w, now));
        has_data = true;
    }
    if has_data {
        lines.push(claude_balance_line(claude));
        lines.push(staleness_line(claude.fetched_at, now));
    }
    if claude.needs_login {
        lines.push(login_hint_line(CLAUDE_LOGIN_HINT));
    } else if !has_data {
        lines.push(dim_line("no rate-limit data"));
    }

    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Renders the Codex quota panel (plan, 5h / 7d gauges, credits, extras,
/// staleness).
fn render_codex_quota(f: &mut Frame, area: Rect, codex: &CodexQuotaSnapshot, now: i64) {
    let title = match codex.source {
        QuotaSource::Api => " Codex ",
        QuotaSource::SessionFallback => " Codex (session) ",
        QuotaSource::None => " Codex ",
    };
    let block = quota_block(title, CODEX_COLOR, codex.limit_reached == Some(true));

    let lines: Vec<Line> = if codex.source == QuotaSource::None {
        let mut v = vec![dim_line("no Codex quota")];
        if codex.needs_login {
            v.push(login_hint_line(CODEX_LOGIN_HINT));
        } else {
            v.push(dim_line("(no auth.json / sessions)"));
        }
        v
    } else {
        let mut v = vec![plan_line(codex.plan_type.as_deref().unwrap_or("?"))];

        if let Some(w) = &codex.primary {
            v.push(quota_gauge_line("5h", w, now));
        }
        if let Some(w) = &codex.secondary {
            v.push(quota_gauge_line("7d", w, now));
        }
        // Keep session-fallback data visible but flag the re-login (S3).
        if codex.needs_login {
            v.push(login_hint_line(CODEX_LOGIN_HINT));
        } else {
            v.push(credits_line(codex));
            if let Some(extra) = codex_extras_line(codex) {
                v.push(extra);
            }
        }
        v.push(staleness_line(codex.fetched_at, now));
        v
    };

    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Renders the Copilot quota panel (plan, premium percent gauge, premium
/// request-count gauge, staleness + login hint).
fn render_copilot_quota(f: &mut Frame, area: Rect, copilot: &CopilotQuotaSnapshot, now: i64) {
    let block = quota_block(" Copilot ", COPILOT_COLOR, copilot.limit_reached);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(plan) = &copilot.plan_type {
        lines.push(plan_line(plan));
    }
    if let Some(w) = &copilot.premium {
        lines.push(quota_gauge_line("prem", w, now));
        // A second gauge showing the premium requests as used/total counts.
        if let (Some(rem), Some(total)) = (copilot.premium_remaining, copilot.premium_entitlement)
            && total > 0
        {
            let used = (total - rem).max(0);
            let pct = (used as f64 / total as f64) * 100.0;
            lines.push(quota_gauge_line_value(
                "reqs",
                pct,
                &format!("{used}/{total}"),
            ));
        }
    } else if copilot.premium_unlimited {
        lines.push(dim_line("premium: unlimited"));
    }
    let has_content = !lines.is_empty();
    if has_content {
        lines.push(staleness_line(copilot.fetched_at, now));
    }
    if copilot.needs_login {
        lines.push(login_hint_line(COPILOT_LOGIN_HINT));
    } else if !has_content {
        lines.push(dim_line("no Copilot quota"));
    }

    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Renders the Cursor quota panel (plan, total / auto / api gauges, optional
/// on-demand spend, staleness + login hint).
fn render_cursor_quota(f: &mut Frame, area: Rect, cursor: &CursorQuotaSnapshot, now: i64) {
    let block = quota_block(" Cursor ", CURSOR_COLOR, cursor.limit_reached);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(plan) = &cursor.plan_type {
        lines.push(plan_line(plan));
    }
    if let Some(w) = &cursor.total {
        lines.push(quota_gauge_line("total", w, now));
    }
    if let Some(w) = &cursor.auto {
        lines.push(quota_gauge_line("auto", w, now));
    }
    if let Some(w) = &cursor.api {
        lines.push(quota_gauge_line("api", w, now));
    }
    if let Some(d) = cursor.on_demand_dollars {
        lines.push(dim_line(&format!("on-demand: ${d:.2}")));
    }
    let has_content = !lines.is_empty();
    if has_content {
        lines.push(staleness_line(cursor.fetched_at, now));
    }
    if cursor.needs_login {
        lines.push(login_hint_line(CURSOR_LOGIN_HINT));
    } else if !has_content {
        lines.push(dim_line("no Cursor quota"));
    }

    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Builds the `Plan: <tier>` line shared by both panels.
fn plan_line(plan: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("Plan: {plan}"),
        Style::default().fg(RatatuiColor::Gray),
    ))
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

/// Builds the optional Codex extras line (`~L-H msgs  ·  cap $X`), shown only
/// when the account has credit-funded messages or a configured spend cap.
fn codex_extras_line(codex: &CodexQuotaSnapshot) -> Option<Line<'static>> {
    let mut parts: Vec<String> = Vec::new();
    if let Some((low, high)) = codex.approx_messages {
        if low == high {
            parts.push(format!("~{low} msgs"));
        } else {
            parts.push(format!("~{low}-{high} msgs"));
        }
    }
    if let Some(cap) = codex.spend_limit {
        let cap_str = if cap.fract() == 0.0 {
            format!("${cap:.0}")
        } else {
            format!("${cap:.2}")
        };
        parts.push(format!("cap {cap_str}"));
    }
    if parts.is_empty() {
        return None;
    }
    Some(Line::from(Span::styled(
        parts.join("  ·  "),
        Style::default().fg(RatatuiColor::DarkGray),
    )))
}

/// Builds the balance line for the Claude panel (mirrors Codex's credits line).
fn claude_balance_line(claude: &ClaudeQuotaSnapshot) -> Line<'static> {
    let mut s = String::from("Balance: ");
    match &claude.balance {
        Some(b) => s.push_str(b),
        None => s.push('-'),
    }
    if let Some(used) = &claude.spend_used {
        s.push_str(&format!("    {used} used"));
    }
    Line::from(Span::styled(s, Style::default().fg(RatatuiColor::Gray)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrange_wide_keeps_table_in_one_row() {
        // Plenty of width for the table + 4 panels.
        match arrange_band(BAND_TABLE_MIN_W + PANEL_MIN_W * 4, 40, 4) {
            BandArrange::SingleRow { table } => assert!(table),
            _ => panic!("expected single row with table"),
        }
    }

    #[test]
    fn arrange_medium_drops_table_but_stays_one_row() {
        // Enough for 4 panels but not the table alongside them.
        match arrange_band(PANEL_MIN_W * 4, 40, 4) {
            BandArrange::SingleRow { table } => assert!(!table),
            _ => panic!("expected single row without table"),
        }
    }

    #[test]
    fn arrange_narrow_tall_wraps_to_two_rows() {
        match arrange_band(80, PANELS_2ROW_MIN_H, 4) {
            BandArrange::TwoRow { top, table_in_hole } => {
                assert_eq!(top, 2);
                assert!(!table_in_hole, "even count fills both rows");
            }
            _ => panic!("expected two-row grid"),
        }
    }

    #[test]
    fn arrange_three_panels_fills_hole_with_table() {
        match arrange_band(80, PANELS_2ROW_MIN_H, 3) {
            BandArrange::TwoRow { top, table_in_hole } => {
                assert_eq!(top, 2);
                assert!(table_in_hole, "odd count leaves a hole for the table");
            }
            _ => panic!("expected two-row grid"),
        }
    }

    #[test]
    fn arrange_narrow_short_falls_back_to_single_row() {
        // Too narrow for one row and too short for two: last-resort even split.
        match arrange_band(80, PANELS_2ROW_MIN_H - 1, 4) {
            BandArrange::SingleRow { table } => assert!(!table),
            _ => panic!("expected single-row fallback"),
        }
    }

    #[test]
    fn arrange_zero_panels_is_table_only() {
        match arrange_band(100, 40, 0) {
            BandArrange::SingleRow { table } => assert!(table),
            _ => panic!("expected table-only row"),
        }
    }

    #[test]
    fn split_band_always_yields_exactly_n_panels() {
        let area = Rect::new(0, 0, 200, 16);
        for n in 0..=4 {
            let arrange = arrange_band(area.width, area.height, n);
            let grid = split_band(area, &arrange, n);
            assert_eq!(grid.panels.len(), n, "n={n}");
        }
    }

    #[test]
    fn split_band_two_row_three_panels_has_table_in_hole() {
        let area = Rect::new(0, 0, 80, 16);
        let arrange = BandArrange::TwoRow {
            top: 2,
            table_in_hole: true,
        };
        let grid = split_band(area, &arrange, 3);
        assert_eq!(grid.panels.len(), 3);
        assert!(grid.table.is_some(), "the hole is filled with the table");
    }
}
