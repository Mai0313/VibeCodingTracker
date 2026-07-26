//! Auto-refreshing TUI for the usage view.
//!
//! Runs a render loop that incrementally re-aggregates the session directories
//! every `refresh_secs` seconds (from `config.toml`), reusing one pricing map
//! for the current UTC day and highlighting rows whose tokens changed since
//! the last tick. The loop holds only the small per-model display state
//! between frames so a resize repaints instantly without re-aggregating; memory
//! is trimmed back to the OS after each refresh.

use crate::display::common::ProviderTotal;
use crate::display::common::table::{
    FOOTER_H, create_controls_with_status, create_provider_row, create_ratatui_table,
    create_summary, frame_layout, init_process_metrics, refresh_process_metrics,
    render_scrollable_table, render_too_small, styled_row,
};
use crate::display::common::tui::{
    InputAction, RefreshWorker, RefreshWorkerError, ScrollState, TerminalSession, UpdateTracker,
    handle_input, overlay_repo_hyperlink, refresh_status, render_loading_frame,
};
use crate::display::usage::averages::{
    ProviderStats, UsageProviderTotals, UsageRow, UsageTotals, build_provider_total_rows,
    build_usage_summary, merge_rows_by_base_model,
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, CrosstermBackend, TestBackend},
    layout::{Constraint, Direction, Layout as RatatuiLayout, Rect},
    style::{Color as RatatuiColor, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Row as RatatuiRow},
};
use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::{Pid, System};
use vct_core::config::ProvidersConfig;
use vct_core::models::{
    ClaudeQuotaSnapshot, CodexQuotaSnapshot, CopilotQuotaSnapshot, CursorQuotaSnapshot,
    GrokQuotaSnapshot, QuotaSource, QuotaWindow,
};
use vct_core::pricing::{ModelPricingMap, fetch_model_pricing};
use vct_core::quota::{
    CLAUDE_LOGIN_HINT, CODEX_LOGIN_HINT, COPILOT_LOGIN_HINT, CURSOR_LOGIN_HINT, ClaudeState,
    CodexState, CopilotState, CursorState, GROK_LOGIN_HINT, GrokState, load_claude_cache,
    load_codex_cache, load_copilot_cache, load_cursor_cache, load_grok_cache, save_claude_cache,
    save_codex_cache, save_copilot_cache, save_cursor_cache, save_grok_cache, spawn_quota_worker,
};
use vct_core::scan::build_scan_pool;
use vct_core::summary_cache::SummaryScanCache;
use vct_core::utils::{
    format_compact, format_cost, format_cost_compact, format_duration_until,
    get_claude_credentials_path, get_copilot_config_path, get_cursor_auth_path, get_grok_auth_path,
    resolve_paths,
};

/// How wide and tall the cards in one grid are.
///
/// The placement formula is shared; only these numbers differ between the
/// always-on grid under the table and the full-screen `Q` overlay.
struct GridSpec {
    /// Narrowest a card may be, and the divisor that decides the column count.
    min_w: u16,
    /// Widest a card grows to; past this the grid centers its slack instead.
    max_w: u16,
    /// Card height, borders included.
    height: u16,
}

/// The always-on grid under the model table.
///
/// A full gauge line is `label(5) + bar(5) + percent(5) + reset(6)` = 21 content
/// columns, so 24 leaves room inside the borders — and it is the width that lets
/// five cards share one row on a 120-column terminal. Five content rows is what
/// the fullest provider needs (Claude: plan, 5h, 7d, scoped, balance).
const CARD_GRID: GridSpec = GridSpec {
    min_w: 24,
    max_w: 40,
    height: 7,
};

/// The `Q` overlay grid: wider and taller, so every provider shows every line
/// no matter how narrow the terminal made the always-on cards.
const OVERLAY_GRID: GridSpec = GridSpec {
    min_w: 34,
    max_w: 46,
    height: 9,
};
/// Claude brand color for the quota panel border.
const CLAUDE_COLOR: RatatuiColor = RatatuiColor::Rgb(190, 116, 87);
/// Codex brand color for the quota panel border.
const CODEX_COLOR: RatatuiColor = RatatuiColor::Rgb(118, 127, 198);
/// Copilot brand color (GitHub green) for the quota panel border.
const COPILOT_COLOR: RatatuiColor = RatatuiColor::Rgb(46, 160, 67);
/// Cursor brand color (teal) for the quota panel border.
const CURSOR_COLOR: RatatuiColor = RatatuiColor::Rgb(64, 180, 180);
/// Grok brand color (xAI near-black, lifted to stay readable on a dark terminal).
const GROK_COLOR: RatatuiColor = RatatuiColor::Rgb(170, 170, 178);

/// Width the scrollable model table needs before a side rail may take the rest.
/// Model(16) + six numeric columns and their gaps, plus borders and scrollbar.
const USAGE_CONTENT_MIN_W: u16 = 66;
/// Body rows the model table must keep for the quota grid to be worth drawing.
/// Below this the grid folds to the one-line digest, which is announced there.
const TABLE_MIN_BODY_H: u16 = 8;
/// How many providers can show a quota card (Claude / Codex / Copilot / Cursor
/// / Grok). The grid never reads this — it only bounds the render dispatch.
const MAX_QUOTA_PANELS: usize = 5;

/// Which provider quota panels have credentials on this machine.
#[derive(Clone, Copy, Default)]
struct QuotaPresence {
    claude: bool,
    codex: bool,
    copilot: bool,
    cursor: bool,
    grok: bool,
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
        let grok = get_grok_auth_path().map(|p| p.exists()).unwrap_or(false);
        Self {
            claude,
            codex,
            copilot,
            cursor,
            grok,
        }
    }

    /// Number of provider quota panels present.
    fn count(&self) -> usize {
        self.claude as usize
            + self.codex as usize
            + self.copilot as usize
            + self.cursor as usize
            + self.grok as usize
    }
}

/// Borrowed quota state passed to the render frame.
struct QuotaView<'a> {
    claude: &'a ClaudeQuotaSnapshot,
    codex: &'a CodexQuotaSnapshot,
    copilot: &'a CopilotQuotaSnapshot,
    cursor: &'a CursorQuotaSnapshot,
    grok: &'a GrokQuotaSnapshot,
    present: QuotaPresence,
    /// Whether the quota surface is shown at all. `false` when
    /// `usage.quota.panels` is empty, which drops the card grid *and* the
    /// Provider Usage rail, not just the individual gauges.
    band_enabled: bool,
    /// Whether the side rail is currently toggled on (`p`).
    rail_visible: bool,
    /// Whether the full-detail quota overlay is open (`Q`).
    overlay_open: bool,
}

/// Upper bound on rows the [`UpdateTracker`] remembers for change highlighting.
const MAX_TRACKED_ROWS: usize = 100;

/// Hard minimum terminal width/height; below this only a notice is drawn.
const USAGE_MIN_W: u16 = 74;
const USAGE_MIN_H: u16 = 14;

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
    grok: Arc<Mutex<GrokQuotaSnapshot>>,
    _guard: QuotaShutdownGuard,
}

impl QuotaRuntime {
    fn start(quota_panels: &[String], providers: ProvidersConfig, quota_refresh_secs: u64) -> Self {
        let panel_on = |name: &str| vct_core::config::quota_panel_selected(quota_panels, name);
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
        present.grok &= providers.grok && panel_on("grok");

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
        let grok = Arc::new(Mutex::new(
            present
                .grok
                .then(load_grok_cache)
                .flatten()
                .unwrap_or_default(),
        ));

        if present.claude || present.codex || present.copilot || present.cursor || present.grok {
            match vct_core::quota::http::build_client() {
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
                    if present.grok {
                        let (client, shutdown, shared) =
                            (client.clone(), Arc::clone(&shutdown), Arc::clone(&grok));
                        let mut state = GrokState::default();
                        spawn_quota_worker(
                            "grok",
                            shared,
                            shutdown,
                            quota_refresh_secs,
                            move || state.resolve(&client),
                            |snapshot| {
                                let _ = save_grok_cache(snapshot);
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
            grok,
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
    rail_visible: bool,
    overlay_open: bool,
    claude: ClaudeQuotaSnapshot,
    codex: CodexQuotaSnapshot,
    copilot: CopilotQuotaSnapshot,
    cursor: CursorQuotaSnapshot,
    grok: GrokQuotaSnapshot,
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
            rail_visible: true,
            overlay_open: false,
            claude: ClaudeQuotaSnapshot::default(),
            codex: CodexQuotaSnapshot::default(),
            copilot: CopilotQuotaSnapshot::default(),
            cursor: CursorQuotaSnapshot::default(),
            grok: GrokQuotaSnapshot::default(),
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
        let _ = vct_core::config::save_merge_models(self.merge_enabled);
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
        self.grok = runtime
            .grok
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
            grok: &self.grok,
            present: runtime.present,
            band_enabled: runtime.band_enabled,
            rail_visible: self.rail_visible,
            overlay_open: self.overlay_open,
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
    time_range: vct_core::models::TimeRange,
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
            let mut scan_options = vct_core::usage::UsageScanOptions::default();
            let mut loaded_pricing_utc_date = None;
            move || {
                let today = chrono::Utc::now().date_naive();
                if loaded_pricing_utc_date != Some(today) {
                    match fetch_model_pricing() {
                        Ok(map) => {
                            // A new pricing map can move tier thresholds; the
                            // scan invalidates its cache when the snapshot's
                            // fingerprint changes.
                            scan_options.tiers = Some(std::sync::Arc::new(map.tier_thresholds()));
                            pricing = map;
                            loaded_pricing_utc_date = Some(today);
                        }
                        Err(error) => {
                            log::warn!("failed to refresh pricing: {error}");
                        }
                    }
                }

                let collection = worker_pool.install(|| {
                    vct_core::usage::aggregate_usage_from_paths_with_cache_opts(
                        &worker_paths,
                        time_range,
                        providers,
                        &mut cache,
                        &scan_options,
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

                let mut summary = build_usage_summary(
                    &collection.data.models,
                    &collection.data.per_provider,
                    &collection.data.provider_days,
                    &pricing,
                    &collection.data.stored_costs,
                );
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
        let metrics_interval =
            Duration::from_millis(vct_core::constants::refresh::METRICS_REFRESH_MS);
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
                        vct_core::utils::release_freed_heap();
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
                // Esc backs out of the quota overlay; with nothing open it quits.
                InputAction::Close if state.overlay_open => {
                    state.overlay_open = false;
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
                InputAction::Close => break,
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
                InputAction::ToggleQuota => {
                    state.overlay_open = !state.overlay_open;
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
                InputAction::TogglePane => {
                    state.rail_visible = !state.rail_visible;
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
    time_range: vct_core::models::TimeRange,
    merge_providers: bool,
    quota_panels: Vec<String>,
    providers: ProvidersConfig,
    refresh_secs: u64,
    quota_refresh_secs: u64,
) -> anyhow::Result<()> {
    let threads = vct_core::config::PerformanceConfig::default().resolved_scan_threads();
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
/// cost, so a fuzzy-price shift can't flicker a row).
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

        // `band_enabled == false` (empty `quota_panels`) drops the whole quota
        // surface, Provider Usage rail included — not just the gauges hidden.
        let n = quota.present.count();
        let grid_h = visible_grid_height(area, quota.band_enabled, n);
        // A folded grid still costs one row, for the digest that replaces it.
        let band_h = if grid_h > 0 {
            grid_h
        } else {
            u16::from(quota.band_enabled && n > 0)
        };
        let chunks = frame_layout(
            area,
            USAGE_CONTENT_MIN_W,
            quota.band_enabled && quota.rail_visible,
            band_h,
        );

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
            chunks.content,
            header,
            rows,
            &widths,
            RatatuiColor::Green,
            row_count,
            scroll,
        );

        if let Some(rail_area) = chunks.rail {
            render_provider_rail(f, rail_area, &provider_rows);
        }

        if let Some(band_area) = chunks.band {
            let now = chrono::Local::now().timestamp();
            if grid_h > 0 {
                let cells = grid_cells(&CARD_GRID, band_area, n);
                // Present providers render in a fixed order (Claude → Codex →
                // Copilot → Cursor → Grok) into the cells; a missing provider
                // consumes no cell, and `grid_cells` always returns exactly `n`.
                let mut idx = 0;
                let mut place = |f: &mut Frame, card: QuotaCard| {
                    if let Some(cell) = cells.get(idx) {
                        render_quota_card(f, *cell, &card);
                    }
                    idx += 1;
                };
                let card_w = cells.first().map_or(CARD_GRID.min_w, |cell| cell.width);
                if quota.present.claude {
                    place(f, claude_card(quota.claude, now, card_w));
                }
                if quota.present.codex {
                    place(f, codex_card(quota.codex, now, card_w));
                }
                if quota.present.copilot {
                    place(f, copilot_card(quota.copilot, now, card_w));
                }
                if quota.present.cursor {
                    place(f, cursor_card(quota.cursor, now, card_w));
                }
                if quota.present.grok {
                    place(f, grok_card(quota.grok, now, card_w));
                }
            } else {
                f.render_widget(
                    Paragraph::new(quota_digest(&digest_items(quota), band_area.width)).centered(),
                    band_area,
                );
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
            create_controls_with_status(
                &[("m", merge_hint), ("p", " panes  "), ("Q", " quota  ")],
                status,
            ),
            chunks.controls,
        );

        // Drawn last so it covers the frame it is layered over.
        if quota.overlay_open {
            render_quota_overlay(f, area, quota, chrono::Local::now().timestamp());
        }
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
    grok: GrokQuotaSnapshot,
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
            grok: GrokQuotaSnapshot::default(),
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
            grok: &self.grok,
            present: QuotaPresence {
                claude: true,
                codex: true,
                copilot: true,
                cursor: true,
                grok: true,
            },
            band_enabled: true,
            rail_visible: true,
            overlay_open: false,
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
    mini_bar_n(pct, 5)
}

/// [`mini_bar`] with a caller-chosen cell count, for the narrower digest bar.
fn mini_bar_n(pct: f64, cells: usize) -> String {
    let filled = ((pct / (100.0 / cells as f64)).ceil() as i64).clamp(0, cells as i64) as usize;
    (0..cells)
        .map(|i| if i < filled { '▰' } else { '▱' })
        .collect()
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

/// Columns and rows the quota grid uses for `n` cards at `width`.
///
/// This is the whole placement rule. The card count enters only as `n`, so a
/// new provider is a longer list and never a new case: the grid packs as many
/// cards per row as the width allows and wraps the remainder onto further rows.
fn quota_grid(spec: &GridSpec, width: u16, n: usize) -> (usize, usize) {
    if n == 0 {
        return (0, 0);
    }
    let cols = usize::from((width / spec.min_w).max(1)).min(n);
    (cols, n.div_ceil(cols))
}

/// Rows the grid needs at `width` for `n` cards.
fn grid_height(spec: &GridSpec, width: u16, n: usize) -> u16 {
    let (_, rows) = quota_grid(spec, width, n);
    (rows as u16).saturating_mul(spec.height)
}

/// The grid height the frame can actually spend, or `0` when the grid folds.
///
/// The model table is the primary content, so the grid is only drawn while the
/// table keeps [`TABLE_MIN_BODY_H`] body rows beneath it (its border, header and
/// header margin cost 4 more). When it folds, the one-line quota digest takes
/// its place and says how many providers moved behind `Q` — the grid is never
/// dropped silently.
fn visible_grid_height(area: Rect, band_enabled: bool, n: usize) -> u16 {
    if !band_enabled || n == 0 {
        return 0;
    }
    let height = grid_height(&CARD_GRID, area.width, n);
    let table_h = area.height.saturating_sub(FOOTER_H).saturating_sub(height);
    if table_h >= TABLE_MIN_BODY_H.saturating_add(4) {
        height
    } else {
        0
    }
}

/// Splits the grid band into exactly `n` card cells, row-major.
///
/// Cards keep a uniform width, so a final row holding fewer than a full set is
/// left ragged rather than stretching its cards out of alignment with the rows
/// above. The returned length is always `n`, which is what lets the render
/// dispatch index it by present-provider order.
fn grid_cells(spec: &GridSpec, band: Rect, n: usize) -> Vec<Rect> {
    let (cols, rows) = quota_grid(spec, band.width, n);
    if cols == 0 {
        return Vec::new();
    }
    let card_w = (band.width / cols as u16).min(spec.max_w);
    // Center the row so capped cards do not leave all their slack on one side.
    let indent = (band.width - card_w * cols as u16) / 2;
    let row_rects = RatatuiLayout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(spec.height); rows])
        .split(band);

    let mut cells = Vec::with_capacity(n);
    for (r, row) in row_rects.iter().enumerate() {
        for c in 0..(n - r * cols).min(cols) {
            cells.push(Rect {
                x: row.x + indent + c as u16 * card_w,
                y: row.y,
                width: card_w,
                height: row.height,
            });
        }
    }
    cells
}

/// Collects every present provider's card, in the fixed render order.
fn collect_cards(quota: &QuotaView, now: i64, width: u16) -> Vec<QuotaCard> {
    let mut cards = Vec::with_capacity(MAX_QUOTA_PANELS);
    if quota.present.claude {
        cards.push(claude_card(quota.claude, now, width));
    }
    if quota.present.codex {
        cards.push(codex_card(quota.codex, now, width));
    }
    if quota.present.copilot {
        cards.push(copilot_card(quota.copilot, now, width));
    }
    if quota.present.cursor {
        cards.push(cursor_card(quota.cursor, now, width));
    }
    if quota.present.grok {
        cards.push(grok_card(quota.grok, now, width));
    }
    cards
}

/// Renders the full-screen quota overlay opened with `Q`.
///
/// This is where a card gets enough room for every line it has, whatever the
/// always-on grid had to leave out. It uses the same placement formula with a
/// roomier spec; a terminal too short even for that says how many providers it
/// could not reach rather than ending at the border.
fn render_quota_overlay(f: &mut Frame, area: Rect, quota: &QuotaView, now: i64) {
    let outer = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    f.render_widget(Clear, outer);

    let cards = collect_cards(quota, now, OVERLAY_GRID.max_w);
    let inner = Rect {
        x: outer.x + 1,
        y: outer.y + 1,
        width: outer.width.saturating_sub(2),
        height: outer.height.saturating_sub(2),
    };
    let (cols, _) = quota_grid(&OVERLAY_GRID, inner.width, cards.len());
    let fits = if cols == 0 {
        0
    } else {
        cards
            .len()
            .min(cols * usize::from(inner.height / OVERLAY_GRID.height))
    };
    let hidden = cards.len() - fits;

    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(" Quota — all providers "))
        .border_style(Style::default().fg(RatatuiColor::Cyan));
    let hint = if hidden > 0 {
        format!(" +{hidden} hidden · esc to close ")
    } else {
        " esc to close ".to_string()
    };
    block = block.title(
        Line::from(Span::styled(
            hint,
            Style::default().fg(RatatuiColor::DarkGray),
        ))
        .right_aligned(),
    );
    f.render_widget(block, outer);

    for (cell, card) in grid_cells(&OVERLAY_GRID, inner, fits)
        .into_iter()
        .zip(&cards)
    {
        render_quota_card(f, cell, card);
    }
}

/// Renders the Provider Usage rail: a three-column table over a stacked share
/// bar, both inside one border.
///
/// The rail is short by design, so a provider list longer than it can hold is
/// truncated with a `+N more` row and a `+N` flag in the title rather than
/// stopping at the border with no sign that anything is missing.
fn render_provider_rail(
    f: &mut Frame,
    area: Rect,
    provider_rows: &[ProviderTotal<'_, ProviderStats>],
) {
    // Drop the "All Providers" aggregate; the summary bar carries the totals.
    let listed: Vec<_> = provider_rows
        .iter()
        .filter(|row| row.label != "All Providers")
        .collect();
    // Border(2) + header + header margin + share bar leave this many data rows.
    let capacity = usize::from(area.height.saturating_sub(5));
    let mut hidden = listed.len().saturating_sub(capacity);
    // The "+N more" row costs a slot of its own.
    if hidden > 0 {
        hidden = listed.len().saturating_sub(capacity.saturating_sub(1));
    }
    let shown = listed.len() - hidden;

    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(" Providers "))
        .border_style(Style::default().fg(RatatuiColor::Magenta));
    if hidden > 0 {
        block = block.title(
            Line::from(Span::styled(
                format!("+{hidden} "),
                Style::default().fg(RatatuiColor::DarkGray),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cells = RatatuiLayout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(2), Constraint::Length(1)])
        .split(inner);
    let (rows_area, bar_area) = (cells[0], cells[1]);

    let mut totals_rows: Vec<RatatuiRow> = listed
        .iter()
        .take(shown)
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

    if hidden > 0 {
        totals_rows.push(
            RatatuiRow::new(vec![
                format!("+{hidden} more"),
                String::new(),
                String::new(),
            ])
            .style(Style::default().fg(RatatuiColor::DarkGray)),
        );
    }
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

    let totals_widths = [
        Constraint::Min(9),
        Constraint::Length(11),
        Constraint::Length(11),
    ];
    // Reuse the shared table builder but strip its own border (the outer block
    // above already draws it) by overriding the block.
    let totals_table = create_ratatui_table(
        totals_rows,
        vec!["Provider", "Tokens", "Cost"],
        &totals_widths,
        RatatuiColor::Magenta,
    )
    .block(Block::default());
    f.render_widget(totals_table, rows_area);

    f.render_widget(
        Paragraph::new(provider_share_bar(provider_rows, bar_area.width)),
        bar_area,
    );
}

/// Every present provider's headline gauge: the most-consumed of its windows,
/// which is the one that will bite first.
fn digest_items(quota: &QuotaView) -> Vec<(&'static str, RatatuiColor, Option<f64>)> {
    fn peak(windows: &[Option<&QuotaWindow>]) -> Option<f64> {
        windows
            .iter()
            .flatten()
            .map(|w| w.used_percent)
            .fold(None, |acc: Option<f64>, pct| {
                Some(acc.map_or(pct, |a| a.max(pct)))
            })
    }

    let mut items = Vec::with_capacity(MAX_QUOTA_PANELS);
    if quota.present.claude {
        items.push((
            "Claude",
            CLAUDE_COLOR,
            peak(&[
                quota.claude.five_hour.as_ref(),
                quota.claude.seven_day.as_ref(),
                quota.claude.scoped_weekly.as_ref(),
            ]),
        ));
    }
    if quota.present.codex {
        items.push((
            "Codex",
            CODEX_COLOR,
            peak(&[quota.codex.primary.as_ref(), quota.codex.secondary.as_ref()]),
        ));
    }
    if quota.present.copilot {
        items.push((
            "Copilot",
            COPILOT_COLOR,
            peak(&[quota.copilot.premium.as_ref()]),
        ));
    }
    if quota.present.cursor {
        items.push((
            "Cursor",
            CURSOR_COLOR,
            peak(&[
                quota.cursor.total.as_ref(),
                quota.cursor.auto.as_ref(),
                quota.cursor.api.as_ref(),
            ]),
        ));
    }
    if quota.present.grok {
        items.push(("Grok", GROK_COLOR, peak(&[quota.grok.included.as_ref()])));
    }
    items
}

/// The one-line quota digest shown in place of the grid on a short terminal.
///
/// Segments are dropped whole from the tail, and the remainder is named in a
/// trailing `+N more → Q`, so the line never implies it lists every provider.
fn quota_digest(items: &[(&'static str, RatatuiColor, Option<f64>)], width: u16) -> Line<'static> {
    const SEP: &str = "  ·  ";
    /// `Claude ▰▰▱ 58%` — a three-cell bar keeps the line short.
    fn segment(name: &str, pct: Option<f64>) -> String {
        match pct {
            Some(pct) => format!("{name} {} {pct:.0}%", mini_bar_n(pct, 3)),
            None => format!("{name} -"),
        }
    }

    if items.is_empty() {
        return dim_line("no quota panels");
    }

    let widths: Vec<usize> = items
        .iter()
        .map(|(name, _, pct)| segment(name, *pct).chars().count())
        .collect();
    let mut shown = items.len();
    let mut used: usize = widths.iter().sum::<usize>() + SEP.chars().count() * (items.len() - 1);
    while shown > 1 {
        let hidden = items.len() - shown;
        // `+N more → Q` needs room of its own once anything is hidden.
        let tail = if hidden > 0 {
            SEP.chars().count() + format!("+{hidden} more → Q").chars().count()
        } else {
            0
        };
        if used + tail <= usize::from(width) {
            break;
        }
        shown -= 1;
        used -= widths[shown] + SEP.chars().count();
    }

    let mut spans: Vec<Span> = Vec::with_capacity(shown * 2);
    for (i, (name, color, pct)) in items.iter().take(shown).enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                SEP,
                Style::default().fg(RatatuiColor::DarkGray),
            ));
        }
        spans.push(Span::styled(
            segment(name, *pct),
            Style::default().fg(*color),
        ));
    }
    let hidden = items.len() - shown;
    if hidden > 0 {
        spans.push(Span::styled(
            SEP,
            Style::default().fg(RatatuiColor::DarkGray),
        ));
        spans.push(Span::styled(
            format!("+{hidden} more → Q"),
            Style::default().fg(RatatuiColor::DarkGray),
        ));
    }
    Line::from(spans)
}

/// One provider's quota, laid out as an ordered list of lines.
///
/// The list is built most-important-first and independently of the cell it will
/// land in, so the same card can be drawn small in the grid and full-size in the
/// `Q` overlay. Whatever does not fit is counted, never dropped in silence.
struct QuotaCard {
    title: &'static str,
    color: RatatuiColor,
    limit_reached: bool,
    lines: Vec<Line<'static>>,
}

/// Content columns available inside a card `width` wide.
fn card_inner_w(width: u16) -> u16 {
    width.saturating_sub(2)
}

/// Renders `card` into `area`, flagging a hit cap and any lines that did not fit.
///
/// The trailing flags share the top border: `+2` for hidden lines, `LIMIT` for a
/// spent window. `+2` is the promise that the overlay has more to show.
fn render_quota_card(f: &mut Frame, area: Rect, card: &QuotaCard) {
    let capacity = usize::from(area.height.saturating_sub(2));
    let hidden = card.lines.len().saturating_sub(capacity);

    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(format!(" {} ", card.title)))
        .border_style(Style::default().fg(card.color));

    let mut flags: Vec<Span> = Vec::new();
    if hidden > 0 {
        flags.push(Span::styled(
            format!("+{hidden} "),
            Style::default().fg(RatatuiColor::DarkGray),
        ));
    }
    if card.limit_reached {
        flags.push(Span::styled(
            "LIMIT ",
            Style::default()
                .fg(RatatuiColor::Red)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !flags.is_empty() {
        block = block.title(Line::from(flags).right_aligned());
    }

    let shown = card.lines[..capacity.min(card.lines.len())].to_vec();
    f.render_widget(Paragraph::new(shown).block(block), area);
}

/// Builds the card's first line: plan tier on the left, fetch age on the right.
///
/// Merging the two saves a row in every card, and they belong together — the age
/// qualifies everything below it.
fn plan_line(plan: &str, fetched_at: i64, now: i64, inner_w: u16) -> Line<'static> {
    let (age, color) = staleness(fetched_at, now);
    let inner = usize::from(inner_w);
    let pad = inner.saturating_sub(plan.chars().count() + age.chars().count());
    Line::from(vec![
        Span::styled(plan.to_string(), Style::default().fg(RatatuiColor::Gray)),
        Span::raw(" ".repeat(pad)),
        Span::styled(age, Style::default().fg(color)),
    ])
}

/// The "how old is this snapshot" marker, escalating in color past 1h and 6h so
/// a panel stuck on stale data reads as such.
fn staleness(fetched_at: i64, now: i64) -> (String, RatatuiColor) {
    if fetched_at <= 0 {
        return ("never".to_string(), RatatuiColor::DarkGray);
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
        "just now".to_string()
    } else {
        format!("{ago} ago")
    };
    (text, color)
}

/// Builds one gauge line: `5h    ▰▰▱▱▱  34%  ↻ 2h11m`.
///
/// The label column is padded to `label_w` so the bars line up down the card.
/// The reset marker is fitted to what is left: full (`↻ 2h11m`), then bare
/// (`2h11m`), then dropped — a narrow card loses the marker rather than having
/// its duration truncated into a smaller, plausible one.
fn gauge_line(
    label: &str,
    label_w: usize,
    pct: f64,
    reset: Option<i64>,
    now: i64,
    inner_w: u16,
) -> Line<'static> {
    let color = gauge_color(pct);
    let mut spans = vec![
        Span::styled(
            format!("{label:<label_w$} "),
            Style::default().fg(RatatuiColor::Gray),
        ),
        Span::styled(mini_bar(pct), Style::default().fg(color)),
        Span::styled(format!(" {pct:>3.0}%"), Style::default().fg(color)),
    ];
    if let Some(reset) = reset {
        let text = format_duration_until(reset, now);
        // label + separator + bar + " nnn%".
        let used = label_w + 1 + 5 + 5;
        let room = usize::from(inner_w).saturating_sub(used);
        let width = text.chars().count();
        let marker = if room >= width + 4 {
            Some(format!("  ↻ {text}"))
        } else if room > width {
            Some(format!(" {text}"))
        } else {
            None
        };
        if let Some(marker) = marker {
            spans.push(Span::styled(
                marker,
                Style::default().fg(RatatuiColor::DarkGray),
            ));
        }
    }
    Line::from(spans)
}

/// Like [`gauge_line`] but labels the bar with a caller-supplied value (e.g.
/// `1080/1500`) instead of a percentage, and carries no reset marker. `pct`
/// still drives the bar fill and its traffic-light color.
///
/// The bar is dropped when the value would not otherwise fit: a used/total pair
/// already says what the bar says, whereas a truncated `$1.24K/$5.` reads as a
/// smaller, plausible number that contradicts the bar beside it.
fn gauge_line_value(
    label: &str,
    label_w: usize,
    pct: f64,
    value: &str,
    inner_w: u16,
) -> Line<'static> {
    let color = gauge_color(pct);
    let mut spans = vec![Span::styled(
        format!("{label:<label_w$} "),
        Style::default().fg(RatatuiColor::Gray),
    )];
    if label_w + 1 + 5 + 1 + value.chars().count() <= usize::from(inner_w) {
        spans.push(Span::styled(mini_bar(pct), Style::default().fg(color)));
        spans.push(Span::styled(
            format!(" {value}"),
            Style::default().fg(color),
        ));
    } else {
        spans.push(Span::styled(value.to_string(), Style::default().fg(color)));
    }
    Line::from(spans)
}

/// Joins detail `parts` with ` · `, keeping as many as fit `inner_w`.
///
/// A part that does not fit is counted in a trailing `+N`, so a card never
/// implies it is showing everything it has. Returns `None` when there is
/// nothing to say.
fn detail_line(parts: &[String], inner_w: u16) -> Option<Line<'static>> {
    if parts.is_empty() {
        return None;
    }
    let inner = usize::from(inner_w);
    let mut kept = 0usize;
    let mut text = String::new();
    for part in parts {
        let candidate = if kept == 0 {
            part.clone()
        } else {
            format!("{text} · {part}")
        };
        // Reserve room for the "+N" marker the remaining parts would need.
        let remaining = parts.len() - kept - 1;
        let reserve = if remaining > 0 { 4 } else { 0 };
        if candidate.chars().count() + reserve > inner {
            break;
        }
        text = candidate;
        kept += 1;
    }
    if kept == 0 {
        // Not even the first part fits; show its head rather than nothing.
        text = parts[0].chars().take(inner.saturating_sub(1)).collect();
        kept = 1;
    }
    let hidden = parts.len() - kept;
    if hidden > 0 {
        text.push_str(&format!(" +{hidden}"));
    }
    Some(Line::from(Span::styled(
        text,
        Style::default().fg(RatatuiColor::DarkGray),
    )))
}

/// Width of the label column in a card, wide enough for its longest label.
fn label_width(labels: &[&str]) -> usize {
    labels
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(5)
        .max(5)
}

/// Builds the Claude card: plan, 5h / 7d / per-model gauges, then balance.
fn claude_card(claude: &ClaudeQuotaSnapshot, now: i64, width: u16) -> QuotaCard {
    let inner = card_inner_w(width);
    let scoped = claude
        .scoped_label
        .as_deref()
        .filter(|_| claude.scoped_weekly.is_some());
    let label_w = label_width(&["5h", "7d", scoped.unwrap_or("")]);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(plan) = &claude.plan_type {
        lines.push(plan_line(plan, claude.fetched_at, now, inner));
    }
    // Track windows separately so a lone Plan line does not count as "has data".
    let mut has_data = false;
    if let Some(w) = &claude.five_hour {
        lines.push(gauge_line(
            "5h",
            label_w,
            w.used_percent,
            w.resets_at_unix,
            now,
            inner,
        ));
        has_data = true;
    }
    if let Some(w) = &claude.seven_day {
        lines.push(gauge_line(
            "7d",
            label_w,
            w.used_percent,
            w.resets_at_unix,
            now,
            inner,
        ));
        has_data = true;
    }
    // The per-model weekly cap (Fable today) is volatile on Anthropic's side, so
    // it is only drawn when both the window and its model label are present.
    if let (Some(w), Some(label)) = (&claude.scoped_weekly, scoped) {
        lines.push(gauge_line(
            label,
            label_w,
            w.used_percent,
            w.resets_at_unix,
            now,
            inner,
        ));
        has_data = true;
    }
    if has_data && let Some(line) = detail_line(&claude_balance_parts(claude), inner) {
        lines.push(line);
    }
    if claude.needs_login {
        lines.push(login_hint_line(CLAUDE_LOGIN_HINT));
    } else if !has_data {
        lines.push(dim_line("no rate-limit data"));
    }

    QuotaCard {
        title: "Claude",
        color: CLAUDE_COLOR,
        limit_reached: claude.limit_reached,
        lines,
    }
}

/// Builds the Codex card: plan, 5h / 7d gauges, credits, then reset extras.
fn codex_card(codex: &CodexQuotaSnapshot, now: i64, width: u16) -> QuotaCard {
    let inner = card_inner_w(width);
    let label_w = label_width(&["5h", "7d"]);
    let title = match codex.source {
        QuotaSource::SessionFallback => "Codex (session)",
        QuotaSource::Api | QuotaSource::None => "Codex",
    };

    let lines: Vec<Line> = if codex.source == QuotaSource::None {
        let mut v = vec![dim_line("no Codex quota")];
        if codex.needs_login {
            v.push(login_hint_line(CODEX_LOGIN_HINT));
        } else {
            v.push(dim_line("(no auth.json / sessions)"));
        }
        v
    } else {
        let mut v = vec![plan_line(
            codex.plan_type.as_deref().unwrap_or("?"),
            codex.fetched_at,
            now,
            inner,
        )];
        if let Some(w) = &codex.primary {
            v.push(gauge_line(
                "5h",
                label_w,
                w.used_percent,
                w.resets_at_unix,
                now,
                inner,
            ));
        }
        if let Some(w) = &codex.secondary {
            v.push(gauge_line(
                "7d",
                label_w,
                w.used_percent,
                w.resets_at_unix,
                now,
                inner,
            ));
        }
        // Keep session-fallback data visible but flag the re-login.
        if codex.needs_login {
            v.push(login_hint_line(CODEX_LOGIN_HINT));
        } else {
            if let Some(line) = detail_line(&codex_credit_parts(codex), inner) {
                v.push(line);
            }
            if let Some(line) = detail_line(&codex_extra_parts(codex, now), inner) {
                v.push(line);
            }
        }
        v
    };

    QuotaCard {
        title,
        color: CODEX_COLOR,
        limit_reached: codex.limit_reached == Some(true),
        lines,
    }
}

/// Builds the Copilot card: plan, premium percent gauge, request-count gauge.
fn copilot_card(copilot: &CopilotQuotaSnapshot, now: i64, width: u16) -> QuotaCard {
    let inner = card_inner_w(width);
    let label_w = label_width(&["prem", "reqs"]);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(plan) = &copilot.plan_type {
        lines.push(plan_line(plan, copilot.fetched_at, now, inner));
    }
    if let Some(w) = &copilot.premium {
        lines.push(gauge_line(
            "prem",
            label_w,
            w.used_percent,
            w.resets_at_unix,
            now,
            inner,
        ));
        // A second gauge showing the premium requests as used/total counts.
        if let (Some(rem), Some(total)) = (copilot.premium_remaining, copilot.premium_entitlement)
            && total > 0
        {
            let used = (total - rem).max(0);
            let pct = (used as f64 / total as f64) * 100.0;
            lines.push(gauge_line_value(
                "reqs",
                label_w,
                pct,
                &format!("{used}/{total}"),
                inner,
            ));
        }
    } else if copilot.premium_unlimited {
        lines.push(dim_line("premium: unlimited"));
    }
    let has_content = !lines.is_empty();
    if copilot.needs_login {
        lines.push(login_hint_line(COPILOT_LOGIN_HINT));
    } else if !has_content {
        lines.push(dim_line("no Copilot quota"));
    }

    QuotaCard {
        title: "Copilot",
        color: COPILOT_COLOR,
        limit_reached: copilot.limit_reached,
        lines,
    }
}

/// Builds the Cursor card: plan, total / auto / api gauges, on-demand spend.
fn cursor_card(cursor: &CursorQuotaSnapshot, now: i64, width: u16) -> QuotaCard {
    let inner = card_inner_w(width);
    let label_w = label_width(&["total", "auto", "api"]);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(plan) = &cursor.plan_type {
        lines.push(plan_line(plan, cursor.fetched_at, now, inner));
    }
    for (label, window) in [
        ("total", &cursor.total),
        ("auto", &cursor.auto),
        ("api", &cursor.api),
    ] {
        if let Some(w) = window {
            lines.push(gauge_line(
                label,
                label_w,
                w.used_percent,
                w.resets_at_unix,
                now,
                inner,
            ));
        }
    }
    if let Some(d) = cursor.on_demand_dollars
        && let Some(line) = detail_line(&[format!("on-demand ${d:.2}")], inner)
    {
        lines.push(line);
    }
    let has_content = !lines.is_empty();
    if cursor.needs_login {
        lines.push(login_hint_line(CURSOR_LOGIN_HINT));
    } else if !has_content {
        lines.push(dim_line("no Cursor quota"));
    }

    QuotaCard {
        title: "Cursor",
        color: CURSOR_COLOR,
        limit_reached: cursor.limit_reached,
        lines,
    }
}

/// Builds the Grok card: plan, included-allowance gauge, on-demand and prepaid.
fn grok_card(grok: &GrokQuotaSnapshot, now: i64, width: u16) -> QuotaCard {
    let inner = card_inner_w(width);
    // The label names the period the allowance runs on, which the API reports
    // per account; an unlabelled window falls back to "incl".
    let period = grok.period_label.as_deref().unwrap_or("incl");
    let label_w = label_width(&[period, "ondmd"]);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(plan) = &grok.plan_type {
        lines.push(plan_line(plan, grok.fetched_at, now, inner));
    }
    // Track the gauge separately so a lone Plan line does not count as data.
    let mut has_data = false;
    if let Some(w) = &grok.included {
        lines.push(gauge_line(
            period,
            label_w,
            w.used_percent,
            w.resets_at_unix,
            now,
            inner,
        ));
        has_data = true;
    }
    match (grok.on_demand_dollars, grok.on_demand_cap_dollars) {
        // With a cap configured the spend reads as its own gauge. Both amounts
        // are compacted (`$1.2K`) so a four-figure cap cannot outgrow the
        // narrowest card and get truncated into a smaller, plausible number.
        (Some(used), Some(cap)) if cap > 0.0 => lines.push(gauge_line_value(
            "ondmd",
            label_w,
            (used / cap * 100.0).clamp(0.0, 100.0),
            &format!("{}/{}", format_cost_compact(used), format_cost_compact(cap)),
            inner,
        )),
        (Some(used), _) => {
            if let Some(line) =
                detail_line(&[format!("on-demand {}", format_cost_compact(used))], inner)
            {
                lines.push(line);
            }
        }
        _ => {}
    }
    if let Some(balance) = grok.prepaid_balance_dollars
        && let Some(line) = detail_line(
            &[format!("balance {}", format_cost_compact(balance))],
            inner,
        )
    {
        lines.push(line);
    }
    if grok.needs_login {
        lines.push(login_hint_line(GROK_LOGIN_HINT));
    } else if !has_data {
        lines.push(dim_line("no Grok quota"));
    }

    QuotaCard {
        title: "Grok",
        color: GROK_COLOR,
        limit_reached: grok.limit_reached,
        lines,
    }
}

/// The credit parts of the Codex card.
fn codex_credit_parts(codex: &CodexQuotaSnapshot) -> Vec<String> {
    let balance = if codex.unlimited == Some(true) {
        "credits unlimited".to_string()
    } else if let Some(bal) = &codex.credits_balance {
        format!("credits {bal}")
    } else {
        "credits -".to_string()
    };
    let mut parts = vec![balance];
    if let Some(n) = codex.reset_credits_available
        && n > 0
    {
        parts.push(format!("+{n} reset"));
    }
    parts
}

/// The Codex extras (`reset expires X`, `~L-H msgs`, `cap $X`).
///
/// Reset-credit details lead so the expiry stays visible in a narrow card even
/// when message and spend metadata are also present.
fn codex_extra_parts(codex: &CodexQuotaSnapshot, now: i64) -> Vec<String> {
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
    parts
}

/// The balance parts of the Claude card (mirrors Codex's credit parts).
fn claude_balance_parts(claude: &ClaudeQuotaSnapshot) -> Vec<String> {
    let mut parts = vec![match &claude.balance {
        Some(b) => format!("bal {b}"),
        None => "bal -".to_string(),
    }];
    if let Some(used) = &claude.spend_used {
        parts.push(format!("{used} used"));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use vct_core::models::Provider;

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

        assert_eq!(
            codex_extra_parts(&codex, now),
            ["reset expires 2h13m", "~120-150 msgs", "cap $50"]
        );
    }

    #[test]
    fn codex_extras_distinguishes_non_expiring_credits() {
        let codex = CodexQuotaSnapshot {
            reset_credits_available: Some(2),
            reset_credit_expirations: Some(vec![None, None]),
            ..Default::default()
        };

        assert_eq!(codex_extra_parts(&codex, 1_000), ["reset never expires"]);
    }

    #[test]
    fn codex_extras_omits_expiry_when_details_are_unavailable() {
        let codex = CodexQuotaSnapshot {
            reset_credits_available: Some(2),
            approx_messages: Some((120, 150)),
            ..Default::default()
        };

        assert_eq!(codex_extra_parts(&codex, 1_000), ["~120-150 msgs"]);
    }

    #[test]
    fn codex_extras_does_not_infer_no_expiry_from_capped_details() {
        let codex = CodexQuotaSnapshot {
            reset_credits_available: Some(3),
            reset_credit_expirations: Some(vec![None, None]),
            ..Default::default()
        };

        assert!(codex_extra_parts(&codex, 1_000).is_empty());
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
        let rendered = render_min_card(&codex_card(&codex, now, CARD_GRID.min_w));

        assert!(rendered.contains("reset expires 2h13m"), "got:\n{rendered}");
        assert!(rendered.contains("just now"), "got:\n{rendered}");
    }

    /// Renders a card into the smallest cell the grid ever hands out and returns
    /// its text, so a test can assert what survives that width.
    fn render_min_card(card: &QuotaCard) -> String {
        let mut terminal =
            Terminal::new(TestBackend::new(CARD_GRID.min_w, CARD_GRID.height)).unwrap();
        terminal
            .draw(|frame| render_quota_card(frame, frame.area(), card))
            .expect("card renders");
        let buffer = terminal.backend().buffer();
        (0..CARD_GRID.height)
            .map(|y| {
                (0..CARD_GRID.min_w)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A pay-as-you-go Grok account fills the panel to its documented maximum:
    /// plan, the period-labelled allowance gauge, the on-demand gauge, the
    /// prepaid balance, and the staleness line all have to fit.
    #[test]
    fn grok_panel_fits_every_line_in_the_minimum_cell() {
        let now = 1_000;
        let grok = GrokQuotaSnapshot {
            source: QuotaSource::Api,
            fetched_at: now,
            plan_type: Some("SuperGrok".to_string()),
            included: Some(QuotaWindow {
                used_percent: 42.0,
                resets_at_unix: Some(now + 86_400),
            }),
            period_label: Some("week".to_string()),
            on_demand_dollars: Some(18.40),
            on_demand_cap_dollars: Some(50.0),
            prepaid_balance_dollars: Some(2.50),
            ..Default::default()
        };
        let rendered = render_min_card(&grok_card(&grok, now, CARD_GRID.min_w));

        assert!(rendered.contains("SuperGrok"), "got:\n{rendered}");
        assert!(rendered.contains("week"));
        assert!(rendered.contains("42%"));
        assert!(rendered.contains("$18.40/$50.00"), "got:\n{rendered}");
        assert!(rendered.contains("balance $2.50"), "got:\n{rendered}");
        assert!(rendered.contains("just now"), "got:\n{rendered}");
    }

    /// A four-figure cap must stay readable in the narrowest cell. Printing it
    /// in full overflows and the clip turns `$5000.00` into `$500`, so the
    /// gauge and the numbers next to it tell contradictory stories.
    #[test]
    fn grok_panel_compacts_large_money_rather_than_truncating_it() {
        let grok = GrokQuotaSnapshot {
            source: QuotaSource::Api,
            included: Some(QuotaWindow::default()),
            on_demand_dollars: Some(1234.50),
            on_demand_cap_dollars: Some(5000.0),
            prepaid_balance_dollars: Some(12345.0),
            ..Default::default()
        };
        let rendered = render_min_card(&grok_card(&grok, 0, CARD_GRID.min_w));
        assert!(rendered.contains("$1.24K/$5.00K"), "got:\n{rendered}");
        assert!(rendered.contains("balance $12.3K"), "got:\n{rendered}");
    }

    /// The common free account: no money lines at all, and an unlabelled period
    /// falls back to a generic gauge label rather than rendering nothing.
    #[test]
    fn grok_panel_omits_zero_money_and_labels_an_unknown_period() {
        let now = 1_000;
        let grok = GrokQuotaSnapshot {
            source: QuotaSource::Api,
            fetched_at: now,
            included: Some(QuotaWindow {
                used_percent: 0.0,
                resets_at_unix: Some(now + 3_600),
            }),
            ..Default::default()
        };
        let rendered = render_min_card(&grok_card(&grok, now, CARD_GRID.min_w));

        assert!(rendered.contains("incl"));
        assert!(!rendered.contains("on-demand"));
        assert!(!rendered.contains("balance"));
    }

    /// With no data at all the panel says so, and a rejected token adds the
    /// login hint rather than an empty box.
    #[test]
    fn grok_panel_reports_missing_data_and_a_needed_login() {
        let rendered = render_min_card(&grok_card(&Default::default(), 0, CARD_GRID.min_w));
        assert!(rendered.contains("no Grok quota"));

        let grok = GrokQuotaSnapshot {
            needs_login: true,
            ..Default::default()
        };
        let rendered = render_min_card(&grok_card(&grok, 0, CARD_GRID.min_w));
        assert!(rendered.contains(GROK_LOGIN_HINT));
        assert!(!rendered.contains("no Grok quota"));
    }

    #[test]
    fn grid_packs_by_width_and_wraps_the_remainder() {
        // One rule for every count: as many cards per row as the width allows.
        assert_eq!(quota_grid(&CARD_GRID, CARD_GRID.min_w * 5, 5), (5, 1));
        assert_eq!(quota_grid(&CARD_GRID, 120, 5), (5, 1));
        assert_eq!(quota_grid(&CARD_GRID, 100, 5), (4, 2));
        assert_eq!(quota_grid(&CARD_GRID, 80, 5), (3, 2));
        // Never more columns than there are cards.
        assert_eq!(quota_grid(&CARD_GRID, 400, 2), (2, 1));
        // A terminal too narrow for even one card still gets a single column
        // rather than a division by zero.
        assert_eq!(quota_grid(&CARD_GRID, 10, 3), (1, 3));
        assert_eq!(quota_grid(&CARD_GRID, 120, 0), (0, 0));
    }

    #[test]
    fn grid_absorbs_a_new_provider_without_a_new_case() {
        // The count only ever enters as a list length, so a sixth provider is
        // placed by the same expression as the first.
        for n in 1..=8 {
            let (cols, rows) = quota_grid(&CARD_GRID, 120, n);
            assert!(cols >= 1 && cols <= n);
            assert!(cols * rows >= n);
            assert_eq!(
                grid_height(&CARD_GRID, 120, n),
                rows as u16 * CARD_GRID.height
            );
        }
    }

    #[test]
    fn grid_cells_always_yield_exactly_n_addressable_cards() {
        // The render dispatch walks these cells in present-provider order, so a
        // short or zero-width cell list would drop (or mis-place) a provider.
        for n in 0..=MAX_QUOTA_PANELS {
            for width in [80u16, 100, 120, 160, 200] {
                let band = Rect::new(0, 0, width, grid_height(&CARD_GRID, width, n));
                let cells = grid_cells(&CARD_GRID, band, n);
                assert_eq!(cells.len(), n, "n={n} width={width}");
                assert!(
                    cells
                        .iter()
                        .all(|c| c.width > 0 && c.height == CARD_GRID.height),
                    "n={n} width={width}"
                );
                assert!(
                    cells.iter().all(|c| c.x + c.width <= width),
                    "no cell may overflow the band: n={n} width={width}"
                );
            }
        }
    }

    #[test]
    fn grid_cards_never_shrink_below_the_readable_minimum() {
        // The old band emitted constraints that over-committed the row and let
        // the solver squeeze panels to 16 columns; the grid picks a column count
        // the width can actually hold.
        for width in [74u16, 80, 96, 100, 120, 160, 200] {
            let band = Rect::new(
                0,
                0,
                width,
                grid_height(&CARD_GRID, width, MAX_QUOTA_PANELS),
            );
            for cell in grid_cells(&CARD_GRID, band, MAX_QUOTA_PANELS) {
                assert!(
                    cell.width >= CARD_GRID.min_w || width < CARD_GRID.min_w,
                    "width={width} gave a {}-column card",
                    cell.width
                );
                assert!(cell.width <= CARD_GRID.max_w);
            }
        }
    }

    #[test]
    fn grid_folds_rather_than_starving_the_model_table() {
        let n = MAX_QUOTA_PANELS;
        // Tall enough: the grid is drawn and the table keeps its floor.
        let tall = Rect::new(0, 0, 120, 40);
        assert_eq!(visible_grid_height(tall, true, n), CARD_GRID.height);
        // Short and narrow enough to need two card rows: folding wins.
        let short = Rect::new(0, 0, 80, 24);
        assert_eq!(visible_grid_height(short, true, n), 0);
        // The exact boundary: the table must keep TABLE_MIN_BODY_H body rows.
        let grid = grid_height(&CARD_GRID, 120, n);
        let floor = grid + FOOTER_H + TABLE_MIN_BODY_H + 4;
        assert_eq!(
            visible_grid_height(Rect::new(0, 0, 120, floor - 1), true, n),
            0
        );
        assert_eq!(
            visible_grid_height(Rect::new(0, 0, 120, floor), true, n),
            grid
        );
    }

    #[test]
    fn disabled_band_stays_hidden() {
        // An empty `usage.quota.panels` drops the whole quota surface.
        assert_eq!(visible_grid_height(Rect::new(0, 0, 200, 60), false, 5), 0);
        // ...and so does having no provider with credentials.
        assert_eq!(visible_grid_height(Rect::new(0, 0, 200, 60), true, 0), 0);
    }

    #[test]
    fn card_counts_the_lines_it_could_not_show() {
        // Six lines into five content rows: the card flags the remainder in its
        // title instead of quietly ending at the border.
        let card = QuotaCard {
            title: "Test",
            color: RatatuiColor::Gray,
            limit_reached: false,
            lines: (0..6).map(|i| dim_line(&format!("line{i}"))).collect(),
        };
        let rendered = render_min_card(&card);
        assert!(rendered.contains("+1"), "got:\n{rendered}");
        assert!(rendered.contains("line4"), "got:\n{rendered}");
        assert!(!rendered.contains("line5"), "got:\n{rendered}");
    }

    #[test]
    fn gauge_line_drops_the_reset_marker_before_truncating_its_duration() {
        let inner = card_inner_w(CARD_GRID.min_w);
        // A month-long cycle is the longest duration the API produces.
        let text = line_text(gauge_line("total", 5, 41.0, Some(30 * 86_400), 0, inner));
        assert!(text.chars().count() <= usize::from(inner), "got {text:?}");
        assert!(
            text.contains("30d0h"),
            "the duration itself is never cut: {text:?}"
        );
        // With room the marker comes back.
        let wide = line_text(gauge_line("total", 5, 41.0, Some(30 * 86_400), 0, 40));
        assert!(wide.contains("↻ 30d0h"), "got {wide:?}");
    }

    #[test]
    fn detail_line_counts_the_parts_it_dropped() {
        let parts = vec![
            "reset expires 2h13m".to_string(),
            "~120-150 msgs".to_string(),
            "cap $50".to_string(),
        ];
        let line = detail_line(&parts, card_inner_w(CARD_GRID.min_w)).expect("has parts");
        let text = line_text(line);
        assert!(text.starts_with("reset expires 2h13m"), "got {text:?}");
        assert!(text.ends_with("+2"), "got {text:?}");
        assert!(text.chars().count() <= usize::from(card_inner_w(CARD_GRID.min_w)));
        // Given room, nothing is hidden and no marker appears.
        let full = line_text(detail_line(&parts, 60).expect("has parts"));
        assert_eq!(full, "reset expires 2h13m · ~120-150 msgs · cap $50");
        assert!(detail_line(&[], 40).is_none());
    }

    #[test]
    fn digest_names_the_providers_it_could_not_fit() {
        let items = vec![
            ("Claude", RatatuiColor::Gray, Some(58.0)),
            ("Codex", RatatuiColor::Gray, Some(31.0)),
            ("Copilot", RatatuiColor::Gray, Some(72.0)),
            ("Cursor", RatatuiColor::Gray, Some(41.0)),
            ("Grok", RatatuiColor::Gray, None),
        ];
        let wide = line_text(quota_digest(&items, 120));
        assert!(wide.contains("Claude"), "got {wide:?}");
        assert!(
            wide.contains("Grok -"),
            "a provider with no window still shows: {wide:?}"
        );
        assert!(
            !wide.contains("more"),
            "nothing hidden at 120 columns: {wide:?}"
        );

        let narrow = line_text(quota_digest(&items, 60));
        assert!(narrow.chars().count() <= 60, "got {narrow:?}");
        assert!(narrow.contains("more → Q"), "got {narrow:?}");

        // Even at an absurd width one provider survives, so the row is never
        // blank without an explanation.
        let tiny = line_text(quota_digest(&items, 10));
        assert!(tiny.contains("Claude"), "got {tiny:?}");
        assert!(tiny.contains("+4 more"), "got {tiny:?}");
    }

    /// Builds a `QuotaView` with every provider present, for overlay tests.
    fn every_provider<'a>(
        claude: &'a ClaudeQuotaSnapshot,
        codex: &'a CodexQuotaSnapshot,
        copilot: &'a CopilotQuotaSnapshot,
        cursor: &'a CursorQuotaSnapshot,
        grok: &'a GrokQuotaSnapshot,
    ) -> QuotaView<'a> {
        QuotaView {
            claude,
            codex,
            copilot,
            cursor,
            grok,
            present: QuotaPresence {
                claude: true,
                codex: true,
                copilot: true,
                cursor: true,
                grok: true,
            },
            band_enabled: true,
            rail_visible: true,
            overlay_open: true,
        }
    }

    fn render_overlay(width: u16, height: u16) -> String {
        let (claude, codex, copilot, cursor, grok) = Default::default();
        let quota = every_provider(&claude, &codex, &copilot, &cursor, &grok);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_quota_overlay(frame, frame.area(), &quota, 0))
            .expect("overlay renders");
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn overlay_shows_every_provider_when_it_has_the_room() {
        let rendered = render_overlay(140, 30);
        for name in ["Claude", "Codex", "Copilot", "Cursor", "Grok"] {
            assert!(rendered.contains(name), "{name} missing from:\n{rendered}");
        }
        assert!(rendered.contains("esc to close"), "got:\n{rendered}");
        assert!(
            !rendered.contains("hidden"),
            "nothing was hidden:\n{rendered}"
        );
    }

    #[test]
    fn overlay_says_how_many_providers_it_could_not_reach() {
        // Two columns of one row is all a short terminal fits; the rest are
        // counted in the title rather than ending at the border unannounced.
        let rendered = render_overlay(80, 13);
        assert!(rendered.contains("hidden"), "got:\n{rendered}");
        assert!(rendered.contains("Claude"), "got:\n{rendered}");
    }

    #[test]
    fn overlay_grid_gives_cards_room_the_always_on_grid_cannot() {
        // The overlay exists so a provider's full line list has somewhere to go.
        const _: () = assert!(OVERLAY_GRID.min_w > CARD_GRID.min_w);
        const _: () = assert!(OVERLAY_GRID.height > CARD_GRID.height);

        // Concretely: the fullest provider's lines all survive the overlay cell
        // even when the always-on card had to flag some of them hidden.
        let claude = ClaudeQuotaSnapshot {
            plan_type: Some("max 20x".into()),
            five_hour: Some(QuotaWindow::default()),
            seven_day: Some(QuotaWindow::default()),
            scoped_weekly: Some(QuotaWindow::default()),
            scoped_label: Some("Opus".into()),
            balance: Some("$5.00".into()),
            spend_used: Some("$1.20".into()),
            ..Default::default()
        };
        let card = claude_card(&claude, 0, OVERLAY_GRID.max_w);
        assert!(card.lines.len() <= usize::from(OVERLAY_GRID.height - 2));
    }
}
