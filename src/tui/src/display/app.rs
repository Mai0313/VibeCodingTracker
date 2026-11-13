//! The interactive app: one terminal session with a tab per view.
//!
//! [`run_app`] owns the event loop. The Usage and Analysis tabs share one
//! background [`RefreshWorker`] and one session ledger, started on the first
//! visit to either, so opening on the Quota tab scans nothing. Only the tab on
//! screen refreshes on its timer.

use crate::display::analysis::{AnalysisUiState, scan_analysis};
use crate::display::common::table::{
    init_process_metrics, process_metrics, refresh_process_metrics,
};
use crate::display::common::tui::{
    InputAction, RefreshWorker, RefreshWorkerError, TerminalSession, handle_input,
    handle_key_input, overlay_repo_hyperlink, refresh_status, render_loading,
};
use crate::display::settings::{SettingsOutcome, SettingsUiState};
use crate::display::usage::{
    QuotaRuntime, QuotaSnapshots, UsageRefreshPayload, UsageScanner, UsageUiState, draw_quota_tab,
};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::{Line, Span},
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::System;
use vct_core::analysis::AnalysisData;
use vct_core::config::{Config, ProvidersConfig};
use vct_core::ledger::SessionLedger;
use vct_core::models::TimeRange;

/// A view of the app, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// Per-model token counts and cost.
    Usage,
    /// Per-model file operations and tool calls.
    Analysis,
    /// Live quota cards.
    Quota,
    /// The settings form.
    Settings,
}

impl Tab {
    const ALL: [Self; 4] = [Self::Usage, Self::Analysis, Self::Quota, Self::Settings];

    fn label(self) -> &'static str {
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

    /// The tab `step` places along, wrapping around.
    fn offset(self, step: isize) -> Self {
        let count = Self::ALL.len() as isize;
        Self::ALL[(self.index() as isize + step).rem_euclid(count) as usize]
    }

    /// Whether the tab shows scanned session data.
    fn scans(self) -> bool {
        matches!(self, Self::Usage | Self::Analysis)
    }
}

/// What [`run_app`] starts from.
pub struct AppOptions {
    /// The tab shown first.
    pub start: Tab,
    /// The time range the data tabs open on.
    pub time_range: TimeRange,
    /// Whether the usage table starts with provider prefixes merged.
    pub merge: bool,
    /// The loaded settings.
    pub config: Config,
    /// The pool session scans run in.
    pub scan_pool: Arc<rayon::ThreadPool>,
}

/// What a scan is run for; the worker reads it at the start of each run.
#[derive(Clone, Copy, PartialEq)]
struct ScanRequest {
    tab: Tab,
    time_range: TimeRange,
    providers: ProvidersConfig,
}

enum ScanData {
    Usage(UsageRefreshPayload),
    Analysis(AnalysisData),
}

/// A finished scan and the request it answered.
struct ScanOutput {
    request: ScanRequest,
    data: ScanData,
}

fn next_range(range: TimeRange) -> TimeRange {
    match range {
        TimeRange::Daily => TimeRange::Weekly,
        TimeRange::Weekly => TimeRange::Monthly,
        TimeRange::Monthly => TimeRange::All,
        TimeRange::All => TimeRange::Daily,
    }
}

/// Whether to scan now for a tab whose data does not answer what is on screen.
///
/// Never while a scan is running: its result may be the one wanted, and if it
/// answers an older request the next pass asks again. After a failure, not
/// before `retry_at`.
fn should_rescan(current: bool, active: bool, retry_at: Option<Instant>, now: Instant) -> bool {
    !current && !active && retry_at.is_none_or(|at| now >= at)
}

/// Starts the worker both data tabs scan through, with the one ledger they
/// share.
fn spawn_scan_worker(
    refresh_secs: u64,
    request: Arc<Mutex<ScanRequest>>,
    paths: vct_core::utils::HelperPaths,
    pool: Arc<rayon::ThreadPool>,
) -> RefreshWorker<ScanOutput> {
    RefreshWorker::new_with_init(refresh_secs, move || {
        let mut ledger = SessionLedger::open(&paths.cache_dir);
        let mut usage = UsageScanner::new();
        move || {
            let request = *request
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let data = if request.tab == Tab::Analysis {
                ScanData::Analysis(scan_analysis(
                    &paths,
                    request.time_range,
                    request.providers,
                    &mut ledger,
                    &pool,
                )?)
            } else {
                ScanData::Usage(usage.scan(
                    &paths,
                    request.time_range,
                    request.providers,
                    &mut ledger,
                    &pool,
                )?)
            };
            Ok(ScanOutput { request, data })
        }
    })
}

/// Runs the app until the user quits.
///
/// # Errors
///
/// Returns an error if the terminal cannot be set up or restored, if reading
/// an input event or drawing a frame fails, or if the scan worker dies.
///
/// # Panics
///
/// Panics if the current process ID cannot be obtained for memory monitoring.
pub fn run_app(options: AppOptions) -> anyhow::Result<()> {
    let mut terminal = TerminalSession::new()?;
    let result = (|| -> anyhow::Result<()> {
        let AppOptions {
            start,
            time_range,
            merge,
            config,
            scan_pool,
        } = options;
        let paths = vct_core::utils::resolve_paths()?;
        let mut tab = start;
        let mut config = config;
        let mut quota = QuotaRuntime::start(
            &config.usage.quota.panels,
            config.providers,
            config.usage.quota_refresh_secs(),
        );
        // A change to what the quota workers poll is applied on leaving
        // Settings, not per key press: each restart fetches at once.
        let mut quota_stale = false;
        let mut usage = UsageUiState::new(merge);
        let mut analysis = AnalysisUiState::new();
        let mut settings =
            SettingsUiState::new(config.clone(), Box::new(vct_core::config::save_setting));

        let request = Arc::new(Mutex::new(ScanRequest {
            tab,
            time_range,
            providers: config.providers,
        }));
        let mut worker: Option<RefreshWorker<ScanOutput>> = None;
        // The request each data tab last showed.
        let mut shown: [Option<ScanRequest>; 2] = [None, None];
        let mut failure_until = None;
        // When a failed scan may be tried again without being asked for.
        let mut retry_at: Option<Instant> = None;

        let pid = sysinfo::get_current_pid()
            .expect("Failed to get current process ID for memory monitoring");
        let mut sys = System::new();
        init_process_metrics(&mut sys, pid);
        let metrics_interval =
            Duration::from_millis(vct_core::constants::refresh::METRICS_REFRESH_MS);
        let mut last_metrics = Instant::now();
        let mut last_draw: Option<Instant> = None;
        let mut spinner = 0usize;

        loop {
            let mut redraw = false;
            let wanted = ScanRequest {
                tab,
                time_range: request
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .time_range,
                providers: config.providers,
            };

            if let Some(result) = worker.as_mut().and_then(RefreshWorker::try_result) {
                match result {
                    Ok(output) => {
                        match output.data {
                            ScanData::Usage(payload) => {
                                usage.apply(payload);
                                shown[0] = Some(output.request);
                            }
                            ScanData::Analysis(data) => {
                                analysis.apply(data);
                                shown[1] = Some(output.request);
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
                    }
                }
                redraw = true;
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
                        Arc::clone(&scan_pool),
                    )
                });
                worker.set_interval(interval);
                *request
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = wanted;
                let current = shown[tab.index()] == Some(wanted);
                // A tab whose data answers another range or provider set is
                // rescanned as soon as the worker is free; a failed scan waits
                // out the refresh interval, or for `r`.
                if should_rescan(current, worker.is_active(), retry_at, Instant::now()) {
                    worker.request();
                    redraw = true;
                } else if current && worker.request_if_due() {
                    redraw = true;
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
            if redraw || last_draw.is_none_or(|at| at.elapsed() >= tick) {
                spinner = spinner.wrapping_add(1);
                let active = worker.as_ref().is_some_and(RefreshWorker::is_active);
                let status = refresh_status(active, failure_until);
                let snapshots = QuotaSnapshots::read(&quota);
                let view = snapshots.view(&quota, usage.provider_panel_open);
                let now = chrono::Local::now().timestamp();
                let metrics = process_metrics(&sys, pid);
                let completed = terminal.terminal_mut().draw(|f| {
                    let [bar, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)])
                        .areas(f.area());
                    let range = tab.scans().then_some(wanted.time_range);
                    f.render_widget(tab_bar(bar.width, tab, range, metrics), bar);
                    match tab {
                        Tab::Usage | Tab::Analysis if loading => {
                            let message = if retry_at.is_some() {
                                "Load failed, press r to retry"
                            } else {
                                "Loading sessions..."
                            };
                            render_loading(f, body, spinner, message);
                        }
                        Tab::Usage => usage.draw(f, body, &view, status),
                        Tab::Analysis => analysis.draw(f, body, status),
                        Tab::Quota => draw_quota_tab(f, body, &view, now),
                        Tab::Settings => settings.draw(f, body),
                    }
                })?;
                // ratatui can't embed the OSC 8 escape itself, so hyperlink the
                // repo label it just drew.
                overlay_repo_hyperlink(completed.buffer)?;
                last_draw = Some(Instant::now());
            }

            let mut go_to = None;
            if tab == Tab::Settings {
                match handle_key_input()? {
                    InputAction::Quit => break,
                    InputAction::Key(key) => {
                        match settings.handle_key(key) {
                            SettingsOutcome::Quit => break,
                            SettingsOutcome::SwitchTab(index) => {
                                go_to = Tab::ALL.get(index).copied()
                            }
                            SettingsOutcome::Changed => {
                                let next = settings.config().clone();
                                quota_stale |= next.usage.quota != config.usage.quota
                                    || next.providers != config.providers;
                                usage.set_merge(next.usage.merge_models);
                                config = next;
                            }
                            SettingsOutcome::Stay => {}
                        }
                        last_draw = None;
                    }
                    InputAction::Resize => last_draw = None,
                    _ => {}
                }
            } else {
                match handle_input()? {
                    InputAction::Quit => break,
                    InputAction::Close if tab == Tab::Usage && usage.provider_panel_open => {
                        usage.provider_panel_open = false;
                        last_draw = None;
                    }
                    InputAction::Close => break,
                    InputAction::SwitchTab(index) => go_to = Tab::ALL.get(index).copied(),
                    InputAction::NextTab => go_to = Some(tab.offset(1)),
                    InputAction::PrevTab => go_to = Some(tab.offset(-1)),
                    InputAction::CycleRange if tab.scans() => {
                        let mut request = request
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        request.time_range = next_range(request.time_range);
                        failure_until = None;
                        retry_at = None;
                        last_draw = None;
                    }
                    InputAction::Refresh if tab.scans() => {
                        if let Some(worker) = worker.as_mut() {
                            failure_until = None;
                            retry_at = None;
                            worker.request();
                        }
                        last_draw = None;
                    }
                    InputAction::CycleSort if tab.scans() && !loading => {
                        if tab == Tab::Usage {
                            usage.cycle_sort();
                        } else {
                            analysis.cycle_sort();
                        }
                        last_draw = None;
                    }
                    InputAction::Navigate(delta) if tab.scans() && !loading => {
                        if tab == Tab::Usage {
                            usage.navigate(delta);
                        } else {
                            analysis.navigate(delta);
                        }
                        last_draw = None;
                    }
                    InputAction::TogglePane if tab == Tab::Usage && !loading => {
                        usage.provider_panel_open = !usage.provider_panel_open;
                        last_draw = None;
                    }
                    InputAction::ToggleMerge if tab == Tab::Usage && !loading => {
                        usage.toggle_merge();
                        last_draw = None;
                    }
                    InputAction::Resize => last_draw = None,
                    _ => {}
                }
            }

            if let Some(next) = go_to
                && next != tab
            {
                if tab == Tab::Settings && quota_stale {
                    quota = QuotaRuntime::start(
                        &config.usage.quota.panels,
                        config.providers,
                        config.usage.quota_refresh_secs(),
                    );
                    quota_stale = false;
                }
                if next == Tab::Settings {
                    // `m` on the Usage tab writes the file too.
                    config.usage.merge_models = usage.merge_enabled();
                    settings.set_config(config.clone());
                }
                tab = next;
                failure_until = None;
                retry_at = None;
                last_draw = None;
            }
        }
        Ok(())
    })();
    terminal.finish(result)
}

/// Builds the tab bar: the tabs on the left; the time range and this process's
/// memory and CPU on the right, each dropped whole when `width` is short.
fn tab_bar(
    width: u16,
    current: Tab,
    range: Option<TimeRange>,
    metrics: (f64, f32),
) -> Line<'static> {
    let dim = Style::default().fg(Color::DarkGray);
    let on = Style::default().fg(Color::Black).bg(Color::Cyan).bold();
    let mut left = vec![Span::styled(" vct ", Style::default().bold())];
    for tab in Tab::ALL {
        let text = format!(" {} {} ", tab.index() + 1, tab.label());
        left.push(if tab == current {
            Span::styled(text, on)
        } else {
            Span::styled(text, dim)
        });
    }

    let mut ranges = Vec::new();
    if let Some(range) = range {
        ranges.push(Span::styled("range ", dim));
        for (candidate, name) in [
            (TimeRange::Daily, "day"),
            (TimeRange::Weekly, "week"),
            (TimeRange::Monthly, "month"),
            (TimeRange::All, "all"),
        ] {
            let text = format!(" {name} ");
            ranges.push(if candidate == range {
                Span::styled(text, on)
            } else {
                Span::styled(text, dim)
            });
        }
    }
    let (memory_mb, cpu) = metrics;
    let usage = vec![Span::styled(
        format!("  {memory_mb:.1} MB · CPU {cpu:.1}% "),
        dim,
    )];

    let span_w = |spans: &[Span]| {
        spans
            .iter()
            .map(|span| span.content.chars().count())
            .sum::<usize>()
    };
    let mut right = Vec::new();
    let mut room = usize::from(width).saturating_sub(span_w(&left) + 1);
    for part in [ranges, usage] {
        // The range matters more than the readout, so nothing after a part
        // that does not fit is shown either.
        if span_w(&part) > room {
            break;
        }
        room -= span_w(&part);
        right.extend(part);
    }
    let gap = usize::from(width).saturating_sub(span_w(&left) + span_w(&right));
    left.push(Span::raw(" ".repeat(gap)));
    left.extend(right);
    Line::from(left)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn tab_bar_fills_the_width_and_names_every_tab() {
        let line = tab_bar(120, Tab::Usage, Some(TimeRange::All), (97.5, 0.0));
        let shown = text(&line);
        assert_eq!(shown.chars().count(), 120);
        for expected in [
            "1 Usage",
            "2 Analysis",
            "3 Quota",
            "4 Settings",
            "month",
            "97.5 MB",
        ] {
            assert!(shown.contains(expected), "{expected} missing: {shown}");
        }
    }

    #[test]
    fn tab_bar_drops_whole_parts_when_narrow() {
        // The range still fits here, the memory readout does not.
        let narrow = text(&tab_bar(
            80,
            Tab::Analysis,
            Some(TimeRange::Daily),
            (97.5, 0.0),
        ));
        assert!(
            narrow.contains("range") && !narrow.contains("MB"),
            "{narrow}"
        );
        let tiny = text(&tab_bar(
            60,
            Tab::Analysis,
            Some(TimeRange::Daily),
            (97.5, 0.0),
        ));
        assert!(
            tiny.contains("4 Settings") && !tiny.contains("range"),
            "{tiny}"
        );
        // The Quota and Settings tabs have no time range to show.
        let quota = text(&tab_bar(120, Tab::Quota, None, (97.5, 0.0)));
        assert!(!quota.contains("range"), "{quota}");
    }

    #[test]
    fn a_stale_tab_rescans_once_the_worker_is_free_and_a_failure_waits() {
        let now = Instant::now();
        let later = now + Duration::from_secs(10);
        assert!(should_rescan(false, false, None, now));
        // Current data is the timer's business, and a running scan is waited for.
        assert!(!should_rescan(true, false, None, now));
        assert!(!should_rescan(false, true, None, now));
        // A failed scan is retried when its wait is over, not before.
        assert!(!should_rescan(false, false, Some(later), now));
        assert!(should_rescan(false, false, Some(now), later));
    }

    #[test]
    fn tabs_wrap_and_ranges_cycle() {
        assert_eq!(Tab::Settings.offset(1), Tab::Usage);
        assert_eq!(Tab::Usage.offset(-1), Tab::Settings);
        assert_eq!(next_range(TimeRange::All), TimeRange::Daily);
        assert_eq!(next_range(TimeRange::Monthly), TimeRange::All);
    }
}
