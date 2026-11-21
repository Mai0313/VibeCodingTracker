//! Interactive tabbed TUI app with pluggable layouts.
//!
//! [`run_app`] owns the event loop. The Usage and Analysis tabs share one background [`RefreshWorker`] and one session ledger started on the first visit to either, and the Quota tab starts its workers on the first visit. Only the visible tab refreshes on its timer, while the active `LayoutView` owns frame geometry and mouse hit-testing.

use crate::display::analysis::{AnalysisScanOutcome, AnalysisScanner, AnalysisUiState};
use crate::display::common::table::{
    init_process_metrics, process_metrics, refresh_process_metrics,
};
use crate::display::common::theme::theme;
use crate::display::common::tui::{
    InputAction, RefreshWorker, RefreshWorkerError, TerminalSession, handle_input,
    handle_key_input, overlay_repo_hyperlink, refresh_status,
};
use crate::display::settings::{SettingsOutcome, SettingsUiState};
use crate::display::usage::{
    QuotaRuntime, QuotaSnapshots, UsageRefreshPayload, UsageScanOutcome, UsageScanner, UsageUiState,
};
use crate::layouts::{EventResult, LayoutCommand, LayoutContext, LayoutView, RANGES};
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use sysinfo::System;
use vct_core::analysis::AnalysisData;
use vct_core::config::{Config, ProvidersConfig};
use vct_core::ledger::SessionLedger;
use vct_core::models::TimeRange;

/// A view of the app, in tab order.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Per-model token counts and cost.
    #[default]
    Usage,
    /// Per-model file operations and tool calls.
    Analysis,
    /// Live quota cards.
    Quota,
    /// The settings form.
    Settings,
}

impl Tab {
    pub(crate) const ALL: [Self; 4] = [Self::Usage, Self::Analysis, Self::Quota, Self::Settings];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Usage => "Usage",
            Self::Analysis => "Analysis",
            Self::Quota => "Quota",
            Self::Settings => "Settings",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    /// Returns the tab `step` places away, wrapping around.
    fn offset(self, step: isize) -> Self {
        let count = Self::ALL.len() as isize;
        Self::ALL[(self.index() as isize + step).rem_euclid(count) as usize]
    }

    /// Returns whether this tab displays scanned session data.
    pub(crate) fn scans(self) -> bool {
        matches!(self, Self::Usage | Self::Analysis)
    }
}

/// Initial state and dependencies for [`run_app`].
pub struct AppOptions {
    /// The tab shown first.
    pub start: Tab,
    /// The time range the data tabs open on.
    pub time_range: TimeRange,
    /// Whether the usage table starts with provider prefixes merged.
    pub merge: bool,
    /// The loaded settings.
    pub config: Config,
    /// Optional pre-built pool session scans run in; when `None`, the pool is built lazily when a data tab starts scanning.
    pub scan_pool: Option<Arc<rayon::ThreadPool>>,
}

/// Parameters for a background scan, read by the worker at the start of each run.
#[derive(Clone, Copy, PartialEq)]
struct ScanRequest {
    tab: Tab,
    time_range: TimeRange,
    providers: ProvidersConfig,
}

enum ScanData {
    Usage(UsageRefreshPayload),
    Analysis(AnalysisData),
    Unchanged,
}

/// A finished scan and the request it answered.
struct ScanOutput {
    request: ScanRequest,
    data: ScanData,
}

/// Returns the range `step` places along `RANGES`, clamped to either end.
fn step_range(range: TimeRange, step: isize) -> TimeRange {
    let at = RANGES
        .iter()
        .position(|(candidate, _)| *candidate == range)
        .unwrap_or(0) as isize;
    RANGES[(at + step).clamp(0, RANGES.len() as isize - 1) as usize].0
}

/// Returns `(reload, message, ok)` after the external config editor exits.
///
/// Reloads whenever `file_is_valid` is true (even if the editor returned an error after saving); an unparseable file is never reloaded, preventing subsequent saves from overwriting it with defaults.
fn after_editor(result: anyhow::Result<()>, file_is_valid: bool) -> (bool, String, bool) {
    match (file_is_valid, result) {
        (false, _) => (
            false,
            "config.toml does not parse, nothing reloaded: fix it (e)".to_string(),
            false,
        ),
        (true, Ok(())) => (true, "reloaded".to_string(), true),
        (true, Err(error)) => (true, format!("reloaded; {error:#}"), false),
    }
}

/// Returns the footer status for Usage and Analysis, letting an active or failed refresh preempt an auto-update notice.
fn data_tab_status<'a>(
    refresh: Option<&'a str>,
    update_notice: Option<&'a str>,
) -> Option<(&'a str, Color)> {
    let theme = theme();
    refresh
        .map(|text| (text, theme.warn))
        .or_else(|| update_notice.map(|text| (text, theme.good)))
}

/// Returns whether a stale tab should start a rescan now (idle worker and past any `retry_at` backoff).
fn should_rescan(current: bool, active: bool, retry_at: Option<Instant>, now: Instant) -> bool {
    !current && !active && retry_at.is_none_or(|at| now >= at)
}

/// Applies `next` settings to the live app state, swapping the active layout immediately when `general.layout` resolves to a different plugin.
///
/// Only a change to `usage.merge_models` updates `usage`, preserving a CLI `--merge-providers` override across unrelated edits.
fn apply_settings_update(
    config: &mut Config,
    next: Config,
    usage: &mut UsageUiState,
    quota: &mut Option<QuotaRuntime>,
    active_layout_id: &mut &'static str,
    layout_view: &mut Box<dyn LayoutView>,
) {
    if next.usage.quota != config.usage.quota || next.providers != config.providers {
        *quota = None;
    }
    if next.usage.merge_models != config.usage.merge_models {
        usage.set_merge(next.usage.merge_models);
    }
    let resolved = crate::layouts::resolve(&next.general.layout);
    if resolved.meta().id != *active_layout_id {
        *active_layout_id = resolved.meta().id;
        *layout_view = resolved.create();
    }
    *config = next;
}

/// Spawns the shared background scan worker and session ledger used by the Usage and Analysis tabs.
fn spawn_scan_worker(
    refresh_secs: u64,
    request: Arc<Mutex<ScanRequest>>,
    paths: vct_core::utils::HelperPaths,
    pool: Option<Arc<rayon::ThreadPool>>,
    scan_threads: usize,
) -> RefreshWorker<ScanOutput> {
    RefreshWorker::new_with_init(refresh_secs, move || {
        let mut ledger = SessionLedger::open(&paths.cache_dir);
        let mut usage = UsageScanner::new();
        let mut analysis = AnalysisScanner::new();
        let mut pool = pool;
        move || {
            let pool = match &pool {
                Some(pool) => pool,
                None => {
                    let built = Arc::new(vct_core::scan::build_scan_pool(scan_threads)?);
                    pool.insert(built)
                }
            };
            let request = *request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let data = if request.tab == Tab::Analysis {
                match analysis.scan(
                    &paths,
                    request.time_range,
                    request.providers,
                    &mut ledger,
                    pool,
                )? {
                    AnalysisScanOutcome::Updated(data) => ScanData::Analysis(data),
                    AnalysisScanOutcome::Unchanged => ScanData::Unchanged,
                }
            } else {
                match usage.scan(
                    &paths,
                    request.time_range,
                    request.providers,
                    &mut ledger,
                    pool,
                )? {
                    UsageScanOutcome::Updated(payload) => ScanData::Usage(payload),
                    UsageScanOutcome::Unchanged => ScanData::Unchanged,
                }
            };
            Ok(ScanOutput { request, data })
        }
    })
}

/// Runs the interactive TUI app until the user quits.
///
/// # Panics
///
/// Panics if the current process ID cannot be obtained for memory monitoring.
///
/// # Errors
///
/// Returns an error if path resolution fails, terminal setup or restoration fails, reading input or drawing a frame fails, or the scan worker disconnects.
pub fn run_app(options: AppOptions) -> anyhow::Result<()> {
    let AppOptions {
        start,
        time_range,
        merge,
        mut config,
        scan_pool,
    } = options;
    let paths = vct_core::utils::resolve_paths()?;
    let mut tab = start;
    let mut current_time_range = time_range;
    let request = Arc::new(Mutex::new(ScanRequest {
        tab,
        time_range: current_time_range,
        providers: config.providers,
    }));
    // Start ledger open and initial scan before entering raw mode so startup overlaps terminal setup and OSC 10/11 probing.
    let mut worker: Option<RefreshWorker<ScanOutput>> = if start.scans() {
        let interval = if start == Tab::Usage {
            config.usage.refresh_secs()
        } else {
            config.analysis.refresh_secs()
        };
        vct_core::remote::spawn_sync_loop(paths.cache_dir.clone(), config.remotes.clone());
        let mut worker = spawn_scan_worker(
            interval,
            Arc::clone(&request),
            paths.clone(),
            scan_pool.clone(),
            config.performance.resolved_scan_threads(),
        );
        worker.request();
        Some(worker)
    } else {
        None
    };

    let mut terminal = TerminalSession::new()?;
    let result = (|| -> anyhow::Result<()> {
        let (mut active_layout_id, mut layout_view) =
            crate::layouts::create(&config.general.layout);
        let update_notice = Arc::new(OnceLock::new());
        if config.general.auto_update {
            let slot = Arc::clone(&update_notice);
            std::thread::spawn(move || {
                if let Some(version) = vct_core::update::maybe_auto_update(true).installed_version()
                {
                    let _ = slot.set(format!("Updated to v{version}"));
                }
            });
        }
        let mut quota: Option<QuotaRuntime> = None;
        let mut usage = UsageUiState::new(merge);
        let mut analysis = AnalysisUiState::new();
        let mut settings = SettingsUiState::new(
            config.clone(),
            Box::new(|path, value| {
                // Saving into an unparseable config file would overwrite it with defaults.
                if !vct_core::config::file_is_valid() {
                    anyhow::bail!("config.toml does not parse, fix it first (e)");
                }
                vct_core::config::save_setting(path, value)
            }),
        );

        let mut last_published_request = ScanRequest {
            tab,
            time_range: current_time_range,
            providers: config.providers,
        };
        // The request each data tab last showed.
        let mut shown: [Option<ScanRequest>; 2] = [None, None];
        let mut failure_until = None;
        // Earliest instant an automatic retry may run after a failed scan.
        let mut retry_at: Option<Instant> = None;

        let pid = sysinfo::get_current_pid()
            .expect("Failed to get current process ID for memory monitoring");
        let mut sys = System::new();
        init_process_metrics(&mut sys, pid);
        let metrics_interval =
            Duration::from_millis(vct_core::constants::refresh::METRICS_REFRESH_MS);
        let mut last_metrics = Instant::now();
        let mut last_draw: Option<Instant> = None;
        let mut refresh_indicator_drawn = false;
        let mut spinner = 0usize;
        let mut help_open = false;

        loop {
            let mut redraw = false;
            let wanted = ScanRequest {
                tab,
                time_range: current_time_range,
                providers: config.providers,
            };

            if let Some(result) = worker.as_mut().and_then(RefreshWorker::try_result) {
                match result {
                    Ok(output) => {
                        let had_failure = failure_until.is_some() || retry_at.is_some();
                        match output.data {
                            ScanData::Usage(payload) => {
                                usage.apply(payload);
                                shown[0] = Some(output.request);
                                redraw = true;
                            }
                            ScanData::Analysis(data) => {
                                analysis.apply(data);
                                shown[1] = Some(output.request);
                                redraw = true;
                            }
                            ScanData::Unchanged => {
                                if output.request.tab.scans() {
                                    shown[output.request.tab.index()] = Some(output.request);
                                }
                                redraw = had_failure || refresh_indicator_drawn;
                            }
                        }
                        failure_until = None;
                        retry_at = None;
                        vct_core::utils::release_freed_heap();
                    }
                    Err(RefreshWorkerError::Disconnected) => {
                        return Err(anyhow::anyhow!("refresh worker disconnected"));
                    }
                    Err(error) => {
                        log::warn!("refresh failed: {error}");
                        failure_until = Some(Instant::now() + Duration::from_secs(3));
                        retry_at =
                            Some(Instant::now() + Duration::from_secs(config.usage.refresh_secs()));
                        vct_core::utils::release_freed_heap();
                        redraw = true;
                    }
                }
            }

            if tab.scans() {
                let interval = if tab == Tab::Usage {
                    config.usage.refresh_secs()
                } else {
                    config.analysis.refresh_secs()
                };
                let worker = worker.get_or_insert_with(|| {
                    vct_core::remote::spawn_sync_loop(
                        paths.cache_dir.clone(),
                        config.remotes.clone(),
                    );
                    spawn_scan_worker(
                        interval,
                        Arc::clone(&request),
                        paths.clone(),
                        scan_pool.clone(),
                        config.performance.resolved_scan_threads(),
                    )
                });
                worker.set_interval(interval);
                if wanted != last_published_request {
                    *request
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = wanted;
                    last_published_request = wanted;
                }
                let current = shown[tab.index()] == Some(wanted);
                // Rescan stale tabs as soon as the worker is free; after a failure, wait out `retry_at` unless refreshed manually.
                if should_rescan(current, worker.is_active(), retry_at, Instant::now()) {
                    worker.request();
                    redraw = true;
                } else if current {
                    worker.request_if_due();
                }
            }

            let loading = tab.scans() && shown[tab.index()].is_none();
            let tick = if loading {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(1)
            };
            if last_metrics.elapsed() >= metrics_interval {
                last_metrics = Instant::now();
                refresh_process_metrics(&mut sys, pid);
            }
            let metrics = process_metrics(&sys, pid);
            if redraw || last_draw.is_none_or(|at| at.elapsed() >= tick) {
                spinner = spinner.wrapping_add(1);
                let active = worker.as_ref().is_some_and(RefreshWorker::is_active);
                let refresh = refresh_status(active, failure_until);
                refresh_indicator_drawn = refresh.is_some();
                let update_text = update_notice.get().map(String::as_str);
                let data_status = data_tab_status(refresh, update_text);
                let update_status = update_text.map(|text| (text, theme().good));
                let quota_snapshots = (tab == Tab::Quota).then(|| {
                    let runtime = quota.get_or_insert_with(|| {
                        QuotaRuntime::start(
                            &config.usage.quota.panels,
                            config.providers,
                            config.usage.quota_refresh_secs(),
                        )
                    });
                    QuotaSnapshots::read(runtime)
                });
                let completed = terminal.terminal_mut().draw(|f| {
                    let quota_view = quota_snapshots.as_ref().map(|snapshots| {
                        let runtime = quota.as_ref().expect("started for Quota tab");
                        (snapshots.view(runtime), chrono::Local::now().timestamp())
                    });
                    let mut ctx = LayoutContext {
                        tab,
                        time_range: wanted.time_range,
                        loading,
                        retry_pending: retry_at.is_some(),
                        spinner,
                        metrics,
                        data_status,
                        update_status,
                        usage: &mut usage,
                        analysis: &mut analysis,
                        quota: quota_view,
                        settings: &mut settings,
                    };
                    layout_view.draw(f, &mut ctx);
                    if help_open {
                        let rows = layout_view.help_rows(tab).unwrap_or_else(|| help_rows(tab));
                        render_help(f, f.area(), tab, &rows);
                    }
                })?;
                // ratatui cannot embed OSC 8 escapes in cells, so hyperlink the rendered repo label directly.
                overlay_repo_hyperlink(completed.buffer)?;
                last_draw = Some(Instant::now());
            }

            let mut go_to = None;
            let mut command = LayoutCommand::default();

            if tab == Tab::Settings {
                let action = handle_key_input()?;
                match action {
                    InputAction::Quit => break,
                    InputAction::Key(key) if help_open => {
                        match key {
                            KeyCode::Char('q') => break,
                            KeyCode::Esc | KeyCode::Char('?') => help_open = false,
                            _ => {}
                        }
                        last_draw = None;
                    }
                    InputAction::Key(key) => {
                        match settings.handle_key(key) {
                            SettingsOutcome::Quit => break,
                            SettingsOutcome::NextTab => go_to = Some(tab.offset(1)),
                            SettingsOutcome::PrevTab => go_to = Some(tab.offset(-1)),
                            SettingsOutcome::Help => help_open = true,
                            outcome @ (SettingsOutcome::Changed | SettingsOutcome::OpenEditor) => {
                                if outcome == SettingsOutcome::OpenEditor {
                                    let result =
                                        terminal.suspend(vct_core::config::open_in_editor)?;
                                    let (reload, message, ok) =
                                        after_editor(result, vct_core::config::file_is_valid());
                                    if reload {
                                        settings.set_config(vct_core::config::load());
                                    }
                                    settings.set_message(message, ok);
                                }
                                let next = settings.config().clone();
                                apply_settings_update(
                                    &mut config,
                                    next,
                                    &mut usage,
                                    &mut quota,
                                    &mut active_layout_id,
                                    &mut layout_view,
                                );
                            }
                            SettingsOutcome::Stay => {}
                        }
                        last_draw = None;
                    }
                    _ if help_open => {
                        if matches!(action, InputAction::Click { .. }) {
                            help_open = false;
                            last_draw = None;
                        }
                    }
                    _ if action.mouse().is_some() => {
                        let mouse = action.mouse().expect("checked");
                        let mut ctx = LayoutContext {
                            tab,
                            time_range: wanted.time_range,
                            loading,
                            retry_pending: retry_at.is_some(),
                            spinner,
                            metrics,
                            data_status: None,
                            update_status: None,
                            usage: &mut usage,
                            analysis: &mut analysis,
                            quota: None,
                            settings: &mut settings,
                        };
                        if let EventResult::Consumed(cmd) = layout_view.on_mouse(mouse, &mut ctx) {
                            command = cmd;
                        }
                        if command.settings_changed {
                            let next = settings.config().clone();
                            apply_settings_update(
                                &mut config,
                                next,
                                &mut usage,
                                &mut quota,
                                &mut active_layout_id,
                                &mut layout_view,
                            );
                        }
                        last_draw = None;
                    }
                    InputAction::Resize => last_draw = None,
                    _ => {}
                }
            } else {
                let action = handle_input()?;
                if !matches!(action, InputAction::Continue) {
                    last_draw = None;
                }
                match action {
                    InputAction::Quit => break,
                    InputAction::Close | InputAction::Help | InputAction::Click { .. }
                        if help_open =>
                    {
                        help_open = false;
                    }
                    _ if help_open => {}
                    InputAction::Help => help_open = true,
                    _ if action.mouse().is_some() => {
                        let mouse = action.mouse().expect("checked");
                        let mut ctx = LayoutContext {
                            tab,
                            time_range: wanted.time_range,
                            loading,
                            retry_pending: retry_at.is_some(),
                            spinner,
                            metrics,
                            data_status: None,
                            update_status: None,
                            usage: &mut usage,
                            analysis: &mut analysis,
                            quota: None,
                            settings: &mut settings,
                        };
                        if let EventResult::Consumed(cmd) = layout_view.on_mouse(mouse, &mut ctx) {
                            command = cmd;
                        }
                    }
                    InputAction::Close
                    | InputAction::Open
                    | InputAction::Space
                    | InputAction::TogglePane
                    | InputAction::Page(_) => {
                        let key = match action {
                            InputAction::Close => KeyCode::Esc,
                            InputAction::Open => KeyCode::Enter,
                            InputAction::Space => KeyCode::Char(' '),
                            InputAction::TogglePane => KeyCode::Char('p'),
                            InputAction::Page(step) if step < 0 => KeyCode::PageUp,
                            InputAction::Page(_) => KeyCode::PageDown,
                            _ => unreachable!(),
                        };
                        let outcome = {
                            let mut ctx = LayoutContext {
                                tab,
                                time_range: wanted.time_range,
                                loading,
                                retry_pending: retry_at.is_some(),
                                spinner,
                                metrics,
                                data_status: None,
                                update_status: None,
                                usage: &mut usage,
                                analysis: &mut analysis,
                                quota: None,
                                settings: &mut settings,
                            };
                            layout_view.on_key(key, &mut ctx)
                        };
                        match outcome {
                            EventResult::Consumed(cmd) => command = cmd,
                            EventResult::Ignored => {
                                if let InputAction::Page(step) = action
                                    && tab.scans()
                                    && !loading
                                {
                                    let delta = crate::display::common::tui::NavDelta {
                                        lines: (step * 8) as i64,
                                    };
                                    if tab == Tab::Usage {
                                        usage.navigate(delta);
                                    } else {
                                        analysis.navigate(delta);
                                    }
                                }
                            }
                        }
                    }
                    InputAction::NextTab => go_to = Some(tab.offset(1)),
                    InputAction::PrevTab => go_to = Some(tab.offset(-1)),
                    InputAction::StepRange(step) if tab.scans() => {
                        command.step_range = Some(step);
                    }
                    InputAction::Refresh if tab.scans() => {
                        command.refresh = true;
                    }
                    InputAction::StepSort(step) if tab.scans() && !loading => {
                        if tab == Tab::Usage {
                            usage.step_sort(step);
                        } else {
                            analysis.step_sort(step);
                        }
                    }
                    InputAction::Navigate(delta) if tab.scans() && !loading => {
                        if tab == Tab::Usage {
                            usage.navigate(delta);
                        } else {
                            analysis.navigate(delta);
                        }
                    }
                    _ => {}
                }
            }

            if command.quit {
                break;
            }
            if command.open_help {
                help_open = true;
                last_draw = None;
            }
            if let Some(next) = command.switch_tab {
                go_to = Some(next);
            }
            if let Some(target_range) = command.set_range
                && tab.scans()
                && current_time_range != target_range
            {
                current_time_range = target_range;
                failure_until = None;
                retry_at = None;
                last_draw = None;
            }
            if let Some(step) = command.step_range
                && tab.scans()
            {
                let next_range = step_range(current_time_range, step);
                if next_range != current_time_range {
                    current_time_range = next_range;
                    failure_until = None;
                    retry_at = None;
                    last_draw = None;
                }
            }
            if command.refresh
                && tab.scans()
                && let Some(worker) = worker.as_mut()
            {
                failure_until = None;
                retry_at = None;
                worker.request();
                last_draw = None;
            }

            if let Some(next) = go_to
                && next != tab
            {
                tab = next;
                help_open = false;
                {
                    let mut ctx = LayoutContext {
                        tab,
                        time_range: wanted.time_range,
                        loading,
                        retry_pending: false,
                        spinner,
                        metrics,
                        data_status: None,
                        update_status: None,
                        usage: &mut usage,
                        analysis: &mut analysis,
                        quota: None,
                        settings: &mut settings,
                    };
                    layout_view.on_tab_changed(tab, &mut ctx);
                }
                failure_until = None;
                retry_at = None;
                last_draw = None;
            }
        }
        Ok(())
    })();
    terminal.finish(result)
}

/// Returns the default help key list for `tab` when the active layout does not override it.
fn help_rows(tab: Tab) -> Vec<(&'static str, &'static str)> {
    let mut rows = match tab {
        Tab::Usage | Tab::Analysis => vec![
            ("↑ ↓ / wheel", "select a model"),
            ("PgUp PgDn", "move up or down by 8 models"),
            ("⏎", "show the selected model's details"),
            ("← →", "sort by the column to the left or right"),
            ("[ ]", "earlier or later time range"),
            ("r", "scan again now"),
        ],
        Tab::Quota => Vec::new(),
        Tab::Settings => vec![
            ("↑ ↓ / wheel", "move between settings"),
            ("PgUp PgDn", "jump to the previous or next section"),
            ("← →", "change the value"),
            ("space", "turn on or off"),
            ("⏎ / click", "edit: type a value or pick from a list"),
            ("x", "reset to the default"),
            ("e", "open config.toml in $EDITOR"),
        ],
    };
    if tab == Tab::Usage {
        rows.push(("p", "per-provider totals"));
    }
    rows.extend([
        ("tab", "next tab, shift+tab for the previous one"),
        ("esc", "close a panel"),
        ("?", "this list"),
        ("q", "quit"),
    ]);
    rows
}

/// Draws the key list for `tab` centered over `area`.
fn render_help(f: &mut Frame, area: Rect, tab: Tab, rows: &[(&'static str, &'static str)]) {
    let theme = theme();
    let key_w = rows
        .iter()
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!(" {key:<key_w$}  "), Style::default().bold()),
                Span::styled(*what, theme.dim()),
            ])
        })
        .collect();
    let width = area.width.saturating_sub(4).min(66);
    let height = (rows.len() as u16 + 2).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.accent())
                .title(Line::from(Span::styled(
                    format!(" Keys · {} ", tab.label()),
                    theme.title(),
                )))
                .title(Line::from(Span::styled(" esc close ", theme.dim())).right_aligned()),
        ),
        rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_tab_rescans_once_the_worker_is_free_and_a_failure_waits() {
        let now = Instant::now();
        let later = now + Duration::from_secs(10);
        assert!(should_rescan(false, false, None, now));
        assert!(!should_rescan(true, false, None, now));
        assert!(!should_rescan(false, true, None, now));
        assert!(!should_rescan(false, false, Some(later), now));
        assert!(should_rescan(false, false, Some(now), later));
    }

    #[test]
    fn a_file_the_editor_left_broken_is_never_reloaded() {
        let failed = || Err(anyhow::anyhow!("Editor 'vi' exited with 1"));
        assert_eq!(
            after_editor(Ok(()), true),
            (true, "reloaded".to_string(), true)
        );
        let (reload, message, ok) = after_editor(failed(), true);
        assert!(
            reload && !ok && message.contains("exited with 1"),
            "{message}"
        );
        for result in [Ok(()), failed()] {
            let (reload, message, ok) = after_editor(result, false);
            assert!(
                !reload && !ok && message.contains("does not parse"),
                "{message}"
            );
        }
    }

    #[test]
    fn refresh_status_preempts_the_background_update_notice() {
        assert_eq!(data_tab_status(None, None), None);
        assert_eq!(
            data_tab_status(None, Some("Updated to v3.2.0")),
            Some(("Updated to v3.2.0", theme().good))
        );
        assert_eq!(
            data_tab_status(Some("Refreshing..."), Some("Updated to v3.2.0")),
            Some(("Refreshing...", theme().warn))
        );
    }

    #[test]
    fn tabs_wrap_and_ranges_stop_at_either_end() {
        assert_eq!(Tab::Settings.offset(1), Tab::Usage);
        assert_eq!(Tab::Usage.offset(-1), Tab::Settings);
        assert_eq!(step_range(TimeRange::Monthly, 1), TimeRange::All);
        assert_eq!(step_range(TimeRange::All, 1), TimeRange::All);
        assert_eq!(step_range(TimeRange::Weekly, -1), TimeRange::Daily);
        assert_eq!(step_range(TimeRange::Daily, -1), TimeRange::Daily);
    }

    #[test]
    fn the_key_list_names_every_tab_s_own_keys_and_how_to_leave() {
        for tab in Tab::ALL {
            let keys: Vec<_> = help_rows(tab).iter().map(|(key, _)| *key).collect();
            for expected in ["tab", "esc", "?", "q"] {
                assert!(keys.contains(&expected), "{tab:?} lacks {expected}");
            }
            assert!(!keys.contains(&"1-4"), "{tab:?} still lists 1-4");
        }
        let keys = |tab| -> Vec<_> { help_rows(tab).iter().map(|(key, _)| *key).collect() };
        assert!(keys(Tab::Usage).contains(&"p") && !keys(Tab::Analysis).contains(&"p"));
        assert!(!keys(Tab::Usage).contains(&"m"));
        assert!(keys(Tab::Settings).contains(&"e") && !keys(Tab::Quota).contains(&"⏎"));
    }

    #[test]
    fn settings_updates_preserve_cli_merge_override_and_swap_layouts_live() {
        use ratatui::{Terminal, backend::TestBackend};
        use vct_core::usage::summary::{UsageProviderTotals, UsageRow, UsageTotals};

        let render = |usage: &mut UsageUiState| {
            let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
            terminal
                .draw(|f| usage.draw(f, f.area(), None))
                .expect("draws usage");
            let buf = terminal.backend().buffer();
            (0..20u16)
                .map(|y| {
                    (0..100u16)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut cfg = Config::default();
        cfg.usage.merge_models = false;
        let mut usage = UsageUiState::new(true);
        usage.apply(UsageRefreshPayload {
            rows: vec![UsageRow {
                model: "openai/gpt-5.4".to_string(),
                total: 10,
                cost: 1.0,
                ..UsageRow::default()
            }],
            merged_rows: vec![UsageRow {
                model: "gpt-5.4".to_string(),
                total: 10,
                cost: 1.0,
                ..UsageRow::default()
            }],
            totals: UsageTotals::default(),
            provider_totals: UsageProviderTotals::default(),
        });
        let mut quota = None;
        let (mut active_id, mut view) = crate::layouts::create(&cfg.general.layout);

        // Editing an unrelated field keeps the merged view intact.
        let mut next = cfg.clone();
        next.usage.refresh_interval = 30;
        apply_settings_update(
            &mut cfg,
            next,
            &mut usage,
            &mut quota,
            &mut active_id,
            &mut view,
        );
        assert!(!render(&mut usage).contains("openai/gpt-5.4"));

        // Switching `general.layout` swaps the active layout immediately.
        if let Some(&other_id) = crate::layouts::LAYOUT_IDS
            .iter()
            .find(|&&id| id != active_id)
        {
            let mut switched = cfg.clone();
            switched.general.layout = other_id.to_string();
            apply_settings_update(
                &mut cfg,
                switched,
                &mut usage,
                &mut quota,
                &mut active_id,
                &mut view,
            );
            assert_eq!(active_id, other_id);
        }

        // Toggling `merge_models` itself updates the live view.
        let mut toggled_on = cfg.clone();
        toggled_on.usage.merge_models = true;
        apply_settings_update(
            &mut cfg,
            toggled_on,
            &mut usage,
            &mut quota,
            &mut active_id,
            &mut view,
        );
        let mut toggled_off = cfg.clone();
        toggled_off.usage.merge_models = false;
        apply_settings_update(
            &mut cfg,
            toggled_off,
            &mut usage,
            &mut quota,
            &mut active_id,
            &mut view,
        );
        assert!(render(&mut usage).contains("openai/gpt-5.4"));
    }
}
