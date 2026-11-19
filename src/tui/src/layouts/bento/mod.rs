//! `bento` layout: `Models` table alongside a clickable horizontal bar chart (`Spend Distribution` on Usage, `Tool Mix` on Analysis), over a 3-column `Selected` inspector strip.

use crate::display::analysis::{
    AnalysisSort, build_analysis_detail_sections, build_analysis_table_row, layout_analysis_columns,
};
use crate::display::app::Tab;
use crate::display::common::ProviderTotal;
use crate::display::common::table::{
    TableChrome, create_controls_with_status, header_column_at, render_detail_panel,
    render_scrollable_table_with_border, render_too_small, table_body_row_at, title_line,
};
use crate::display::common::theme::theme;
use crate::display::common::tui::{MouseInput, NavDelta};
use crate::display::usage::{
    ProviderStats, build_provider_total_rows, build_usage_detail_sections, build_usage_table_row,
    layout_usage_columns, provider_digest, render_provider_panel,
};
use crate::layouts::{
    EventResult, LayoutContext, LayoutMeta, LayoutView, TopBarHits, TuiLayout, bar_spans,
    build_analysis_strip_columns, build_usage_strip_columns, contains, fit_table_title,
    handle_settings_mouse, handle_top_bar_click, render_standard_non_data, render_strip_pane,
    render_top_bar, truncate_ellipsis,
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
pub(crate) static LAYOUT: BentoPlugin = BentoPlugin;

const CHART_SPLIT_MIN_W: u16 = 96;
const STRIP_MIN_WORK_H: u16 = 19;
const STRIP_H: u16 = 7;

pub(crate) struct BentoPlugin;

impl TuiLayout for BentoPlugin {
    fn meta(&self) -> LayoutMeta {
        LayoutMeta {
            id: "bento",
            title: "Bento",
            summary: "Models table + visual bar chart top row with a 3-column bottom inspector",
        }
    }

    fn create(&self) -> Box<dyn LayoutView> {
        Box::new(BentoView::default())
    }
}

#[derive(Default)]
struct BentoView {
    top_bar: TopBarHits,
    detail_modal_open: bool,
    provider_modal_open: bool,
    strip_visible: bool,
    table_area: Option<Rect>,
    digest_area: Option<Rect>,
    chart_model_hits: Vec<(Rect, usize)>,
    chart_provider_area: Option<Rect>,
    chart_tool_hits: Vec<(Rect, AnalysisSort)>,
}

impl BentoView {
    fn draw_usage(&mut self, f: &mut Frame, area: Rect, ctx: &mut LayoutContext<'_>) {
        if area.width < 74 || area.height < 14 {
            self.top_bar = TopBarHits::default();
            render_too_small(f, 74, 14);
            return;
        }
        if ctx.usage.selected_row().is_none() {
            self.detail_modal_open = false;
        }
        let [work, controls] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

        self.strip_visible = work.height >= STRIP_MIN_WORK_H;
        let (top_area, strip_area) = if self.strip_visible {
            let [top, strip] =
                Layout::vertical([Constraint::Min(8), Constraint::Length(STRIP_H)]).areas(work);
            (top, Some(strip))
        } else {
            (work, None)
        };

        let show_chart = top_area.width >= CHART_SPLIT_MIN_W;
        let (left_area, chart_area) = if show_chart {
            let chart_w = (top_area.width * 2 / 5).clamp(34, 52);
            let [l, r] = Layout::horizontal([Constraint::Min(56), Constraint::Length(chart_w)])
                .areas(top_area);
            (l, Some(r))
        } else {
            (top_area, None)
        };

        let table_area = if show_chart {
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
        let title = fit_table_title(
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
        );
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

        if let Some(chart) = chart_area {
            let (model_hits, prov_rect) = render_usage_chart_pane(f, chart, ctx, &provider_rows);
            self.chart_model_hits = model_hits;
            self.chart_provider_area = prov_rect;
        }

        if let Some(strip) = strip_area {
            if let Some(row) = ctx.usage.selected_row() {
                let strip_cols = build_usage_strip_columns(row, ctx.usage.totals());
                let max_title = usize::from(strip.width).saturating_sub(32);
                let heading = format!(
                    "Selected · {}",
                    truncate_ellipsis(&row.model, max_title.max(8))
                );
                render_strip_pane(
                    f,
                    strip,
                    title_line(&heading),
                    Some(Line::from(Span::styled(" ⏎ full details ", theme().dim()))),
                    false,
                    &strip_cols,
                );
            } else {
                render_empty_strip(f, strip);
            }
        }

        f.render_widget(
            create_controls_with_status(
                &[
                    ("↑↓", "move"),
                    ("←→", "sort"),
                    ("[ ]", "range"),
                    ("p", "providers"),
                    ("⏎", "details"),
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
        if ctx.analysis.selected_row().is_none() {
            self.detail_modal_open = false;
        }
        let [work, controls] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

        self.strip_visible = work.height >= STRIP_MIN_WORK_H;
        let (top_area, strip_area) = if self.strip_visible {
            let [top, strip] =
                Layout::vertical([Constraint::Min(8), Constraint::Length(STRIP_H)]).areas(work);
            (top, Some(strip))
        } else {
            (work, None)
        };

        let show_chart = top_area.width >= CHART_SPLIT_MIN_W;
        let (table_area, chart_area) = if show_chart {
            let chart_w = (top_area.width * 2 / 5).clamp(32, 48);
            let [l, r] = Layout::horizontal([Constraint::Min(60), Constraint::Length(chart_w)])
                .areas(top_area);
            (l, Some(r))
        } else {
            (top_area, None)
        };
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
        let sorted = cols.sorted_header_index(sort);
        let dropped = cols.dropped;
        let note = if dropped > 0 {
            format!("{} · −{dropped} cols", sort.label())
        } else {
            sort.label().to_string()
        };
        let title = fit_table_title(
            format!(
                "Models · {} lines · {} calls · {row_count} models",
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
        );
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

        if let Some(chart) = chart_area {
            self.chart_tool_hits = render_analysis_tool_chart(f, chart, ctx);
        }

        if let Some(strip) = strip_area {
            if let Some(row) = ctx.analysis.selected_row() {
                let strip_cols = build_analysis_strip_columns(row, ctx.analysis.totals());
                let max_title = usize::from(strip.width).saturating_sub(32);
                let heading = format!(
                    "Selected · {}",
                    truncate_ellipsis(&row.model, max_title.max(8))
                );
                render_strip_pane(
                    f,
                    strip,
                    title_line(&heading),
                    Some(Line::from(Span::styled(" ⏎ full details ", theme().dim()))),
                    false,
                    &strip_cols,
                );
            } else {
                render_empty_strip(f, strip);
            }
        }

        f.render_widget(
            create_controls_with_status(
                &[
                    ("↑↓", "move"),
                    ("←→", "sort"),
                    ("[ ]", "range"),
                    ("⏎", "details"),
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

fn render_empty_strip(f: &mut Frame, area: Rect) {
    let theme = theme();
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(" No model selected", theme.dim()))).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.border())
                .title(title_line("Selected")),
        ),
        area,
    );
}

fn render_usage_chart_pane(
    f: &mut Frame,
    area: Rect,
    ctx: &LayoutContext<'_>,
    provider_rows: &[ProviderTotal<'_, ProviderStats>],
) -> (Vec<(Rect, usize)>, Option<Rect>) {
    let theme = theme();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border())
        .title(title_line("Spend Distribution"))
        .title(Line::from(Span::styled(" click bar ", theme.dim())).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 16 || inner.height == 0 {
        return (Vec::new(), None);
    }

    let padded = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let rows = ctx.usage.rows();
    let selected = ctx.usage.selected_index();
    let mut listed_provs: Vec<_> = provider_rows
        .iter()
        .filter(|r| r.label != "All Providers")
        .collect();
    listed_provs.sort_by(|a, b| b.stats.total_cost.total_cmp(&a.stats.total_cost));

    let show_provs = !listed_provs.is_empty() && padded.height >= 9;
    let prov_reserve = if show_provs {
        (listed_provs.len().min(4) + 2).min(usize::from(padded.height) / 2)
    } else {
        0
    };
    let model_slots = usize::from(padded.height).saturating_sub(prov_reserve);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(usize::from(padded.height));
    let mut hits: Vec<(Rect, usize)> = Vec::new();
    let w = usize::from(padded.width);
    let val_w = 8usize;
    let lbl_w = (w / 3).clamp(8, 16);
    let bar_w = w.saturating_sub(lbl_w + val_w + 2).max(4);

    if rows.is_empty() {
        lines.push(Line::from(Span::styled("No models in range", theme.dim())));
    } else {
        let max_cost = rows.iter().map(|r| r.cost).fold(0.0_f64, f64::max);
        let max_tok = rows.iter().map(|r| r.total).max().unwrap_or(0);
        let hidden = if rows.len() > model_slots {
            rows.len() - model_slots.saturating_sub(1)
        } else {
            0
        };
        let shown = rows.len() - hidden;
        for (idx, row) in rows.iter().take(shown).enumerate() {
            let ratio = if max_cost > 0.0 {
                row.cost / max_cost
            } else if max_tok > 0 {
                row.total as f64 / max_tok as f64
            } else {
                0.0
            };
            let is_sel = selected == Some(idx);
            let name = truncate_ellipsis(&row.model, lbl_w);
            let value = format_cost_compact(row.cost);
            let [filled, empty] = bar_spans(ratio, bar_w, '█', '░', theme.accent());
            let mut line = Line::from(vec![
                Span::styled(
                    format!("{name:<lbl_w$} "),
                    if is_sel {
                        Style::default().bold()
                    } else {
                        Style::default()
                    },
                ),
                filled,
                empty,
                Span::styled(format!(" {value:>val_w$}"), theme.dim()),
            ]);
            if is_sel {
                line = line.style(theme.selected());
            }
            let y = padded.y + lines.len() as u16;
            hits.push((Rect::new(padded.x, y, padded.width, 1), idx));
            lines.push(line);
        }
        if hidden > 0 {
            lines.push(Line::from(Span::styled(
                format!("+{hidden} more models"),
                theme.dim(),
            )));
        }
    }

    let mut prov_rect = None;
    if show_provs && lines.len() + 2 <= usize::from(padded.height) {
        let start_y = padded.y + lines.len() as u16 + 1;
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled("Providers", theme.accent().bold()),
            Span::styled(" (p)", theme.dim()),
        ]));
        let avail = usize::from(padded.height).saturating_sub(lines.len());
        let hidden = if listed_provs.len() > avail {
            listed_provs.len() - avail.saturating_sub(1)
        } else {
            0
        };
        let shown = listed_provs.len() - hidden;
        for row in listed_provs.iter().take(shown) {
            let val = format_cost_compact(row.stats.total_cost);
            let lbl = truncate_ellipsis(row.label, w.saturating_sub(2 + 1 + val.len()));
            let gap = w.saturating_sub(2 + lbl.chars().count() + val.len());
            lines.push(Line::from(vec![
                Span::styled("● ", Style::default().fg(row.tui_color)),
                Span::raw(lbl),
                Span::raw(" ".repeat(gap)),
                Span::raw(val),
            ]));
        }
        if hidden > 0 {
            lines.push(Line::from(Span::styled(
                format!("+{hidden} more → p"),
                theme.dim(),
            )));
        }
        let end_y = padded.y + lines.len() as u16;
        prov_rect = Some(Rect::new(
            padded.x,
            start_y,
            padded.width,
            end_y.saturating_sub(start_y),
        ));
    }

    f.render_widget(Paragraph::new(lines), padded);
    (hits, prov_rect)
}

fn render_analysis_tool_chart(
    f: &mut Frame,
    area: Rect,
    ctx: &LayoutContext<'_>,
) -> Vec<(Rect, AnalysisSort)> {
    let theme = theme();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border())
        .title(title_line("Tool Mix"))
        .title(Line::from(Span::styled(" click to sort ", theme.dim())).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 16 || inner.height == 0 {
        return Vec::new();
    }

    let padded = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let totals = ctx.analysis.totals();
    let active_sort = ctx.analysis.sort();
    let tools = [
        ("Read", totals.read_count, AnalysisSort::Read),
        ("Bash", totals.bash_count, AnalysisSort::Bash),
        ("Edit", totals.edit_count, AnalysisSort::Edit),
        ("Write", totals.write_count, AnalysisSort::Write),
        ("Todo", totals.todo_write_count, AnalysisSort::TodoWrite),
    ];
    let max_calls = tools.iter().map(|(_, c, _)| *c).max().unwrap_or(0);

    let w = usize::from(padded.width);
    let lbl_w = 7usize;
    let val_w = 7usize;
    let bar_w = w.saturating_sub(lbl_w + val_w + 2).max(4);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(usize::from(padded.height));
    let mut hits: Vec<(Rect, AnalysisSort)> = Vec::new();
    for (name, count, sort) in tools {
        if lines.len() >= usize::from(padded.height) {
            break;
        }
        let ratio = if max_calls > 0 {
            count as f64 / max_calls as f64
        } else {
            0.0
        };
        let is_active = active_sort == sort;
        let [filled, empty] = bar_spans(ratio, bar_w, '█', '░', theme.accent());
        let mut line = Line::from(vec![
            Span::styled(
                format!("{name:<lbl_w$} "),
                if is_active {
                    theme.accent().bold()
                } else {
                    Style::default()
                },
            ),
            filled,
            empty,
            Span::styled(
                format!(" {:>val_w$}", format_compact(count as i64)),
                theme.dim(),
            ),
        ]);
        if is_active {
            line = line.style(theme.selected());
        }
        let y = padded.y + lines.len() as u16;
        hits.push((Rect::new(padded.x, y, padded.width, 1), sort));
        lines.push(line);
    }

    if lines.len() + 5 <= usize::from(padded.height) {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("Lines Mix", theme.accent().bold())));
        let line_buckets = [
            ("Read L", totals.read_lines, AnalysisSort::ReadLines),
            ("Edit L", totals.edit_lines, AnalysisSort::EditLines),
            ("Write L", totals.write_lines, AnalysisSort::WriteLines),
        ];
        let max_lines = line_buckets.iter().map(|(_, l, _)| *l).max().unwrap_or(0);
        for (name, count, sort) in line_buckets {
            if lines.len() >= usize::from(padded.height) {
                break;
            }
            let ratio = if max_lines > 0 {
                count as f64 / max_lines as f64
            } else {
                0.0
            };
            let is_active = active_sort == sort;
            let [filled, empty] =
                bar_spans(ratio, bar_w, '█', '░', Style::default().fg(theme.good));
            let mut line = Line::from(vec![
                Span::styled(
                    format!("{name:<lbl_w$} "),
                    if is_active {
                        theme.accent().bold()
                    } else {
                        Style::default()
                    },
                ),
                filled,
                empty,
                Span::styled(
                    format!(" {:>val_w$}", format_compact(count as i64)),
                    theme.dim(),
                ),
            ]);
            if is_active {
                line = line.style(theme.selected());
            }
            let y = padded.y + lines.len() as u16;
            hits.push((Rect::new(padded.x, y, padded.width, 1), sort));
            lines.push(line);
        }
    }

    f.render_widget(Paragraph::new(lines), padded);
    hits
}

impl LayoutView for BentoView {
    fn draw(&mut self, f: &mut Frame, ctx: &mut LayoutContext<'_>) {
        self.table_area = None;
        self.digest_area = None;
        self.chart_model_hits.clear();
        self.chart_provider_area = None;
        self.chart_tool_hits.clear();
        self.strip_visible = false;

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

                if ctx.tab == Tab::Usage {
                    if let Some(prov) = self.chart_provider_area
                        && contains(prov, column, row)
                    {
                        self.provider_modal_open = !self.provider_modal_open;
                        return EventResult::consumed();
                    }
                    if let Some((_, idx)) = self
                        .chart_model_hits
                        .iter()
                        .find(|(rect, _)| contains(*rect, column, row))
                    {
                        let same = ctx.usage.selected_index() == Some(*idx);
                        ctx.usage.select_index(*idx);
                        if same && !self.strip_visible {
                            self.detail_modal_open = true;
                        }
                        return EventResult::consumed();
                    }
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
                            if same {
                                self.detail_modal_open = true;
                            }
                            return EventResult::consumed();
                        }
                    }
                } else if ctx.tab == Tab::Analysis {
                    if let Some((_, sort)) = self
                        .chart_tool_hits
                        .iter()
                        .find(|(rect, _)| contains(*rect, column, row))
                    {
                        ctx.analysis.set_sort(*sort);
                        return EventResult::consumed();
                    }
                    if let Some(table) = self.table_area {
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
                            if same {
                                self.detail_modal_open = true;
                            }
                            return EventResult::consumed();
                        }
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
                }
                EventResult::consumed()
            }
            KeyCode::Enter if ctx.tab.scans() && !ctx.loading => {
                let has_sel = if ctx.tab == Tab::Usage {
                    ctx.usage.selected_row().is_some()
                } else {
                    ctx.analysis.selected_row().is_some()
                };
                self.detail_modal_open = has_sel && !self.detail_modal_open;
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
            ("↑ ↓ / wheel", "select a model (live in bottom strip)"),
            ("PgUp PgDn", "move up or down by 8 models"),
            ("⏎ / re-click", "open full model Details popup"),
            ("click header", "sort by the clicked column"),
            ("← →", "sort by the column to the left or right"),
            ("[ ] / click", "change time range"),
            ("r", "scan again now"),
        ];
        if tab == Tab::Usage {
            rows.push(("click bar", "select model in Spend Distribution"));
            rows.push(("p", "per-provider totals"));
        } else {
            rows.push(("click bar", "sort by tool or line metric"));
        }
        rows.extend([
            ("tab", "next tab, shift+tab for the previous one"),
            ("esc", "close a panel"),
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
        let row_a = UsageRow {
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
        let row_b = UsageRow {
            model: "gpt-5.5".to_string(),
            input_tokens: 90_000,
            output_tokens: 20_000,
            total: 110_000,
            cost: 3.10,
            providers: vec![ModelProviderUsage {
                provider: Provider::Codex,
                total_tokens: 110_000,
                cost: 3.10,
            }],
            ..UsageRow::default()
        };
        let mut totals = UsageTotals::default();
        totals.accumulate(&row_a);
        totals.accumulate(&row_b);
        let mut pt: ProviderTotals<ProviderStats> = ProviderTotals::default();
        pt.claude.total_tokens = 930_000;
        pt.claude.total_cost = 16.20;
        pt.claude.days_count = 2;
        pt.codex.total_tokens = 110_000;
        pt.codex.total_cost = 3.10;
        pt.codex.days_count = 1;
        usage.apply(UsageRefreshPayload {
            rows: vec![row_a.clone(), row_b.clone()],
            merged_rows: vec![row_a, row_b],
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
    fn renders_chart_and_bottom_strip_and_clicking_bar_selects_model() {
        let (mut usage, mut analysis, mut settings) = seeded();
        let mut view = LAYOUT.create();

        let lines = render(
            &mut view,
            112,
            26,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        let screen = lines.join("\n");
        assert!(screen.contains("Spend Distribution"), "{screen}");
        assert!(screen.contains("Selected · claude-opus-4-8"), "{screen}");
        assert!(screen.contains("Efficiency & Share"), "{screen}");
        assert!(lines.last().unwrap().contains("Star on GitHub"), "{screen}");

        // Click gpt-5.5 in the right-side Spend Distribution chart.
        let chart_row = lines
            .iter()
            .position(|l| l.contains("gpt-5.5") && l.contains('█'))
            .unwrap() as u16;
        let chart_byte = lines[chart_row as usize].rfind("gpt-5.5").unwrap();
        let chart_x = lines[chart_row as usize][..chart_byte].chars().count() as u16;
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
                    column: chart_x + 1,
                    row: chart_row,
                },
                &mut ctx,
            );
        }
        assert_eq!(usage.selected_row().unwrap().model, "gpt-5.5");

        // Analysis renders Tool Mix without a Providers band; clicking a bar sorts by that tool.
        let a_lines = render(
            &mut view,
            112,
            26,
            Tab::Analysis,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        let a_screen = a_lines.join("\n");
        assert!(a_screen.contains("Tool Mix"), "{a_screen}");
        assert!(!a_screen.contains("All Providers"), "{a_screen}");
        let bash_y = a_lines
            .iter()
            .position(|l| l.contains("Bash") && l.contains('░'))
            .unwrap() as u16;
        let bash_byte = a_lines[bash_y as usize].rfind("Bash").unwrap();
        let bash_x = a_lines[bash_y as usize][..bash_byte].chars().count() as u16;
        {
            let mut ctx = LayoutContext {
                tab: Tab::Analysis,
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
                    column: bash_x + 1,
                    row: bash_y,
                },
                &mut ctx,
            );
        }
        assert_eq!(analysis.sort(), AnalysisSort::Bash);
    }

    #[test]
    fn narrow_split_table_and_long_model_strip_titles_never_collide() {
        let mut usage = UsageUiState::new(true);
        let row = UsageRow {
            model: "very-long-model-name-that-easily-exceeds-fifty-five-characters-total"
                .to_string(),
            input_tokens: 100_000,
            output_tokens: 30_000,
            total: 130_000,
            cost: 16.20,
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
        assert!(
            lines[1].contains("Models · $16.20") && lines[1].contains("sort: cost"),
            "table title collided with sort note:\n{screen}"
        );
        let strip_hdr = lines.iter().find(|l| l.contains("Selected ·")).unwrap();
        assert!(
            strip_hdr.contains("…") && strip_hdr.contains("⏎ full details"),
            "strip title overlapped right hint:\n{strip_hdr}"
        );
    }
}
