//! Usage tab state, scanner, and renderer, plus shared quota workers and card builders.
//!
//! [`UsageScanner`] caches one LiteLLM pricing map per UTC day across scans. [`UsageUiState`] retains the rendered model rows and totals between frames so resizes repaint without re-scanning, and highlights rows whose token counts changed.

use crate::display::common::table::{
    ColumnSpec, DetailItem, DetailSection, TableChrome, aligned_cells, create_controls_with_status,
    create_provider_row, create_ratatui_table, fit_columns, frame_layout, render_detail_panel,
    render_scrollable_table, render_too_small,
};
use crate::display::common::theme::theme;
use crate::display::common::tui::{
    LEDGER_SAVE_INTERVAL, NavDelta, ScrollState, UpdateTracker, remote_snapshot_stamps,
};
use crate::display::common::{ProviderTotal, provider_tui_color};
use crate::display::usage::averages::{
    ProviderStats, UsageProviderTotals, UsageRow, UsageTotals, build_provider_total_rows,
    build_usage_summary, merge_rows_by_base_model,
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, TestBackend},
    layout::{Constraint, Direction, Layout as RatatuiLayout, Rect},
    style::{Color as RatatuiColor, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell as RatatuiCell, Clear, Paragraph, Row as RatatuiRow,
    },
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use vct_core::config::ProvidersConfig;
use vct_core::ledger::SessionLedger;
use vct_core::models::{
    AntigravityBucket, AntigravityQuotaSnapshot, ClaudeQuotaSnapshot, CodexQuotaSnapshot,
    CopilotQuotaSnapshot, CursorQuotaSnapshot, GrokQuotaSnapshot, QuotaSource,
};
use vct_core::pricing::{ModelPricingMap, fetch_model_pricing};
use vct_core::quota::{
    ANTIGRAVITY_LOGIN_HINT, AntigravityState, CLAUDE_LOGIN_HINT, CODEX_LOGIN_HINT,
    COPILOT_LOGIN_HINT, CURSOR_LOGIN_HINT, ClaudeState, CodexState, CopilotState, CursorState,
    GROK_LOGIN_HINT, GrokState, antigravity_quota_present, load_antigravity_cache,
    load_claude_cache, load_codex_cache, load_copilot_cache, load_cursor_cache, load_grok_cache,
    save_antigravity_cache, save_claude_cache, save_codex_cache, save_copilot_cache,
    save_cursor_cache, save_grok_cache, spawn_quota_worker,
};
use vct_core::utils::{
    format_compact, format_cost, format_cost_compact, format_duration_until, format_number,
    get_claude_credentials_path, get_copilot_config_path, get_cursor_auth_path, get_grok_auth_path,
    resolve_paths,
};

/// How wide and tall the cards in a quota grid are.
pub(crate) struct GridSpec {
    /// Narrowest a card may be, and the divisor that decides the column count.
    pub(crate) min_w: u16,
    /// Widest a card grows to; past this the grid centers its slack instead.
    pub(crate) max_w: u16,
    /// Card height, borders included.
    pub(crate) height: u16,
}

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
/// Antigravity color (Google blue) for the quota panel border.
const ANTIGRAVITY_COLOR: RatatuiColor = RatatuiColor::Rgb(66, 133, 244);

/// Height of the permanent Provider Usage digest line; overflow moves into the `p` panel.
const PROVIDER_LINE_H: u16 = 1;
/// Minimum width of the Model column, sized to keep date-suffixed model IDs (such as `claude-sonnet-4-5-20250929`) distinct.
const MODEL_COL_MIN_W: u16 = 26;
/// Model column width threshold above which extra horizontal space goes to the Share bar.
const MODEL_COL_COMFORTABLE_W: u16 = 34;
/// Minimum and maximum widths for the Share column, shown only after all numeric columns fit.
const SHARE_COL_MIN_W: u16 = 12;
const SHARE_COL_MAX_W: u16 = 24;
/// Maximum number of quota cards across all supported quota providers.
const MAX_QUOTA_PANELS: usize = 6;
/// Maximum width of Claude's per-model weekly gauge label so it does not crowd out the gauge bars.
const SCOPED_LABEL_W: u16 = 6;
/// Maximum width of an Antigravity gauge label, fitting `weekly`/`5h`/`20h` while capping longer titles.
const ANTIGRAVITY_LABEL_W: u16 = 9;

/// Which provider quota panels have credentials on this machine.
#[derive(Clone, Copy, Default)]
pub(crate) struct QuotaPresence {
    pub(crate) claude: bool,
    pub(crate) codex: bool,
    pub(crate) copilot: bool,
    pub(crate) cursor: bool,
    pub(crate) grok: bool,
    pub(crate) antigravity: bool,
}

impl QuotaPresence {
    /// Detects presence from each provider's credential file (once at launch).
    fn detect() -> Self {
        let claude = get_claude_credentials_path()
            .map(|p| p.exists())
            .unwrap_or(false);
        // Either session root counts because the API-less fallback reads rate limits from both.
        let codex = resolve_paths()
            .map(|p| {
                p.codex_dir.join("auth.json").exists()
                    || p.codex_session_dirs().iter().any(|d| d.exists())
            })
            .unwrap_or(false);
        let copilot = get_copilot_config_path()
            .map(|p| p.exists())
            .unwrap_or(false);
        let cursor = get_cursor_auth_path().map(|p| p.exists()).unwrap_or(false);
        let grok = get_grok_auth_path().map(|p| p.exists()).unwrap_or(false);
        let antigravity = antigravity_quota_present();
        Self {
            claude,
            codex,
            copilot,
            cursor,
            grok,
            antigravity,
        }
    }

    /// Returns the number of present provider quota panels.
    fn count(&self) -> usize {
        self.claude as usize
            + self.codex as usize
            + self.copilot as usize
            + self.cursor as usize
            + self.grok as usize
            + self.antigravity as usize
    }
}

/// Borrowed quota state passed to the render frame.
pub(crate) struct QuotaView<'a> {
    pub(crate) claude: &'a ClaudeQuotaSnapshot,
    pub(crate) codex: &'a CodexQuotaSnapshot,
    pub(crate) copilot: &'a CopilotQuotaSnapshot,
    pub(crate) cursor: &'a CursorQuotaSnapshot,
    pub(crate) grok: &'a GrokQuotaSnapshot,
    pub(crate) antigravity: &'a AntigravityQuotaSnapshot,
    pub(crate) present: QuotaPresence,
    /// Whether any quota panel is enabled (`!usage.quota.panels.is_empty()`).
    pub(crate) band_enabled: bool,
}

/// Upper bound on rows the [`UpdateTracker`] remembers for change highlighting.
const MAX_TRACKED_ROWS: usize = 100;

/// Hard minimum terminal width/height; below this only a notice is drawn.
const USAGE_MIN_W: u16 = 74;
const USAGE_MIN_H: u16 = 14;

pub(crate) struct UsageRefreshPayload {
    pub(crate) rows: Vec<UsageRow>,
    pub(crate) merged_rows: Vec<UsageRow>,
    pub(crate) totals: UsageTotals,
    pub(crate) provider_totals: UsageProviderTotals,
}

struct QuotaShutdownGuard {
    shutdown: Arc<AtomicBool>,
}

impl Drop for QuotaShutdownGuard {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

pub(crate) struct QuotaRuntime {
    pub(crate) present: QuotaPresence,
    pub(crate) band_enabled: bool,
    pub(crate) claude: Arc<Mutex<ClaudeQuotaSnapshot>>,
    pub(crate) codex: Arc<Mutex<CodexQuotaSnapshot>>,
    pub(crate) copilot: Arc<Mutex<CopilotQuotaSnapshot>>,
    pub(crate) cursor: Arc<Mutex<CursorQuotaSnapshot>>,
    pub(crate) grok: Arc<Mutex<GrokQuotaSnapshot>>,
    pub(crate) antigravity: Arc<Mutex<AntigravityQuotaSnapshot>>,
    _guard: QuotaShutdownGuard,
}

impl QuotaRuntime {
    pub(crate) fn start(
        quota_panels: &[String],
        providers: ProvidersConfig,
        quota_refresh_secs: u64,
    ) -> Self {
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
        present.antigravity &= providers.antigravity && panel_on("antigravity");

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
        let antigravity = Arc::new(Mutex::new(
            present
                .antigravity
                .then(load_antigravity_cache)
                .flatten()
                .unwrap_or_default(),
        ));

        if present.claude
            || present.codex
            || present.copilot
            || present.cursor
            || present.grok
            || present.antigravity
        {
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
                    if present.antigravity {
                        let (client, shutdown, shared) = (
                            client.clone(),
                            Arc::clone(&shutdown),
                            Arc::clone(&antigravity),
                        );
                        let mut state = AntigravityState::default();
                        spawn_quota_worker(
                            "antigravity",
                            shared,
                            shutdown,
                            quota_refresh_secs,
                            move || state.resolve(&client),
                            |snapshot| {
                                let _ = save_antigravity_cache(snapshot);
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
            antigravity,
            _guard: QuotaShutdownGuard { shutdown },
        }
    }
}

/// Active sort column for the usage table, in display column order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum UsageSort {
    Model,
    Input,
    Output,
    CacheRead,
    CacheWrite,
    Total,
    #[default]
    Cost,
}

impl UsageSort {
    const ALL: [Self; 7] = [
        Self::Model,
        Self::Input,
        Self::Output,
        Self::CacheRead,
        Self::CacheWrite,
        Self::Total,
        Self::Cost,
    ];

    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|sort| *sort == self).unwrap_or(0)
    }

    /// Returns the sort column `step` positions away, clamped to the ends.
    pub(crate) fn step(self, step: isize) -> Self {
        let last = Self::ALL.len() as isize - 1;
        Self::ALL[(self.index() as isize + step).clamp(0, last) as usize]
    }

    /// Returns the sort indicator label shown in the table title.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Cost => "sort: cost ▼",
            Self::Total => "sort: total ▼",
            Self::Input => "sort: input ▼",
            Self::Output => "sort: output ▼",
            Self::CacheRead => "sort: cache read ▼",
            Self::CacheWrite => "sort: cache write ▼",
            Self::Model => "sort: model ▲",
        }
    }

    /// Orders `rows` by this column, model name breaking ties.
    fn apply(self, rows: &mut [UsageRow]) {
        rows.sort_by(|a, b| {
            let primary = match self {
                Self::Cost => b
                    .cost
                    .partial_cmp(&a.cost)
                    .unwrap_or(std::cmp::Ordering::Equal),
                Self::Total => b.total.cmp(&a.total),
                Self::Input => b.input_tokens.cmp(&a.input_tokens),
                Self::Output => b.output_with_reasoning().cmp(&a.output_with_reasoning()),
                Self::CacheRead => b.cache_read.cmp(&a.cache_read),
                Self::CacheWrite => b.cache_creation.cmp(&a.cache_creation),
                Self::Model => a.model.cmp(&b.model),
            };
            primary.then_with(|| a.model.cmp(&b.model))
        });
    }
}

/// A copy of every quota worker's latest snapshot, taken once per frame.
#[derive(Default)]
pub(crate) struct QuotaSnapshots {
    claude: ClaudeQuotaSnapshot,
    codex: CodexQuotaSnapshot,
    copilot: CopilotQuotaSnapshot,
    cursor: CursorQuotaSnapshot,
    grok: GrokQuotaSnapshot,
    antigravity: AntigravityQuotaSnapshot,
}

impl QuotaSnapshots {
    /// Copies the current snapshots out of `runtime`.
    pub(crate) fn read(runtime: &QuotaRuntime) -> Self {
        fn copy<T: Clone + Default>(shared: &Mutex<T>) -> T {
            shared.lock().map(|value| value.clone()).unwrap_or_default()
        }
        Self {
            claude: copy(&runtime.claude),
            codex: copy(&runtime.codex),
            copilot: copy(&runtime.copilot),
            cursor: copy(&runtime.cursor),
            grok: copy(&runtime.grok),
            antigravity: copy(&runtime.antigravity),
        }
    }

    /// Returns a [`QuotaView`] borrowing these snapshots and `runtime`'s presence flags.
    pub(crate) fn view(&self, runtime: &QuotaRuntime) -> QuotaView<'_> {
        QuotaView {
            claude: &self.claude,
            codex: &self.codex,
            copilot: &self.copilot,
            cursor: &self.cursor,
            grok: &self.grok,
            antigravity: &self.antigravity,
            present: runtime.present,
            band_enabled: runtime.band_enabled,
        }
    }
}

/// What the usage tab keeps between frames.
pub(crate) struct UsageUiState {
    rows: Vec<UsageRow>,
    merged_rows: Vec<UsageRow>,
    totals: UsageTotals,
    provider_totals: UsageProviderTotals,
    update_tracker: UpdateTracker,
    scroll: ScrollState,
    merge_enabled: bool,
    sort: UsageSort,
}

impl UsageUiState {
    pub(crate) fn new(merge_enabled: bool) -> Self {
        Self {
            rows: Vec::new(),
            merged_rows: Vec::new(),
            totals: UsageTotals::default(),
            provider_totals: UsageProviderTotals::default(),
            update_tracker: UpdateTracker::new(MAX_TRACKED_ROWS, 1000),
            scroll: ScrollState::new(),
            merge_enabled,
            sort: UsageSort::default(),
        }
    }

    fn view(&self) -> &[UsageRow] {
        current_view(self.merge_enabled, &self.rows, &self.merged_rows)
    }

    fn selected_model(&self) -> Option<String> {
        self.scroll
            .table
            .selected()
            .and_then(|index| self.view().get(index))
            .map(|row| row.model.clone())
    }

    /// Re-syncs selection and highlight tracking to the active rows; flashes differences only when `changed` is `true`.
    fn resync(&mut self, previous: Option<String>, changed: bool) {
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
            if changed {
                self.update_tracker.track_update(model, &fingerprint);
            } else {
                self.update_tracker.prime(model, &fingerprint);
            }
        }
    }

    /// Replaces the cached scan data with `payload`, re-sorts, and flashes changed rows.
    pub(crate) fn apply(&mut self, payload: UsageRefreshPayload) {
        let previous = self.selected_model();
        self.rows = payload.rows;
        self.merged_rows = payload.merged_rows;
        self.totals = payload.totals;
        self.provider_totals = payload.provider_totals;
        self.sort.apply(&mut self.rows);
        self.sort.apply(&mut self.merged_rows);
        self.resync(previous, true);
    }

    /// Sets provider-prefix merging without saving it.
    pub(crate) fn set_merge(&mut self, enabled: bool) {
        if enabled != self.merge_enabled {
            let previous = self.selected_model();
            self.merge_enabled = enabled;
            self.resync(previous, false);
        }
    }

    /// Sets the sort directly (e.g. when clicking a column header).
    pub(crate) fn set_sort(&mut self, sort: UsageSort) {
        if sort != self.sort {
            let previous = self.selected_model();
            self.sort = sort;
            self.sort.apply(&mut self.rows);
            self.sort.apply(&mut self.merged_rows);
            self.resync(previous, false);
        }
    }

    /// Moves the sort one column left or right (`←` / `→`).
    pub(crate) fn step_sort(&mut self, step: isize) {
        let previous = self.selected_model();
        self.sort = self.sort.step(step);
        self.sort.apply(&mut self.rows);
        self.sort.apply(&mut self.merged_rows);
        self.resync(previous, false);
    }

    /// Moves the selection.
    pub(crate) fn navigate(&mut self, delta: NavDelta) {
        let rows = self.view().len();
        self.scroll.apply(delta, rows);
    }

    /// Currently visible rows (respecting `merge_enabled`).
    pub(crate) fn rows(&self) -> &[UsageRow] {
        self.view()
    }

    /// Grand totals across all models.
    pub(crate) fn totals(&self) -> &UsageTotals {
        &self.totals
    }

    /// Per-provider totals.
    pub(crate) fn provider_totals(&self) -> &UsageProviderTotals {
        &self.provider_totals
    }

    /// Recent-update highlight tracker.
    pub(crate) fn update_tracker(&self) -> &UpdateTracker {
        &self.update_tracker
    }

    /// Table scroll and selection state.
    pub(crate) fn scroll(&self) -> &ScrollState {
        &self.scroll
    }

    /// Mutable table scroll and selection state.
    pub(crate) fn scroll_mut(&mut self) -> &mut ScrollState {
        &mut self.scroll
    }

    /// Current sort column.
    pub(crate) fn sort(&self) -> UsageSort {
        self.sort
    }

    /// Selected model index, if any.
    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.scroll.table.selected()
    }

    /// Selects model row `index` directly (clamped into range).
    pub(crate) fn select_index(&mut self, index: usize) {
        let len = self.view().len();
        if len > 0 {
            self.scroll.table.select(Some(index.min(len - 1)));
        }
    }

    /// Currently selected row, if any.
    pub(crate) fn selected_row(&self) -> Option<&UsageRow> {
        self.selected_index().and_then(|at| self.view().get(at))
    }

    /// Draws the Usage tab into `area`.
    #[cfg(test)]
    pub(crate) fn draw(&mut self, f: &mut Frame, area: Rect, status: Option<(&str, RatatuiColor)>) {
        let rows = current_view(self.merge_enabled, &self.rows, &self.merged_rows);
        draw_usage_view(
            f,
            area,
            rows,
            &self.totals,
            &self.provider_totals,
            &self.update_tracker,
            &mut self.scroll,
            self.sort,
            status,
            false,
            false,
        );
    }
}

/// Result of one [`UsageScanner::scan`] run.
pub(crate) enum UsageScanOutcome {
    /// New or changed priced usage data to apply to the UI.
    Updated(UsageRefreshPayload),
    /// Zero sources changed and every input affecting the priced summary matched the prior emission.
    Unchanged,
}

#[derive(PartialEq)]
struct UsageSnapshotKey {
    time_range: vct_core::models::TimeRange,
    providers: ProvidersConfig,
    pricing_date: Option<chrono::NaiveDate>,
    local_date: chrono::NaiveDate,
    remote_stamps: Vec<(std::path::PathBuf, Option<std::time::SystemTime>, u64)>,
    total_parsed_sources: usize,
    ledger_entries: usize,
    retained: usize,
    failure_count: usize,
}

/// Stateful usage scanner that caches one pricing map per UTC day across refreshes.
pub(crate) struct UsageScanner {
    pricing: ModelPricingMap,
    scan_options: vct_core::usage::UsageScanOptions,
    loaded_pricing_utc_date: Option<chrono::NaiveDate>,
    last_emitted: Option<UsageSnapshotKey>,
}

impl UsageScanner {
    pub(crate) fn new() -> Self {
        Self {
            pricing: ModelPricingMap::new(HashMap::new()),
            scan_options: vct_core::usage::UsageScanOptions::default(),
            loaded_pricing_utc_date: None,
            last_emitted: None,
        }
    }

    /// Scans `paths` for `time_range` and prices the result, returning [`UsageScanOutcome::Unchanged`] when nothing affecting the displayed summary changed since the previous emission.
    ///
    /// # Errors
    ///
    /// Returns an error if the scan fails or every source fails to parse; partial failures keep the parsed data and log a warning.
    pub(crate) fn scan(
        &mut self,
        paths: &vct_core::utils::HelperPaths,
        time_range: vct_core::models::TimeRange,
        providers: ProvidersConfig,
        ledger: &mut SessionLedger,
        pool: &rayon::ThreadPool,
    ) -> anyhow::Result<UsageScanOutcome> {
        let today = chrono::Utc::now().date_naive();
        if self.loaded_pricing_utc_date != Some(today) {
            match fetch_model_pricing() {
                Ok(map) => {
                    // Refresh tier thresholds with the pricing map; a changed threshold fingerprint invalidates cached scan entries.
                    self.scan_options.tiers = Some(Arc::new(map.tier_thresholds()));
                    self.pricing = map;
                    self.loaded_pricing_utc_date = Some(today);
                }
                Err(error) => {
                    log::warn!("failed to refresh pricing: {error}");
                }
            }
        }

        let local_date = chrono::Local::now().date_naive();
        let remote_before = remote_snapshot_stamps(&paths.cache_dir);
        let collection = match pool.install(|| {
            vct_core::usage::aggregate_usage_from_paths_with_cache_opts(
                paths,
                time_range,
                providers,
                ledger,
                &self.scan_options,
            )
        }) {
            Ok(collection) => collection,
            Err(error) => {
                self.last_emitted = None;
                return Err(error);
            }
        };
        // Persist the ledger at most once per `LEDGER_SAVE_INTERVAL` so live sessions do not rewrite it every refresh.
        if let Err(error) = ledger.save_if_due(LEDGER_SAVE_INTERVAL) {
            log::warn!("failed to save the session ledger: {error:#}");
        }
        if collection.diagnostics.all_failed() {
            self.last_emitted = None;
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

        let stats = ledger.stats();
        let remote_after = remote_snapshot_stamps(&paths.cache_dir);
        let key = UsageSnapshotKey {
            time_range,
            providers,
            pricing_date: self.loaded_pricing_utc_date,
            local_date,
            remote_stamps: remote_after,
            total_parsed_sources: stats.total_parsed_sources,
            ledger_entries: stats.entries,
            retained: stats.retained,
            failure_count: collection.diagnostics.failures.len(),
        };
        if stats.parsed_sources == 0
            && remote_before == key.remote_stamps
            && self.last_emitted.as_ref() == Some(&key)
        {
            return Ok(UsageScanOutcome::Unchanged);
        }

        let mut summary = build_usage_summary(
            &collection.data.models,
            &collection.data.per_provider,
            &collection.data.provider_days,
            &self.pricing,
            &collection.data.stored_costs,
        );
        summary.rows.retain(|row| row.total != 0 || row.cost != 0.0);
        let merged_rows = merge_rows_by_base_model(&summary.rows);
        self.last_emitted = Some(key);
        Ok(UsageScanOutcome::Updated(UsageRefreshPayload {
            rows: summary.rows,
            merged_rows,
            totals: summary.totals,
            provider_totals: summary.provider_totals,
        }))
    }
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

/// Returns the token-only change-highlight fingerprint for `row`, excluding cost and folding `reasoning_tokens` into output.
fn row_fingerprint(row: &UsageRow) -> (i64, i64, i64, i64) {
    (
        row.input_tokens,
        row.output_with_reasoning(),
        row.cache_read,
        row.cache_creation,
    )
}

const USAGE_NUMERIC_COLS: [ColumnSpec; 6] = [
    ColumnSpec {
        header: "Input",
        width: 9,
        drop_rank: 3,
    },
    ColumnSpec {
        header: "Output",
        width: 9,
        drop_rank: 2,
    },
    ColumnSpec {
        header: "Cache Read",
        width: 11,
        drop_rank: 4,
    },
    ColumnSpec {
        header: "Cache Write",
        width: 11,
        drop_rank: 5,
    },
    ColumnSpec {
        header: "Total",
        width: 9,
        drop_rank: 1,
    },
    ColumnSpec {
        header: "Cost (USD)",
        width: 12,
        drop_rank: 0,
    },
];

/// Fitted column layout for a usage model table of a given inner width.
pub(crate) struct UsageTableColumns {
    pub(crate) kept: Vec<usize>,
    pub(crate) dropped: usize,
    pub(crate) model_w: u16,
    pub(crate) share_w: u16,
    pub(crate) header: Vec<&'static str>,
    pub(crate) widths: Vec<Constraint>,
}

impl UsageTableColumns {
    /// Header index corresponding to `sort`, when that column was not dropped.
    pub(crate) fn sorted_header_index(&self, sort: UsageSort) -> Option<usize> {
        match sort.index().checked_sub(1) {
            None => Some(0),
            Some(numeric) => self
                .kept
                .iter()
                .position(|&i| i == numeric)
                .map(|at| at + 1),
        }
    }

    /// Maps a clicked visible header column index back to a [`UsageSort`].
    pub(crate) fn sort_for_header(&self, header_index: usize) -> Option<UsageSort> {
        if header_index == 0 {
            return Some(UsageSort::Model);
        }
        if let Some(&numeric) = self.kept.get(header_index - 1) {
            return UsageSort::ALL.get(numeric + 1).copied();
        }
        if self.share_w > 0 && header_index == self.kept.len() + 1 {
            return Some(UsageSort::Total);
        }
        None
    }
}

/// Computes which usage columns fit inside `inner` content cells.
pub(crate) fn layout_usage_columns(inner: u16) -> UsageTableColumns {
    let (kept, dropped) = fit_columns(&USAGE_NUMERIC_COLS, inner, MODEL_COL_MIN_W);
    let numeric_span: u16 = kept.iter().map(|&i| USAGE_NUMERIC_COLS[i].width + 1).sum();
    // Slack past the model column's cap becomes the share bar.
    let avail = inner.saturating_sub(numeric_span);
    let slack = avail.saturating_sub(MODEL_COL_COMFORTABLE_W);
    let share_w = if dropped == 0 && slack > SHARE_COL_MIN_W {
        (slack - 1).min(SHARE_COL_MAX_W)
    } else {
        0
    };
    let model_w = avail
        .saturating_sub(if share_w > 0 { share_w + 1 } else { 0 })
        .max(MODEL_COL_MIN_W);

    let mut header: Vec<&'static str> = Vec::with_capacity(kept.len() + 2);
    header.push("Model");
    header.extend(kept.iter().map(|&i| USAGE_NUMERIC_COLS[i].header));
    if share_w > 0 {
        header.push("Share");
    }

    let mut widths: Vec<Constraint> = Vec::with_capacity(header.len());
    widths.push(Constraint::Length(model_w));
    widths.extend(
        kept.iter()
            .map(|&i| Constraint::Length(USAGE_NUMERIC_COLS[i].width)),
    );
    if share_w > 0 {
        widths.push(Constraint::Length(share_w));
    }

    UsageTableColumns {
        kept,
        dropped,
        model_w,
        share_w,
        header,
        widths,
    }
}

/// Builds one formatted model row for the usage table, prepending `prefix` to the model name.
pub(crate) fn build_usage_table_row(
    row: &UsageRow,
    total_tokens: i64,
    cols: &UsageTableColumns,
    prefix: &str,
    update_tracker: &UpdateTracker,
) -> RatatuiRow<'static> {
    let theme = theme();
    let style = if update_tracker.is_recently_updated(&row.model) {
        Style::default().bg(theme.flash_bg).bold()
    } else {
        Style::default()
    };
    let all = [
        format_compact(row.input_tokens),
        format_compact(row.output_with_reasoning()),
        format_compact(row.cache_read),
        format_compact(row.cache_creation),
        format_compact(row.total),
        format_cost(row.cost),
    ];
    let mut cells = Vec::with_capacity(cols.header.len());
    cells.push(format!("{prefix}{}", row.model));
    cells.extend(cols.kept.iter().map(|&i| all[i].clone()));
    let mut cells = aligned_cells(cells, 1);
    if cols.share_w > 0 {
        let share = share_cell(row.total, total_tokens, cols.share_w);
        let bar: String = share.chars().take_while(|c| *c == '━').collect();
        let rest = share[bar.len()..].to_string();
        cells.push(RatatuiCell::from(Line::from(vec![
            Span::styled(bar, theme.accent()),
            Span::styled(rest, theme.dim()),
        ])));
    }
    RatatuiRow::new(cells).style(style)
}

/// Draws the Usage tab into `area`: the model table, Provider Usage digest, controls bar, and open overlays.
#[allow(clippy::too_many_arguments)]
fn draw_usage_view(
    f: &mut Frame,
    area: Rect,
    rows_data: &[UsageRow],
    totals: &UsageTotals,
    provider_totals: &UsageProviderTotals,
    update_tracker: &UpdateTracker,
    scroll: &mut ScrollState,
    sort: UsageSort,
    status: Option<(&str, RatatuiColor)>,
    provider_panel_open: bool,
    detail_open: bool,
) {
    let provider_rows = build_provider_total_rows(provider_totals);
    if area.width < USAGE_MIN_W || area.height < USAGE_MIN_H {
        render_too_small(f, USAGE_MIN_W, USAGE_MIN_H);
        return;
    }

    let chunks = frame_layout(area, PROVIDER_LINE_H);
    let inner = chunks.content.width.saturating_sub(2);
    let cols = layout_usage_columns(inner);

    // Grand totals live in the table title rather than as a selectable row.
    let rows: Vec<RatatuiRow> = rows_data
        .iter()
        .map(|row| build_usage_table_row(row, totals.total, &cols, "", update_tracker))
        .collect();

    let row_count = rows.len();
    let title = format!(
        "Models · {} · {} tokens · {} models",
        format_cost(totals.cost),
        format_compact(totals.total),
        rows_data.len()
    );
    let sorted = cols.sorted_header_index(sort);
    let dropped = cols.dropped;
    render_scrollable_table(
        f,
        chunks.content,
        TableChrome {
            title: &title,
            note: Some(if dropped > 0 {
                format!("{} · −{dropped} cols", sort.label())
            } else {
                sort.label().to_string()
            }),
            sorted,
        },
        cols.header,
        rows,
        &cols.widths,
        row_count,
        scroll,
    );

    if let Some(band_area) = chunks.band {
        f.render_widget(
            Paragraph::new(provider_digest(&provider_rows, band_area.width)).centered(),
            band_area,
        );
    }

    f.render_widget(
        create_controls_with_status(
            // Ordered by priority because narrow terminals drop trailing hints; `p` must precede less essential hints.
            &[
                ("↑↓", "move"),
                ("←→", "sort"),
                ("[ ]", "range"),
                ("p", "providers"),
                ("⏎", "details"),
                ("r", "refresh"),
            ],
            status,
            chunks.controls.width,
        ),
        chunks.controls,
    );

    // Drawn last so they cover the frame they are layered over.
    if provider_panel_open {
        render_provider_panel(f, area, &provider_rows);
    }
    if detail_open && let Some(row) = scroll.table.selected().and_then(|at| rows_data.get(at)) {
        let sections = build_usage_detail_sections(row, totals);
        render_detail_panel(f, area, &row.model, &sections);
    }
}

/// Builds the structured sections for one usage row's `Details` panel.
pub(crate) fn build_usage_detail_sections(
    row: &UsageRow,
    totals: &UsageTotals,
) -> Vec<DetailSection> {
    let share = |part: f64, whole: f64| {
        if whole > 0.0 {
            format!("{:.1}%", part / whole * 100.0)
        } else {
            "-".to_string()
        }
    };

    let providers: Vec<DetailItem> = row
        .providers
        .iter()
        .map(|entry| DetailItem::Provider {
            color: provider_tui_color(entry.provider),
            label: entry.provider.display_name(),
            value: format!(
                "{} tok · {}",
                format_compact(entry.total_tokens),
                format_cost(entry.cost)
            ),
        })
        .collect();

    let mut cost_items = vec![DetailItem::Stat {
        label: "Cost",
        value: format_cost(row.cost),
    }];
    if row.total > 0 && row.cost > 0.0 {
        let per_million = row.cost / (row.total as f64 / 1_000_000.0);
        let rate = if per_million < 0.01 {
            format!("${per_million:.4} / 1M")
        } else {
            format!("{} / 1M", format_cost(per_million))
        };
        cost_items.push(DetailItem::Stat {
            label: "Effective rate",
            value: rate,
        });
    }
    cost_items.extend([
        DetailItem::Stat {
            label: "Share of cost",
            value: share(row.cost, totals.cost),
        },
        DetailItem::Stat {
            label: "Share of tokens",
            value: share(row.total as f64, totals.total as f64),
        },
    ]);
    if let Some(matched) = &row.matched_model {
        cost_items.push(DetailItem::Stat {
            label: "Priced as",
            value: matched.clone(),
        });
    }
    if row.variants.len() == 1 {
        cost_items.push(DetailItem::Stat {
            label: "Raw model",
            value: row.variants[0].clone(),
        });
    } else if row.variants.len() > 1 {
        cost_items.push(DetailItem::Stat {
            label: "Merged from",
            value: row.variants.join(", "),
        });
    }

    let mut token_items = vec![
        DetailItem::Stat {
            label: "Input",
            value: format_number(row.input_tokens),
        },
        DetailItem::Stat {
            label: "Output",
            value: format_number(row.output_tokens),
        },
    ];
    if row.reasoning_tokens > 0 {
        token_items.push(DetailItem::Stat {
            label: "Reasoning",
            value: format_number(row.reasoning_tokens),
        });
    }
    token_items.extend([
        DetailItem::Stat {
            label: "Cache read",
            value: format_number(row.cache_read),
        },
        DetailItem::Stat {
            label: "Cache write",
            value: format_number(row.cache_creation),
        },
    ]);
    if row.cache_creation_1h > 0 {
        token_items.push(DetailItem::Stat {
            label: "Cache write (5m)",
            value: format_number(row.cache_creation_5m),
        });
        token_items.push(DetailItem::Stat {
            label: "Cache write (1h)",
            value: format_number(row.cache_creation_1h),
        });
    }
    if row.cache_read > 0 {
        let prompt_total = row.input_tokens + row.cache_read + row.cache_creation;
        if prompt_total > 0 {
            token_items.push(DetailItem::Stat {
                label: "Cache hit rate",
                value: format!(
                    "{:.1}%",
                    row.cache_read as f64 / prompt_total as f64 * 100.0
                ),
            });
        }
    }
    if row.tool_tokens > 0 {
        token_items.push(DetailItem::Stat {
            label: "Tool tokens",
            value: format_number(row.tool_tokens),
        });
    }
    if row.above_tier_tokens > 0 {
        token_items.push(DetailItem::Stat {
            label: "Long-context tier",
            value: format_number(row.above_tier_tokens),
        });
    }
    if row.web_search_requests > 0 {
        token_items.push(DetailItem::Stat {
            label: "Web searches",
            value: format_number(row.web_search_requests),
        });
    }
    token_items.push(DetailItem::Stat {
        label: "Total tokens",
        value: format_number(row.total),
    });

    vec![
        DetailSection {
            heading: "Providers",
            items: providers,
        },
        DetailSection {
            heading: "Cost & Share",
            items: cost_items,
        },
        DetailSection {
            heading: "Tokens",
            items: token_items,
        },
    ]
}

/// Draws one full Usage tab frame into `terminal`.
#[allow(clippy::too_many_arguments)]
fn render_usage_frame<B: Backend<Error: Send + Sync + 'static>>(
    terminal: &mut Terminal<B>,
    rows_data: &[UsageRow],
    totals: &UsageTotals,
    provider_totals: &UsageProviderTotals,
    update_tracker: &UpdateTracker,
    scroll: &mut ScrollState,
    status: Option<&str>,
    provider_panel_open: bool,
) -> anyhow::Result<()> {
    terminal.draw(|f| {
        draw_usage_view(
            f,
            f.area(),
            rows_data,
            totals,
            provider_totals,
            update_tracker,
            scroll,
            UsageSort::default(),
            status.map(|text| (text, theme().warn)),
            provider_panel_open,
            false,
        );
    })?;
    Ok(())
}

/// Production-shaped usage frame fixture used by Criterion benchmarks.
///
/// Owns a [`TestBackend`] so benchmarks exercise the table, provider digest, controls, and terminal diff path without writing control sequences to stdout.
#[doc(hidden)]
pub struct UsageFrameBenchmark {
    terminal: Terminal<TestBackend>,
    rows: Vec<UsageRow>,
    totals: UsageTotals,
    provider_totals: UsageProviderTotals,
    update_tracker: UpdateTracker,
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
                model,
                matched_model: None,
                input_tokens,
                output_tokens,
                reasoning_tokens,
                cache_read,
                cache_creation,
                total,
                cost: scale as f64 * 0.0175,
                ..UsageRow::default()
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

        Ok(Self {
            terminal: Terminal::new(TestBackend::new(width, height))?,
            rows,
            totals,
            provider_totals,
            update_tracker,
            scroll,
        })
    }

    /// Renders one frame with the supplied footer status.
    pub fn render(&mut self, status: Option<&str>) -> anyhow::Result<()> {
        render_usage_frame(
            &mut self.terminal,
            &self.rows,
            &self.totals,
            &self.provider_totals,
            &self.update_tracker,
            &mut self.scroll,
            status,
            false,
        )
    }
}

/// Returns the bar width in cells: 5 normally, or the slack left after label, percent, and reset marker when `wide` is `true`.
fn bar_cells(label_w: usize, inner_w: u16, wide: bool) -> usize {
    if wide {
        usize::from(inner_w)
            .saturating_sub(label_w + 1 + 5 + 10)
            .max(5)
    } else {
        5
    }
}

/// Returns filled and unfilled spans for a `cells`-wide gauge at `pct`% (any positive usage fills at least one cell).
fn bar_spans(pct: f64, cells: usize) -> [Span<'static>; 2] {
    let theme = theme();
    let filled = ((pct / (100.0 / cells as f64)).ceil() as i64).clamp(0, cells as i64) as usize;
    [
        Span::styled("━".repeat(filled), Style::default().fg(theme.gauge(pct))),
        Span::styled("━".repeat(cells - filled), theme.border()),
    ]
}

/// Builds a `width`-column stacked share bar colored by provider token share using largest-remainder rounding, or a dim placeholder when empty.
pub(crate) fn provider_share_bar(
    rows: &[ProviderTotal<'_, ProviderStats>],
    width: u16,
) -> Line<'static> {
    let width = width as usize;
    let segments: Vec<(RatatuiColor, i64)> = rows
        .iter()
        .filter(|row| row.label != "All Providers" && row.stats.total_tokens > 0)
        .map(|row| (row.tui_color, row.stats.total_tokens))
        .collect();
    let total: i64 = segments.iter().map(|(_, t)| *t).sum();

    if width == 0 || total <= 0 {
        return Line::from(Span::styled("░".repeat(width), theme().border()));
    }

    // Largest-remainder apportionment ensures segment widths sum to exactly `width`.
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
pub(crate) fn dim_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), theme().dim()))
}

/// [`dim_line`] fitted to a card's content width.
fn dim_line_fit(text: &str, inner_w: u16) -> Line<'static> {
    dim_line(&fit_text(text, inner_w))
}

/// Returns a bold error-colored login hint fitted to `inner_w` with an ellipsis rather than hard-clipped.
fn login_hint_line(hint: &str, inner_w: u16) -> Line<'static> {
    Line::from(Span::styled(
        fit_text(hint, inner_w),
        Style::default().fg(theme().bad).bold(),
    ))
}

/// Returns `(cols, rows)` to pack `n` quota cards into `width` columns under `spec`.
fn quota_grid(spec: &GridSpec, width: u16, n: usize) -> (usize, usize) {
    if n == 0 {
        return (0, 0);
    }
    let cols = usize::from((width / spec.min_w).max(1)).min(n);
    (cols, n.div_ceil(cols))
}

/// Returns the total height in terminal rows needed for `n` cards at `width`.
fn grid_height(spec: &GridSpec, width: u16, n: usize) -> u16 {
    let (_, rows) = quota_grid(spec, width, n);
    (rows as u16).saturating_mul(spec.height)
}

/// Splits `band` into exactly `n` uniform-width, row-major card cells (leaving any short final row ragged).
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

/// Formats `value / total` as a bar and percentage fitted to `width` columns (empty if `total <= 0` or `width < 6`).
fn share_cell(value: i64, total: i64, width: u16) -> String {
    if total <= 0 || width < 6 {
        return String::new();
    }
    let share = (value as f64 / total as f64).clamp(0.0, 1.0);
    // Reserve 6 columns for "100.0%".
    let bar_w = usize::from(width - 6);
    let filled = (share * bar_w as f64).round() as usize;
    format!(
        "{:<bar_w$}{:>5.1}%",
        "━".repeat(filled.min(bar_w)),
        share * 100.0
    )
}

/// Collects every present provider's card, in the fixed render order.
fn collect_cards(quota: &QuotaView, now: i64, width: u16, wide: bool) -> Vec<QuotaCard> {
    let mut cards = Vec::with_capacity(MAX_QUOTA_PANELS);
    if quota.present.claude {
        cards.push(claude_card(quota.claude, now, width, wide));
    }
    if quota.present.codex {
        cards.push(codex_card(quota.codex, now, width, wide));
    }
    if quota.present.copilot {
        cards.push(copilot_card(quota.copilot, now, width, wide));
    }
    if quota.present.cursor {
        cards.push(cursor_card(quota.cursor, now, width, wide));
    }
    if quota.present.grok {
        cards.push(grok_card(quota.grok, now, width, wide));
    }
    if quota.present.antigravity {
        cards.push(antigravity_card(quota.antigravity, now, width, wide));
    }
    cards
}

/// Returns a keep-mask of digest segments fitting within `width`, dropping indices in `drop_order` while accounting for the `+{hidden} {tail}` suffix and always keeping at least one segment.
fn fit_digest(
    widths: &[usize],
    sep: usize,
    drop_order: &[usize],
    tail: &str,
    width: usize,
) -> Vec<bool> {
    let mut keep = vec![true; widths.len()];
    let mut shown = widths.len();
    let mut used: usize = widths.iter().sum::<usize>() + sep * shown.saturating_sub(1);
    for &victim in drop_order {
        if shown <= 1 {
            break;
        }
        let hidden = widths.len() - shown;
        let tail_w = if hidden > 0 {
            sep + format!("+{hidden} {tail}").chars().count()
        } else {
            0
        };
        if used + tail_w <= width {
            break;
        }
        keep[victim] = false;
        used -= widths[victim] + sep;
        shown -= 1;
    }
    keep
}

/// Column count of one digest segment.
fn segment_width(segment: &[Span<'_>]) -> usize {
    segment
        .iter()
        .map(|span| span.content.chars().count())
        .sum()
}

/// Joins the segments marked `true` in `keep` with `sep` and appends `+{hidden} {tail}` when any are omitted.
fn digest_line(
    segments: Vec<Vec<Span<'static>>>,
    keep: &[bool],
    sep: &'static str,
    tail: &str,
) -> Line<'static> {
    let dim = theme().dim();
    let mut spans: Vec<Span> = Vec::with_capacity(segments.len() * 3);
    for segment in segments
        .into_iter()
        .zip(keep)
        .filter_map(|(segment, keep)| keep.then_some(segment))
    {
        if !spans.is_empty() {
            spans.push(Span::styled(sep, dim));
        }
        spans.extend(segment);
    }
    let hidden = keep.iter().filter(|keep| !**keep).count();
    if hidden > 0 {
        spans.push(Span::styled(sep, dim));
        spans.push(Span::styled(format!("+{hidden} {tail}"), dim));
    }
    Line::from(spans)
}

/// Builds the one-line per-provider cost digest (`● Provider $cost`), dropping the cheapest providers into `+N more → p` when `width` is tight.
pub(crate) fn provider_digest(
    rows: &[ProviderTotal<'_, ProviderStats>],
    width: u16,
) -> Line<'static> {
    /// Compact two-space separator; each segment's colored dot marks the boundary.
    const SEP: &str = "  ";
    const TAIL: &str = "more → p";
    // Skip the aggregate row because the table title already shows the grand total.
    let listed: Vec<_> = rows
        .iter()
        .filter(|row| row.label != "All Providers")
        .collect();
    if listed.is_empty() {
        return dim_line("no provider data yet");
    }

    let segments: Vec<Vec<Span<'static>>> = listed
        .iter()
        .map(|row| {
            vec![
                Span::styled("● ", Style::default().fg(row.tui_color)),
                Span::raw(format!(
                    "{} {}",
                    row.label,
                    format_cost_compact(row.stats.total_cost)
                )),
            ]
        })
        .collect();

    // Drop the cheapest providers first (breaking ties from the tail) so a high-spend provider late in the enum is never hidden.
    let mut drop_order: Vec<usize> = (0..listed.len()).collect();
    drop_order.sort_by(|&a, &b| {
        listed[a]
            .stats
            .total_cost
            .partial_cmp(&listed[b].stats.total_cost)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.cmp(&a))
    });

    let widths: Vec<usize> = segments.iter().map(|spans| segment_width(spans)).collect();
    let keep = fit_digest(
        &widths,
        SEP.chars().count(),
        &drop_order,
        TAIL,
        usize::from(width),
    );
    digest_line(segments, &keep, SEP, TAIL)
}

/// Renders the centered Provider Usage modal (`p`) with per-provider tokens, cost, and the stacked share bar.
pub(crate) fn render_provider_panel(
    f: &mut Frame,
    area: Rect,
    provider_rows: &[ProviderTotal<'_, ProviderStats>],
) {
    let listed = provider_rows
        .iter()
        .filter(|row| row.label != "All Providers")
        .count();
    // Border, header, header margin, share bar, plus a row per provider.
    let height = (listed as u16 + 5).min(area.height.saturating_sub(2));
    let width = area.width.saturating_sub(4).min(48);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    f.render_widget(Clear, rect);
    render_provider_table(f, rect, provider_rows);
}

/// Returns the empty-state message explaining why no quota cards are shown.
pub(crate) fn no_cards_reason(quota: &QuotaView) -> &'static str {
    if quota.band_enabled {
        "No provider on this machine has quota credentials."
    } else {
        "Quota panels are off: turn one on under Settings > Usage."
    }
}

/// Returns `(cell, card)` pairs that fit `area` on `base`, plus the count of omitted providers.
pub(crate) fn fit_cards(
    base: &GridSpec,
    area: Rect,
    quota: &QuotaView,
    now: i64,
    wide: bool,
) -> (Vec<(Rect, QuotaCard)>, usize) {
    // Measure height at `base.min_w` first, then compute the final cell widths before building the rendered cards so line fitting matches the drawn cell.
    let present = quota.present.count();
    // Grow card height to the tallest card only if all present providers still fit.
    let tallest = collect_cards(quota, now, base.min_w, wide)
        .iter()
        .map(|card| card.lines.len())
        .max()
        .unwrap_or(0);
    let grown = GridSpec {
        height: base.height.max(tallest as u16 + 2),
        ..*base
    };
    let spec = if grid_height(&grown, area.width, present) <= area.height {
        &grown
    } else {
        base
    };
    let rows_that_fit = usize::from(area.height / spec.height);
    let (cols, _) = quota_grid(spec, area.width, present);
    let fits = present.min(cols.saturating_mul(rows_that_fit));
    let cells = grid_cells(spec, area, fits);
    let card_w = cells.first().map_or(spec.min_w, |cell| cell.width);
    let cards = collect_cards(quota, now, card_w, wide);
    (cells.into_iter().zip(cards).collect(), present - fits)
}

/// Renders the modal Provider Usage table and stacked share bar, truncating overflow with `+N more`.
fn render_provider_table(
    f: &mut Frame,
    area: Rect,
    provider_rows: &[ProviderTotal<'_, ProviderStats>],
) {
    render_provider_box(
        f,
        area,
        provider_rows,
        Line::from(Span::styled(" Providers ", theme().title())),
        None,
        true,
        BorderType::Plain,
    );
}

/// Renders the non-modal Provider Usage table and stacked share bar with caller-supplied chrome.
pub(crate) fn render_provider_pane(
    f: &mut Frame,
    area: Rect,
    provider_rows: &[ProviderTotal<'_, ProviderStats>],
    title: Line<'static>,
    right_note: Option<Line<'static>>,
    border_type: BorderType,
) {
    render_provider_box(
        f,
        area,
        provider_rows,
        title,
        right_note,
        false,
        border_type,
    );
}

fn render_provider_box(
    f: &mut Frame,
    area: Rect,
    provider_rows: &[ProviderTotal<'_, ProviderStats>],
    title: Line<'static>,
    right_note: Option<Line<'static>>,
    modal: bool,
    border_type: BorderType,
) {
    // Drop the "All Providers" aggregate; the table title carries the totals.
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

    let theme = theme();
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .title(title)
        .border_style(if modal {
            theme.accent()
        } else {
            theme.border()
        });
    if modal {
        let flag = if hidden > 0 {
            format!(" +{hidden} · esc close ")
        } else {
            " esc close ".to_string()
        };
        block = block.title(Line::from(Span::styled(flag, theme.dim())).right_aligned());
    } else if let Some(mut note) = right_note {
        if hidden > 0 {
            note.spans
                .insert(0, Span::styled(format!(" +{hidden} ·"), theme.dim()));
        }
        block = block.title(note.right_aligned());
    } else if hidden > 0 {
        block = block
            .title(Line::from(Span::styled(format!(" +{hidden} "), theme.dim())).right_aligned());
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
            .style(theme.dim()),
        );
    }
    // Render the empty placeholder as a paragraph; as a table cell it would clip to the Provider column width.
    if totals_rows.is_empty() {
        f.render_widget(
            Paragraph::new(dim_line_fit("No provider data yet", rows_area.width)),
            rows_area,
        );
        f.render_widget(
            Paragraph::new(provider_share_bar(provider_rows, bar_area.width)),
            bar_area,
        );
        return;
    }

    let totals_widths = [
        Constraint::Min(9),
        Constraint::Length(11),
        Constraint::Length(11),
    ];
    // Strip the table's own border because the outer block above already draws it.
    let totals_table = create_ratatui_table(
        totals_rows,
        vec!["Provider", "Tokens", "Cost"],
        &totals_widths,
        None,
    )
    .block(Block::default());
    f.render_widget(totals_table, rows_area);

    f.render_widget(
        Paragraph::new(provider_share_bar(provider_rows, bar_area.width)),
        bar_area,
    );
}

/// Pre-wrapped quota card lines ordered most-important-first; excess lines are counted by [`render_quota_card_with_border`].
pub(crate) struct QuotaCard {
    title: &'static str,
    color: RatatuiColor,
    limit_reached: bool,
    lines: Vec<Line<'static>>,
}

/// Content columns available inside a card `width` wide.
fn card_inner_w(width: u16) -> u16 {
    width.saturating_sub(2)
}

#[cfg(test)]
pub(crate) fn render_quota_card(f: &mut Frame, area: Rect, card: &QuotaCard) {
    render_quota_card_with_border(f, area, card, BorderType::Plain);
}

/// Renders `card` into `area` with `border_type`, placing `+N` (hidden lines) and `LIMIT` flags on the top border.
pub(crate) fn render_quota_card_with_border(
    f: &mut Frame,
    area: Rect,
    card: &QuotaCard,
    border_type: BorderType,
) {
    let capacity = usize::from(area.height.saturating_sub(2));
    let hidden = card.lines.len().saturating_sub(capacity);

    let theme = theme();
    let mut flags: Vec<Span> = Vec::new();
    if hidden > 0 {
        flags.push(Span::styled(format!("+{hidden} "), theme.dim()));
    }
    if card.limit_reached {
        flags.push(Span::styled(
            "LIMIT ",
            Style::default().fg(theme.bad).bold(),
        ));
    }
    // A narrow card gives up the dot before a flag overwrites the name.
    let name_w = card.title.chars().count() + 2;
    let room = usize::from(area.width.saturating_sub(2));
    let dot = if name_w + 2 + segment_width(&flags) <= room {
        " ● "
    } else {
        " "
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .title(Line::from(vec![
            Span::styled(dot, Style::default().fg(card.color)),
            Span::styled(format!("{} ", card.title), theme.title()),
        ]))
        .border_style(theme.border());
    if !flags.is_empty() {
        block = block.title(Line::from(flags).right_aligned());
    }

    let shown = card.lines[..capacity.min(card.lines.len())].to_vec();
    f.render_widget(Paragraph::new(shown).block(block), area);
}

/// Truncates `text` to `width` characters, appending `…` when shortened.
fn fit_text(text: &str, width: u16) -> String {
    let width = usize::from(width);
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}

/// Builds a card's header line with `plan` on the left (truncated if needed) and the untruncated fetch age on the right.
fn plan_line(plan: Option<&str>, fetched_at: i64, now: i64, inner_w: u16) -> Line<'static> {
    let (age, color) = staleness(fetched_at, now);
    let inner = usize::from(inner_w);
    let age_w = age.chars().count();
    // One column of separation, so the two never run together.
    let plan_w = inner.saturating_sub(age_w + 1);
    let plan = fit_text(plan.unwrap_or("-"), plan_w as u16);
    let pad = inner.saturating_sub(plan.chars().count() + age_w);
    Line::from(vec![
        Span::styled(plan, theme().dim()),
        Span::raw(" ".repeat(pad)),
        Span::styled(age, Style::default().fg(color)),
    ])
}

/// Returns the formatted fetch age and severity color (dim, warning past 1h, error past 6h).
fn staleness(fetched_at: i64, now: i64) -> (String, RatatuiColor) {
    let theme = theme();
    if fetched_at <= 0 {
        return ("never".to_string(), theme.dim);
    }
    let age = (now - fetched_at).max(0);
    let color = if age > 6 * 3600 {
        theme.bad
    } else if age > 3600 {
        theme.warn
    } else {
        theme.dim
    };
    let ago = format_duration_until(now, fetched_at);
    let text = if ago == "now" {
        "just now".to_string()
    } else {
        format!("{ago} ago")
    };
    (text, color)
}

/// Builds a gauge line padded to `label_w`, degrading the reset marker from `↻ dur` to `dur` to omitted rather than truncating the duration.
fn gauge_line(
    label: &str,
    label_w: usize,
    pct: f64,
    reset: Option<i64>,
    now: i64,
    inner_w: u16,
    wide: bool,
) -> Line<'static> {
    let bar = bar_cells(label_w, inner_w, wide);
    let mut spans = vec![Span::raw(format!("{label:<label_w$} "))];
    spans.extend(bar_spans(pct, bar));
    spans.push(Span::raw(format!(" {pct:>3.0}%")));
    if let Some(reset) = reset {
        let text = format_duration_until(reset, now);
        // label + separator + bar + " nnn%".
        let used = label_w + 1 + bar + 5;
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
            spans.push(Span::styled(marker, theme().dim()));
        }
    }
    Line::from(spans)
}

/// Builds a gauge line displaying `value` instead of a percentage, dropping the bar rather than truncating `value`.
fn gauge_line_value(
    label: &str,
    label_w: usize,
    pct: f64,
    value: &str,
    inner_w: u16,
    wide: bool,
) -> Line<'static> {
    // Match the card's standard bar width when `value` leaves room; otherwise shrink to fit.
    let room = usize::from(inner_w).saturating_sub(label_w + 2 + value.chars().count());
    let bar = bar_cells(label_w, inner_w, wide).min(room);
    let mut spans = vec![Span::raw(format!("{label:<label_w$} "))];
    if bar >= 5 {
        spans.extend(bar_spans(pct, bar));
        spans.push(Span::raw(format!(" {value}")));
    } else {
        // With no bar to carry it, the value takes the traffic-light color.
        spans.push(Span::styled(
            value.to_string(),
            Style::default().fg(theme().gauge(pct)),
        ));
    }
    Line::from(spans)
}

/// Joins as many `parts` as fit in `inner_w` with ` · `, appending `+N` for omitted parts, or returns `None` if empty.
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
    Some(Line::from(Span::styled(text, theme().dim())))
}

/// Returns the label column width for `labels`, with a minimum of 5.
fn label_width(labels: &[&str]) -> usize {
    labels
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(5)
        .max(5)
}

/// Builds the Claude card: plan, 5h / 7d / per-model gauges, then balance.
fn claude_card(claude: &ClaudeQuotaSnapshot, now: i64, width: u16, wide: bool) -> QuotaCard {
    let inner = card_inner_w(width);
    // Cap Anthropic's full model display name to `SCOPED_LABEL_W` so it does not widen the shared label column.
    let scoped = claude
        .scoped_label
        .as_deref()
        .filter(|_| claude.scoped_weekly.is_some())
        .map(|label| fit_text(label, SCOPED_LABEL_W));
    let label_w = label_width(&["5h", "7d", scoped.as_deref().unwrap_or("")]);

    // Always emit the plan/age header so cached snapshots still display staleness.
    let mut lines: Vec<Line> = vec![plan_line(
        claude.plan_type.as_deref(),
        claude.fetched_at,
        now,
        inner,
    )];
    // Place the login hint above gauges so short cells never drop the actionable line.
    if claude.needs_login {
        lines.push(login_hint_line(CLAUDE_LOGIN_HINT, inner));
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
            wide,
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
            wide,
        ));
        has_data = true;
    }
    // Draw the optional per-model weekly cap only when both the window and its label are present.
    if let (Some(w), Some(label)) = (&claude.scoped_weekly, scoped.as_deref()) {
        lines.push(gauge_line(
            label,
            label_w,
            w.used_percent,
            w.resets_at_unix,
            now,
            inner,
            wide,
        ));
        has_data = true;
    }
    if has_data && let Some(line) = detail_line(&claude_balance_parts(claude), inner) {
        lines.push(line);
    }
    // The login hint already explains an empty card, so the two never stack.
    if !has_data && !claude.needs_login {
        lines.push(dim_line_fit("no rate-limit data", inner));
    }

    QuotaCard {
        title: "Claude",
        color: CLAUDE_COLOR,
        limit_reached: claude.limit_reached,
        lines,
    }
}

/// Builds the Codex card: plan, 5h / 7d gauges, credits, then reset extras.
fn codex_card(codex: &CodexQuotaSnapshot, now: i64, width: u16, wide: bool) -> QuotaCard {
    let inner = card_inner_w(width);
    let label_w = label_width(&["5h", "7d"]);
    let title = match codex.source {
        QuotaSource::SessionFallback => "Codex (session)",
        QuotaSource::Api | QuotaSource::None => "Codex",
    };

    let lines: Vec<Line> = if codex.source == QuotaSource::None {
        let mut v = vec![dim_line_fit("no Codex quota", inner)];
        if codex.needs_login {
            v.push(login_hint_line(CODEX_LOGIN_HINT, inner));
        } else {
            v.push(dim_line_fit("(no auth.json / sessions)", inner));
        }
        v
    } else {
        let mut v = vec![plan_line(
            codex.plan_type.as_deref(),
            codex.fetched_at,
            now,
            inner,
        )];
        if codex.needs_login {
            v.push(login_hint_line(CODEX_LOGIN_HINT, inner));
        }
        if let Some(w) = &codex.primary {
            v.push(gauge_line(
                "5h",
                label_w,
                w.used_percent,
                w.resets_at_unix,
                now,
                inner,
                wide,
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
                wide,
            ));
        }
        // Keep gauges visible on re-login, but omit credit/extra lines so they do not crowd the hint.
        if !codex.needs_login {
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
fn copilot_card(copilot: &CopilotQuotaSnapshot, now: i64, width: u16, wide: bool) -> QuotaCard {
    let inner = card_inner_w(width);
    let label_w = label_width(&["prem", "reqs"]);

    let mut lines: Vec<Line> = vec![plan_line(
        copilot.plan_type.as_deref(),
        copilot.fetched_at,
        now,
        inner,
    )];
    if copilot.needs_login {
        lines.push(login_hint_line(COPILOT_LOGIN_HINT, inner));
    }
    let mut has_data = false;
    if let Some(w) = &copilot.premium {
        lines.push(gauge_line(
            "prem",
            label_w,
            w.used_percent,
            w.resets_at_unix,
            now,
            inner,
            wide,
        ));
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
                wide,
            ));
        }
        has_data = true;
    } else if copilot.premium_unlimited {
        lines.push(dim_line_fit("premium: unlimited", inner));
        has_data = true;
    }
    // The login hint already explains an empty card, so the two never stack.
    if !has_data && !copilot.needs_login {
        lines.push(dim_line_fit("no Copilot quota", inner));
    }

    QuotaCard {
        title: "Copilot",
        color: COPILOT_COLOR,
        limit_reached: copilot.limit_reached,
        lines,
    }
}

/// Builds the Cursor card: plan, total / auto / api gauges, on-demand spend.
fn cursor_card(cursor: &CursorQuotaSnapshot, now: i64, width: u16, wide: bool) -> QuotaCard {
    let inner = card_inner_w(width);
    let label_w = label_width(&["total", "auto", "api"]);

    let mut lines: Vec<Line> = vec![plan_line(
        cursor.plan_type.as_deref(),
        cursor.fetched_at,
        now,
        inner,
    )];
    if cursor.needs_login {
        lines.push(login_hint_line(CURSOR_LOGIN_HINT, inner));
    }
    let mut has_data = false;
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
                wide,
            ));
            has_data = true;
        }
    }
    if let Some(d) = cursor.on_demand_dollars
        && let Some(line) = detail_line(&[format!("on-demand ${d:.2}")], inner)
    {
        lines.push(line);
    }
    // The login hint already explains an empty card, so the two never stack.
    if !has_data && !cursor.needs_login {
        lines.push(dim_line_fit("no Cursor quota", inner));
    }

    QuotaCard {
        title: "Cursor",
        color: CURSOR_COLOR,
        limit_reached: cursor.limit_reached,
        lines,
    }
}

/// Builds the Grok card: plan, included-allowance gauge, on-demand and prepaid.
fn grok_card(grok: &GrokQuotaSnapshot, now: i64, width: u16, wide: bool) -> QuotaCard {
    let inner = card_inner_w(width);
    let period = grok.period_label.as_deref().unwrap_or("incl");
    let label_w = label_width(&[period, "ondmd"]);

    let mut lines: Vec<Line> = vec![plan_line(
        grok.plan_type.as_deref(),
        grok.fetched_at,
        now,
        inner,
    )];
    if grok.needs_login {
        lines.push(login_hint_line(GROK_LOGIN_HINT, inner));
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
            wide,
        ));
        has_data = true;
    }
    match (grok.on_demand_dollars, grok.on_demand_cap_dollars) {
        // Compact both amounts so a four-figure cap fits narrow cards without truncating into a misleading value.
        (Some(used), Some(cap)) if cap > 0.0 => lines.push(gauge_line_value(
            "ondmd",
            label_w,
            (used / cap * 100.0).clamp(0.0, 100.0),
            &format!("{}/{}", format_cost_compact(used), format_cost_compact(cap)),
            inner,
            wide,
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
    // The login hint already explains an empty card, so the two never stack.
    if !has_data && !grok.needs_login {
        lines.push(dim_line_fit("no Grok quota", inner));
    }

    QuotaCard {
        title: "Grok",
        color: GROK_COLOR,
        limit_reached: grok.limit_reached,
        lines,
    }
}

/// Builds the Antigravity card with age-only header (no plan name in the quota summary), grouped by pool with `5h` limits first.
fn antigravity_card(
    antigravity: &AntigravityQuotaSnapshot,
    now: i64,
    width: u16,
    wide: bool,
) -> QuotaCard {
    let inner = card_inner_w(width);
    let mut lines: Vec<Line> = vec![plan_line(None, antigravity.fetched_at, now, inner)];
    if antigravity.needs_login {
        lines.push(login_hint_line(ANTIGRAVITY_LOGIN_HINT, inner));
    }
    let label_w = label_width(
        &antigravity
            .buckets
            .iter()
            .map(|b| b.window.as_str())
            .collect::<Vec<_>>(),
    )
    .min(usize::from(ANTIGRAVITY_LABEL_W));
    let mut pools: Vec<(&str, Vec<&AntigravityBucket>)> = Vec::new();
    for bucket in &antigravity.buckets {
        match pools.iter_mut().find(|(name, _)| *name == bucket.group) {
            Some((_, limits)) => limits.push(bucket),
            None => pools.push((&bucket.group, vec![bucket])),
        }
    }
    for (name, mut limits) in pools {
        limits.sort_by_key(|b| b.window != "5h");
        lines.push(Line::from(fit_text(name, inner)));
        for bucket in limits {
            lines.push(gauge_line(
                &fit_text(&bucket.window, ANTIGRAVITY_LABEL_W),
                label_w,
                bucket.usage.used_percent,
                bucket.usage.resets_at_unix,
                now,
                inner,
                wide,
            ));
        }
    }
    // The login hint already explains an empty card, so the two never stack.
    if antigravity.buckets.is_empty() && !antigravity.needs_login {
        lines.push(dim_line_fit("no Antigravity quota", inner));
    }

    QuotaCard {
        title: "Antigravity",
        color: ANTIGRAVITY_COLOR,
        limit_reached: antigravity.limit_reached,
        lines,
    }
}

/// Returns the credit balance detail parts for the Codex card.
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

/// Returns Codex extra detail parts ordered with reset-credit expiry first so it survives narrow-card truncation.
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

/// Returns the balance and spend detail parts for the Claude card.
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
    use crate::display::usage::draw_quota_tab;
    use vct_core::models::{Provider, QuotaWindow};

    /// Compact card dimensions used to pin narrow-card truncation behavior.
    const CARD_GRID: GridSpec = GridSpec {
        min_w: 24,
        max_w: 40,
        height: 7,
    };

    /// A cell with more room than the narrow test grid gives a card.
    const ROOMY_GRID: GridSpec = GridSpec {
        min_w: 34,
        max_w: 46,
        height: 9,
    };

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
    fn wide_value_gauge_keeps_a_bar_beside_a_long_value() {
        let text = line_text(gauge_line_value(
            "ondmd",
            5,
            25.0,
            "$123.45/$500.00",
            38,
            true,
        ));
        assert!(text.contains('━'), "{text}");
        assert!(text.ends_with("$123.45/$500.00"), "{text}");
        assert!(text.chars().count() <= 38, "{text}");
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
        let rendered = render_min_card(&codex_card(&codex, now, CARD_GRID.min_w, false));

        assert!(rendered.contains("reset expires 2h13m"), "got:\n{rendered}");
        assert!(rendered.contains("just now"), "got:\n{rendered}");
    }

    #[test]
    fn codex_weekly_only_card_omits_five_hour_label() {
        let now = 1_000;
        let codex = CodexQuotaSnapshot {
            source: QuotaSource::Api,
            fetched_at: now,
            plan_type: Some("plus".to_string()),
            secondary: Some(QuotaWindow {
                used_percent: 42.0,
                resets_at_unix: Some(now + 7 * 86_400),
            }),
            ..Default::default()
        };

        let rendered = render_min_card(&codex_card(&codex, now, CARD_GRID.min_w, false));
        assert!(rendered.contains("7d"), "got:\n{rendered}");
        assert!(!rendered.contains("5h"), "got:\n{rendered}");
    }

    /// Renders `card` into the smallest [`CARD_GRID`] cell and returns its buffer text.
    fn render_min_card(card: &QuotaCard) -> String {
        render_card_in(card, CARD_GRID.min_w, CARD_GRID.height)
    }

    /// Renders `card` into a `w` x `h` cell (borders included) and returns its buffer text.
    fn render_card_in(card: &QuotaCard, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| render_quota_card(frame, frame.area(), card))
            .expect("card renders");
        let buffer = terminal.backend().buffer();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Full pay-as-you-go Grok snapshots fit plan, allowance, on-demand gauge, balance, and age in the minimum cell.
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
        let rendered = render_min_card(&grok_card(&grok, now, CARD_GRID.min_w, false));

        assert!(rendered.contains("SuperGrok"), "got:\n{rendered}");
        assert!(rendered.contains("week"));
        assert!(rendered.contains("42%"));
        assert!(rendered.contains("$18.40/$50.00"), "got:\n{rendered}");
        assert!(rendered.contains("balance $2.50"), "got:\n{rendered}");
        assert!(rendered.contains("just now"), "got:\n{rendered}");
    }

    /// Large Grok dollar amounts are compacted so they stay readable in the narrowest cell without truncation.
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
        let rendered = render_min_card(&grok_card(&grok, 0, CARD_GRID.min_w, false));
        assert!(rendered.contains("$1.24K/$5.00K"), "got:\n{rendered}");
        assert!(rendered.contains("balance $12.3K"), "got:\n{rendered}");
    }

    /// Free Grok snapshots omit money lines and label an unknown allowance period as `incl`.
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
        let rendered = render_min_card(&grok_card(&grok, now, CARD_GRID.min_w, false));

        assert!(rendered.contains("incl"));
        assert!(!rendered.contains("on-demand"));
        assert!(!rendered.contains("balance"));
    }

    /// Empty Grok snapshots show a no-data notice, and `needs_login` replaces it with the login hint.
    #[test]
    fn grok_panel_reports_missing_data_and_a_needed_login() {
        let rendered = render_min_card(&grok_card(&Default::default(), 0, CARD_GRID.min_w, false));
        assert!(rendered.contains("no Grok quota"));

        let grok = GrokQuotaSnapshot {
            needs_login: true,
            ..Default::default()
        };
        let rendered = render_min_card(&grok_card(&grok, 0, CARD_GRID.min_w, false));
        assert!(rendered.contains(GROK_LOGIN_HINT));
        assert!(!rendered.contains("no Grok quota"));
    }

    fn antigravity_snapshot(now: i64) -> AntigravityQuotaSnapshot {
        let bucket =
            |group: &str, window: &str, used_percent: f64, reset_in: i64| AntigravityBucket {
                group: group.to_string(),
                window: window.to_string(),
                usage: QuotaWindow {
                    used_percent,
                    resets_at_unix: Some(now + reset_in),
                },
            };
        AntigravityQuotaSnapshot {
            source: QuotaSource::Api,
            fetched_at: now,
            buckets: vec![
                bucket("Gemini Models", "weekly", 8.0, 2 * 86_400 + 18 * 3_600),
                bucket("Gemini Models", "5h", 23.0, 2 * 3_600 + 50 * 60),
                bucket("Claude and GPT models", "weekly", 57.0, 20 * 3_600),
                bucket("Claude and GPT models", "5h", 0.0, 5 * 3_600),
            ],
            ..Default::default()
        }
    }

    /// Antigravity cards group buckets by pool name and sort `5h` limits ahead of other windows.
    #[test]
    fn antigravity_panel_lists_each_pool_then_its_limits_5h_first() {
        let now = 1_000;
        let card = antigravity_card(&antigravity_snapshot(now), now, ROOMY_GRID.min_w, false);
        let lines: Vec<String> = card
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        let heads: Vec<&str> = lines[1..]
            .iter()
            .map(|line| line.split_whitespace().next().unwrap_or(""))
            .collect();
        assert_eq!(
            heads,
            ["Gemini", "5h", "weekly", "Claude", "5h", "weekly"],
            "got {lines:#?}"
        );
        assert!(lines[1].starts_with("Gemini Models"), "got {lines:#?}");
        assert!(
            lines[4].starts_with("Claude and GPT models"),
            "got {lines:#?}"
        );
        assert!(lines[2].contains("23%"), "got {lines:#?}");
        assert!(lines[3].contains("8%"), "got {lines:#?}");
    }

    /// Roomy cells fit all Antigravity pool lines while compact cells flag overflow with `+N`.
    #[test]
    fn antigravity_panel_fits_a_roomy_cell_and_flags_the_rest_in_the_grid() {
        let now = 1_000;
        let snapshot = antigravity_snapshot(now);
        let roomy = render_card_in(
            &antigravity_card(&snapshot, now, ROOMY_GRID.min_w, false),
            ROOMY_GRID.min_w,
            ROOMY_GRID.height,
        );
        assert!(roomy.contains("Claude and GPT models"), "got:\n{roomy}");
        assert!(!roomy.contains('+'), "nothing may be hidden:\n{roomy}");

        let grid = render_min_card(&antigravity_card(&snapshot, now, CARD_GRID.min_w, false));
        assert!(grid.contains("+2"), "hidden lines must be flagged:\n{grid}");
    }

    /// Narrow cards drop the title dot before truncating the provider name beside `LIMIT` and `+N` flags.
    #[test]
    fn a_narrow_card_title_gives_up_its_dot_before_its_name() {
        let card = |title, lines: usize| QuotaCard {
            title,
            color: RatatuiColor::Gray,
            limit_reached: true,
            lines: vec![Line::from("x"); lines],
        };
        let crowded = render_min_card(&card("Antigravity", usize::from(CARD_GRID.height)));
        let title = crowded.lines().next().unwrap();
        assert!(
            title.contains("Antigravity") && title.contains("LIMIT") && title.contains('+'),
            "{title}"
        );
        assert!(!title.contains('●'), "{title}");
        // With room for all of it the dot stays.
        let roomy = render_min_card(&card("Codex", 1));
        assert!(roomy.lines().next().unwrap().contains("● Codex"), "{roomy}");
    }

    /// Long Antigravity window titles truncate with an ellipsis so the gauge bar and percent stay visible.
    #[test]
    fn antigravity_panel_cuts_a_long_label_rather_than_the_gauge() {
        let antigravity = AntigravityQuotaSnapshot {
            buckets: vec![AntigravityBucket {
                group: "Gemini Models".to_string(),
                window: "Weekly Limit Remaining".to_string(),
                usage: QuotaWindow {
                    used_percent: 42.0,
                    resets_at_unix: None,
                },
            }],
            ..Default::default()
        };
        let rendered = render_min_card(&antigravity_card(&antigravity, 0, CARD_GRID.min_w, false));
        assert!(rendered.contains("Weekly L…"), "got:\n{rendered}");
        assert!(rendered.contains("42%"), "got:\n{rendered}");
    }

    #[test]
    fn antigravity_panel_reports_missing_data_and_a_needed_login() {
        let rendered = render_min_card(&antigravity_card(
            &Default::default(),
            0,
            CARD_GRID.min_w,
            false,
        ));
        assert!(
            rendered.contains("no Antigravity quota"),
            "got:\n{rendered}"
        );

        let antigravity = AntigravityQuotaSnapshot {
            needs_login: true,
            ..Default::default()
        };
        let rendered = render_min_card(&antigravity_card(&antigravity, 0, CARD_GRID.min_w, false));
        assert!(
            rendered.contains(ANTIGRAVITY_LOGIN_HINT),
            "got:\n{rendered}"
        );
        assert!(!rendered.contains("no Antigravity quota"));
    }

    /// All 10 providers in display order for full-capacity digest and panel tests.
    const PROVIDER_NAMES: [Provider; 10] = [
        Provider::ClaudeCode,
        Provider::Codex,
        Provider::Copilot,
        Provider::Gemini,
        Provider::Grok,
        Provider::DeepSeek,
        Provider::Antigravity,
        Provider::OpenCode,
        Provider::Cursor,
        Provider::Hermes,
    ];

    /// `n` provider total rows, each carrying the same nonzero stats.
    fn provider_rows(n: usize, stats: &ProviderStats) -> Vec<ProviderTotal<'_, ProviderStats>> {
        PROVIDER_NAMES
            .iter()
            .take(n)
            .map(|p| ProviderTotal::new(*p, stats, false))
            .collect()
    }

    /// Renders the Provider Usage panel table at a given size and returns its text.
    fn render_panel(width: u16, height: u16, providers: usize) -> String {
        let stats = ProviderStats {
            total_tokens: 1_000,
            total_cost: 1.0,
            days_count: 1,
        };
        let rows = provider_rows(providers, &stats);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| render_provider_table(frame, frame.area(), &rows))
            .expect("panel renders");
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
    fn provider_panel_says_how_many_providers_it_could_not_list() {
        // Overflowing rows are counted in both the title and a trailing "+N more" row.
        let short = render_panel(34, 9, PROVIDER_NAMES.len());
        assert!(short.contains("+7"), "got:\n{short}");
        assert!(short.contains("more"), "got:\n{short}");

        // With room for all of them, neither marker appears.
        let tall = render_panel(34, 16, PROVIDER_NAMES.len());
        assert!(!tall.contains("more"), "got:\n{tall}");
        assert!(tall.contains("Hermes"), "got:\n{tall}");

        // No data at all still says so rather than drawing an empty box.
        let empty = render_panel(34, 10, 0);
        assert!(empty.contains("No provider data yet"), "got:\n{empty}");
    }

    #[test]
    fn provider_digest_drops_whole_segments_and_counts_the_rest() {
        let stats = ProviderStats {
            total_tokens: 1_000,
            total_cost: 1.0,
            days_count: 1,
        };
        let rows = provider_rows(PROVIDER_NAMES.len(), &stats);

        // Wide terminal: every provider fits with no "+N more" suffix.
        let wide = line_text(provider_digest(&rows, 170));
        for provider in PROVIDER_NAMES {
            assert!(
                wide.contains(provider.display_name()),
                "width=170 dropped {}: {wide}",
                provider.display_name()
            );
        }
        assert!(!wide.contains("more"), "got: {wide}");
        assert!(wide.chars().count() <= 170, "got: {wide}");

        // 120 columns cannot fit all 10 providers, so the tail reports the omitted count.
        let squeezed = line_text(provider_digest(&rows, 120));
        assert!(squeezed.contains("more → p"), "got: {squeezed}");
        assert!(squeezed.chars().count() <= 120, "got: {squeezed}");

        // Narrow terminal: drop whole segments from the tail on equal costs and count them.
        let narrow = line_text(provider_digest(&rows, 46));
        assert!(narrow.contains("more → p"), "got: {narrow}");
        assert!(!narrow.contains("Hermes"), "got: {narrow}");
        assert!(narrow.chars().count() <= 46, "got: {narrow}");

        // The aggregate row is the table title's job, not the digest's.
        let overall = ProviderTotal::new_overall(&stats);
        let mut with_overall = provider_rows(2, &stats);
        with_overall.push(overall);
        let line = line_text(provider_digest(&with_overall, 120));
        assert!(!line.contains("All Providers"), "got: {line}");

        // An empty scan says so rather than drawing a blank row.
        assert_eq!(line_text(provider_digest(&[], 120)), "no provider data yet");
    }

    #[test]
    fn provider_digest_keeps_the_costliest_provider_however_narrow_it_gets() {
        // Highest spender sits last in enum order to ensure dropping sorts by cost, not position.
        let idle = ProviderStats {
            total_tokens: 10,
            total_cost: 0.0,
            days_count: 1,
        };
        let big = ProviderStats {
            total_tokens: 90_000_000,
            total_cost: 412.75,
            days_count: 1,
        };
        let mut rows = provider_rows(7, &idle);
        rows.push(ProviderTotal::new(Provider::Hermes, &big, false));

        // Down to a single segment, the surviving one is the expensive one.
        for width in [120u16, 80, 60, 40, 20] {
            let line = line_text(provider_digest(&rows, width));
            assert!(
                line.contains("Hermes $412.75"),
                "width={width} dropped the costliest provider: {line}"
            );
        }

        // The rest are still accounted for rather than silently missing.
        let narrow = line_text(provider_digest(&rows, 20));
        assert!(narrow.contains("+7 more → p"), "got: {narrow}");
    }

    #[test]
    fn model_table_columns_stay_readable_at_every_supported_width() {
        // The production column set, at the widths the frame actually produces.
        const NUMERIC: [ColumnSpec; 6] = [
            ColumnSpec {
                header: "Input",
                width: 9,
                drop_rank: 3,
            },
            ColumnSpec {
                header: "Output",
                width: 9,
                drop_rank: 2,
            },
            ColumnSpec {
                header: "Cache Read",
                width: 11,
                drop_rank: 4,
            },
            ColumnSpec {
                header: "Cache Write",
                width: 11,
                drop_rank: 5,
            },
            ColumnSpec {
                header: "Total",
                width: 9,
                drop_rank: 1,
            },
            ColumnSpec {
                header: "Cost (USD)",
                width: 12,
                drop_rank: 0,
            },
        ];
        for width in [USAGE_MIN_W, 80, 96, 100, 110, 120, 140, 160, 180, 200, 240] {
            let content = frame_layout(Rect::new(0, 0, width, 30), 0).content.width;
            let inner = content.saturating_sub(2);
            let (kept, dropped) = fit_columns(&NUMERIC, inner, MODEL_COL_MIN_W);
            let span: u16 = kept.iter().map(|&i| NUMERIC[i].width + 1).sum();
            let model_w = inner.saturating_sub(span).max(MODEL_COL_MIN_W);

            // Cost is never dropped: it is why the view exists.
            assert!(kept.contains(&5), "width={width} dropped Cost");
            // A date-suffixed model id stays distinguishable at every width.
            assert!(
                model_w >= MODEL_COL_MIN_W,
                "width={width} gave the name {model_w} columns"
            );
            // Whatever is kept actually fits beside the name.
            assert!(
                span + MODEL_COL_MIN_W <= inner || kept.is_empty(),
                "width={width} kept {} columns that do not fit",
                kept.len()
            );
            assert_eq!(dropped, NUMERIC.len() - kept.len());
        }
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
        // Terminal narrower than one card still yields one column without dividing by zero.
        assert_eq!(quota_grid(&CARD_GRID, 10, 3), (1, 3));
        assert_eq!(quota_grid(&CARD_GRID, 120, 0), (0, 0));
    }

    #[test]
    fn grid_absorbs_a_new_provider_without_a_new_case() {
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
        // Render dispatch indexes these cells in present-provider order, so length must equal n.
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
        // Each cell stays within [min_w, max_w] whenever the terminal is at least min_w wide.
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
    fn login_hint_survives_a_full_card() {
        // Login hint stays visible above gauges even when other lines overflow behind "+N".
        let claude = ClaudeQuotaSnapshot {
            plan_type: Some("max 20x".into()),
            five_hour: Some(QuotaWindow::default()),
            seven_day: Some(QuotaWindow::default()),
            scoped_weekly: Some(QuotaWindow::default()),
            scoped_label: Some("Opus".into()),
            balance: Some("$5.00".into()),
            spend_used: Some("$1.20".into()),
            needs_login: true,
            ..Default::default()
        };
        let rendered = render_min_card(&claude_card(&claude, 0, CARD_GRID.min_w, false));
        assert!(rendered.contains(CLAUDE_LOGIN_HINT), "got:\n{rendered}");
        assert!(rendered.contains("+"), "and it says something was hidden");
    }

    #[test]
    fn claude_card_shortens_a_long_scoped_label() {
        // Long scoped model names truncate with an ellipsis instead of pushing gauge bars right.
        let claude = ClaudeQuotaSnapshot {
            five_hour: Some(QuotaWindow::default()),
            scoped_weekly: Some(QuotaWindow::default()),
            scoped_label: Some("Claude Opus 4.5".into()),
            ..Default::default()
        };
        let rendered = render_min_card(&claude_card(&claude, 0, CARD_GRID.min_w, false));
        assert!(rendered.contains("Claud…"), "got:\n{rendered}");
        assert!(!rendered.contains("Opus"), "the tail is gone:\n{rendered}");
    }

    #[test]
    fn plan_line_shortens_the_plan_and_never_the_age() {
        // Unbounded plan names truncate before the staleness age.
        let inner = card_inner_w(CARD_GRID.min_w);
        let text = line_text(plan_line(
            Some("copilot_pro_plus_enterprise"),
            1,
            100_000,
            inner,
        ));
        assert_eq!(text.chars().count(), usize::from(inner), "got {text:?}");
        assert!(text.ends_with("ago"), "the age survives whole: {text:?}");
        assert!(text.contains('…'), "and the cut is marked: {text:?}");

        // A provider with no plan label still shows its age.
        let text = line_text(plan_line(None, 1, 100_000, inner));
        assert!(text.ends_with("ago"), "got {text:?}");
        assert!(text.starts_with('-'), "got {text:?}");

        // Never fetched reads as such rather than as a fresh snapshot.
        assert!(line_text(plan_line(Some("pro"), 0, 100_000, inner)).contains("never"));
    }

    #[test]
    fn card_counts_the_lines_it_could_not_show() {
        // Six lines in five content rows flags "+1" in the title and hides the last line.
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
        let text = line_text(gauge_line(
            "total",
            5,
            41.0,
            Some(30 * 86_400),
            0,
            inner,
            false,
        ));
        assert!(text.chars().count() <= usize::from(inner), "got {text:?}");
        assert!(
            text.contains("30d0h"),
            "the duration itself is never cut: {text:?}"
        );
        // With room the marker comes back.
        let wide = line_text(gauge_line(
            "total",
            5,
            41.0,
            Some(30 * 86_400),
            0,
            40,
            false,
        ));
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

    /// Returns a [`QuotaView`] with all providers marked present.
    fn every_provider<'a>(
        claude: &'a ClaudeQuotaSnapshot,
        codex: &'a CodexQuotaSnapshot,
        copilot: &'a CopilotQuotaSnapshot,
        cursor: &'a CursorQuotaSnapshot,
        grok: &'a GrokQuotaSnapshot,
        antigravity: &'a AntigravityQuotaSnapshot,
    ) -> QuotaView<'a> {
        QuotaView {
            claude,
            codex,
            copilot,
            cursor,
            grok,
            antigravity,
            present: ALL_PRESENT,
            band_enabled: true,
        }
    }

    fn render_quota_tab(width: u16, height: u16) -> String {
        let (claude, codex, copilot, cursor, grok, antigravity) = Default::default();
        let quota = every_provider(&claude, &codex, &copilot, &cursor, &grok, &antigravity);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw_quota_tab(frame, frame.area(), &quota, 0, None))
            .expect("quota tab renders");
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
    fn quota_tab_shows_every_provider_when_it_has_the_room() {
        let rendered = render_quota_tab(140, 30);
        for name in [
            "Claude",
            "Codex",
            "Copilot",
            "Cursor",
            "Grok",
            "Antigravity",
        ] {
            assert!(rendered.contains(name), "{name} missing from:\n{rendered}");
        }
        assert!(
            !rendered.contains("hidden"),
            "nothing was hidden:\n{rendered}"
        );
    }

    /// Quota tab cards grow to fit tall multi-pool snapshots when rows permit, and otherwise flag hidden lines.
    #[test]
    fn quota_tab_grows_its_cards_to_the_tallest_while_every_provider_fits() {
        let pools = [
            "Gemini Pro Next",
            "Gemini Flash Next",
            "Gemini Teamfood Models",
            "Gemini Flash",
            "Gemini Flash Lite",
            "Third-Party Models",
        ];
        let antigravity = AntigravityQuotaSnapshot {
            buckets: pools
                .iter()
                .map(|pool| AntigravityBucket {
                    group: pool.to_string(),
                    window: "20h".to_string(),
                    usage: QuotaWindow {
                        used_percent: 1.0,
                        resets_at_unix: None,
                    },
                })
                .collect(),
            ..Default::default()
        };
        let (c, x, p, u, g) = Default::default();
        let quota = QuotaView {
            present: QuotaPresence {
                antigravity: true,
                ..Default::default()
            },
            ..every_provider(&c, &x, &p, &u, &g, &antigravity)
        };
        let render = |width: u16, height: u16| {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| draw_quota_tab(frame, frame.area(), &quota, 0, None))
                .unwrap();
            let buffer = terminal.backend().buffer();
            (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| buffer.cell((x, y)).unwrap().symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let tall = render(60, 20);
        assert!(tall.contains("Third-Party Models"), "got:\n{tall}");
        assert!(!tall.contains('+'), "nothing may be hidden:\n{tall}");

        let short = render(60, 14);
        assert!(short.contains("Antigravity"), "got:\n{short}");
        assert!(short.contains("+8"), "the rest must be flagged:\n{short}");
    }

    #[test]
    fn quota_tab_explains_itself_when_there_is_nothing_to_show() {
        let (c, x, p, u, g, a) = Default::default();
        let mut quota = QuotaView {
            claude: &c,
            codex: &x,
            copilot: &p,
            cursor: &u,
            grok: &g,
            antigravity: &a,
            present: QuotaPresence::default(),
            band_enabled: true,
        };
        let render = |quota: &QuotaView| {
            let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
            terminal
                .draw(|frame| draw_quota_tab(frame, frame.area(), quota, 0, None))
                .unwrap();
            let buffer = terminal.backend().buffer();
            (0..20u16)
                .map(|y| {
                    (0..80u16)
                        .map(|x| buffer.cell((x, y)).unwrap().symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("")
        };
        // No credentials anywhere: an empty box would be a dead end.
        assert!(render(&quota).contains("quota credentials"));
        // Panels switched off is a setting, and the tab says where to change it.
        quota.band_enabled = false;
        assert!(render(&quota).contains("Settings > Usage"));
    }

    #[test]
    fn quota_tab_says_how_many_providers_it_could_not_reach() {
        // Short terminals count cards that did not fit in the footer.
        let rendered = render_quota_tab(80, 13);
        assert!(rendered.contains("hidden"), "got:\n{rendered}");
        assert!(rendered.contains("Claude"), "got:\n{rendered}");
    }

    #[test]
    fn share_cell_bar_is_proportional() {
        let width = 26u16;
        let bar = |value: i64| {
            share_cell(value, 100, width)
                .chars()
                .filter(|c| *c == '━')
                .count()
        };
        // Twice the share, twice the bar (the column reserves 6 for the number).
        assert_eq!(bar(50), usize::from(width - 6) / 2);
        assert_eq!(bar(100), usize::from(width - 6));
        assert!(bar(25) < bar(50) && bar(50) < bar(75));
    }

    #[test]
    fn share_cell_fills_its_column_without_overflowing() {
        for width in [SHARE_COL_MIN_W, 18, SHARE_COL_MAX_W] {
            for (value, total) in [(0i64, 100i64), (50, 100), (100, 100), (7, 1000)] {
                let cell = share_cell(value, total, width);
                assert!(
                    cell.chars().count() <= usize::from(width),
                    "{value}/{total} at {width}: {cell:?}"
                );
            }
        }
        // A full share fills the bar; an empty one draws none of it.
        assert!(share_cell(100, 100, 20).starts_with('━'));
        assert!(!share_cell(0, 100, 20).starts_with('━'));
        // Zero total or width below the minimum renders empty.
        assert_eq!(share_cell(5, 0, 20), "");
        assert_eq!(share_cell(5, 100, 4), "");
    }

    /// Renders a full Usage tab frame at `w` x `h` and returns its screen rows.
    fn render_frame(w: u16, h: u16, provider_panel: bool) -> Vec<String> {
        let mut bench = UsageFrameBenchmark::new(w, h).expect("frame fixture");
        render_usage_frame(
            &mut bench.terminal,
            &bench.rows,
            &bench.totals,
            &bench.provider_totals,
            &bench.update_tracker,
            &mut bench.scroll,
            None,
            provider_panel,
        )
        .expect("frame renders");
        let buffer = bench.terminal.backend().buffer();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            })
            .collect()
    }

    const ALL_PRESENT: QuotaPresence = QuotaPresence {
        claude: true,
        codex: true,
        copilot: true,
        cursor: true,
        grok: true,
        antigravity: true,
    };

    /// Terminal sizes exercised by full-frame layout tests (minimum, common, and wide).
    const FRAME_SIZES: [(u16, u16); 8] = [
        (USAGE_MIN_W, USAGE_MIN_H),
        (80, 24),
        (95, 30),
        (96, 30),
        (100, 30),
        (120, 40),
        (160, 45),
        (220, 50),
    ];

    /// Asserts no cell in `row` ends on a clipped `·` separator or truncated `+` overflow marker.
    fn assert_no_clipped_markers(row: &str, context: &str) {
        for cell in row.split('│') {
            let cell = cell.trim_end();
            assert!(
                !cell.ends_with('·'),
                "{context} was clipped mid-list: {row:?}"
            );
            assert!(
                !cell.ends_with('+'),
                "{context} clipped a `+N` marker: {row:?}"
            );
        }
    }

    #[test]
    fn frame_never_draws_outside_its_own_width() {
        // Also exercise the centered provider panel overlay at every terminal size.
        for (w, h) in FRAME_SIZES {
            let plain = render_frame(w, h, false);
            for panel in [false, true] {
                let context = format!("{w}x{h} panel={panel}");
                for (y, row) in render_frame(w, h, panel).iter().enumerate() {
                    assert_eq!(row.chars().count(), usize::from(w), "{context}");
                    // When the modal covers the middle of a row, check only the columns it overwrote; the underlying row is checked with `panel = false`.
                    let checked: String = if panel {
                        let changed: Vec<bool> = row
                            .chars()
                            .zip(plain[y].chars())
                            .map(|(a, b)| a != b)
                            .collect();
                        match (
                            changed.iter().position(|c| *c),
                            changed.iter().rposition(|c| *c),
                        ) {
                            (Some(first), Some(last)) => {
                                row.chars().skip(first).take(last + 1 - first).collect()
                            }
                            _ => String::new(),
                        }
                    } else {
                        row.clone()
                    };
                    assert_no_clipped_markers(&checked, &format!("{context} row {y}"));
                }
            }
        }
    }

    #[test]
    fn every_size_shows_the_model_table_without_quota_cards() {
        for (w, h) in FRAME_SIZES {
            let screen = render_frame(w, h, false).join("\n");
            assert!(screen.contains("Models"), "{w}x{h} lost the model table");
            assert!(
                !screen.contains("no Codex quota") && !screen.contains("rate-limit data"),
                "{w}x{h} still drew quota cards on the Usage tab"
            );
            // Quitting is always discoverable.
            assert!(screen.contains("q quit"), "{w}x{h} lost the quit hint");
        }
    }

    #[test]
    fn every_size_shows_the_provider_line_and_can_reach_the_panel() {
        for (w, h) in FRAME_SIZES {
            // The digest drops cheapest providers first, so the fixture's highest spender survives at every size.
            let screen = render_frame(w, h, false).join("\n");
            assert!(
                screen.contains("Hermes $"),
                "{w}x{h} dropped the highest-cost provider"
            );
            assert!(screen.contains("p providers"), "{w}x{h} lost the `p` hint");

            // ...and `p` reaches the numbers the line could not carry.
            let panel = render_frame(w, h, true).join("\n");
            assert!(panel.contains("Tokens"), "{w}x{h} panel lost its columns");
            assert!(
                panel.contains("esc close"),
                "{w}x{h} panel lost its close hint"
            );
        }
    }

    #[test]
    fn quota_tab_cards_are_built_for_the_cell_they_are_drawn_in() {
        // Codex's extras sit on the truncation boundary, catching cards built for a wider width than their drawn cell.
        for (w, h) in [(100u16, 30u16), (120, 40), (160, 45), (220, 50)] {
            let codex = CodexQuotaSnapshot {
                source: QuotaSource::Api,
                fetched_at: 1,
                plan_type: Some("plus".into()),
                credits_balance: Some("0".into()),
                reset_credits_available: Some(2),
                reset_credit_expirations: Some(vec![Some(10_000)]),
                approx_messages: Some((120, 150)),
                spend_limit: Some(50.0),
                ..Default::default()
            };
            let (claude, copilot, cursor, grok, antigravity) = Default::default();
            let quota = every_provider(&claude, &codex, &copilot, &cursor, &grok, &antigravity);
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|frame| draw_quota_tab(frame, frame.area(), &quota, 0, None))
                .expect("quota tab renders");
            let buffer = terminal.backend().buffer();
            for y in 0..h {
                let row: String = (0..w)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect();
                assert_no_clipped_markers(&row, &format!("{w}x{h} row {y}"));
            }
        }
    }

    #[test]
    fn model_table_shows_raw_model_and_moves_pricing_match_into_details() {
        use crate::display::usage::averages::ModelProviderUsage;
        use vct_core::models::Provider;

        let rows = vec![UsageRow {
            model: "custom-sonnet-preview".to_string(),
            matched_model: Some("claude-sonnet-4-6".to_string()),
            input_tokens: 1_000,
            cache_read: 9_000,
            total: 10_000,
            cost: 0.05,
            providers: vec![
                ModelProviderUsage {
                    provider: Provider::ClaudeCode,
                    total_tokens: 8_000,
                    cost: 0.04,
                },
                ModelProviderUsage {
                    provider: Provider::Antigravity,
                    total_tokens: 2_000,
                    cost: 0.01,
                },
            ],
            variants: vec![
                "custom-sonnet-preview".to_string(),
                "anthropic/custom-sonnet-preview".to_string(),
            ],
            ..UsageRow::default()
        }];
        let mut totals = UsageTotals::default();
        totals.accumulate(&rows[0]);
        let provider_totals = UsageProviderTotals::default();
        let tracker = UpdateTracker::new(MAX_TRACKED_ROWS, 0);
        let mut scroll = ScrollState::new();
        scroll.sync(None, &[rows[0].model.clone()]);

        let draw = |detail_open: bool, scroll: &mut ScrollState| {
            let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
            terminal
                .draw(|f| {
                    draw_usage_view(
                        f,
                        f.area(),
                        &rows,
                        &totals,
                        &provider_totals,
                        &tracker,
                        scroll,
                        UsageSort::default(),
                        None,
                        false,
                        detail_open,
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            (0..32u16)
                .map(|y| {
                    (0..120u16)
                        .map(|x| buffer.cell((x, y)).unwrap().symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let table = draw(false, &mut scroll);
        assert!(table.contains("custom-sonnet-preview"), "got:\n{table}");
        assert!(
            !table.contains("claude-sonnet-4-6"),
            "table row must not append matched pricing model:\n{table}"
        );

        let details = draw(true, &mut scroll);
        assert!(details.contains("Providers"), "got:\n{details}");
        assert!(details.contains("Claude"), "got:\n{details}");
        assert!(details.contains("Antigravity"), "got:\n{details}");
        assert!(details.contains("Priced as"), "got:\n{details}");
        assert!(details.contains("claude-sonnet-4-6"), "got:\n{details}");
        assert!(details.contains("Merged from"), "got:\n{details}");
        assert!(details.contains("Cache hit rate"), "got:\n{details}");
    }

    #[test]
    fn usage_scanner_short_circuits_unchanged_refreshes_and_invalidates_on_changes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vct_core::utils::resolve_paths_from_home(dir.path());
        let project = paths.claude_session_dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let session = project.join("session.jsonl");
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/sessions/claude_code.jsonl");
        std::fs::copy(fixture, &session).unwrap();

        let pool = vct_core::scan::build_scan_pool(1).unwrap();
        let mut ledger = SessionLedger::new();
        let mut scanner = UsageScanner::new();
        // Pre-stamp today's pricing date so the test stays hermetic without touching network or ~/.vct.
        scanner.loaded_pricing_utc_date = Some(chrono::Utc::now().date_naive());
        let providers = ProvidersConfig::default();

        let first = scanner
            .scan(
                &paths,
                vct_core::models::TimeRange::All,
                providers,
                &mut ledger,
                &pool,
            )
            .unwrap();
        assert!(matches!(first, UsageScanOutcome::Updated(_)));

        let second = scanner
            .scan(
                &paths,
                vct_core::models::TimeRange::All,
                providers,
                &mut ledger,
                &pool,
            )
            .unwrap();
        assert!(
            matches!(second, UsageScanOutcome::Unchanged),
            "identical refresh with zero changed sources must short-circuit"
        );

        let range_changed = scanner
            .scan(
                &paths,
                vct_core::models::TimeRange::Daily,
                providers,
                &mut ledger,
                &pool,
            )
            .unwrap();
        assert!(matches!(range_changed, UsageScanOutcome::Updated(_)));

        // Adding a remote snapshot file invalidates the short-circuit even with zero local parses.
        let remote_sessions = paths
            .cache_dir
            .join(vct_core::ledger::REMOTES_DIR_NAME)
            .join("host-a")
            .join(vct_core::ledger::LEDGER_DIR_NAME);
        std::fs::create_dir_all(&remote_sessions).unwrap();
        std::fs::write(remote_sessions.join("claude.json"), "{}").unwrap();
        let remote_changed = scanner
            .scan(
                &paths,
                vct_core::models::TimeRange::Daily,
                providers,
                &mut ledger,
                &pool,
            )
            .unwrap();
        assert!(matches!(remote_changed, UsageScanOutcome::Updated(_)));
    }
}
