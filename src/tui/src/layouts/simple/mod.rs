//! `simple` layout: collapsible left navigation rail paired with inline expandable tree rows (`▸` / `▾`) in the Usage and Analysis tables.

use crate::display::analysis::{
    AnalysisRow, AnalysisTableColumns, build_analysis_table_row, layout_analysis_columns,
};
use crate::display::app::Tab;
use crate::display::common::provider_tui_color;
use crate::display::common::table::{
    TableChrome, create_controls_with_status, header_column_at,
    render_scrollable_table_with_border, render_too_small, table_body_row_at, title_line,
};
use crate::display::common::theme::theme;
use crate::display::common::tui::{MouseInput, NavDelta, ScrollState};
use crate::display::settings::SettingsOutcome;
use crate::display::usage::{
    UsageRow, UsageTableColumns, UsageTotals, build_provider_total_rows, build_usage_table_row,
    layout_usage_columns, provider_digest, render_provider_panel,
};
use crate::layouts::{
    EventResult, LayoutCommand, LayoutContext, LayoutMeta, LayoutView, RANGES, TopBarHits,
    TuiLayout, contains, render_standard_non_data, render_top_bar, truncate_ellipsis,
};
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Cell as RatatuiCell, Paragraph, Row as RatatuiRow},
};
use std::collections::HashSet;
use vct_core::models::TimeRange;
use vct_core::utils::{format_compact, format_cost};

/// Static plugin entry discovered by `src/tui/build.rs`.
pub(crate) static LAYOUT: SimplePlugin = SimplePlugin;

const RAIL_W: u16 = 20;
const RAIL_MIN_FRAME_W: u16 = 104;
const RAIL_MIN_FRAME_H: u16 = 16;

pub(crate) struct SimplePlugin;

impl TuiLayout for SimplePlugin {
    fn meta(&self) -> LayoutMeta {
        LayoutMeta {
            id: "simple",
            title: "Simple",
            summary: "Left navigation rail with inline expandable model tree rows",
        }
    }

    fn create(&self) -> Box<dyn LayoutView> {
        Box::new(SimpleView::default())
    }
}

#[derive(Default)]
struct SimpleView {
    rail_active: bool,
    top_bar: TopBarHits,
    rail_tabs: [(Tab, Option<Rect>); 4],
    rail_ranges: [(TimeRange, Option<Rect>); 4],
    expanded_usage: HashSet<String>,
    expanded_analysis: HashSet<String>,
    usage_visual_scroll: ScrollState,
    analysis_visual_scroll: ScrollState,
    visual_to_model: Vec<usize>,
    table_area: Option<Rect>,
    digest_area: Option<Rect>,
    provider_modal_open: bool,
}

impl SimpleView {
    fn draw_rail(&mut self, f: &mut Frame, area: Rect, ctx: &LayoutContext<'_>) {
        self.rail_tabs = Tab::ALL.map(|tab| (tab, None));
        self.rail_ranges = RANGES.map(|(r, _)| (r, None));

        let theme = theme();
        let dim = theme.dim();
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border())
            .title(title_line("vct"));
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut y = inner.y;

        lines.push(Line::from(Span::styled(" Views", dim.bold())));
        y += 1;

        for (idx, tab) in Tab::ALL.into_iter().enumerate() {
            if usize::from(y - inner.y) >= usize::from(inner.height) {
                break;
            }
            self.rail_tabs[idx].1 = Some(Rect::new(inner.x, y, inner.width, 1));
            let line = if tab == ctx.tab {
                let pad = usize::from(inner.width).saturating_sub(tab.label().chars().count() + 3);
                Line::from(Span::styled(
                    format!(" ▸ {}{}", tab.label(), " ".repeat(pad)),
                    theme.active(),
                ))
            } else {
                Line::from(Span::raw(format!("   {}", tab.label())))
            };
            lines.push(line);
            y += 1;
        }

        if ctx.tab.scans() && usize::from(y - inner.y) + 6 <= usize::from(inner.height) {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(" Range", dim.bold())));
            y += 2;
            for (idx, (candidate, label)) in RANGES.into_iter().enumerate() {
                if usize::from(y - inner.y) >= usize::from(inner.height) {
                    break;
                }
                self.rail_ranges[idx].1 = Some(Rect::new(inner.x, y, inner.width, 1));
                let line = if candidate == ctx.time_range {
                    Line::from(vec![
                        Span::styled(" • ", theme.accent().bold()),
                        Span::styled(label, Style::default().bold()),
                    ])
                } else {
                    Line::from(Span::styled(format!("   {label}"), dim))
                };
                lines.push(line);
                y += 1;
            }
        }

        let used = lines.len();
        let avail = usize::from(inner.height);
        if avail >= used + 3 {
            let (mem_mb, cpu) = ctx.metrics;
            for _ in 0..(avail - used - 2) {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(
                format!(" MEM {mem_mb:.1} MB"),
                dim,
            )));
            lines.push(Line::from(Span::styled(format!(" CPU {cpu:.1}%"), dim)));
        }

        f.render_widget(Paragraph::new(lines), inner);
    }

    fn hit_rail_tab(&self, column: u16, row: u16) -> Option<Tab> {
        self.rail_tabs
            .iter()
            .find_map(|(tab, rect)| rect.filter(|r| contains(*r, column, row)).map(|_| *tab))
    }

    fn hit_rail_range(&self, column: u16, row: u16) -> Option<TimeRange> {
        self.rail_ranges
            .iter()
            .find_map(|(range, rect)| rect.filter(|r| contains(*r, column, row)).map(|_| *range))
    }

    fn toggle_selected_row(&mut self, ctx: &LayoutContext<'_>) {
        if ctx.tab == Tab::Usage {
            if let Some(row) = ctx.usage.selected_row() {
                toggle_set(&mut self.expanded_usage, &row.model);
            }
        } else if ctx.tab == Tab::Analysis
            && let Some(row) = ctx.analysis.selected_row()
        {
            toggle_set(&mut self.expanded_analysis, &row.model);
        }
    }

    fn draw_usage(
        &mut self,
        f: &mut Frame,
        work_area: Rect,
        controls_area: Rect,
        ctx: &mut LayoutContext<'_>,
    ) {
        if work_area.width < 74 || f.area().height < 15 {
            render_too_small(f, 74, 15);
            return;
        }
        let provider_rows = build_provider_total_rows(ctx.usage.provider_totals());
        let [table_area, band_area] =
            Layout::vertical([Constraint::Min(6), Constraint::Length(1)]).areas(work_area);
        self.table_area = Some(table_area);
        self.digest_area = Some(band_area);

        let cols = layout_usage_columns(table_area.width.saturating_sub(2));
        let totals = ctx.usage.totals();
        let mut visual_rows: Vec<RatatuiRow> = Vec::new();
        let mut model_to_visual: Vec<usize> = Vec::with_capacity(ctx.usage.rows().len());
        self.visual_to_model.clear();

        for (idx, row) in ctx.usage.rows().iter().enumerate() {
            let open = self.expanded_usage.contains(&row.model);
            let prefix = if open { "▾ " } else { "▸ " };
            model_to_visual.push(visual_rows.len());
            self.visual_to_model.push(idx);
            visual_rows.push(build_usage_table_row(
                row,
                totals.total,
                &cols,
                prefix,
                ctx.usage.update_tracker(),
            ));
            if open {
                for child in build_usage_children(row, totals, &cols) {
                    self.visual_to_model.push(idx);
                    visual_rows.push(child);
                }
            }
        }

        sync_tree_viewport(
            &mut self.usage_visual_scroll,
            ctx.usage.selected_index(),
            &model_to_visual,
            visual_rows.len(),
            usize::from(table_area.height.saturating_sub(4)),
        );

        let visual_count = visual_rows.len();
        let title = format!(
            "Models · {} · {} tokens · {} models",
            format_cost(totals.cost),
            format_compact(totals.total),
            ctx.usage.rows().len()
        );
        let sort = ctx.usage.sort();
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
            visual_rows,
            &cols.widths,
            visual_count,
            &mut self.usage_visual_scroll,
            BorderType::Rounded,
        );

        f.render_widget(
            Paragraph::new(provider_digest(&provider_rows, band_area.width)).centered(),
            band_area,
        );

        f.render_widget(
            create_controls_with_status(
                &[
                    ("↑↓", "move"),
                    ("⏎", "expand"),
                    ("←→", "sort"),
                    ("[ ]", "range"),
                    ("p", "providers"),
                    ("r", "refresh"),
                ],
                ctx.data_status,
                controls_area.width,
            ),
            controls_area,
        );

        if self.provider_modal_open {
            render_provider_panel(f, work_area, &provider_rows);
        }
    }

    fn draw_analysis(
        &mut self,
        f: &mut Frame,
        work_area: Rect,
        controls_area: Rect,
        ctx: &mut LayoutContext<'_>,
    ) {
        if work_area.width < 84 || f.area().height < 15 {
            render_too_small(f, 84, 15);
            return;
        }
        let table_area = work_area;
        self.table_area = Some(table_area);

        let cols = layout_analysis_columns(table_area.width.saturating_sub(2));
        let totals = ctx.analysis.totals();
        let mut visual_rows: Vec<RatatuiRow> = Vec::new();
        let mut model_to_visual: Vec<usize> = Vec::with_capacity(ctx.analysis.rows().len());
        self.visual_to_model.clear();

        for (idx, row) in ctx.analysis.rows().iter().enumerate() {
            let open = self.expanded_analysis.contains(&row.model);
            let prefix = if open { "▾ " } else { "▸ " };
            model_to_visual.push(visual_rows.len());
            self.visual_to_model.push(idx);
            visual_rows.push(build_analysis_table_row(
                row,
                &cols,
                prefix,
                ctx.analysis.update_tracker(),
            ));
            if open {
                for child in build_analysis_children(row, totals, &cols) {
                    self.visual_to_model.push(idx);
                    visual_rows.push(child);
                }
            }
        }

        sync_tree_viewport(
            &mut self.analysis_visual_scroll,
            ctx.analysis.selected_index(),
            &model_to_visual,
            visual_rows.len(),
            usize::from(table_area.height.saturating_sub(4)),
        );

        let visual_count = visual_rows.len();
        let title = format!(
            "Models · {} lines · {} tool calls · {} models",
            format_compact(totals.total_lines() as i64),
            format_compact(totals.total_calls() as i64),
            ctx.analysis.rows().len()
        );
        let sort = ctx.analysis.sort();
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
            visual_rows,
            &cols.widths,
            visual_count,
            &mut self.analysis_visual_scroll,
            BorderType::Rounded,
        );

        f.render_widget(
            create_controls_with_status(
                &[
                    ("↑↓", "move"),
                    ("⏎", "expand"),
                    ("←→", "sort"),
                    ("[ ]", "range"),
                    ("r", "refresh"),
                ],
                ctx.data_status,
                controls_area.width,
            ),
            controls_area,
        );
    }
}

fn toggle_set(set: &mut HashSet<String>, key: &str) {
    if !set.remove(key) {
        set.insert(key.to_string());
    }
}

fn pack_fact_lines(facts: &[String], max_chars: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for fact in facts {
        let fitted = truncate_ellipsis(fact, max_chars);
        if let Some(last) = lines.last_mut()
            && last.chars().count() + 3 + fitted.chars().count() <= max_chars
        {
            last.push_str(" · ");
            last.push_str(&fitted);
        } else {
            lines.push(fitted);
        }
    }
    lines
}

fn sync_tree_viewport(
    scroll: &mut ScrollState,
    selected_model: Option<usize>,
    model_to_visual: &[usize],
    total_visual: usize,
    viewport: usize,
) {
    let Some(m) = selected_model.filter(|&m| m < model_to_visual.len()) else {
        scroll.table.select(None);
        return;
    };
    let vis_start = model_to_visual[m];
    let vis_end = model_to_visual
        .get(m + 1)
        .copied()
        .unwrap_or(total_visual)
        .saturating_sub(1);
    scroll.table.select(Some(vis_start));
    if viewport == 0 {
        return;
    }
    let offset = scroll.table.offset_mut();
    if *offset > vis_start {
        *offset = vis_start;
    } else if vis_end >= *offset + viewport {
        *offset = (vis_end + 1 - viewport).min(vis_start);
    }
}

fn build_usage_children(
    row: &UsageRow,
    totals: &UsageTotals,
    cols: &UsageTableColumns,
) -> Vec<RatatuiRow<'static>> {
    let theme = theme();
    let dim = theme.dim();
    let col_count = cols.header.len();
    let total_col = cols.kept.iter().position(|&i| i == 4).map(|at| at + 1);
    let cost_col = cols.kept.iter().position(|&i| i == 5).map(|at| at + 1);
    let share_col = (cols.share_w > 0).then_some(col_count - 1);

    let mut facts = Vec::new();
    if row.total > 0 && row.cost > 0.0 {
        let rate = row.cost / (row.total as f64 / 1_000_000.0);
        facts.push(format!("eff ${rate:.2}/1M"));
    }
    if totals.cost > 0.0 {
        facts.push(format!("cost {:.1}%", row.cost / totals.cost * 100.0));
    }
    let prompt_total = row.input_tokens + row.cache_read + row.cache_creation;
    if row.cache_read > 0 && prompt_total > 0 {
        facts.push(format!(
            "hit {:.0}%",
            row.cache_read as f64 / prompt_total as f64 * 100.0
        ));
    }
    if row.reasoning_tokens > 0 {
        facts.push(format!("reason {}", format_compact(row.reasoning_tokens)));
    }
    if row.cache_creation_1h > 0 {
        facts.push(format!("1h {}", format_compact(row.cache_creation_1h)));
    }
    if let Some(matched) = &row.matched_model {
        facts.push(format!("as {matched}"));
    }
    let avail = usize::from(cols.model_w).saturating_sub(5).max(8);
    let fact_lines = pack_fact_lines(&facts, avail);

    let mut out = Vec::new();
    for (idx, entry) in row.providers.iter().enumerate() {
        let is_last = idx + 1 == row.providers.len() && fact_lines.is_empty();
        let branch = if is_last { "  └─ " } else { "  ├─ " };
        let mut cells: Vec<RatatuiCell<'static>> =
            (0..col_count).map(|_| RatatuiCell::from("")).collect();

        let inline_suffix = if total_col.is_none() && cost_col.is_none() {
            format!(
                " ({} · {})",
                format_compact(entry.total_tokens),
                format_cost(entry.cost)
            )
        } else {
            String::new()
        };
        cells[0] = RatatuiCell::from(Line::from(vec![
            Span::styled(branch, dim),
            Span::styled(
                "● ",
                Style::default().fg(provider_tui_color(entry.provider)),
            ),
            Span::styled(
                format!("{}{inline_suffix}", entry.provider.display_name()),
                dim,
            ),
        ]));
        if let Some(c) = total_col {
            cells[c] = RatatuiCell::from(
                Text::from(format_compact(entry.total_tokens)).alignment(Alignment::Right),
            )
            .style(dim);
        }
        if let Some(c) = cost_col {
            cells[c] =
                RatatuiCell::from(Text::from(format_cost(entry.cost)).alignment(Alignment::Right))
                    .style(dim);
        }
        if let Some(c) = share_col
            && row.total > 0
        {
            let pct = entry.total_tokens as f64 / row.total as f64 * 100.0;
            cells[c] =
                RatatuiCell::from(Text::from(format!("{pct:.0}%")).alignment(Alignment::Right))
                    .style(dim);
        }
        out.push(RatatuiRow::new(cells));
    }

    for (idx, line) in fact_lines.iter().enumerate() {
        let branch = if idx + 1 == fact_lines.len() {
            "  └─ "
        } else {
            "  ├─ "
        };
        let mut cells: Vec<RatatuiCell<'static>> =
            (0..col_count).map(|_| RatatuiCell::from("")).collect();
        cells[0] = RatatuiCell::from(Line::from(vec![
            Span::styled(branch, dim),
            Span::styled(line.clone(), dim),
        ]));
        out.push(RatatuiRow::new(cells));
    }

    out
}

fn build_analysis_children(
    row: &AnalysisRow,
    totals: &AnalysisRow,
    cols: &AnalysisTableColumns,
) -> Vec<RatatuiRow<'static>> {
    let dim = theme().dim();
    let col_count = cols.header.len();
    let mut facts = Vec::new();
    if totals.total_lines() > 0 {
        facts.push(format!(
            "lines {:.1}%",
            row.total_lines() as f64 / totals.total_lines() as f64 * 100.0
        ));
    }
    if row.edit_count > 0 && row.edit_lines > 0 {
        facts.push(format!("edit ~{}/op", row.edit_lines / row.edit_count));
    }
    if row.read_count > 0 && row.read_lines > 0 {
        facts.push(format!("read ~{}/op", row.read_lines / row.read_count));
    }
    let avail = usize::from(cols.model_w).saturating_sub(5).max(8);
    let fact_lines = pack_fact_lines(&facts, avail);

    let mut out = Vec::new();
    for (idx, entry) in row.providers.iter().enumerate() {
        let is_last = idx + 1 == row.providers.len() && fact_lines.is_empty();
        let branch = if is_last { "  └─ " } else { "  ├─ " };
        let mut cells: Vec<RatatuiCell<'static>> =
            (0..col_count).map(|_| RatatuiCell::from("")).collect();
        let inline = if col_count < 3 {
            format!(
                " ({}L · {}c)",
                format_compact(entry.total_lines as i64),
                format_compact(entry.total_calls as i64)
            )
        } else {
            String::new()
        };
        cells[0] = RatatuiCell::from(Line::from(vec![
            Span::styled(branch, dim),
            Span::styled(
                "● ",
                Style::default().fg(provider_tui_color(entry.provider)),
            ),
            Span::styled(format!("{}{inline}", entry.provider.display_name()), dim),
        ]));
        if col_count >= 3 {
            cells[1] = RatatuiCell::from(
                Text::from(format!("{}L", format_compact(entry.total_lines as i64)))
                    .alignment(Alignment::Right),
            )
            .style(dim);
            cells[2] = RatatuiCell::from(
                Text::from(format!("{}c", format_compact(entry.total_calls as i64)))
                    .alignment(Alignment::Right),
            )
            .style(dim);
        }
        out.push(RatatuiRow::new(cells));
    }

    for (idx, line) in fact_lines.iter().enumerate() {
        let branch = if idx + 1 == fact_lines.len() {
            "  └─ "
        } else {
            "  ├─ "
        };
        let mut cells: Vec<RatatuiCell<'static>> =
            (0..col_count).map(|_| RatatuiCell::from("")).collect();
        cells[0] = RatatuiCell::from(Line::from(vec![
            Span::styled(branch, dim),
            Span::styled(line.clone(), dim),
        ]));
        out.push(RatatuiRow::new(cells));
    }

    out
}

impl LayoutView for SimpleView {
    fn draw(&mut self, f: &mut Frame, ctx: &mut LayoutContext<'_>) {
        self.table_area = None;
        self.digest_area = None;
        let area = f.area();
        self.rail_active = area.width >= RAIL_MIN_FRAME_W && area.height >= RAIL_MIN_FRAME_H;

        if matches!(ctx.tab, Tab::Usage | Tab::Analysis) && !ctx.loading {
            let (work, controls) = if self.rail_active {
                let [upper, footer] =
                    Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);
                let [rail, main] =
                    Layout::horizontal([Constraint::Length(RAIL_W), Constraint::Min(0)])
                        .areas(upper);
                self.draw_rail(f, rail, ctx);
                (main, footer)
            } else {
                let [bar, main] =
                    Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
                let range = ctx.tab.scans().then_some(ctx.time_range);
                self.top_bar = render_top_bar(f, bar, ctx.tab, range, ctx.metrics);
                let [work, footer] =
                    Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(main);
                (work, footer)
            };
            match ctx.tab {
                Tab::Usage => self.draw_usage(f, work, controls, ctx),
                Tab::Analysis => self.draw_analysis(f, work, controls, ctx),
                _ => {}
            }
            return;
        }

        let body = if self.rail_active {
            let [rail, main] =
                Layout::horizontal([Constraint::Length(RAIL_W), Constraint::Min(0)]).areas(area);
            self.draw_rail(f, rail, ctx);
            main
        } else {
            let [bar, main] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
            let range = ctx.tab.scans().then_some(ctx.time_range);
            self.top_bar = render_top_bar(f, bar, ctx.tab, range, ctx.metrics);
            main
        };

        render_standard_non_data(f, body, ctx, BorderType::Rounded);
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
            if let MouseInput::Click { column, row } = mouse {
                let hit = if self.rail_active {
                    self.hit_rail_tab(column, row)
                } else {
                    self.top_bar.hit_tab(column, row)
                };
                if let Some(next) = hit {
                    return EventResult::Consumed(LayoutCommand {
                        switch_tab: Some(next),
                        ..LayoutCommand::default()
                    });
                }
            }
            let changed = ctx.settings.handle_mouse(mouse) == SettingsOutcome::Changed;
            return EventResult::Consumed(LayoutCommand {
                settings_changed: changed,
                ..LayoutCommand::default()
            });
        }

        match mouse {
            MouseInput::Click { column, row } => {
                if self.provider_modal_open {
                    self.provider_modal_open = false;
                    return EventResult::consumed();
                }
                let tab_hit = if self.rail_active {
                    self.hit_rail_tab(column, row)
                } else {
                    self.top_bar.hit_tab(column, row)
                };
                if let Some(next) = tab_hit {
                    return EventResult::Consumed(LayoutCommand {
                        switch_tab: Some(next),
                        ..LayoutCommand::default()
                    });
                }
                if ctx.tab.scans() {
                    let range_hit = if self.rail_active {
                        self.hit_rail_range(column, row)
                    } else {
                        self.top_bar.hit_range(column, row)
                    };
                    if let Some(range) = range_hit {
                        return EventResult::Consumed(LayoutCommand {
                            set_range: Some(range),
                            ..LayoutCommand::default()
                        });
                    }
                }
                if ctx.loading {
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
                        if let Some(vis_idx) = table_body_row_at(
                            table,
                            &self.usage_visual_scroll,
                            self.visual_to_model.len(),
                            column,
                            row,
                        ) && let Some(&model_idx) = self.visual_to_model.get(vis_idx)
                        {
                            ctx.usage.select_index(model_idx);
                            self.toggle_selected_row(ctx);
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
                    if let Some(vis_idx) = table_body_row_at(
                        table,
                        &self.analysis_visual_scroll,
                        self.visual_to_model.len(),
                        column,
                        row,
                    ) && let Some(&model_idx) = self.visual_to_model.get(vis_idx)
                    {
                        ctx.analysis.select_index(model_idx);
                        self.toggle_selected_row(ctx);
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
                if self.provider_modal_open {
                    self.provider_modal_open = false;
                } else if ctx.tab == Tab::Usage {
                    self.expanded_usage.clear();
                } else if ctx.tab == Tab::Analysis {
                    self.expanded_analysis.clear();
                }
                EventResult::consumed()
            }
            KeyCode::Enter | KeyCode::Char(' ') if ctx.tab.scans() && !ctx.loading => {
                self.toggle_selected_row(ctx);
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
        self.provider_modal_open = false;
    }

    fn help_rows(&self, tab: Tab) -> Option<Vec<(&'static str, &'static str)>> {
        if !tab.scans() {
            return None;
        }
        let mut rows = vec![
            ("↑ ↓ / wheel", "select a model"),
            ("PgUp PgDn", "move up or down by 8 models"),
            ("⏎ / space / click", "expand or collapse inline model tree"),
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
            ("esc", "collapse expanded rows or close a panel"),
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
    use vct_core::models::{Provider, ProviderTotals};

    fn seeded() -> (UsageUiState, AnalysisUiState, SettingsUiState) {
        let mut usage = UsageUiState::new(true);
        let row = UsageRow {
            model: "claude-sonnet-4-6".to_string(),
            input_tokens: 80_000,
            output_tokens: 20_000,
            reasoning_tokens: 5_000,
            cache_read: 400_000,
            total: 505_000,
            cost: 3.75,
            providers: vec![ModelProviderUsage {
                provider: Provider::ClaudeCode,
                total_tokens: 505_000,
                cost: 3.75,
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
        usage: &mut UsageUiState,
        analysis: &mut AnalysisUiState,
        settings: &mut SettingsUiState,
    ) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|f| {
                let mut ctx = LayoutContext {
                    tab: Tab::Usage,
                    time_range: TimeRange::Weekly,
                    loading: false,
                    retry_pending: false,
                    spinner: 0,
                    metrics: (52.0, 0.3),
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
    fn sidebar_renders_on_wide_screens_and_tree_rows_expand_inline() {
        let (mut usage, mut analysis, mut settings) = seeded();
        let mut view = LAYOUT.create();

        let collapsed = render(&mut view, 120, 24, &mut usage, &mut analysis, &mut settings);
        let text = collapsed.join("\n");
        assert!(text.contains("Views") && text.contains("Range"), "{text}");
        assert!(text.contains("▸ claude-sonnet-4-6"), "{text}");
        assert!(text.contains("Star on GitHub"), "{text}");

        // Click the model row to expand inline provider + summary children.
        let model_y = collapsed
            .iter()
            .position(|line| line.contains("▸ claude-sonnet-4-6"))
            .unwrap() as u16;
        let model_x = collapsed[model_y as usize]
            .find("▸ claude-sonnet-4-6")
            .unwrap() as u16;
        {
            let mut ctx = LayoutContext {
                tab: Tab::Usage,
                time_range: TimeRange::Weekly,
                loading: false,
                retry_pending: false,
                spinner: 0,
                metrics: (52.0, 0.3),
                data_status: None,
                update_status: None,
                usage: &mut usage,
                analysis: &mut analysis,
                quota: None,
                settings: &mut settings,
            };
            view.on_mouse(
                MouseInput::Click {
                    column: model_x + 2,
                    row: model_y,
                },
                &mut ctx,
            );
        }

        let expanded =
            render(&mut view, 120, 24, &mut usage, &mut analysis, &mut settings).join("\n");
        assert!(expanded.contains("▾ claude-sonnet-4-6"), "{expanded}");
        assert!(expanded.contains("├─ ● Claude"), "{expanded}");
        assert!(expanded.contains("reason 5.00K"), "{expanded}");

        // Narrow 80x24 collapses the left rail into the top bar without error.
        let narrow = render(&mut view, 80, 24, &mut usage, &mut analysis, &mut settings).join("\n");
        assert!(!narrow.contains("Views"), "{narrow}");
        assert!(narrow.contains("▾ claude-sonnet-4-6"), "{narrow}");
    }

    #[test]
    fn clicking_sidebar_switches_tabs_and_ranges() {
        let (mut usage, mut analysis, mut settings) = seeded();
        let mut view = LAYOUT.create();
        let lines = render(&mut view, 120, 24, &mut usage, &mut analysis, &mut settings);

        let analysis_y = lines
            .iter()
            .position(|line| line.contains("Analysis"))
            .unwrap() as u16;
        let month_y = lines
            .iter()
            .position(|line| line.contains("month"))
            .unwrap() as u16;

        let mut ctx = LayoutContext {
            tab: Tab::Usage,
            time_range: TimeRange::Weekly,
            loading: false,
            retry_pending: false,
            spinner: 0,
            metrics: (52.0, 0.3),
            data_status: None,
            update_status: None,
            usage: &mut usage,
            analysis: &mut analysis,
            quota: None,
            settings: &mut settings,
        };
        assert_eq!(
            view.on_mouse(
                MouseInput::Click {
                    column: 5,
                    row: analysis_y,
                },
                &mut ctx
            ),
            EventResult::Consumed(LayoutCommand {
                switch_tab: Some(Tab::Analysis),
                ..LayoutCommand::default()
            })
        );
        assert_eq!(
            view.on_mouse(
                MouseInput::Click {
                    column: 5,
                    row: month_y,
                },
                &mut ctx
            ),
            EventResult::Consumed(LayoutCommand {
                set_range: Some(TimeRange::Monthly),
                ..LayoutCommand::default()
            })
        );
    }

    #[test]
    fn analysis_tab_omits_bottom_providers_band() {
        let (mut usage, mut analysis, mut settings) = seeded();
        let mut view = LAYOUT.create();
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|f| {
                let mut ctx = LayoutContext {
                    tab: Tab::Analysis,
                    time_range: TimeRange::Weekly,
                    loading: false,
                    retry_pending: false,
                    spinner: 0,
                    metrics: (52.0, 0.3),
                    data_status: None,
                    update_status: None,
                    usage: &mut usage,
                    analysis: &mut analysis,
                    quota: None,
                    settings: &mut settings,
                };
                view.draw(f, &mut ctx);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let screen: String = (0..24u16)
            .map(|y| {
                (0..120u16)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("Models ·"), "{screen}");
        assert!(!screen.contains("All Providers"), "{screen}");
        assert!(!screen.contains(" Providers "), "{screen}");
    }
}
