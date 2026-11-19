//! `drawer` layout: underline-accent top bar, full-width `Models` table, and a collapsible horizontal bottom `Drawer` toggled with `Enter`, `Space`, or click.

use crate::display::analysis::{
    build_analysis_detail_sections, build_analysis_table_row, layout_analysis_columns,
};
use crate::display::app::Tab;
use crate::display::common::table::{
    TableChrome, create_controls_with_status, header_column_at, render_detail_panel,
    render_scrollable_table_with_border, render_too_small, table_body_row_at, title_line,
};
use crate::display::common::theme::theme;
use crate::display::common::tui::{MouseInput, NavDelta};
use crate::display::usage::{
    build_provider_total_rows, build_usage_detail_sections, build_usage_table_row,
    layout_usage_columns, provider_digest, render_provider_panel,
};
use crate::layouts::{
    EventResult, LayoutContext, LayoutMeta, LayoutView, TopBarHits, TuiLayout,
    build_analysis_strip_columns, build_usage_strip_columns, contains, handle_settings_mouse,
    handle_top_bar_click, render_standard_non_data, render_strip_pane, render_top_bar_styled,
    truncate_ellipsis,
};
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use vct_core::utils::{format_compact, format_cost, format_cost_compact};

/// Static plugin entry discovered by `src/tui/build.rs`.
pub(crate) static LAYOUT: DrawerPlugin = DrawerPlugin;

const EXPANDED_DRAWER_H: u16 = 7;
const COLLAPSED_BAR_H: u16 = 3;
const MIN_H_FOR_EXPANDED: u16 = 18;

pub(crate) struct DrawerPlugin;

impl TuiLayout for DrawerPlugin {
    fn meta(&self) -> LayoutMeta {
        LayoutMeta {
            id: "drawer",
            title: "Drawer",
            summary: "Full-width models table with a collapsible horizontal bottom detail drawer",
        }
    }

    fn create(&self) -> Box<dyn LayoutView> {
        Box::new(DrawerView::default())
    }
}

struct DrawerView {
    top_bar: TopBarHits,
    drawer_open: bool,
    drawer_can_expand: bool,
    detail_modal_open: bool,
    provider_modal_open: bool,
    table_area: Option<Rect>,
    digest_area: Option<Rect>,
    drawer_toggle_area: Option<Rect>,
}

impl Default for DrawerView {
    fn default() -> Self {
        Self {
            top_bar: TopBarHits::default(),
            drawer_open: true,
            drawer_can_expand: true,
            detail_modal_open: false,
            provider_modal_open: false,
            table_area: None,
            digest_area: None,
            drawer_toggle_area: None,
        }
    }
}

impl DrawerView {
    fn draw_usage(&mut self, f: &mut Frame, area: Rect, ctx: &mut LayoutContext<'_>) {
        if area.width < 74 || area.height < 14 {
            self.top_bar = TopBarHits::default();
            render_too_small(f, 74, 14);
            return;
        }
        let [work, controls] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

        self.drawer_can_expand = work.height >= MIN_H_FOR_EXPANDED;
        if self.drawer_can_expand || ctx.usage.selected_row().is_none() {
            self.detail_modal_open = false;
        }
        let expanded = self.drawer_open && self.drawer_can_expand;
        let drawer_h = if expanded {
            EXPANDED_DRAWER_H
        } else if work.height >= 12 {
            COLLAPSED_BAR_H
        } else {
            1
        };

        let [table_area, digest_area, drawer_area] = Layout::vertical([
            Constraint::Min(6),
            Constraint::Length(1),
            Constraint::Length(drawer_h),
        ])
        .areas(work);
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
            "Models · {} · {} tokens · {row_count} models",
            format_cost(totals_cost),
            format_compact(totals_tokens),
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

        if expanded {
            self.drawer_toggle_area = Some(Rect::new(
                drawer_area.x,
                drawer_area.y,
                drawer_area.width,
                1,
            ));
            if let Some(row) = ctx.usage.selected_row() {
                let strip_cols = build_usage_strip_columns(row, ctx.usage.totals());
                let max_title = usize::from(drawer_area.width).saturating_sub(28);
                let heading = format!(
                    "▾ Drawer · {}",
                    truncate_ellipsis(&row.model, max_title.max(8))
                );
                render_strip_pane(
                    f,
                    drawer_area,
                    title_line(&heading),
                    Some(Line::from(Span::styled(" ⏎ collapse ", theme().dim()))),
                    true,
                    &strip_cols,
                );
            } else {
                render_empty_drawer(f, drawer_area);
            }
        } else {
            self.drawer_toggle_area = Some(drawer_area);
            let summary = ctx.usage.selected_row().map(|row| {
                let mut provs: Vec<String> = row
                    .providers
                    .iter()
                    .take(3)
                    .map(|p| {
                        format!(
                            "{} {}",
                            p.provider.display_name(),
                            format_cost_compact(p.cost)
                        )
                    })
                    .collect();
                if row.providers.len() > 3 {
                    provs.push(format!("+{} more", row.providers.len() - 3));
                }
                (row.model.as_str(), provs.join(" · "))
            });
            render_collapsed_bar(f, drawer_area, summary, self.drawer_can_expand);
        }

        f.render_widget(
            create_controls_with_status(
                &[
                    ("↑↓", "move"),
                    (
                        "⏎",
                        if self.drawer_can_expand {
                            "drawer"
                        } else {
                            "details"
                        },
                    ),
                    ("←→", "sort"),
                    ("[ ]", "range"),
                    ("p", "providers"),
                    ("r", "refresh"),
                ],
                ctx.data_status,
                controls.width,
            ),
            controls,
        );

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

    fn draw_analysis(&mut self, f: &mut Frame, area: Rect, ctx: &mut LayoutContext<'_>) {
        if area.width < 84 || area.height < 14 {
            self.top_bar = TopBarHits::default();
            render_too_small(f, 84, 14);
            return;
        }
        let [work, controls] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

        self.drawer_can_expand = work.height >= MIN_H_FOR_EXPANDED;
        if self.drawer_can_expand || ctx.analysis.selected_row().is_none() {
            self.detail_modal_open = false;
        }
        let expanded = self.drawer_open && self.drawer_can_expand;
        let drawer_h = if expanded {
            EXPANDED_DRAWER_H
        } else if work.height >= 12 {
            COLLAPSED_BAR_H
        } else {
            1
        };

        let [table_area, drawer_area] =
            Layout::vertical([Constraint::Min(6), Constraint::Length(drawer_h)]).areas(work);
        self.table_area = Some(table_area);

        let cols = layout_analysis_columns(table_area.width.saturating_sub(2));
        let totals = ctx.analysis.totals();
        let sort = ctx.analysis.sort();
        let rows: Vec<_> = ctx
            .analysis
            .rows()
            .iter()
            .map(|row| build_analysis_table_row(row, &cols, "", ctx.analysis.update_tracker()))
            .collect();
        let row_count = rows.len();
        let title = format!(
            "Models · {} lines · {} calls · {row_count} models",
            format_compact(totals.total_lines() as i64),
            format_compact(totals.total_calls() as i64),
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

        if expanded {
            self.drawer_toggle_area = Some(Rect::new(
                drawer_area.x,
                drawer_area.y,
                drawer_area.width,
                1,
            ));
            if let Some(row) = ctx.analysis.selected_row() {
                let strip_cols = build_analysis_strip_columns(row, ctx.analysis.totals());
                let max_title = usize::from(drawer_area.width).saturating_sub(28);
                let heading = format!(
                    "▾ Drawer · {}",
                    truncate_ellipsis(&row.model, max_title.max(8))
                );
                render_strip_pane(
                    f,
                    drawer_area,
                    title_line(&heading),
                    Some(Line::from(Span::styled(" ⏎ collapse ", theme().dim()))),
                    true,
                    &strip_cols,
                );
            } else {
                render_empty_drawer(f, drawer_area);
            }
        } else {
            self.drawer_toggle_area = Some(drawer_area);
            let summary = ctx.analysis.selected_row().map(|row| {
                let text = format!(
                    "{} lines · {} calls",
                    format_compact(row.total_lines() as i64),
                    format_compact(row.total_calls() as i64)
                );
                (row.model.as_str(), text)
            });
            render_collapsed_bar(f, drawer_area, summary, self.drawer_can_expand);
        }

        f.render_widget(
            create_controls_with_status(
                &[
                    ("↑↓", "move"),
                    (
                        "⏎",
                        if self.drawer_can_expand {
                            "drawer"
                        } else {
                            "details"
                        },
                    ),
                    ("←→", "sort"),
                    ("[ ]", "range"),
                    ("r", "refresh"),
                ],
                ctx.data_status,
                controls.width,
            ),
            controls,
        );

        if self.detail_modal_open
            && let Some(row) = ctx.analysis.selected_row()
        {
            let sections = build_analysis_detail_sections(row, ctx.analysis.totals());
            render_detail_panel(f, area, &row.model, &sections);
        }
    }
}

fn render_empty_drawer(f: &mut Frame, area: Rect) {
    let theme = theme();
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(" No model selected", theme.dim()))).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.accent())
                .title(title_line("▾ Drawer"))
                .title(Line::from(Span::styled(" ⏎ collapse ", theme.dim())).right_aligned()),
        ),
        area,
    );
}

fn render_collapsed_bar(
    f: &mut Frame,
    area: Rect,
    summary: Option<(&str, String)>,
    can_expand: bool,
) {
    let theme = theme();
    let hint = if can_expand {
        " (⏎ or click to open drawer)"
    } else {
        " (⏎ for details)"
    };
    let inner_w = if area.height >= 3 {
        usize::from(area.width.saturating_sub(4))
    } else {
        usize::from(area.width.saturating_sub(2))
    };
    let line = if let Some((model, detail)) = summary {
        let mut spans = vec![
            Span::styled("▸ ", theme.accent().bold()),
            Span::styled(
                truncate_ellipsis(model, inner_w / 2),
                Style::default().bold(),
            ),
        ];
        let used = 2 + model.chars().count().min(inner_w / 2);
        let tail = if detail.is_empty() {
            hint.to_string()
        } else {
            format!(" — {detail}{hint}")
        };
        let rem = inner_w.saturating_sub(used);
        if rem >= 4 {
            spans.push(Span::styled(truncate_ellipsis(&tail, rem), theme.dim()));
        }
        Line::from(spans)
    } else {
        Line::from(Span::styled(
            truncate_ellipsis(&format!("▸ Drawer{hint}"), inner_w),
            theme.dim(),
        ))
    };

    if area.height >= 3 {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border());
        let inner = block.inner(area);
        f.render_widget(block, area);
        let padded = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 1);
        f.render_widget(Paragraph::new(line), padded);
    } else {
        f.render_widget(Paragraph::new(line), area);
    }
}

impl LayoutView for DrawerView {
    fn draw(&mut self, f: &mut Frame, ctx: &mut LayoutContext<'_>) {
        self.table_area = None;
        self.digest_area = None;
        self.drawer_toggle_area = None;

        let [bar, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(f.area());
        let range = ctx.tab.scans().then_some(ctx.time_range);
        self.top_bar = render_top_bar_styled(f, bar, ctx.tab, range, ctx.metrics, true);

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
                if self.detail_modal_open || self.provider_modal_open {
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

                if ctx.tab.scans()
                    && let Some(toggle) = self.drawer_toggle_area
                    && contains(toggle, column, row)
                {
                    if self.drawer_can_expand {
                        self.drawer_open = !self.drawer_open;
                    } else {
                        let has_sel = if ctx.tab == Tab::Usage {
                            ctx.usage.selected_row().is_some()
                        } else {
                            ctx.analysis.selected_row().is_some()
                        };
                        self.detail_modal_open = has_sel && !self.detail_modal_open;
                    }
                    return EventResult::consumed();
                }

                if ctx.tab == Tab::Usage {
                    if let Some(digest) = self.digest_area
                        && contains(digest, column, row)
                    {
                        self.provider_modal_open = !self.provider_modal_open;
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
                            if same && !self.drawer_open && self.drawer_can_expand {
                                self.drawer_open = true;
                            } else if same && !self.drawer_can_expand {
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
                        if same && !self.drawer_open && self.drawer_can_expand {
                            self.drawer_open = true;
                        } else if same && !self.drawer_can_expand {
                            self.detail_modal_open = true;
                        }
                        return EventResult::consumed();
                    }
                }
                EventResult::consumed()
            }
            MouseInput::Scroll { lines, .. } => {
                if ctx.tab.scans() && !ctx.loading {
                    let delta = NavDelta {
                        lines: i64::from(lines),
                    };
                    if ctx.tab == Tab::Usage {
                        ctx.usage.navigate(delta);
                    } else {
                        ctx.analysis.navigate(delta);
                    }
                }
                EventResult::consumed()
            }
        }
    }

    fn on_key(&mut self, key: KeyCode, ctx: &mut LayoutContext<'_>) -> EventResult {
        match key {
            KeyCode::Esc => {
                if self.detail_modal_open {
                    self.detail_modal_open = false;
                } else if self.provider_modal_open {
                    self.provider_modal_open = false;
                } else if self.drawer_open && self.drawer_can_expand {
                    self.drawer_open = false;
                }
                EventResult::consumed()
            }
            KeyCode::Enter | KeyCode::Char(' ') if ctx.tab.scans() && !ctx.loading => {
                if self.drawer_can_expand {
                    self.drawer_open = !self.drawer_open;
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
                self.provider_modal_open = !self.provider_modal_open;
                EventResult::consumed()
            }
            _ => EventResult::Ignored,
        }
    }

    fn on_tab_changed(&mut self, _next: Tab, _ctx: &mut LayoutContext<'_>) {
        self.detail_modal_open = false;
        self.provider_modal_open = false;
    }

    fn help_rows(&self, tab: Tab) -> Option<Vec<(&'static str, &'static str)>> {
        if !tab.scans() {
            return None;
        }
        let mut rows = vec![
            ("↑ ↓ / wheel", "select a model (live in Drawer)"),
            ("PgUp PgDn", "move up or down by 8 models"),
            ("⏎ / space / click", "toggle bottom detail Drawer"),
            ("click header", "sort by the clicked column"),
            ("← →", "sort by the column to the left or right"),
            ("[ ] / click", "change time range"),
            ("r", "scan again now"),
        ];
        if tab == Tab::Usage {
            rows.push(("p", "per-provider totals"));
        }
        rows.extend([
            ("tab", "next tab, shift+tab for the previous one"),
            ("esc", "collapse Drawer or close a panel"),
            ("?", "this list"),
            ("q", "quit"),
        ]);
        Some(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::analysis::AnalysisUiState;
    use crate::display::settings::SettingsUiState;
    use crate::display::usage::{
        ModelProviderUsage, UsageRefreshPayload, UsageRow, UsageTotals, UsageUiState,
    };
    use ratatui::{Terminal, backend::TestBackend};
    use vct_core::config::Config;
    use vct_core::models::{Provider, ProviderTotals, TimeRange};

    fn seeded() -> (UsageUiState, AnalysisUiState, SettingsUiState) {
        let mut usage = UsageUiState::new(true);
        let row = UsageRow {
            model: "claude-opus-4-8".to_string(),
            input_tokens: 100_000,
            output_tokens: 30_000,
            cache_read: 800_000,
            total: 930_000,
            cost: 16.20,
            providers: vec![ModelProviderUsage {
                provider: Provider::ClaudeCode,
                total_tokens: 930_000,
                cost: 16.20,
            }],
            ..UsageRow::default()
        };
        let mut totals = UsageTotals::default();
        totals.accumulate(&row);
        usage.apply(UsageRefreshPayload {
            rows: vec![row.clone()],
            merged_rows: vec![row],
            totals,
            provider_totals: ProviderTotals::default(),
        });
        (
            usage,
            AnalysisUiState::new(),
            SettingsUiState::new(Config::default(), Box::new(|_, _| Ok(()))),
        )
    }

    fn render(
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
                    metrics: (50.0, 0.2),
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
    fn drawer_opens_by_default_toggles_with_enter_and_analysis_omits_providers_band() {
        let (mut usage, mut analysis, mut settings) = seeded();
        let mut view = LAYOUT.create();

        let expanded = render(
            &mut view,
            96,
            26,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        let screen = expanded.join("\n");
        assert!(screen.contains("▾ Drawer · claude-opus-4-8"), "{screen}");
        assert!(screen.contains("Efficiency & Share"), "{screen}");

        let wide = render(
            &mut view,
            120,
            26,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        assert!(
            wide.last().unwrap().contains("Star on GitHub"),
            "{}",
            wide.join("\n")
        );

        // Enter collapses the drawer into a compact 3-row summary bar.
        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (50.0, 0.2),
                data_status: None,
                update_status: None,
                usage: &mut usage,
                analysis: &mut analysis,
                quota: None,
                settings: &mut settings,
            };
            view.on_key(KeyCode::Enter, &mut ctx);
        }
        let collapsed_lines = render(
            &mut view,
            96,
            26,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        let collapsed = collapsed_lines.join("\n");
        assert!(collapsed.contains("▸ claude-opus-4-8"), "{collapsed}");
        assert!(!collapsed.contains("▾ Drawer"), "{collapsed}");

        // Clicking the collapsed bar re-opens the drawer.
        let bar_y = collapsed_lines
            .iter()
            .position(|l| l.contains("▸ claude-opus-4-8"))
            .unwrap() as u16;
        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (50.0, 0.2),
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
                    row: bar_y,
                },
                &mut ctx,
            );
        }
        let reopened = render(
            &mut view,
            96,
            26,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(
            reopened.contains("▾ Drawer · claude-opus-4-8"),
            "{reopened}"
        );

        let a_screen = render(
            &mut view,
            96,
            26,
            Tab::Analysis,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(!a_screen.contains("All Providers"), "{a_screen}");
    }

    #[test]
    fn resizing_taller_clears_short_screen_modal_and_collapsed_bar_counts_extra_providers() {
        let mut usage = UsageUiState::new(true);
        let row = UsageRow {
            model: "claude-opus-4-8".to_string(),
            total: 1_000_000,
            cost: 20.0,
            providers: vec![
                ModelProviderUsage {
                    provider: Provider::ClaudeCode,
                    total_tokens: 400_000,
                    cost: 8.0,
                },
                ModelProviderUsage {
                    provider: Provider::Antigravity,
                    total_tokens: 300_000,
                    cost: 6.0,
                },
                ModelProviderUsage {
                    provider: Provider::Codex,
                    total_tokens: 200_000,
                    cost: 4.0,
                },
                ModelProviderUsage {
                    provider: Provider::OpenCode,
                    total_tokens: 100_000,
                    cost: 2.0,
                },
            ],
            ..UsageRow::default()
        };
        let mut totals = UsageTotals::default();
        totals.accumulate(&row);
        usage.apply(UsageRefreshPayload {
            rows: vec![row.clone()],
            merged_rows: vec![row],
            totals,
            provider_totals: ProviderTotals::default(),
        });
        let mut analysis = AnalysisUiState::new();
        let mut settings = SettingsUiState::new(Config::default(), Box::new(|_, _| Ok(())));
        let mut view = LAYOUT.create();

        // Short terminal (`work.height < 18`): collapsed bar shows 3 providers plus `+1 more`, and Enter opens the details modal.
        let short = render(
            &mut view,
            120,
            16,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(short.contains("+1 more"), "missing +1 more:\n{short}");
        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::All,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (50.0, 0.2),
                data_status: None,
                update_status: None,
                usage: &mut usage,
                analysis: &mut analysis,
                quota: None,
                settings: &mut settings,
            };
            view.on_key(KeyCode::Enter, &mut ctx);
        }
        let with_modal = render(
            &mut view,
            120,
            16,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(with_modal.contains("esc close"), "{with_modal}");

        // Resizing tall enough to expand must clear the stale Details modal.
        let tall = render(
            &mut view,
            120,
            26,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(tall.contains("▾ Drawer"), "{tall}");
        assert!(
            !tall.contains("esc close"),
            "stale modal survived resize:\n{tall}"
        );
    }
}
