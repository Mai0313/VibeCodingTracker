//! `classic` layout (default): split master-detail workbench with rounded borders, clickable top bar and column headers, and a live side `Inspector` pane on wide terminals (falling back to modal popups on narrow terminals).

use crate::display::analysis::{
    build_analysis_detail_sections, build_analysis_table_row, layout_analysis_columns,
};
use crate::display::app::Tab;
use crate::display::common::table::{
    DetailPaneChrome, TableChrome, create_controls_with_status, header_column_at,
    render_detail_pane, render_detail_panel, render_scrollable_table_with_border, render_too_small,
    table_body_row_at, title_line,
};
use crate::display::common::tui::{MouseInput, NavDelta};
use crate::display::usage::{
    build_provider_total_rows, build_usage_detail_sections, build_usage_table_row,
    layout_usage_columns, provider_digest, render_provider_pane, render_provider_panel,
};
use crate::layouts::{
    EventResult, LayoutContext, LayoutMeta, LayoutView, TopBarHits, TuiLayout, contains,
    handle_settings_mouse, handle_top_bar_click, render_empty_inspector, render_standard_non_data,
    render_top_bar, side_toggle_note,
};
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    widgets::{BorderType, Paragraph},
};
use vct_core::utils::{format_compact, format_cost};

/// Static plugin entry discovered by `src/tui/build.rs`.
pub(crate) static LAYOUT: ClassicPlugin = ClassicPlugin;

const USAGE_SPLIT_MIN_W: u16 = 112;
const ANALYSIS_SPLIT_MIN_W: u16 = 118;

pub(crate) struct ClassicPlugin;

impl TuiLayout for ClassicPlugin {
    fn meta(&self) -> LayoutMeta {
        LayoutMeta {
            id: "classic",
            title: "Classic",
            summary: "Split master-detail workbench with a live side inspector and rounded borders",
        }
    }

    fn create(&self) -> Box<dyn LayoutView> {
        Box::new(ClassicView::default())
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum SideMode {
    #[default]
    Inspector,
    Providers,
}

#[derive(Default)]
struct ClassicView {
    top_bar: TopBarHits,
    side_mode: SideMode,
    inspector_scroll: usize,
    last_usage_model: Option<String>,
    last_analysis_model: Option<String>,
    detail_modal_open: bool,
    provider_modal_open: bool,
    split_active: bool,
    table_area: Option<Rect>,
    side_area: Option<Rect>,
    digest_area: Option<Rect>,
}

impl ClassicView {
    fn sync_inspector_target(&mut self, ctx: &LayoutContext<'_>) {
        match ctx.tab {
            Tab::Usage => {
                let cur = ctx.usage.selected_row().map(|r| r.model.clone());
                if cur != self.last_usage_model {
                    self.last_usage_model = cur;
                    self.inspector_scroll = 0;
                }
            }
            Tab::Analysis => {
                let cur = ctx.analysis.selected_row().map(|r| r.model.clone());
                if cur != self.last_analysis_model {
                    self.last_analysis_model = cur;
                    self.inspector_scroll = 0;
                }
            }
            _ => {}
        }
    }

    fn draw_usage(&mut self, f: &mut Frame, area: Rect, ctx: &mut LayoutContext<'_>) {
        if area.width < 74 || area.height < 14 {
            self.top_bar = TopBarHits::default();
            render_too_small(f, 74, 14);
            return;
        }
        if ctx.usage.selected_row().is_none() {
            self.detail_modal_open = false;
        }
        self.sync_inspector_target(ctx);
        let [work, controls] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

        self.split_active = work.width >= USAGE_SPLIT_MIN_W;
        let (left_area, right_area) = if self.split_active {
            self.detail_modal_open = false;
            self.provider_modal_open = false;
            let side_w = (work.width / 3).clamp(38, 48);
            let [l, r] =
                Layout::horizontal([Constraint::Min(68), Constraint::Length(side_w)]).areas(work);
            (l, Some(r))
        } else {
            (work, None)
        };
        self.side_area = right_area;

        let [table_area, digest_area] =
            Layout::vertical([Constraint::Min(6), Constraint::Length(1)]).areas(left_area);
        self.table_area = Some(table_area);
        self.digest_area = Some(digest_area);

        let cols = layout_usage_columns(table_area.width.saturating_sub(2));
        let totals_tokens = ctx.usage.totals().total;
        let totals_cost = ctx.usage.totals().cost;
        let sort = ctx.usage.sort();
        let rows: Vec<_> = ctx
            .usage
            .rows()
            .iter()
            .map(|row| {
                build_usage_table_row(row, totals_tokens, &cols, "", ctx.usage.update_tracker())
            })
            .collect();
        let row_count = rows.len();
        let title = format!(
            "Models · {} · {} tokens · {} models",
            format_cost(totals_cost),
            format_compact(totals_tokens),
            row_count
        );
        let sorted = cols.sorted_header_index(sort);
        let dropped = cols.dropped;
        render_scrollable_table_with_border(
            f,
            table_area,
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
            ctx.usage.scroll_mut(),
            BorderType::Rounded,
        );

        let provider_rows = build_provider_total_rows(ctx.usage.provider_totals());
        f.render_widget(
            Paragraph::new(provider_digest(&provider_rows, digest_area.width)).centered(),
            digest_area,
        );

        if let Some(side) = right_area {
            let toggle = side_toggle_note(self.side_mode == SideMode::Inspector, side.width);
            match self.side_mode {
                SideMode::Inspector => {
                    if let Some(row) = ctx.usage.selected_row() {
                        let sections = build_usage_detail_sections(row, ctx.usage.totals());
                        render_detail_pane(
                            f,
                            side,
                            DetailPaneChrome {
                                title: title_line("Inspector"),
                                right_note: Some(toggle),
                                border_type: BorderType::Rounded,
                            },
                            &row.model,
                            &sections,
                            &mut self.inspector_scroll,
                        );
                    } else {
                        render_empty_inspector(f, side, Some(toggle), "No model selected");
                    }
                }
                SideMode::Providers => {
                    render_provider_pane(
                        f,
                        side,
                        &provider_rows,
                        title_line("Providers"),
                        Some(toggle),
                        BorderType::Rounded,
                    );
                }
            }
        }

        let hints: &[(&str, &str)] = if self.split_active {
            &[
                ("↑↓", "move"),
                ("←→", "sort"),
                ("[ ]", "range"),
                ("p", "providers"),
                ("r", "refresh"),
            ]
        } else {
            &[
                ("↑↓", "move"),
                ("←→", "sort"),
                ("[ ]", "range"),
                ("p", "providers"),
                ("⏎", "details"),
                ("r", "refresh"),
            ]
        };
        f.render_widget(
            create_controls_with_status(hints, ctx.data_status, controls.width),
            controls,
        );

        if !self.split_active {
            if self.provider_modal_open {
                render_provider_panel(f, area, &provider_rows);
            }
            if self.detail_modal_open
                && let Some(row) = ctx.usage.selected_row()
            {
                let sections = build_usage_detail_sections(row, ctx.usage.totals());
                render_detail_panel(f, area, &row.model, &sections);
            }
        }
    }

    fn draw_analysis(&mut self, f: &mut Frame, area: Rect, ctx: &mut LayoutContext<'_>) {
        if area.width < 84 || area.height < 14 {
            self.top_bar = TopBarHits::default();
            render_too_small(f, 84, 14);
            return;
        }
        if ctx.analysis.selected_row().is_none() {
            self.detail_modal_open = false;
        }
        self.sync_inspector_target(ctx);
        let [work, controls] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

        self.split_active = work.width >= ANALYSIS_SPLIT_MIN_W;
        let (left_area, right_area) = if self.split_active {
            self.detail_modal_open = false;
            self.provider_modal_open = false;
            let side_w = (work.width / 3).clamp(38, 48);
            let [l, r] =
                Layout::horizontal([Constraint::Min(76), Constraint::Length(side_w)]).areas(work);
            (l, Some(r))
        } else {
            (work, None)
        };
        self.side_area = right_area;

        let table_area = left_area;
        self.table_area = Some(table_area);

        let cols = layout_analysis_columns(table_area.width.saturating_sub(2));
        let total_lines = ctx.analysis.totals().total_lines();
        let total_calls = ctx.analysis.totals().total_calls();
        let sort = ctx.analysis.sort();
        let rows: Vec<_> = ctx
            .analysis
            .rows()
            .iter()
            .map(|row| build_analysis_table_row(row, &cols, "", ctx.analysis.update_tracker()))
            .collect();
        let row_count = rows.len();
        let title = format!(
            "Models · {} lines · {} tool calls · {} models",
            format_compact(total_lines as i64),
            format_compact(total_calls as i64),
            row_count
        );
        let sorted = cols.sorted_header_index(sort);
        let dropped = cols.dropped;
        render_scrollable_table_with_border(
            f,
            table_area,
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
            ctx.analysis.scroll_mut(),
            BorderType::Rounded,
        );

        if let Some(side) = right_area {
            if let Some(row) = ctx.analysis.selected_row() {
                let sections = build_analysis_detail_sections(row, ctx.analysis.totals());
                render_detail_pane(
                    f,
                    side,
                    DetailPaneChrome {
                        title: title_line("Inspector"),
                        right_note: None,
                        border_type: BorderType::Rounded,
                    },
                    &row.model,
                    &sections,
                    &mut self.inspector_scroll,
                );
            } else {
                render_empty_inspector(f, side, None, "No model selected");
            }
        }

        let hints: &[(&str, &str)] = if self.split_active {
            &[
                ("↑↓", "move"),
                ("←→", "sort"),
                ("[ ]", "range"),
                ("r", "refresh"),
            ]
        } else {
            &[
                ("↑↓", "move"),
                ("←→", "sort"),
                ("[ ]", "range"),
                ("⏎", "details"),
                ("r", "refresh"),
            ]
        };
        f.render_widget(
            create_controls_with_status(hints, ctx.data_status, controls.width),
            controls,
        );

        if !self.split_active
            && self.detail_modal_open
            && let Some(row) = ctx.analysis.selected_row()
        {
            let sections = build_analysis_detail_sections(row, ctx.analysis.totals());
            render_detail_panel(f, area, &row.model, &sections);
        }
    }
}

impl LayoutView for ClassicView {
    fn draw(&mut self, f: &mut Frame, ctx: &mut LayoutContext<'_>) {
        self.table_area = None;
        self.side_area = None;
        self.digest_area = None;
        self.split_active = false;

        let [bar, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(f.area());
        let range = ctx.tab.scans().then_some(ctx.time_range);
        self.top_bar = render_top_bar(f, bar, ctx.tab, range, ctx.metrics);

        match ctx.tab {
            Tab::Usage if !ctx.loading => self.draw_usage(f, body, ctx),
            Tab::Analysis if !ctx.loading => self.draw_analysis(f, body, ctx),
            _ => render_standard_non_data(f, body, ctx, BorderType::Rounded),
        }
    }

    fn on_mouse(&mut self, mouse: MouseInput, ctx: &mut LayoutContext<'_>) -> EventResult {
        if ctx.tab == Tab::Settings {
            return handle_settings_mouse(mouse, ctx, &self.top_bar);
        }

        match mouse {
            MouseInput::Click { column, row } => {
                if !self.split_active && (self.detail_modal_open || self.provider_modal_open) {
                    if self.detail_modal_open {
                        self.detail_modal_open = false;
                    } else {
                        self.provider_modal_open = false;
                    }
                    return EventResult::consumed();
                }
                if let Some(cmd) = handle_top_bar_click(column, row, &self.top_bar, ctx.tab) {
                    return EventResult::Consumed(cmd);
                }
                if ctx.loading {
                    return EventResult::consumed();
                }

                if ctx.tab == Tab::Usage {
                    // Clicking the right pane's top border toggles Model <-> Providers.
                    if let Some(side) = self.side_area
                        && row == side.y
                        && column >= side.x
                        && column < side.right()
                    {
                        self.side_mode = match self.side_mode {
                            SideMode::Inspector => SideMode::Providers,
                            SideMode::Providers => SideMode::Inspector,
                        };
                        return EventResult::consumed();
                    }
                    if let Some(digest) = self.digest_area
                        && contains(digest, column, row)
                    {
                        if self.split_active {
                            self.side_mode = match self.side_mode {
                                SideMode::Inspector => SideMode::Providers,
                                SideMode::Providers => SideMode::Inspector,
                            };
                        } else {
                            self.provider_modal_open = !self.provider_modal_open;
                        }
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
                            let same = ctx.usage.selected_index() == Some(row_idx);
                            ctx.usage.select_index(row_idx);
                            self.side_mode = SideMode::Inspector;
                            if !self.split_active && same {
                                self.detail_modal_open = true;
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
                        let same = ctx.analysis.selected_index() == Some(row_idx);
                        ctx.analysis.select_index(row_idx);
                        if !self.split_active && same {
                            self.detail_modal_open = true;
                        }
                        return EventResult::consumed();
                    }
                }
                EventResult::consumed()
            }
            MouseInput::Scroll {
                column, row, lines, ..
            } => {
                if !ctx.tab.scans() || ctx.loading {
                    return EventResult::consumed();
                }
                if let Some(side) = self.side_area
                    && contains(side, column, row)
                {
                    if ctx.tab != Tab::Usage || self.side_mode == SideMode::Inspector {
                        let next = (self.inspector_scroll as isize + isize::from(lines)).max(0);
                        self.inspector_scroll = next as usize;
                    }
                    return EventResult::consumed();
                }
                let delta = NavDelta {
                    lines: i64::from(lines),
                };
                if ctx.tab == Tab::Usage {
                    ctx.usage.navigate(delta);
                } else {
                    ctx.analysis.navigate(delta);
                }
                EventResult::consumed()
            }
        }
    }

    fn on_key(&mut self, key: KeyCode, ctx: &mut LayoutContext<'_>) -> EventResult {
        let inspector_visible =
            self.split_active && (ctx.tab != Tab::Usage || self.side_mode == SideMode::Inspector);
        match key {
            KeyCode::Esc => {
                if self.detail_modal_open {
                    self.detail_modal_open = false;
                } else if self.provider_modal_open {
                    self.provider_modal_open = false;
                } else if self.side_mode == SideMode::Providers {
                    self.side_mode = SideMode::Inspector;
                }
                EventResult::consumed()
            }
            KeyCode::Enter if ctx.tab.scans() && !ctx.loading => {
                if self.split_active {
                    if ctx.tab == Tab::Usage && self.side_mode == SideMode::Providers {
                        self.side_mode = SideMode::Inspector;
                    }
                } else {
                    let has_sel = if ctx.tab == Tab::Usage {
                        ctx.usage.selected_row().is_some()
                    } else {
                        ctx.analysis.selected_row().is_some()
                    };
                    self.detail_modal_open = has_sel && !self.detail_modal_open;
                }
                EventResult::consumed()
            }
            KeyCode::Char('p' | 'P') if ctx.tab == Tab::Usage && !ctx.loading => {
                if self.split_active {
                    self.side_mode = match self.side_mode {
                        SideMode::Inspector => SideMode::Providers,
                        SideMode::Providers => SideMode::Inspector,
                    };
                } else {
                    self.provider_modal_open = !self.provider_modal_open;
                }
                EventResult::consumed()
            }
            KeyCode::PageUp if ctx.tab.scans() && !ctx.loading && inspector_visible => {
                self.inspector_scroll = self.inspector_scroll.saturating_sub(5);
                EventResult::consumed()
            }
            KeyCode::PageDown if ctx.tab.scans() && !ctx.loading && inspector_visible => {
                self.inspector_scroll = self.inspector_scroll.saturating_add(5);
                EventResult::consumed()
            }
            _ => EventResult::Ignored,
        }
    }

    fn on_tab_changed(&mut self, _next: Tab, _ctx: &mut LayoutContext<'_>) {
        self.detail_modal_open = false;
        self.provider_modal_open = false;
        self.side_mode = SideMode::Inspector;
        self.inspector_scroll = 0;
    }

    fn help_rows(&self, tab: Tab) -> Option<Vec<(&'static str, &'static str)>> {
        if !tab.scans() {
            return None;
        }
        let mut rows = if self.split_active {
            let mut r = vec![
                ("↑ ↓ / wheel", "select a model (live in Inspector)"),
                ("click header", "sort by the clicked column"),
                ("← →", "sort by the column to the left or right"),
                ("[ ] / click", "change time range"),
                ("PgUp PgDn", "scroll the side Inspector"),
                ("r", "scan again now"),
            ];
            if tab == Tab::Usage {
                r.push(("p", "toggle side pane between Model and Providers"));
            }
            r.extend([
                ("tab", "next tab, shift+tab for the previous one"),
                ("esc", "return side pane to Model inspector"),
                ("?", "this list"),
                ("q", "quit"),
            ]);
            r
        } else {
            let mut r = vec![
                ("↑ ↓ / wheel", "select a model"),
                ("PgUp PgDn", "move up or down by 8 models"),
                ("⏎ / click", "show the selected model's details"),
                ("click header", "sort by the clicked column"),
                ("← →", "sort by the column to the left or right"),
                ("[ ] / click", "change time range"),
                ("r", "scan again now"),
            ];
            if tab == Tab::Usage {
                r.push(("p", "per-provider totals"));
            }
            r.extend([
                ("tab", "next tab, shift+tab for the previous one"),
                ("esc", "close a panel"),
                ("?", "this list"),
                ("q", "quit"),
            ]);
            r
        };
        let _ = &mut rows;
        Some(rows)
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

    fn seeded_states() -> (UsageUiState, AnalysisUiState, SettingsUiState) {
        let mut usage = UsageUiState::new(true);
        let row_a = UsageRow {
            model: "claude-opus-4-7".to_string(),
            input_tokens: 100_000,
            output_tokens: 25_000,
            total: 125_000,
            cost: 12.50,
            providers: vec![ModelProviderUsage {
                provider: Provider::ClaudeCode,
                total_tokens: 125_000,
                cost: 12.50,
            }],
            ..UsageRow::default()
        };
        let row_b = UsageRow {
            model: "gpt-5.5".to_string(),
            input_tokens: 200_000,
            output_tokens: 40_000,
            total: 240_000,
            cost: 4.20,
            providers: vec![ModelProviderUsage {
                provider: Provider::Codex,
                total_tokens: 240_000,
                cost: 4.20,
            }],
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

        let analysis = AnalysisUiState::new();
        let settings = SettingsUiState::new(Config::default(), Box::new(|_, _| Ok(())));
        (usage, analysis, settings)
    }

    fn render_lines(
        view: &mut Box<dyn LayoutView>,
        w: u16,
        h: u16,
        tab: Tab,
        usage: &mut UsageUiState,
        analysis: &mut AnalysisUiState,
        settings: &mut SettingsUiState,
    ) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                let mut ctx = LayoutContext {
                    tab,
                    time_range: TimeRange::All,
                    loading: false,
                    retry_pending: false,
                    spinner: 0,
                    metrics: (48.0, 0.2),
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
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn wide_terminal_renders_side_inspector_and_narrow_falls_back_to_modal() {
        let (mut usage, mut analysis, mut settings) = seeded_states();
        let mut view = LAYOUT.create();

        let wide = render_lines(
            &mut view,
            132,
            26,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(wide.contains("Inspector"), "missing Inspector:\n{wide}");
        assert!(
            wide.contains("Claude"),
            "missing provider breakdown:\n{wide}"
        );
        assert!(
            wide.contains("Star on GitHub"),
            "missing footer repo label:\n{wide}"
        );

        // Narrow 80x24 drops the side pane cleanly and uses Enter for modal details.
        let narrow = render_lines(
            &mut view,
            80,
            24,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(
            !narrow.contains("Inspector"),
            "unexpected side pane:\n{narrow}"
        );
        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (48.0, 0.2),
                data_status: None,
                update_status: None,
                usage: &mut usage,
                analysis: &mut analysis,
                quota: None,
                settings: &mut settings,
            };
            view.on_key(KeyCode::Enter, &mut ctx);
        }
        let modal = render_lines(
            &mut view,
            80,
            24,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(
            modal.contains("Details") && modal.contains("esc close"),
            "{modal}"
        );
    }

    #[test]
    fn clicking_rows_and_column_headers_updates_selection_and_sort() {
        let (mut usage, mut analysis, mut settings) = seeded_states();
        let mut view = LAYOUT.create();
        let lines = render_lines(
            &mut view,
            132,
            26,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );

        // Locate "gpt-5.5" row and click it -> Inspector updates to gpt-5.5 (Codex).
        let gpt_row = lines
            .iter()
            .position(|line| line.contains("gpt-5.5"))
            .unwrap() as u16;
        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (48.0, 0.2),
                data_status: None,
                update_status: None,
                usage: &mut usage,
                analysis: &mut analysis,
                quota: None,
                settings: &mut settings,
            };
            view.on_mouse(
                MouseInput::Click {
                    column: 6,
                    row: gpt_row,
                },
                &mut ctx,
            );
        }
        assert_eq!(usage.selected_row().unwrap().model, "gpt-5.5");

        let updated = render_lines(
            &mut view,
            132,
            26,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        let joined = updated.join("\n");
        assert!(
            joined.contains("Codex"),
            "Inspector should show Codex:\n{joined}"
        );

        // Locate "Total" column header and click it -> sorts by Total descending (gpt-5.5 first).
        let header_row = updated
            .iter()
            .position(|line| line.contains("Cost (USD)"))
            .unwrap() as u16;
        let total_col = updated[header_row as usize].find("Total").unwrap() as u16;
        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (48.0, 0.2),
                data_status: None,
                update_status: None,
                usage: &mut usage,
                analysis: &mut analysis,
                quota: None,
                settings: &mut settings,
            };
            view.on_mouse(
                MouseInput::Click {
                    column: total_col + 1,
                    row: header_row,
                },
                &mut ctx,
            );
        }
        assert_eq!(usage.sort(), UsageSort::Total);
        assert_eq!(usage.rows()[0].model, "gpt-5.5");
    }

    #[test]
    fn narrow_split_preserves_inspector_title_on_overflow_and_clears_stale_modals() {
        let (mut usage, mut analysis, mut settings) = seeded_states();
        let mut view = LAYOUT.create();

        // Open Details popup at 80x24, then widen to 112x16 (narrowest split width).
        let _ = render_lines(
            &mut view,
            80,
            24,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (48.0, 0.2),
                data_status: None,
                update_status: None,
                usage: &mut usage,
                analysis: &mut analysis,
                quota: None,
                settings: &mut settings,
            };
            view.on_key(KeyCode::Enter, &mut ctx);
        }

        // At 112x16 the Inspector overflows vertically (`+N ↓`), keeping the left `Inspector` title intact beside the compact right toggle.
        let split_lines = render_lines(
            &mut view,
            112,
            16,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        assert!(
            split_lines[1].contains("Inspector") && split_lines[1].contains("↓"),
            "Inspector title overwritten on 112x16:\n{}",
            split_lines.join("\n")
        );

        // Toggle to Providers (`p`): PageDown falls through (`EventResult::Ignored`) instead of scrolling the hidden Inspector, and `Esc` returns to Inspector because stale modals were cleared.
        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (48.0, 0.2),
                data_status: None,
                update_status: None,
                usage: &mut usage,
                analysis: &mut analysis,
                quota: None,
                settings: &mut settings,
            };
            view.on_key(KeyCode::Char('p'), &mut ctx);
            assert_eq!(
                view.on_key(KeyCode::PageDown, &mut ctx),
                EventResult::Ignored
            );
            view.on_key(KeyCode::Esc, &mut ctx);
        }
        let after_esc = render_lines(
            &mut view,
            112,
            16,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(after_esc.contains("Inspector"), "{after_esc}");

        // Shrinking back to 80x24 must not resurrect the stale Details modal.
        let shrunk = render_lines(
            &mut view,
            80,
            24,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(!shrunk.contains("esc close"), "{shrunk}");
    }

    #[test]
    fn analysis_tab_omits_bottom_providers_band() {
        let (mut usage, mut analysis, mut settings) = seeded_states();
        let mut view = LAYOUT.create();
        let screen = render_lines(
            &mut view,
            132,
            26,
            Tab::Analysis,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(screen.contains("Models ·"), "{screen}");
        assert!(screen.contains("Inspector"), "{screen}");
        assert!(!screen.contains("All Providers"), "{screen}");
        assert!(!screen.contains(" Providers "), "{screen}");
    }
}
