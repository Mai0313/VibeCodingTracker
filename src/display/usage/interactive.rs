//! Auto-refreshing TUI for the usage view.
//!
//! Runs a render loop that incrementally re-aggregates the session directories
//! every `refresh_secs` seconds (from `config.toml`), reusing one pricing map
//! for the current UTC day and highlighting rows whose tokens changed since
//! the last tick. The loop holds only the small per-model display state
//! between frames so a resize repaints instantly without re-aggregating; memory
//! is trimmed back to the OS after each refresh.

use crate::config::ProvidersConfig;
use crate::display::common::ProviderTotal;
use crate::display::common::table::{
    create_controls_with_status, create_provider_row, create_ratatui_table, create_summary,
    init_process_metrics, main_layout, refresh_process_metrics, render_scrollable_table,
    render_too_small, styled_row,
};
use crate::display::common::tui::{
    InputAction, RefreshWorker, RefreshWorkerError, ScrollState, TerminalSession, UpdateTracker,
    handle_input, overlay_repo_hyperlink, refresh_status, render_loading_frame,
};
use crate::display::usage::averages::{
    ProviderStats, UsageProviderTotals, UsageRow, UsageTotals, build_provider_total_rows,
    build_usage_summary_from_data, merge_rows_by_base_model,
};
use crate::models::{
    ClaudeQuotaSnapshot, CodexQuotaSnapshot, CopilotQuotaSnapshot, CursorQuotaSnapshot,
    QuotaSource, QuotaWindow,
};
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use crate::quota::{
    CLAUDE_LOGIN_HINT, CODEX_LOGIN_HINT, COPILOT_LOGIN_HINT, CURSOR_LOGIN_HINT, ClaudeState,
    CodexState, CopilotState, CursorState, load_claude_cache, load_codex_cache, load_copilot_cache,
    load_cursor_cache, save_claude_cache, save_codex_cache, save_copilot_cache, save_cursor_cache,
    spawn_quota_worker,
};
use crate::summary_cache::{SummaryScanCache, build_scan_pool};
use crate::utils::{
    format_compact, format_cost, format_cost_compact, format_duration_until,
    get_claude_credentials_path, get_copilot_config_path, get_cursor_auth_path, resolve_paths,
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, CrosstermBackend, TestBackend},
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
/// (Claude: 5h/7d/scoped/balance/staleness; Codex:
/// plan/5h/7d/credits/reset-expiry/staleness) plus the border. A rare overlap
/// clips the least-critical bottom line, which `Paragraph` handles safely.
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
/// Matches the table's own column widths (Provider 9 + Tokens 11 + Cost 11 plus
/// borders/spacing) so it is only kept when it can render without truncating;
/// otherwise the band drops it and the panels take the full width.
const BAND_TABLE_MIN_W: u16 = 38;
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
    /// Whether the bottom band is shown at all. `false` when `usage.quota.panels`
    /// is empty, which drops the whole band (panels *and* the Provider Usage
    /// table), not just the individual gauges.
    band_enabled: bool,
}

/// Upper bound on rows the [`UpdateTracker`] remembers for change highlighting.
const MAX_TRACKED_ROWS: usize = 100;

/// Hard minimum terminal width/height; below this only a notice is drawn.
const USAGE_MIN_W: u16 = 74;
const USAGE_MIN_H: u16 = 14;
/// At or above this height the provider/quota band is shown; below it the band
/// is dropped so the scrollable table keeps a usable height.
const USAGE_PANELS_MIN_H: u16 = 22;
/// Minimum combined height reserved for the model table, summary, and controls.
const USAGE_NON_PANEL_MIN_H: u16 = 10;

struct UsageRefreshPayload {
    rows: Vec<UsageRow>,
    merged_rows: Vec<UsageRow>,
    totals: UsageTotals,
    provider_totals: UsageProviderTotals,
}

struct QuotaShutdownGuard {
    shutdown: Arc<AtomicBool>,
}

impl Drop for QuotaShutdownGuard {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

struct QuotaRuntime {
    present: QuotaPresence,
    band_enabled: bool,
    claude: Arc<Mutex<ClaudeQuotaSnapshot>>,
    codex: Arc<Mutex<CodexQuotaSnapshot>>,
    copilot: Arc<Mutex<CopilotQuotaSnapshot>>,
    cursor: Arc<Mutex<CursorQuotaSnapshot>>,
    _guard: QuotaShutdownGuard,
}

impl QuotaRuntime {
    fn start(quota_panels: &[String], providers: ProvidersConfig, quota_refresh_secs: u64) -> Self {
        let panel_on = |name: &str| crate::config::quota_panel_selected(quota_panels, name);
        let band_enabled = !quota_panels.is_empty();
        let mut present = if band_enabled {
            QuotaPresence::detect()
        } else {
            QuotaPresence::default()
        };
        present.claude &= providers.claude && panel_on("claude");
        present.codex &= providers.codex && panel_on("codex");
        present.copilot &= providers.copilot && panel_on("copilot");
        present.cursor &= providers.cursor && panel_on("cursor");

        let shutdown = Arc::new(AtomicBool::new(false));
        let claude = Arc::new(Mutex::new(
            present
                .claude
                .then(load_claude_cache)
                .flatten()
                .unwrap_or_default(),
        ));
        let codex = Arc::new(Mutex::new(
            present
                .codex
                .then(load_codex_cache)
                .flatten()
                .unwrap_or_default(),
        ));
        let copilot = Arc::new(Mutex::new(
            present
                .copilot
                .then(load_copilot_cache)
                .flatten()
                .unwrap_or_default(),
        ));
        let cursor = Arc::new(Mutex::new(
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
                        let (client, shutdown, shared) =
                            (client.clone(), Arc::clone(&shutdown), Arc::clone(&claude));
                        let mut state = ClaudeState::default();
                        spawn_quota_worker(
                            "claude",
                            shared,
                            shutdown,
                            quota_refresh_secs,
                            move || state.resolve(&client),
                            |snapshot| {
                                let _ = save_claude_cache(snapshot);
                            },
                        );
                    }
                    if present.codex {
                        let (client, shutdown, shared) =
                            (client.clone(), Arc::clone(&shutdown), Arc::clone(&codex));
                        let mut state = CodexState::default();
                        spawn_quota_worker(
                            "codex",
                            shared,
                            shutdown,
                            quota_refresh_secs,
                            move || state.resolve(&client),
                            |snapshot| {
                                let _ = save_codex_cache(snapshot);
                            },
                        );
                    }
                    if present.copilot {
                        let (client, shutdown, shared) =
                            (client.clone(), Arc::clone(&shutdown), Arc::clone(&copilot));
                        let mut state = CopilotState;
                        spawn_quota_worker(
                            "copilot",
                            shared,
                            shutdown,
                            quota_refresh_secs,
                            move || state.resolve(&client),
                            |snapshot| {
                                let _ = save_copilot_cache(snapshot);
                            },
                        );
                    }
                    if present.cursor {
                        let (client, shutdown, shared) =
                            (client.clone(), Arc::clone(&shutdown), Arc::clone(&cursor));
                        let mut state = CursorState;
                        spawn_quota_worker(
                            "cursor",
                            shared,
                            shutdown,
                            quota_refresh_secs,
                            move || state.resolve(&client),
                            |snapshot| {
                                let _ = save_cursor_cache(snapshot);
                            },
                        );
                    }
                }
                Err(error) => {
                    log::warn!("quota workers disabled: failed to build HTTP client: {error}")
                }
            }
        }

        Self {
            present,
            band_enabled,
            claude,
            codex,
            copilot,
            cursor,
            _guard: QuotaShutdownGuard { shutdown },
        }
    }
}

struct UsageUiState {
    rows: Vec<UsageRow>,
    merged_rows: Vec<UsageRow>,
    totals: UsageTotals,
    provider_totals: UsageProviderTotals,
    update_tracker: UpdateTracker,
    scroll: ScrollState,
    merge_enabled: bool,
    claude: ClaudeQuotaSnapshot,
    codex: CodexQuotaSnapshot,
    copilot: CopilotQuotaSnapshot,
    cursor: CursorQuotaSnapshot,
}

impl UsageUiState {
    fn new(merge_enabled: bool) -> Self {
        Self {
            rows: Vec::new(),
            merged_rows: Vec::new(),
            totals: UsageTotals::default(),
            provider_totals: UsageProviderTotals::default(),
            update_tracker: UpdateTracker::new(MAX_TRACKED_ROWS, 1000),
            scroll: ScrollState::new(),
            merge_enabled,
            claude: ClaudeQuotaSnapshot::default(),
            codex: CodexQuotaSnapshot::default(),
            copilot: CopilotQuotaSnapshot::default(),
            cursor: CursorQuotaSnapshot::default(),
        }
    }

    fn view(&self) -> &[UsageRow] {
        current_view(self.merge_enabled, &self.rows, &self.merged_rows)
    }

    fn apply(&mut self, payload: UsageRefreshPayload) {
        let previous = self
            .scroll
            .table
            .selected()
            .and_then(|index| self.view().get(index))
            .map(|row| row.model.clone());
        self.rows = payload.rows;
        self.merged_rows = payload.merged_rows;
        self.totals = payload.totals;
        self.provider_totals = payload.provider_totals;

        let fingerprints: Vec<_> = self
            .view()
            .iter()
            .map(|row| (row.model.clone(), row_fingerprint(row)))
            .collect();
        let models: Vec<_> = fingerprints
            .iter()
            .map(|(model, _)| model.clone())
            .collect();
        self.scroll.sync(previous.as_deref(), &models);
        self.update_tracker.cleanup(models);
        for (model, fingerprint) in fingerprints {
            self.update_tracker.track_update(model, &fingerprint);
        }
    }

    fn toggle_merge(&mut self) {
        let previous = self
            .scroll
            .table
            .selected()
            .and_then(|index| self.view().get(index))
            .map(|row| row.model.clone());
        self.merge_enabled = !self.merge_enabled;
        let _ = crate::config::save_merge_models(self.merge_enabled);
        let fingerprints: Vec<_> = self
            .view()
            .iter()
            .map(|row| (row.model.clone(), row_fingerprint(row)))
            .collect();
        let models: Vec<_> = fingerprints
            .iter()
            .map(|(model, _)| model.clone())
            .collect();
        self.scroll.sync(previous.as_deref(), &models);
        self.update_tracker.cleanup(models);
        for (model, fingerprint) in fingerprints {
            self.update_tracker.prime(model, &fingerprint);
        }
    }

    fn refresh_quota(&mut self, runtime: &QuotaRuntime) {
        self.claude = runtime
            .claude
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        self.codex = runtime
            .codex
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        self.copilot = runtime
            .copilot
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        self.cursor = runtime
            .cursor
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
    }

    fn render(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        sys: &System,
        pid: Pid,
        runtime: &QuotaRuntime,
        status: Option<&str>,
    ) -> anyhow::Result<()> {
        self.refresh_quota(runtime);
        let quota = QuotaView {
            claude: &self.claude,
            codex: &self.codex,
            copilot: &self.copilot,
            cursor: &self.cursor,
            present: runtime.present,
            band_enabled: runtime.band_enabled,
        };
        let rows = current_view(self.merge_enabled, &self.rows, &self.merged_rows);
        render_usage_frame_with_status(
            terminal,
            rows,
            &self.totals,
            &self.provider_totals,
            &self.update_tracker,
            sys,
            pid,
            &quota,
            &mut self.scroll,
            self.merge_enabled,
            status,
            true,
        )
    }
}

/// Displays usage with a dedicated scan pool supplied by the CLI.
#[allow(clippy::too_many_arguments)]
pub fn display_usage_interactive_with_pool(
    time_range: crate::cli::TimeRange,
    merge_providers: bool,
    quota_panels: Vec<String>,
    providers: ProvidersConfig,
    refresh_secs: u64,
    quota_refresh_secs: u64,
    scan_pool: Arc<rayon::ThreadPool>,
) -> anyhow::Result<()> {
    let mut terminal = TerminalSession::new()?;
    let result = (|| -> anyhow::Result<()> {
        let mut spinner_index = 0usize;
        render_loading_frame(terminal.terminal_mut(), spinner_index)?;

        let paths = resolve_paths()?;
        let quota = QuotaRuntime::start(&quota_panels, providers, quota_refresh_secs);
        let worker_paths = paths.clone();
        let worker_pool = Arc::clone(&scan_pool);
        let mut worker = RefreshWorker::new_with_init(refresh_secs, move || {
            let mut cache = SummaryScanCache::new();
            let mut pricing = ModelPricingMap::new(HashMap::new());
            let mut loaded_pricing_utc_date = None;
            move || {
                let today = chrono::Utc::now().date_naive();
                if loaded_pricing_utc_date != Some(today) {
                    match fetch_model_pricing() {
                        Ok(map) => {
                            pricing = map;
                            loaded_pricing_utc_date = Some(today);
                        }
                        Err(error) => {
                            log::warn!("failed to refresh pricing: {error}");
                        }
                    }
                }

                let collection = worker_pool.install(|| {
                    crate::usage::get_usage_from_paths_with_cache(
                        &worker_paths,
                        time_range,
                        providers,
                        &mut cache,
                    )
                })?;
                if collection.diagnostics.all_failed() {
                    let first = collection
                        .diagnostics
                        .failures
                        .first()
                        .map(|failure| failure.error.as_str())
                        .unwrap_or("unknown source failure");
                    anyhow::bail!(
                        "failed to parse all {} usage sources: {first}",
                        collection.diagnostics.candidates
                    );
                }
                if collection.diagnostics.partially_failed() {
                    log::warn!(
                        "usage refresh kept partial data after {} source failures",
                        collection.diagnostics.failures.len()
                    );
                }

                let mut summary = build_usage_summary_from_data(&collection.data, &pricing);
                summary.rows.retain(|row| row.total != 0 || row.cost != 0.0);
                let merged_rows = merge_rows_by_base_model(&summary.rows);
                Ok(UsageRefreshPayload {
                    rows: summary.rows,
                    merged_rows,
                    totals: summary.totals,
                    provider_totals: summary.provider_totals,
                })
            }
        });
        worker.request();

        let pid = sysinfo::get_current_pid()
            .expect("Failed to get current process ID for memory monitoring");
        let mut sys = System::new();
        init_process_metrics(&mut sys, pid);
        let metrics_interval = Duration::from_millis(crate::constants::refresh::METRICS_REFRESH_MS);
        let mut last_metrics = Instant::now();
        let mut last_spinner = Instant::now();
        let mut state = UsageUiState::new(merge_providers);
        let mut loaded = false;
        let mut failure_until = None;

        loop {
            if let Some(result) = worker.try_result() {
                match result {
                    Ok(payload) => {
                        state.apply(payload);
                        loaded = true;
                        failure_until = None;
                        refresh_process_metrics(&mut sys, pid);
                        state.render(
                            terminal.terminal_mut(),
                            &sys,
                            pid,
                            &quota,
                            refresh_status(worker.is_active(), failure_until),
                        )?;
                        crate::utils::release_freed_heap();
                        last_metrics = Instant::now();
                    }
                    Err(RefreshWorkerError::Disconnected) => {
                        return Err(anyhow::anyhow!("refresh worker disconnected"));
                    }
                    Err(error) if !loaded => {
                        return Err(anyhow::anyhow!("initial usage load failed: {error}"));
                    }
                    Err(error) => {
                        log::warn!("usage refresh failed: {error}");
                        failure_until = Some(Instant::now() + Duration::from_secs(3));
                        state.render(
                            terminal.terminal_mut(),
                            &sys,
                            pid,
                            &quota,
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
                state.render(terminal.terminal_mut(), &sys, pid, &quota, status)?;
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
                            &quota,
                            refresh_status(worker.is_active(), failure_until),
                        )?;
                    }
                }
                InputAction::ToggleMerge => {
                    state.toggle_merge();
                    if loaded {
                        state.render(
                            terminal.terminal_mut(),
                            &sys,
                            pid,
                            &quota,
                            refresh_status(worker.is_active(), failure_until),
                        )?;
                    }
                }
                InputAction::Navigate(delta) if loaded => {
                    state.scroll.apply(delta, state.view().len());
                    state.render(
                        terminal.terminal_mut(),
                        &sys,
                        pid,
                        &quota,
                        refresh_status(worker.is_active(), failure_until),
                    )?;
                }
                InputAction::Resize if loaded => {
                    state.render(
                        terminal.terminal_mut(),
                        &sys,
                        pid,
                        &quota,
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
    terminal.finish(result)
}

/// Displays token usage data in an interactive TUI with auto-refresh.
///
/// Runs until the user quits; `time_range` filters which sessions are scanned.
///
/// Features:
/// - Auto-refresh on a configurable interval (usage + pricing)
/// - Real-time memory monitoring
/// - Provider-grouped totals
/// - Scrollable model table (arrow keys)
/// - Keyboard controls: `q`, `Esc`, or `Ctrl+C` to exit, `r` to refresh, `m` to
///   toggle merging models that share a base name across provider prefixes
///   (e.g. `openai/gpt-5.5` + `azure/gpt-5.5`). `merge_providers` seeds the
///   initial state and the `m` toggle is persisted back to `config.toml`.
///
/// `quota_panels` selects which live quota panels to show (by provider name);
/// an empty list drops the band entirely. `providers` (from the config) selects
/// which providers are aggregated. `refresh_secs` is the TUI re-aggregation
/// cadence; `quota_refresh_secs` is the shared poll cadence for every live quota
/// worker.
///
/// # Errors
///
/// Returns an error if the terminal cannot be set up or restored, if the initial
/// usage load fails, if reading a terminal input event fails, or if a frame fails
/// to draw. A later refresh failure is logged and the previous data is kept.
///
/// # Panics
///
/// Panics if the current process ID cannot be obtained for memory monitoring.
pub fn display_usage_interactive(
    time_range: crate::cli::TimeRange,
    merge_providers: bool,
    quota_panels: Vec<String>,
    providers: ProvidersConfig,
    refresh_secs: u64,
    quota_refresh_secs: u64,
) -> anyhow::Result<()> {
    let threads = crate::config::PerformanceConfig::default().resolved_scan_threads();
    let pool = Arc::new(build_scan_pool(threads)?);
    display_usage_interactive_with_pool(
        time_range,
        merge_providers,
        quota_panels,
        providers,
        refresh_secs,
        quota_refresh_secs,
        pool,
    )
}

fn current_view<'a>(
    merge_enabled: bool,
    rows_data: &'a [UsageRow],
    display_rows: &'a [UsageRow],
) -> &'a [UsageRow] {
    if merge_enabled {
        display_rows
    } else {
        rows_data
    }
}

/// The change-highlight fingerprint of a row: the token buckets only (never
/// cost, so a pricing-data refresh can't flicker a row).
///
/// Reasoning is folded into the second field so a Gemini session whose only
/// delta lands in `thoughts_tokens` still registers as a change. When merging is
/// on this is computed over the summed row, so a collapsed base name highlights
/// whenever **any** of its folded-in provider variants grows — a base name that
/// looks idle can flash because a hidden variant (a subagent, another provider
/// prefix, a background session) is being written. That is truthful, not a bug.
fn row_fingerprint(row: &UsageRow) -> (i64, i64, i64, i64) {
    (
        row.input_tokens,
        row.output_with_reasoning(),
        row.cache_read,
        row.cache_creation,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_usage_frame_with_status<B: Backend>(
    terminal: &mut Terminal<B>,
    rows_data: &[UsageRow],
    totals: &UsageTotals,
    provider_totals: &UsageProviderTotals,
    update_tracker: &UpdateTracker,
    sys: &System,
    pid: Pid,
    quota: &QuotaView,
    scroll: &mut ScrollState,
    merge_enabled: bool,
    status: Option<&str>,
    write_hyperlink: bool,
) -> anyhow::Result<()> {
    let provider_rows = build_provider_total_rows(provider_totals);

    let completed = terminal.draw(|f| {
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
        // `band_enabled == false` (empty `quota_panels`) drops the whole band, so
        // the scrollable table takes the full height — not just the gauges hidden.
        let panels_height = visible_band_height(
            area.height,
            quota.band_enabled,
            &arrange,
            provider_rows.len(),
        );
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
                // Draw one outer border for the whole cell, then split its inside
                // into the provider table (top) and a stacked share bar pinned to
                // the bottom line, so both live inside the same box.
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(RatatuiColor::Magenta));
                let inner = block.inner(table_area);
                f.render_widget(block, table_area);

                let cells = RatatuiLayout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(2), Constraint::Length(1)])
                    .split(inner);
                let (rows_area, bar_area) = (cells[0], cells[1]);

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
                                format_cost_compact(row.stats.total_cost),
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
                    Constraint::Min(9),
                    Constraint::Length(11),
                    Constraint::Length(11),
                ];

                // Reuse the shared table builder but strip its own border (the
                // outer block above already draws it) by overriding the block.
                let totals_table = create_ratatui_table(
                    totals_rows,
                    totals_header,
                    &totals_widths,
                    RatatuiColor::Magenta,
                )
                .block(Block::default());
                f.render_widget(totals_table, rows_area);

                f.render_widget(
                    Paragraph::new(provider_share_bar(&provider_rows, bar_area.width)),
                    bar_area,
                );
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

        let summary = create_summary(summary_items, sys, pid, chunks.summary.width);
        f.render_widget(summary, chunks.summary);

        // When merged, the toggle un-merges, so label it "split" to match.
        let merge_hint = if merge_enabled {
            " split  "
        } else {
            " merge  "
        };
        f.render_widget(
            create_controls_with_status(&[("m", merge_hint)], status),
            chunks.controls,
        );
    })?;

    // ratatui can't embed the OSC 8 escape itself, so hyperlink the repo label
    // it just drew (a no-op on terminals without hyperlink support).
    if write_hyperlink {
        overlay_repo_hyperlink(completed.buffer)?;
    }

    Ok(())
}

/// Production-shaped usage frame fixture used by Criterion benchmarks.
///
/// The fixture owns a [`TestBackend`] so benchmarks exercise the same table,
/// provider band, quota panels, summary, controls, and terminal diff path as
/// the interactive renderer without writing control sequences to stdout.
#[doc(hidden)]
pub struct UsageFrameBenchmark {
    terminal: Terminal<TestBackend>,
    rows: Vec<UsageRow>,
    totals: UsageTotals,
    provider_totals: UsageProviderTotals,
    update_tracker: UpdateTracker,
    sys: System,
    pid: Pid,
    claude: ClaudeQuotaSnapshot,
    codex: CodexQuotaSnapshot,
    copilot: CopilotQuotaSnapshot,
    cursor: CursorQuotaSnapshot,
    scroll: ScrollState,
}

impl UsageFrameBenchmark {
    /// Creates a populated benchmark frame at the requested terminal size.
    pub fn new(width: u16, height: u16) -> anyhow::Result<Self> {
        const MODELS: [&str; 8] = [
            "claude-sonnet-4-6",
            "gpt-5.5-codex",
            "copilot/gpt-5.4",
            "gemini-3.1-pro",
            "grok-code-fast-1",
            "opencode/deepseek-v4",
            "cursor/auto",
            "hermes/qwen3-coder",
        ];

        let mut rows = Vec::with_capacity(32);
        let mut totals = UsageTotals::default();
        let mut provider_totals = UsageProviderTotals::default();
        for index in 0..32 {
            let scale = index as i64 + 1;
            let input_tokens = 12_000 * scale;
            let output_tokens = 2_400 * scale;
            let reasoning_tokens = 600 * scale;
            let cache_read = 48_000 * scale;
            let cache_creation = 1_200 * scale;
            let total =
                input_tokens + output_tokens + reasoning_tokens + cache_read + cache_creation;
            let model = format!("{}-{index}", MODELS[index % MODELS.len()]);
            let row = UsageRow {
                display_model: model.clone(),
                model,
                input_tokens,
                output_tokens,
                reasoning_tokens,
                cache_read,
                cache_creation,
                total,
                cost: scale as f64 * 0.0175,
            };
            totals.accumulate(&row);
            let stats = match index % MODELS.len() {
                0 => &mut provider_totals.claude,
                1 => &mut provider_totals.codex,
                2 => &mut provider_totals.copilot,
                3 => &mut provider_totals.gemini,
                4 => &mut provider_totals.grok,
                5 => &mut provider_totals.opencode,
                6 => &mut provider_totals.cursor,
                _ => &mut provider_totals.hermes,
            };
            stats.total_tokens += row.total;
            stats.total_cost += row.cost;
            stats.days_count = 7;
            provider_totals.overall.total_tokens += row.total;
            provider_totals.overall.total_cost += row.cost;
            rows.push(row);
        }
        provider_totals.overall.days_count = 7;

        let models: Vec<_> = rows.iter().map(|row| row.model.clone()).collect();
        let mut update_tracker = UpdateTracker::new(MAX_TRACKED_ROWS, 0);
        for row in &rows {
            update_tracker.prime(row.model.clone(), &row_fingerprint(row));
        }
        let mut scroll = ScrollState::new();
        scroll.sync(None, &models);

        let pid = sysinfo::get_current_pid()
            .map_err(|error| anyhow::anyhow!("get benchmark process ID: {error}"))?;
        let mut sys = System::new();
        init_process_metrics(&mut sys, pid);

        Ok(Self {
            terminal: Terminal::new(TestBackend::new(width, height))?,
            rows,
            totals,
            provider_totals,
            update_tracker,
            sys,
            pid,
            claude: ClaudeQuotaSnapshot::default(),
            codex: CodexQuotaSnapshot::default(),
            copilot: CopilotQuotaSnapshot::default(),
            cursor: CursorQuotaSnapshot::default(),
            scroll,
        })
    }

    /// Renders one frame with the supplied footer status.
    pub fn render(&mut self, status: Option<&str>) -> anyhow::Result<()> {
        let quota = QuotaView {
            claude: &self.claude,
            codex: &self.codex,
            copilot: &self.copilot,
            cursor: &self.cursor,
            present: QuotaPresence {
                claude: true,
                codex: true,
                copilot: true,
                cursor: true,
            },
            band_enabled: true,
        };
        render_usage_frame_with_status(
            &mut self.terminal,
            &self.rows,
            &self.totals,
            &self.provider_totals,
            &self.update_tracker,
            &self.sys,
            self.pid,
            &quota,
            &mut self.scroll,
            false,
            status,
            false,
        )
    }
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

/// Builds a horizontal stacked share bar filling `width` columns: one solid
/// colored segment per provider, sized by its token share of the total.
///
/// Each segment reuses the provider's `tui_color` so it lines up with the table
/// rows above. Segment widths use largest-remainder rounding so they always sum
/// to exactly `width`. Falls back to a dim placeholder bar when there is no
/// token data (or zero width).
fn provider_share_bar(rows: &[ProviderTotal<'_, ProviderStats>], width: u16) -> Line<'static> {
    let width = width as usize;
    // Providers that actually contributed tokens (skip the "All Providers"
    // aggregate and any empty provider).
    let segments: Vec<(RatatuiColor, i64)> = rows
        .iter()
        .filter(|row| row.label != "All Providers" && row.stats.total_tokens > 0)
        .map(|row| (row.tui_color, row.stats.total_tokens))
        .collect();
    let total: i64 = segments.iter().map(|(_, t)| *t).sum();

    if width == 0 || total <= 0 {
        return Line::from(Span::styled(
            "░".repeat(width),
            Style::default().fg(RatatuiColor::DarkGray),
        ));
    }

    // Largest-remainder apportionment: floor each share, then hand the leftover
    // columns to the largest fractional remainders so the bar fills exactly.
    let mut widths: Vec<usize> = Vec::with_capacity(segments.len());
    let mut remainders: Vec<(usize, f64)> = Vec::with_capacity(segments.len());
    let mut used = 0usize;
    for (i, (_, tokens)) in segments.iter().enumerate() {
        let exact = *tokens as f64 / total as f64 * width as f64;
        let floor = exact.floor() as usize;
        widths.push(floor);
        remainders.push((i, exact - floor as f64));
        used += floor;
    }
    let mut leftover = width.saturating_sub(used);
    remainders.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (i, _) in remainders {
        if leftover == 0 {
            break;
        }
        widths[i] += 1;
        leftover -= 1;
    }

    let spans: Vec<Span<'static>> = segments
        .iter()
        .enumerate()
        .filter_map(|(i, (color, _))| {
            let w = widths[i];
            (w > 0).then(|| Span::styled("█".repeat(w), Style::default().fg(*color)))
        })
        .collect();
    Line::from(spans)
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
        // The rendered table omits the overall row already included in
        // `provider_rows`: border(2) + header/margin(2) + rows(-1) + bar(1).
        BandArrange::SingleRow { table: true } => (provider_rows as u16)
            .saturating_add(4)
            .max(QUOTA_PANEL_MIN_HEIGHT),
        BandArrange::SingleRow { table: false } => QUOTA_PANEL_MIN_HEIGHT,
        BandArrange::TwoRow { .. } => QUOTA_PANEL_MIN_HEIGHT.saturating_mul(2),
    }
}

fn visible_band_height(
    area_height: u16,
    band_enabled: bool,
    arrange: &BandArrange,
    provider_rows: usize,
) -> Option<u16> {
    if !band_enabled || area_height < USAGE_PANELS_MIN_H {
        return None;
    }

    let height = band_height(arrange, provider_rows);
    (area_height >= height.saturating_add(USAGE_NON_PANEL_MIN_H)).then_some(height)
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
            if let Some(extra) = codex_extras_line(codex, now) {
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

/// Builds the optional Codex extras line (`reset expires X  ·  ~L-H msgs  ·
/// cap $X`). Reset-credit details lead the line so the expiry stays visible in
/// a narrow panel even when message and spend metadata are also present.
fn codex_extras_line(codex: &CodexQuotaSnapshot, now: i64) -> Option<Line<'static>> {
    let mut parts: Vec<String> = Vec::new();
    if codex.reset_credits_available.is_some_and(|count| count > 0)
        && let Some(expirations) = &codex.reset_credit_expirations
    {
        if let Some(expires_at) = expirations.iter().flatten().min() {
            parts.push(format!(
                "reset expires {}",
                format_duration_until(*expires_at, now)
            ));
        } else if codex
            .reset_credits_available
            .and_then(|count| usize::try_from(count).ok())
            .is_some_and(|count| count > 0 && expirations.len() >= count)
        {
            parts.push("reset never expires".to_string());
        }
    }
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
    use crate::models::Provider;

    fn line_text(line: Line<'_>) -> String {
        line.spans
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect()
    }

    fn stats(tokens: i64) -> ProviderStats {
        ProviderStats {
            total_tokens: tokens,
            total_cost: 0.0,
            days_count: 1,
        }
    }

    #[test]
    fn quota_shutdown_guard_never_waits_and_sets_flag() {
        let shutdown = Arc::new(AtomicBool::new(false));
        drop(QuotaShutdownGuard {
            shutdown: Arc::clone(&shutdown),
        });
        assert!(shutdown.load(Ordering::Relaxed));
    }

    #[test]
    fn share_bar_fills_exact_width() {
        let (c, x, p) = (stats(710), stats(210), stats(80));
        let rows = vec![
            ProviderTotal::new(Provider::ClaudeCode, &c, false),
            ProviderTotal::new(Provider::Codex, &x, false),
            ProviderTotal::new(Provider::Copilot, &p, false),
        ];
        let bar = provider_share_bar(&rows, 20);
        let total: usize = bar.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(total, 20, "segments must fill the whole bar width");
        // Largest share (Claude) gets the widest segment.
        assert!(bar.spans[0].content.chars().count() >= bar.spans[1].content.chars().count());
    }

    #[test]
    fn share_bar_placeholder_when_no_tokens() {
        let empty = stats(0);
        let rows = vec![ProviderTotal::new(Provider::ClaudeCode, &empty, false)];
        let bar = provider_share_bar(&rows, 10);
        let total: usize = bar.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(total, 10);
        assert_eq!(
            bar.spans.len(),
            1,
            "no-data bar is a single placeholder span"
        );
    }

    #[test]
    fn share_bar_zero_width_is_empty() {
        let c = stats(100);
        let rows = vec![ProviderTotal::new(Provider::ClaudeCode, &c, false)];
        let bar = provider_share_bar(&rows, 0);
        let total: usize = bar.spans.iter().map(|s| s.content.chars().count()).sum();
        assert_eq!(total, 0);
    }

    #[test]
    fn codex_extras_shows_earliest_reset_credit_expiry() {
        let now = 1_000;
        let codex = CodexQuotaSnapshot {
            reset_credits_available: Some(5),
            reset_credit_expirations: Some(vec![
                Some(now + 4 * 86_400 + 2 * 3_600),
                None,
                Some(now + 2 * 3_600 + 13 * 60),
            ]),
            approx_messages: Some((120, 150)),
            spend_limit: Some(50.0),
            ..Default::default()
        };

        let line = codex_extras_line(&codex, now).unwrap();
        assert_eq!(
            line_text(line),
            "reset expires 2h13m  ·  ~120-150 msgs  ·  cap $50"
        );
    }

    #[test]
    fn codex_extras_distinguishes_non_expiring_credits() {
        let codex = CodexQuotaSnapshot {
            reset_credits_available: Some(2),
            reset_credit_expirations: Some(vec![None, None]),
            ..Default::default()
        };

        let line = codex_extras_line(&codex, 1_000).unwrap();
        assert_eq!(line_text(line), "reset never expires");
        assert!("reset never expires".chars().count() <= usize::from(PANEL_MIN_W - 2));
    }

    #[test]
    fn codex_extras_omits_expiry_when_details_are_unavailable() {
        let codex = CodexQuotaSnapshot {
            reset_credits_available: Some(2),
            approx_messages: Some((120, 150)),
            ..Default::default()
        };

        let line = codex_extras_line(&codex, 1_000).unwrap();
        assert_eq!(line_text(line), "~120-150 msgs");
    }

    #[test]
    fn codex_extras_does_not_infer_no_expiry_from_capped_details() {
        let codex = CodexQuotaSnapshot {
            reset_credits_available: Some(3),
            reset_credit_expirations: Some(vec![None, None]),
            ..Default::default()
        };

        assert!(codex_extras_line(&codex, 1_000).is_none());
    }

    #[test]
    fn codex_reset_expiry_and_staleness_fit_minimum_panel() {
        let now = 1_000;
        let codex = CodexQuotaSnapshot {
            source: QuotaSource::Api,
            fetched_at: now,
            plan_type: Some("plus".to_string()),
            primary: Some(QuotaWindow {
                used_percent: 10.0,
                resets_at_unix: Some(now + 3_600),
            }),
            secondary: Some(QuotaWindow {
                used_percent: 20.0,
                resets_at_unix: Some(now + 86_400),
            }),
            credits_balance: Some("0".to_string()),
            reset_credits_available: Some(2),
            reset_credit_expirations: Some(vec![Some(now + 2 * 3_600 + 13 * 60)]),
            ..Default::default()
        };
        let mut terminal =
            Terminal::new(TestBackend::new(PANEL_MIN_W, QUOTA_PANEL_MIN_HEIGHT)).unwrap();

        terminal
            .draw(|frame| render_codex_quota(frame, frame.area(), &codex, now))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = (0..QUOTA_PANEL_MIN_HEIGHT)
            .map(|y| {
                (0..PANEL_MIN_W)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("reset expires 2h13m"));
        assert!(rendered.contains("updated just now"));
    }

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
    fn band_height_preserves_all_provider_rows() {
        let arrange = BandArrange::SingleRow { table: true };

        assert_eq!(visible_band_height(22, true, &arrange, 8), Some(12));
        assert_eq!(visible_band_height(22, true, &arrange, 9), None);
        assert_eq!(visible_band_height(23, true, &arrange, 9), Some(13));
    }

    #[test]
    fn disabled_band_stays_hidden() {
        let arrange = BandArrange::SingleRow { table: true };
        assert_eq!(visible_band_height(40, false, &arrange, 9), None);
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
