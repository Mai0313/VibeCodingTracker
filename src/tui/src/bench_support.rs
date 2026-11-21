//! Internal benchmark helpers exposing crate-private TUI layout plugins and tab state.
//!
//! Used exclusively by `benches/benchmarks.rs`; not part of the public API.

use crate::display::analysis::AnalysisUiState;
use crate::display::app::Tab;
use crate::display::common::theme::theme;
use crate::display::common::tui::NavDelta;
use crate::display::settings::SettingsUiState;
use crate::display::usage::{
    ModelProviderUsage, UsageProviderTotals, UsageRefreshPayload, UsageRow, UsageTotals,
    UsageUiState, merge_rows_by_base_model,
};
use crate::layouts::{self, LAYOUT_IDS, LayoutContext, LayoutView};
use crossterm::event::KeyCode;
use ratatui::{Terminal, backend::TestBackend};
use vct_core::analysis::{AnalysisData, PerProviderAnalysisRows};
use vct_core::config::Config;
use vct_core::models::{AggregatedAnalysisRow, Provider, ProviderActiveDays, TimeRange};

/// Returns the compile-time registered layout plugin identifiers (`classic` first).
pub fn layout_ids() -> &'static [&'static str] {
    LAYOUT_IDS
}

/// Constructs a fresh `SettingsUiState` over default configuration with a no-op saver.
///
/// Passes the constructed state through [`std::hint::black_box`] so LTO cannot elide unread fields.
pub fn init_settings_ui() -> usize {
    let state = SettingsUiState::new(Config::default(), Box::new(|_, _| Ok(())));
    std::hint::black_box(&state);
    state.config().general.layout.len()
}

/// Production-shaped layout rendering fixture backed by an in-memory [`TestBackend`].
///
/// Exercises the compiled-in `LayoutView` plugins across [`Tab::Usage`], [`Tab::Analysis`], and [`Tab::Settings`] without reading `~/.vct` or writing control sequences to stdout.
pub struct LayoutFrameBenchmark {
    terminal: Terminal<TestBackend>,
    layout: Box<dyn LayoutView>,
    usage: UsageUiState,
    analysis: AnalysisUiState,
    settings: SettingsUiState,
    row_count: usize,
    step: usize,
}

impl LayoutFrameBenchmark {
    /// Creates a populated layout fixture for `layout_id` at `(width, height)` with `row_count` synthetic model rows.
    ///
    /// # Errors
    ///
    /// Returns an error if the backing [`Terminal`] cannot be initialized.
    pub fn new(layout_id: &str, width: u16, height: u16, row_count: usize) -> anyhow::Result<Self> {
        let (_, layout) = layouts::create(layout_id);
        let mut usage = UsageUiState::new(false);
        usage.apply(build_synthetic_usage_payload(row_count));

        let mut analysis = AnalysisUiState::new();
        analysis.apply(build_synthetic_analysis_data(row_count));

        let settings = SettingsUiState::new(Config::default(), Box::new(|_, _| Ok(())));

        Ok(Self {
            terminal: Terminal::new(TestBackend::new(width, height))?,
            layout,
            usage,
            analysis,
            settings,
            row_count: row_count.max(1),
            step: 0,
        })
    }

    /// Renders one frame for `tab`, optionally advancing row selection so each frame produces a non-trivial terminal diff.
    ///
    /// # Errors
    ///
    /// Returns an error if ratatui fails to draw into the [`TestBackend`].
    pub fn render_tab(
        &mut self,
        tab: Tab,
        step_selection: bool,
        status: Option<&str>,
    ) -> anyhow::Result<()> {
        if step_selection {
            match tab {
                Tab::Usage => {
                    let next = (self.step + 1) % self.row_count;
                    self.usage.select_index(next);
                }
                Tab::Analysis => {
                    let delta = if self.step.is_multiple_of(2) { 1 } else { -1 };
                    self.analysis.navigate(NavDelta { lines: delta });
                }
                Tab::Settings => {
                    let key = if self.step.is_multiple_of(2) {
                        KeyCode::Down
                    } else {
                        KeyCode::Up
                    };
                    let _ = self.settings.handle_key(key);
                }
                Tab::Quota => {}
            }
        }
        self.step = self.step.wrapping_add(1);

        let data_status = status.map(|text| (text, theme().warn));
        let spinner = self.step;
        let layout = &mut self.layout;
        let usage = &mut self.usage;
        let analysis = &mut self.analysis;
        let settings = &mut self.settings;

        self.terminal.draw(|frame| {
            let mut ctx = LayoutContext {
                tab,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner,
                metrics: (48.2, 1.5),
                data_status,
                update_status: None,
                usage,
                analysis,
                quota: None,
                settings,
            };
            layout.draw(frame, &mut ctx);
        })?;
        Ok(())
    }
}

/// Builds a synthetic [`UsageRefreshPayload`] with `row_count` multi-provider model rows.
fn build_synthetic_usage_payload(row_count: usize) -> UsageRefreshPayload {
    const PROVIDERS: [Provider; 10] = [
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
    const PREFIXES: [&str; 10] = [
        "claude-sonnet-4-6",
        "gpt-5.4-codex",
        "copilot/gpt-5.4",
        "gemini-2.5-pro",
        "grok-code-fast-1",
        "deepseek-reasoner",
        "antigravity/gemini-2.5-pro",
        "opencode/claude-sonnet-4-6",
        "cursor/gpt-5.4",
        "hermes/qwen3-coder",
    ];

    let mut rows = Vec::with_capacity(row_count);
    let mut totals = UsageTotals::default();
    let mut provider_totals = UsageProviderTotals::default();

    for index in 0..row_count {
        let provider_idx = index % PROVIDERS.len();
        let provider = PROVIDERS[provider_idx];
        let scale = (index as i64) + 1;
        let input_tokens = 14_000 * scale;
        let output_tokens = 2_800 * scale;
        let reasoning_tokens = if index % 3 == 0 { 900 * scale } else { 0 };
        let cache_read = 52_000 * scale;
        let cache_creation = 1_600 * scale;
        let cache_creation_1h = if index % 4 == 0 { 400 * scale } else { 0 };
        let total = input_tokens + output_tokens + reasoning_tokens + cache_read + cache_creation;
        let cost = (scale as f64) * 0.0215;

        let model = format!("{}-{index:03}", PREFIXES[provider_idx]);
        let row = UsageRow {
            model,
            matched_model: (index % 5 == 0).then(|| "claude-sonnet-4-6".to_string()),
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read,
            cache_creation,
            cache_creation_5m: cache_creation - cache_creation_1h,
            cache_creation_1h,
            total,
            cost,
            providers: vec![ModelProviderUsage {
                provider,
                total_tokens: total,
                cost,
            }],
            ..UsageRow::default()
        };

        totals.accumulate(&row);
        let stats = match provider {
            Provider::ClaudeCode => &mut provider_totals.claude,
            Provider::Codex => &mut provider_totals.codex,
            Provider::Copilot => &mut provider_totals.copilot,
            Provider::Gemini => &mut provider_totals.gemini,
            Provider::Grok => &mut provider_totals.grok,
            Provider::DeepSeek => &mut provider_totals.deepseek,
            Provider::Antigravity => &mut provider_totals.antigravity,
            Provider::OpenCode => &mut provider_totals.opencode,
            Provider::Cursor => &mut provider_totals.cursor,
            Provider::Hermes => &mut provider_totals.hermes,
            Provider::Unknown => &mut provider_totals.overall,
        };
        stats.total_tokens += row.total;
        stats.total_cost += row.cost;
        stats.days_count = 14;
        provider_totals.overall.total_tokens += row.total;
        provider_totals.overall.total_cost += row.cost;
        rows.push(row);
    }
    provider_totals.overall.days_count = 14;

    let merged_rows = merge_rows_by_base_model(&rows);
    UsageRefreshPayload {
        rows,
        merged_rows,
        totals,
        provider_totals,
    }
}

/// Builds synthetic [`AnalysisData`] with `row_count` multi-provider model rows.
fn build_synthetic_analysis_data(row_count: usize) -> AnalysisData {
    const MODELS: [&str; 9] = [
        "claude-sonnet-4-6",
        "gpt-5.4-codex",
        "copilot/gpt-5.4",
        "gemini-2.5-pro",
        "grok-code-fast-1",
        "deepseek-reasoner",
        "antigravity/gemini-2.5-pro",
        "opencode/claude-sonnet-4-6",
        "cursor/gpt-5.4",
    ];

    let mut rows = Vec::with_capacity(row_count);
    let mut per_provider = PerProviderAnalysisRows::default();

    for index in 0..row_count {
        let slot = index % MODELS.len();
        let scale = index + 1;
        let row = AggregatedAnalysisRow {
            model: format!("{}-{index:03}", MODELS[slot]),
            edit_lines: 42 * scale,
            read_lines: 180 * scale,
            write_lines: 65 * scale,
            bash_count: 3 * scale,
            edit_count: 5 * scale,
            read_count: 9 * scale,
            todo_write_count: scale,
            write_count: 2 * scale,
        };
        match slot {
            0 => per_provider.claude.push(row.clone()),
            1 => per_provider.codex.push(row.clone()),
            2 => per_provider.copilot.push(row.clone()),
            3 => per_provider.gemini.push(row.clone()),
            4 => per_provider.grok.push(row.clone()),
            5 => per_provider.deepseek.push(row.clone()),
            6 => per_provider.antigravity.push(row.clone()),
            7 => per_provider.opencode.push(row.clone()),
            _ => per_provider.cursor.push(row.clone()),
        }
        rows.push(row);
    }

    let provider_days = ProviderActiveDays {
        claude: 14,
        codex: 14,
        copilot: 10,
        gemini: 12,
        grok: 8,
        deepseek: 9,
        antigravity: 7,
        opencode: 11,
        cursor: 6,
        hermes: 0,
        total: 14,
    };

    AnalysisData {
        rows,
        per_provider,
        provider_days,
    }
}
