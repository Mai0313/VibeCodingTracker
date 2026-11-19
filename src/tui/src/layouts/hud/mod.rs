//! `hud` layout: 3-card KPI telemetry strip over a split master-detail
//! workbench that activates starting at 94 columns (98 on Analysis), with
//! width-adaptive `Inspector` / `Providers` side switching and full mouse
//! support.

use crate::display::analysis::{
    build_analysis_detail_sections, build_analysis_table_row, layout_analysis_columns,
};
use crate::display::app::Tab;
use crate::display::common::ProviderTotal;
use crate::display::common::table::{
    DetailPaneChrome, TableChrome, create_controls_with_status, header_column_at,
    render_detail_pane, render_detail_panel, render_scrollable_table_with_border, render_too_small,
    table_body_row_at, title_line,
};
use crate::display::common::theme::theme;
use crate::display::common::tui::{MouseInput, NavDelta};
use crate::display::usage::{
    ProviderStats, build_provider_total_rows, build_usage_detail_sections, build_usage_table_row,
    layout_usage_columns, provider_digest, provider_share_bar, render_provider_pane,
    render_provider_panel,
};
use crate::layouts::{
    EventResult, LayoutContext, LayoutMeta, LayoutView, TopBarHits, TuiLayout, contains,
    fit_table_title, handle_settings_mouse, handle_top_bar_click, render_empty_inspector,
    render_standard_non_data, render_top_bar, side_toggle_note, truncate_ellipsis,
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
pub(crate) static LAYOUT: HudPlugin = HudPlugin;

const USAGE_SPLIT_MIN_W: u16 = 94;
const ANALYSIS_SPLIT_MIN_W: u16 = 98;
const KPI_MIN_WORK_H: u16 = 16;

pub(crate) struct HudPlugin;

impl TuiLayout for HudPlugin {
    fn meta(&self) -> LayoutMeta {
        LayoutMeta {
            id: "hud",
            title: "HUD",
            summary: "3-card KPI telemetry strip over a 94-column split workbench",
        }
    }

    fn create(&self) -> Box<dyn LayoutView> {
        Box::new(HudView::default())
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum SideMode {
    #[default]
    Inspector,
    Providers,
}

#[derive(Default)]
struct HudView {
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
    provider_kpi_area: Option<Rect>,
}

impl HudView {
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

    fn toggle_providers(&mut self) {
        if self.split_active {
            self.side_mode = match self.side_mode {
                SideMode::Inspector => SideMode::Providers,
                SideMode::Providers => SideMode::Inspector,
            };
        } else {
            self.provider_modal_open = !self.provider_modal_open;
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

        let show_kpi = work.height >= KPI_MIN_WORK_H;
        let main_area = if show_kpi {
            let [kpi_band, lower] =
                Layout::vertical([Constraint::Length(4), Constraint::Min(8)]).areas(work);
            let kpi_providers = build_provider_total_rows(ctx.usage.provider_totals());
            self.provider_kpi_area = Some(render_usage_kpi_strip(f, kpi_band, ctx, &kpi_providers));
            lower
        } else {
            work
        };

        self.split_active = main_area.width >= USAGE_SPLIT_MIN_W;
        let (left_area, right_area) = if self.split_active {
            self.detail_modal_open = false;
            self.provider_modal_open = false;
            let side_w = (main_area.width / 3).clamp(32, 46);
            let [l, r] = Layout::horizontal([Constraint::Min(58), Constraint::Length(side_w)])
                .areas(main_area);
            (l, Some(r))
        } else {
            (main_area, None)
        };
        self.side_area = right_area;

        let table_area = if show_kpi {
            left_area
        } else {
            let [tbl, digest] =
                Layout::vertical([Constraint::Min(6), Constraint::Length(1)]).areas(left_area);
            self.digest_area = Some(digest);
            tbl
        };
        self.table_area = Some(table_area);

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
        let sorted = cols.sorted_header_index(sort);
        let dropped = cols.dropped;
        let note = if dropped > 0 {
            format!("{} · −{dropped} cols", sort.label())
        } else {
            sort.label().to_string()
        };
        let title = if show_kpi {
            fit_table_title(
                format!("Models · {row_count} active"),
                format!("Models · {row_count}"),
                "Models".to_string(),
                table_area.width,
                &note,
            )
        } else {
            fit_table_title(
                format!(
                    "Models · {} · {} tokens · {row_count} models",
                    format_cost(totals_cost),
                    format_compact(totals_tokens),
                ),
                format!(
                    "Models · {} · {}",
                    format_cost(totals_cost),
                    format_compact(totals_tokens),
                ),
                format!("Models · {}", format_cost(totals_cost)),
                table_area.width,
                &note,
            )
        };
        render_scrollable_table_with_border(
            f,
            table_area,
            TableChrome {
                title: &title,
                note: Some(note),
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
        if let Some(digest) = self.digest_area {
            f.render_widget(
                Paragraph::new(provider_digest(&provider_rows, digest.width)).centered(),
                digest,
            );
        }

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

        let show_kpi = work.height >= KPI_MIN_WORK_H;
        let main_area = if show_kpi {
            let [kpi_band, lower] =
                Layout::vertical([Constraint::Length(4), Constraint::Min(8)]).areas(work);
            render_analysis_kpi_strip(f, kpi_band, ctx);
            lower
        } else {
            work
        };

        self.split_active = main_area.width >= ANALYSIS_SPLIT_MIN_W;
        let (table_area, right_area) = if self.split_active {
            self.detail_modal_open = false;
            self.provider_modal_open = false;
            let side_w = (main_area.width / 3).clamp(32, 46);
            let [l, r] = Layout::horizontal([Constraint::Min(62), Constraint::Length(side_w)])
                .areas(main_area);
            (l, Some(r))
        } else {
            (main_area, None)
        };
        self.table_area = Some(table_area);
        self.side_area = right_area;

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
        let sorted = cols.sorted_header_index(sort);
        let dropped = cols.dropped;
        let note = if dropped > 0 {
            format!("{} · −{dropped} cols", sort.label())
        } else {
            sort.label().to_string()
        };
        let title = if show_kpi {
            fit_table_title(
                format!("Models · {row_count} active"),
                format!("Models · {row_count}"),
                "Models".to_string(),
                table_area.width,
                &note,
            )
        } else {
            fit_table_title(
                format!(
                    "Models · {} lines · {} tool calls · {row_count} models",
                    format_compact(totals.total_lines() as i64),
                    format_compact(totals.total_calls() as i64),
                ),
                format!(
                    "Models · {}L · {}c",
                    format_compact(totals.total_lines() as i64),
                    format_compact(totals.total_calls() as i64),
                ),
                format!("Models · {}L", format_compact(totals.total_lines() as i64)),
                table_area.width,
                &note,
            )
        };
        render_scrollable_table_with_border(
            f,
            table_area,
            TableChrome {
                title: &title,
                note: Some(note),
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

fn render_kpi_card(f: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'static>>) {
    let theme = theme();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border())
        .title(title_line(title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let padded = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    f.render_widget(Paragraph::new(lines), padded);
}

fn render_usage_kpi_strip(
    f: &mut Frame,
    area: Rect,
    ctx: &LayoutContext<'_>,
    provider_rows: &[ProviderTotal<'_, ProviderStats>],
) -> Rect {
    let theme = theme();
    let [c1, c2, c3] = Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .spacing(1)
    .areas(area);

    let totals = ctx.usage.totals();
    let rows = ctx.usage.rows();
    let top_row = rows.iter().max_by(|a, b| a.cost.total_cmp(&b.cost));
    let c1_w = usize::from(c1.width.saturating_sub(4));
    let spend_sub = if let Some(top) = top_row {
        let pct = if totals.cost > 0.0 {
            (top.cost / totals.cost * 100.0).round() as u32
        } else {
            0
        };
        truncate_ellipsis(&format!("Top: {} ({pct}%)", top.model), c1_w)
    } else {
        "No sessions in range".to_string()
    };
    render_kpi_card(
        f,
        c1,
        "Total Spend",
        vec![
            Line::from(Span::styled(
                format_cost(totals.cost),
                theme.accent().bold(),
            )),
            Line::from(Span::styled(spend_sub, theme.dim())),
        ],
    );

    let prompt_total = totals.input_tokens + totals.cache_read + totals.cache_creation;
    let hit_pct = if totals.cache_read > 0 && prompt_total > 0 {
        (totals.cache_read as f64 / prompt_total as f64 * 100.0).round() as u32
    } else {
        0
    };
    let c2_w = usize::from(c2.width.saturating_sub(4));
    let token_sub = if totals.cache_read > 0 {
        let tail = format!(" · {} models", rows.len());
        let label = "Cache hit ";
        let pct_str = format!("{hit_pct}%");
        if label.len() + pct_str.len() + tail.len() <= c2_w {
            Line::from(vec![
                Span::styled(label, theme.dim()),
                Span::styled(pct_str, Style::default().fg(theme.good).bold()),
                Span::styled(tail, theme.dim()),
            ])
        } else {
            Line::from(Span::styled(
                truncate_ellipsis(&format!("Cache {hit_pct}% · {}m", rows.len()), c2_w),
                theme.dim(),
            ))
        }
    } else {
        Line::from(Span::styled(
            truncate_ellipsis(&format!("{} active models", rows.len()), c2_w),
            theme.dim(),
        ))
    };
    render_kpi_card(
        f,
        c2,
        "Token Volume",
        vec![
            Line::from(Span::styled(
                format_compact(totals.total),
                Style::default().bold(),
            )),
            token_sub,
        ],
    );

    let bar_w = c3.width.saturating_sub(4);
    let mix_bar = provider_share_bar(provider_rows, bar_w);
    let mix_sub = compact_provider_legend(provider_rows, usize::from(bar_w));
    render_kpi_card(f, c3, "Provider Mix", vec![mix_bar, mix_sub]);
    c3
}

fn compact_provider_legend(
    rows: &[ProviderTotal<'_, ProviderStats>],
    max_w: usize,
) -> Line<'static> {
    let theme = theme();
    let mut listed: Vec<_> = rows.iter().filter(|r| r.label != "All Providers").collect();
    if listed.is_empty() {
        return Line::from(Span::styled("no providers", theme.dim()));
    }
    listed.sort_by(|a, b| b.stats.total_cost.total_cmp(&a.stats.total_cost));

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut shown = 0usize;
    for (idx, row) in listed.iter().enumerate() {
        let seg = format!(
            "{} {}",
            row.label,
            format_cost_compact(row.stats.total_cost)
        );
        let seg_w = seg.chars().count();
        let sep_w = if shown > 0 { 2 } else { 0 };
        let need = sep_w + 2 + seg_w;
        let remaining = listed.len().saturating_sub(idx + 1);
        let tail_reserve = if remaining > 0 {
            format!(" +{remaining}").len()
        } else {
            0
        };
        if shown > 0 && used + need + tail_reserve > max_w {
            break;
        }
        if shown == 0 && need + tail_reserve > max_w {
            let trimmed_w = max_w.saturating_sub(2 + tail_reserve);
            let trimmed = truncate_ellipsis(&seg, trimmed_w);
            let actual_w = trimmed.chars().count();
            spans.push(Span::styled("● ", Style::default().fg(row.tui_color)));
            spans.push(Span::styled(trimmed, theme.dim()));
            used = 2 + actual_w;
            shown = 1;
            break;
        }
        if shown > 0 {
            spans.push(Span::raw("  "));
            used += 2;
        }
        spans.push(Span::styled("● ", Style::default().fg(row.tui_color)));
        spans.push(Span::styled(seg, theme.dim()));
        used += 2 + seg_w;
        shown += 1;
    }
    let hidden = listed.len().saturating_sub(shown);
    if hidden > 0 {
        let tail = format!(" +{hidden}");
        if used + tail.len() <= max_w {
            spans.push(Span::styled(tail, theme.dim()));
        }
    }
    Line::from(spans)
}

fn render_analysis_kpi_strip(f: &mut Frame, area: Rect, ctx: &LayoutContext<'_>) {
    let theme = theme();
    let [c1, c2, c3] = Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .spacing(1)
    .areas(area);

    let totals = ctx.analysis.totals();
    let c1_w = usize::from(c1.width.saturating_sub(4));
    render_kpi_card(
        f,
        c1,
        "Code Touched",
        vec![
            Line::from(Span::styled(
                format!("{} lines", format_compact(totals.total_lines() as i64)),
                theme.accent().bold(),
            )),
            Line::from(Span::styled(
                truncate_ellipsis(
                    &format!(
                        "Edit {} · Read {}",
                        format_compact(totals.edit_lines as i64),
                        format_compact(totals.read_lines as i64),
                    ),
                    c1_w,
                ),
                theme.dim(),
            )),
        ],
    );

    let c2_w = usize::from(c2.width.saturating_sub(4));
    render_kpi_card(
        f,
        c2,
        "Tool Calls",
        vec![
            Line::from(Span::styled(
                format!("{} calls", format_compact(totals.total_calls() as i64)),
                Style::default().bold(),
            )),
            Line::from(Span::styled(
                truncate_ellipsis(
                    &format!(
                        "Bash {} · Edit {} · Read {}",
                        format_compact(totals.bash_count as i64),
                        format_compact(totals.edit_count as i64),
                        format_compact(totals.read_count as i64),
                    ),
                    c2_w,
                ),
                theme.dim(),
            )),
        ],
    );

    let c3_w = usize::from(c3.width.saturating_sub(4));
    let top = ctx
        .analysis
        .rows()
        .iter()
        .max_by_key(|r| (r.total_lines(), r.total_calls()));
    let (top_title, top_sub) = if let Some(row) = top {
        (
            truncate_ellipsis(&row.model, c3_w),
            truncate_ellipsis(
                &format!(
                    "{} edit L · {} bash",
                    format_compact(row.edit_lines as i64),
                    format_compact(row.bash_count as i64),
                ),
                c3_w,
            ),
        )
    } else {
        ("None".to_string(), "No sessions in range".to_string())
    };
    render_kpi_card(
        f,
        c3,
        "Top Model",
        vec![
            Line::from(Span::styled(top_title, Style::default().bold())),
            Line::from(Span::styled(top_sub, theme.dim())),
        ],
    );
}

impl LayoutView for HudView {
    fn draw(&mut self, f: &mut Frame, ctx: &mut LayoutContext<'_>) {
        self.table_area = None;
        self.side_area = None;
        self.digest_area = None;
        self.provider_kpi_area = None;
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
                    if let Some(kpi) = self.provider_kpi_area
                        && contains(kpi, column, row)
                    {
                        self.toggle_providers();
                        return EventResult::consumed();
                    }
                    if let Some(side) = self.side_area
                        && row == side.y
                        && column >= side.x
                        && column < side.right()
                    {
                        self.toggle_providers();
                        return EventResult::consumed();
                    }
                    if let Some(digest) = self.digest_area
                        && contains(digest, column, row)
                    {
                        self.toggle_providers();
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
                self.toggle_providers();
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
                r.push((
                    "p / click mix",
                    "toggle side pane between Model and Providers",
                ));
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
                r.push(("p / click mix", "per-provider totals"));
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
            output_tokens: 25_000,
            cache_read: 900_000,
            total: 1_025_000,
            cost: 18.40,
            providers: vec![ModelProviderUsage {
                provider: Provider::ClaudeCode,
                total_tokens: 1_025_000,
                cost: 18.40,
            }],
            ..UsageRow::default()
        };
        let mut totals = UsageTotals::default();
        totals.accumulate(&row);
        let mut pt: ProviderTotals<ProviderStats> = ProviderTotals::default();
        pt.claude.total_tokens = 1_025_000;
        pt.claude.total_cost = 18.40;
        pt.claude.days_count = 3;
        pt.overall = pt.claude.clone();
        usage.apply(UsageRefreshPayload {
            rows: vec![row.clone()],
            merged_rows: vec![row],
            totals,
            provider_totals: pt,
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
    fn renders_kpi_strip_and_split_inspector_at_96_cols_and_clean_analysis() {
        let (mut usage, mut analysis, mut settings) = seeded();
        let mut view = LAYOUT.create();

        let lines = render(
            &mut view,
            96,
            26,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        let screen = lines.join("\n");
        assert!(screen.contains("Total Spend"), "{screen}");
        assert!(screen.contains("Token Volume"), "{screen}");
        assert!(screen.contains("Provider Mix"), "{screen}");
        assert!(screen.contains("Inspector"), "{screen}");
        assert!(lines.last().unwrap().contains("Star on GitHub"), "{screen}");

        // Narrowest split width (94x22) still preserves Inspector title even with overflow.
        let tight = render(
            &mut view,
            94,
            22,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        assert!(
            tight.iter().any(|l| l.contains("Inspector")),
            "Inspector title missing at 94x22:\n{}",
            tight.join("\n")
        );

        // Analysis tab shows KPI cards + Inspector and omits any bottom Providers block.
        let a_screen = render(
            &mut view,
            104,
            26,
            Tab::Analysis,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(a_screen.contains("Code Touched"), "{a_screen}");
        assert!(a_screen.contains("Tool Calls"), "{a_screen}");
        assert!(!a_screen.contains("All Providers"), "{a_screen}");
        assert!(!a_screen.contains(" Providers "), "{a_screen}");
    }

    #[test]
    fn widening_clears_stale_modals_and_pagedown_ignores_when_providers_shown() {
        let (mut usage, mut analysis, mut settings) = seeded();
        let mut view = LAYOUT.create();

        let _ = render(
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
        // Widen into split -> clears stale detail modal, `p` flips to Providers.
        let _ = render(
            &mut view,
            100,
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
                metrics: (50.0, 0.2),
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
        let shrunk = render(
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
    fn narrow_provider_legend_reserves_plus_n_and_short_split_avoids_title_collision() {
        let mut pt: ProviderTotals<ProviderStats> = ProviderTotals::default();
        pt.antigravity.total_cost = 123.45;
        pt.antigravity.days_count = 3;
        pt.claude.total_cost = 50.0;
        pt.claude.days_count = 2;
        pt.codex.total_cost = 10.0;
        pt.codex.days_count = 1;
        let rows = build_provider_total_rows(&pt);
        let line = compact_provider_legend(&rows, 20);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.chars().count() <= 20,
            "legend exceeded max_w: {text:?}"
        );
        assert!(text.ends_with(" +2"), "missing +2 count: {text:?}");

        let (mut usage, mut analysis, mut settings) = seeded();
        let mut view = LAYOUT.create();
        let lines = render(
            &mut view,
            96,
            16,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        assert!(
            lines[1].contains("Models · $18.40") && lines[1].contains("sort: cost"),
            "short split table title collided:\n{}",
            lines.join("\n")
        );
    }
}
