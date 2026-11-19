//! `legacy` layout: the full-width table interface with modal `Details` and
//! `Providers` popups.

use crate::display::analysis::{
    analysis_panels_height, build_analysis_provider_rows, layout_analysis_columns,
};
use crate::display::app::Tab;
use crate::display::common::table::{frame_layout, header_column_at, table_body_row_at};
use crate::display::common::tui::{MouseInput, NavDelta, render_loading};
use crate::display::settings::SettingsOutcome;
use crate::display::usage::{draw_quota_tab, layout_usage_columns};
use crate::layouts::{
    EventResult, LayoutCommand, LayoutContext, LayoutMeta, LayoutView, TopBarHits, TuiLayout,
    contains, render_top_bar,
};
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
};

/// Static plugin entry discovered by `src/tui/build.rs`.
pub(crate) static LAYOUT: LegacyPlugin = LegacyPlugin;

pub(crate) struct LegacyPlugin;

impl TuiLayout for LegacyPlugin {
    fn meta(&self) -> LayoutMeta {
        LayoutMeta {
            id: "legacy",
            title: "Legacy",
            summary: "Full-width tables with centered modal detail and provider popups",
        }
    }

    fn create(&self) -> Box<dyn LayoutView> {
        Box::new(LegacyView::default())
    }
}

#[derive(Default)]
struct LegacyView {
    top_bar: TopBarHits,
    table_area: Option<Rect>,
    digest_area: Option<Rect>,
}

impl LayoutView for LegacyView {
    fn draw(&mut self, f: &mut Frame, ctx: &mut LayoutContext<'_>) {
        self.table_area = None;
        self.digest_area = None;

        let [bar, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(f.area());
        let range = ctx.tab.scans().then_some(ctx.time_range);
        self.top_bar = render_top_bar(f, bar, ctx.tab, range, ctx.metrics);

        match ctx.tab {
            Tab::Usage | Tab::Analysis if ctx.loading => {
                let message = if ctx.retry_pending {
                    "Load failed, press r to retry"
                } else {
                    "Loading sessions..."
                };
                render_loading(f, body, ctx.spinner, message);
            }
            Tab::Usage => {
                if body.width >= 74 && body.height >= 14 {
                    let chunks = frame_layout(body, 1);
                    self.table_area = Some(chunks.content);
                    self.digest_area = chunks.band;
                }
                ctx.usage.draw(f, body, ctx.data_status);
            }
            Tab::Analysis => {
                if body.width >= 84 && body.height >= 14 {
                    let provider_rows =
                        build_analysis_provider_rows(ctx.analysis.provider_totals());
                    let band_h =
                        analysis_panels_height(body.height, provider_rows.len()).unwrap_or(1);
                    let chunks = frame_layout(body, band_h);
                    self.table_area = Some(chunks.content);
                }
                ctx.analysis.draw(f, body, ctx.data_status);
            }
            Tab::Quota => {
                if let Some((ref quota, now)) = ctx.quota {
                    draw_quota_tab(f, body, quota, now, ctx.update_status);
                }
            }
            Tab::Settings => ctx.settings.draw(f, body, ctx.update_status),
        }
    }

    fn on_mouse(&mut self, mouse: MouseInput, ctx: &mut LayoutContext<'_>) -> EventResult {
        if ctx.tab == Tab::Settings {
            if ctx.settings.has_open_editor() {
                let changed = ctx.settings.handle_mouse(mouse) == SettingsOutcome::Changed;
                return EventResult::Consumed(LayoutCommand {
                    settings_changed: changed,
                    ..LayoutCommand::default()
                });
            }
            if let MouseInput::Click { column, row } = mouse
                && let Some(next) = self.top_bar.hit_tab(column, row)
            {
                return EventResult::Consumed(LayoutCommand {
                    switch_tab: Some(next),
                    ..LayoutCommand::default()
                });
            }
            let changed = ctx.settings.handle_mouse(mouse) == SettingsOutcome::Changed;
            return EventResult::Consumed(LayoutCommand {
                settings_changed: changed,
                ..LayoutCommand::default()
            });
        }

        // Dismiss open modals on primary click before dispatching underlying widgets.
        if let MouseInput::Click { column, row } = mouse {
            if ctx.usage.detail_open || ctx.analysis.detail_open {
                ctx.usage.detail_open = false;
                ctx.analysis.detail_open = false;
                return EventResult::consumed();
            }
            if ctx.usage.provider_panel_open {
                ctx.usage.provider_panel_open = false;
                return EventResult::consumed();
            }
            if let Some(next) = self.top_bar.hit_tab(column, row) {
                return EventResult::Consumed(LayoutCommand {
                    switch_tab: Some(next),
                    ..LayoutCommand::default()
                });
            }
            if ctx.tab.scans()
                && let Some(range) = self.top_bar.hit_range(column, row)
            {
                return EventResult::Consumed(LayoutCommand {
                    set_range: Some(range),
                    ..LayoutCommand::default()
                });
            }
            if ctx.loading {
                return EventResult::consumed();
            }

            if ctx.tab == Tab::Usage {
                if let Some(digest) = self.digest_area
                    && contains(digest, column, row)
                {
                    ctx.usage.provider_panel_open = true;
                    return EventResult::consumed();
                }
                if let Some(table) = self.table_area {
                    let cols = layout_usage_columns(table.width.saturating_sub(2));
                    if let Some(col_idx) = header_column_at(table, &cols.widths, column, row)
                        && let Some(sort) = cols.sort_for_header(col_idx)
                    {
                        ctx.usage.set_sort(sort);
                        return EventResult::consumed();
                    }
                    if let Some(row_idx) = table_body_row_at(
                        table,
                        ctx.usage.scroll(),
                        ctx.usage.rows().len(),
                        column,
                        row,
                    ) {
                        if ctx.usage.selected_index() == Some(row_idx) {
                            ctx.usage.detail_open = true;
                        } else {
                            ctx.usage.select_index(row_idx);
                        }
                        return EventResult::consumed();
                    }
                }
            } else if ctx.tab == Tab::Analysis
                && let Some(table) = self.table_area
            {
                let cols = layout_analysis_columns(table.width.saturating_sub(2));
                if let Some(col_idx) = header_column_at(table, &cols.widths, column, row)
                    && let Some(sort) = cols.sort_for_header(col_idx)
                {
                    ctx.analysis.set_sort(sort);
                    return EventResult::consumed();
                }
                if let Some(row_idx) = table_body_row_at(
                    table,
                    ctx.analysis.scroll(),
                    ctx.analysis.rows().len(),
                    column,
                    row,
                ) {
                    if ctx.analysis.selected_index() == Some(row_idx) {
                        ctx.analysis.detail_open = true;
                    } else {
                        ctx.analysis.select_index(row_idx);
                    }
                    return EventResult::consumed();
                }
            }
            return EventResult::consumed();
        }

        if let MouseInput::Scroll { lines, .. } = mouse
            && ctx.tab.scans()
            && !ctx.loading
        {
            let delta = NavDelta {
                lines: i64::from(lines),
            };
            if ctx.tab == Tab::Usage {
                ctx.usage.navigate(delta);
            } else {
                ctx.analysis.navigate(delta);
            }
            return EventResult::consumed();
        }

        EventResult::consumed()
    }

    fn on_key(&mut self, key: KeyCode, ctx: &mut LayoutContext<'_>) -> EventResult {
        match key {
            KeyCode::Esc => {
                if ctx.usage.detail_open || ctx.analysis.detail_open {
                    ctx.usage.detail_open = false;
                    ctx.analysis.detail_open = false;
                } else {
                    ctx.usage.provider_panel_open = false;
                }
                EventResult::consumed()
            }
            KeyCode::Enter if ctx.tab.scans() && !ctx.loading => {
                if ctx.tab == Tab::Usage {
                    ctx.usage.detail_open = !ctx.usage.detail_open;
                } else {
                    ctx.analysis.detail_open = !ctx.analysis.detail_open;
                }
                EventResult::consumed()
            }
            KeyCode::Char('p' | 'P') if ctx.tab == Tab::Usage && !ctx.loading => {
                ctx.usage.provider_panel_open = !ctx.usage.provider_panel_open;
                EventResult::consumed()
            }
            _ => EventResult::Ignored,
        }
    }

    fn on_tab_changed(&mut self, _next: Tab, ctx: &mut LayoutContext<'_>) {
        ctx.usage.detail_open = false;
        ctx.analysis.detail_open = false;
        ctx.usage.provider_panel_open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::analysis::AnalysisUiState;
    use crate::display::settings::SettingsUiState;
    use crate::display::usage::{
        ModelProviderUsage, UsageRefreshPayload, UsageRow, UsageSort, UsageTotals, UsageUiState,
    };
    use ratatui::{Terminal, backend::TestBackend};
    use vct_core::config::Config;
    use vct_core::models::{Provider, ProviderTotals, TimeRange};

    fn render_legacy(
        view: &mut Box<dyn LayoutView>,
        usage: &mut UsageUiState,
        analysis: &mut AnalysisUiState,
        settings: &mut SettingsUiState,
    ) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
        terminal
            .draw(|f| {
                let mut ctx = LayoutContext {
                    tab: Tab::Usage,
                    time_range: TimeRange::All,
                    loading: false,
                    retry_pending: false,
                    spinner: 0,
                    metrics: (44.0, 0.1),
                    data_status: None,
                    update_status: None,
                    usage,
                    analysis,
                    quota: None,
                    settings,
                };
                view.draw(f, &mut ctx);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..24)
            .map(|y| (0..110).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn modals_and_header_clicks_work_in_legacy_layout() {
        let mut usage = UsageUiState::new(true);
        let row_a = UsageRow {
            model: "claude-opus-4-7".to_string(),
            input_tokens: 50_000,
            total: 50_000,
            cost: 8.0,
            providers: vec![ModelProviderUsage {
                provider: Provider::ClaudeCode,
                total_tokens: 50_000,
                cost: 8.0,
            }],
            ..UsageRow::default()
        };
        let row_b = UsageRow {
            model: "gpt-5.5".to_string(),
            input_tokens: 120_000,
            total: 120_000,
            cost: 2.0,
            ..UsageRow::default()
        };
        let mut totals = UsageTotals::default();
        totals.accumulate(&row_a);
        totals.accumulate(&row_b);
        usage.apply(UsageRefreshPayload {
            rows: vec![row_a.clone(), row_b.clone()],
            merged_rows: vec![row_a, row_b],
            totals,
            provider_totals: ProviderTotals::default(),
        });
        let mut analysis = AnalysisUiState::new();
        let mut settings = SettingsUiState::new(Config::default(), Box::new(|_, _| Ok(())));
        let mut view = LAYOUT.create();

        let initial = render_legacy(&mut view, &mut usage, &mut analysis, &mut settings);
        let text = initial.join("\n");
        assert!(text.contains("Star on GitHub"), "{text}");

        // Enter opens Details modal, Esc closes it.
        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (44.0, 0.1),
                data_status: None,
                update_status: None,
                usage: &mut usage,
                analysis: &mut analysis,
                quota: None,
                settings: &mut settings,
            };
            view.on_key(KeyCode::Enter, &mut ctx);
        }
        assert!(usage.detail_open);
        let with_modal =
            render_legacy(&mut view, &mut usage, &mut analysis, &mut settings).join("\n");
        assert!(with_modal.contains("Details"), "{with_modal}");

        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (44.0, 0.1),
                data_status: None,
                update_status: None,
                usage: &mut usage,
                analysis: &mut analysis,
                quota: None,
                settings: &mut settings,
            };
            view.on_key(KeyCode::Esc, &mut ctx);
        }
        assert!(!usage.detail_open);

        // Clicking the Total column header sorts by Total descending.
        let header_y = initial
            .iter()
            .position(|l| l.contains("Cost (USD)"))
            .unwrap() as u16;
        let total_x = initial[header_y as usize].find("Total").unwrap() as u16;
        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (44.0, 0.1),
                data_status: None,
                update_status: None,
                usage: &mut usage,
                analysis: &mut analysis,
                quota: None,
                settings: &mut settings,
            };
            view.on_mouse(
                MouseInput::Click {
                    column: total_x + 1,
                    row: header_y,
                },
                &mut ctx,
            );
        }
        assert_eq!(usage.sort(), UsageSort::Total);
    }
}
