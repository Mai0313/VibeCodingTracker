//! `editorial` layout: single unified outer rounded frame divided cleanly by
//! hairline rules (`─` and `│`) with an accent `▎` selection cursor and zero
//! nested box-in-box clutter.

use crate::display::analysis::{
    AnalysisRow, AnalysisTableColumns, build_analysis_detail_sections, layout_analysis_columns,
};
use crate::display::app::Tab;
use crate::display::common::ProviderTotal;
use crate::display::common::table::{
    aligned_cells, create_controls_with_status, create_provider_row, create_ratatui_table,
    format_detail_lines, render_detail_panel, render_too_small,
};
use crate::display::common::theme::theme;
use crate::display::common::tui::{MouseInput, NavDelta, ScrollState, UpdateTracker};
use crate::display::usage::{
    ProviderStats, UsageRow, UsageTableColumns, build_provider_total_rows,
    build_usage_detail_sections, layout_usage_columns, provider_digest, provider_share_bar,
    render_provider_panel,
};
use crate::layouts::{
    EventResult, LayoutContext, LayoutMeta, LayoutView, RANGES, TopBarHits, TuiLayout, contains,
    handle_settings_mouse, handle_top_bar_click, render_standard_non_data, render_top_bar_styled,
};
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Cell as RatatuiCell, Paragraph, Row as RatatuiRow, Scrollbar,
        ScrollbarOrientation, ScrollbarState,
    },
};
use vct_core::models::TimeRange;
use vct_core::utils::{format_compact, format_cost, format_cost_compact};

/// Static plugin entry discovered by `src/tui/build.rs`.
pub(crate) static LAYOUT: EditorialPlugin = EditorialPlugin;

const USAGE_SPLIT_MIN_INNER_W: u16 = 92;
const ANALYSIS_SPLIT_MIN_INNER_W: u16 = 96;

pub(crate) struct EditorialPlugin;

impl TuiLayout for EditorialPlugin {
    fn meta(&self) -> LayoutMeta {
        LayoutMeta {
            id: "editorial",
            title: "Editorial",
            summary: "Single outer rounded frame with hairline dividers and cyan selection cursor",
        }
    }

    fn create(&self) -> Box<dyn LayoutView> {
        Box::new(EditorialView::default())
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum SideMode {
    #[default]
    Inspector,
    Providers,
}

#[derive(Default)]
struct EditorialView {
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
    side_header_area: Option<Rect>,
    digest_area: Option<Rect>,
}

impl EditorialView {
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

    fn draw_data_frame(&mut self, f: &mut Frame, ctx: &mut LayoutContext<'_>) {
        let area = f.area();
        let min_w = if ctx.tab == Tab::Analysis { 84 } else { 74 };
        if area.width < min_w || area.height < 14 {
            render_too_small(f, min_w, 14);
            return;
        }
        self.sync_inspector_target(ctx);

        let [frame_area, controls] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

        let theme = theme();
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border());
        let inner = outer.inner(frame_area);
        f.render_widget(outer, frame_area);

        let [head_area, rule_area, content_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(inner);

        let headline = if ctx.tab == Tab::Usage {
            (
                format_cost(ctx.usage.totals().cost),
                format!(" · {}", format_compact(ctx.usage.totals().total)),
            )
        } else {
            (
                format!(
                    "{}L",
                    format_compact(ctx.analysis.totals().total_lines() as i64)
                ),
                format!(
                    " · {}c",
                    format_compact(ctx.analysis.totals().total_calls() as i64)
                ),
            )
        };
        self.top_bar = render_editorial_header(
            f,
            head_area,
            ctx.tab,
            Some(ctx.time_range),
            &headline.0,
            &headline.1,
        );

        let min_split = if ctx.tab == Tab::Usage {
            USAGE_SPLIT_MIN_INNER_W
        } else {
            ANALYSIS_SPLIT_MIN_INNER_W
        };
        self.split_active = content_area.width >= min_split;

        let (left_area, divider_x, right_area) = if self.split_active {
            self.detail_modal_open = false;
            self.provider_modal_open = false;
            let side_w = (content_area.width / 3).clamp(30, 44);
            let [l, div, r] = Layout::horizontal([
                Constraint::Min(54),
                Constraint::Length(1),
                Constraint::Length(side_w),
            ])
            .areas(content_area);
            (l, Some(div.x), Some(r))
        } else {
            (content_area, None, None)
        };
        self.side_area = right_area;

        // Top hairline divider, joined cleanly with `┬` at the split column.
        let mut rule = String::with_capacity(usize::from(rule_area.width));
        for dx in 0..rule_area.width {
            if Some(rule_area.x + dx) == divider_x {
                rule.push('┬');
            } else {
                rule.push('─');
            }
        }
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(rule, theme.border()))),
            rule_area,
        );

        if let Some(div_x) = divider_x {
            let div_rect = Rect::new(div_x, content_area.y, 1, content_area.height);
            let lines: Vec<Line<'static>> = (0..div_rect.height)
                .map(|_| Line::from(Span::styled("│", theme.border())))
                .collect();
            f.render_widget(Paragraph::new(lines), div_rect);
        }

        if ctx.tab == Tab::Usage {
            self.draw_usage_inside(f, left_area, right_area, controls, frame_area, ctx);
        } else {
            self.draw_analysis_inside(f, left_area, right_area, controls, frame_area, ctx);
        }
    }

    fn draw_usage_inside(
        &mut self,
        f: &mut Frame,
        left_area: Rect,
        right_area: Option<Rect>,
        controls: Rect,
        modal_area: Rect,
        ctx: &mut LayoutContext<'_>,
    ) {
        if ctx.usage.selected_row().is_none() {
            self.detail_modal_open = false;
        }
        let theme = theme();
        let [tbl_outer, digest_rule, digest_row] = Layout::vertical([
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(left_area);
        let [tbl_head, tbl_body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(4)]).areas(tbl_outer);
        let table_area = Rect::new(
            tbl_body.x + 1,
            tbl_body.y,
            tbl_body.width.saturating_sub(2),
            tbl_body.height,
        );
        self.table_area = Some(table_area);
        self.digest_area = Some(digest_row);

        let cols = layout_usage_columns(table_area.width);
        let totals_tokens = ctx.usage.totals().total;
        let selected = ctx.usage.selected_index();
        let sort = ctx.usage.sort();
        let rows: Vec<_> = ctx
            .usage
            .rows()
            .iter()
            .enumerate()
            .map(|(idx, row)| {
                build_editorial_usage_row(
                    row,
                    totals_tokens,
                    &cols,
                    selected == Some(idx),
                    ctx.usage.update_tracker(),
                )
            })
            .collect();
        let row_count = rows.len();
        let sorted = cols.sorted_header_index(sort);
        let dropped = cols.dropped;
        let sort_note = if dropped > 0 {
            format!("{} · −{dropped} cols", sort.label())
        } else {
            sort.label().to_string()
        };
        let head_pad = Rect::new(
            tbl_head.x + 1,
            tbl_head.y,
            tbl_head.width.saturating_sub(2),
            1,
        );
        render_side_heading(
            f,
            head_pad,
            &format!("MODELS · {row_count}"),
            Some(sort_note),
        );
        let scrollbar_gutter = Rect::new(
            tbl_body.right().saturating_sub(1),
            tbl_body.y + 2,
            1,
            tbl_body.height.saturating_sub(2),
        );
        render_borderless_table(
            f,
            table_area,
            scrollbar_gutter,
            cols.header,
            rows,
            &cols.widths,
            sorted,
            row_count,
            ctx.usage.scroll_mut(),
        );

        let rule_str = "─".repeat(usize::from(digest_rule.width));
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(rule_str, theme.border()))),
            digest_rule,
        );

        let provider_rows = build_provider_total_rows(ctx.usage.provider_totals());
        let digest_inner = Rect::new(
            digest_row.x + 1,
            digest_row.y,
            digest_row.width.saturating_sub(2),
            1,
        );
        let mut digest_spans = vec![Span::styled("Providers  ", theme.dim())];
        let digest_line = provider_digest(&provider_rows, digest_inner.width.saturating_sub(11));
        digest_spans.extend(digest_line.spans);
        f.render_widget(Paragraph::new(Line::from(digest_spans)), digest_inner);

        if let Some(side) = right_area {
            let side_pad = Rect::new(
                side.x + 1,
                side.y,
                side.width.saturating_sub(2),
                side.height,
            );
            self.side_header_area = Some(Rect::new(side.x, side.y, side.width, 1));
            match self.side_mode {
                SideMode::Inspector => {
                    if let Some(row) = ctx.usage.selected_row() {
                        let sections = build_usage_detail_sections(row, ctx.usage.totals());
                        render_borderless_inspector(
                            f,
                            side_pad,
                            "INSPECTOR",
                            Some("p: providers"),
                            &row.model,
                            &sections,
                            &mut self.inspector_scroll,
                        );
                    } else {
                        render_borderless_empty(f, side_pad, "INSPECTOR", Some("p: providers"));
                    }
                }
                SideMode::Providers => {
                    render_borderless_providers(f, side_pad, &provider_rows);
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
                render_provider_panel(f, modal_area, &provider_rows);
            }
            if self.detail_modal_open
                && let Some(row) = ctx.usage.selected_row()
            {
                let sections = build_usage_detail_sections(row, ctx.usage.totals());
                render_detail_panel(f, modal_area, &row.model, &sections);
            }
        }
    }

    fn draw_analysis_inside(
        &mut self,
        f: &mut Frame,
        left_area: Rect,
        right_area: Option<Rect>,
        controls: Rect,
        modal_area: Rect,
        ctx: &mut LayoutContext<'_>,
    ) {
        if ctx.analysis.selected_row().is_none() {
            self.detail_modal_open = false;
        }
        let [tbl_head, tbl_body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(4)]).areas(left_area);
        let table_area = Rect::new(
            tbl_body.x + 1,
            tbl_body.y,
            tbl_body.width.saturating_sub(2),
            tbl_body.height,
        );
        self.table_area = Some(table_area);

        let cols = layout_analysis_columns(table_area.width);
        let selected = ctx.analysis.selected_index();
        let sort = ctx.analysis.sort();
        let rows: Vec<_> = ctx
            .analysis
            .rows()
            .iter()
            .enumerate()
            .map(|(idx, row)| {
                build_editorial_analysis_row(
                    row,
                    &cols,
                    selected == Some(idx),
                    ctx.analysis.update_tracker(),
                )
            })
            .collect();
        let row_count = rows.len();
        let sorted = cols.sorted_header_index(sort);
        let dropped = cols.dropped;
        let sort_note = if dropped > 0 {
            format!("{} · −{dropped} cols", sort.label())
        } else {
            sort.label().to_string()
        };
        let head_pad = Rect::new(
            tbl_head.x + 1,
            tbl_head.y,
            tbl_head.width.saturating_sub(2),
            1,
        );
        render_side_heading(
            f,
            head_pad,
            &format!("MODELS · {row_count}"),
            Some(sort_note),
        );
        let scrollbar_gutter = Rect::new(
            tbl_body.right().saturating_sub(1),
            tbl_body.y + 2,
            1,
            tbl_body.height.saturating_sub(2),
        );
        render_borderless_table(
            f,
            table_area,
            scrollbar_gutter,
            cols.header,
            rows,
            &cols.widths,
            sorted,
            row_count,
            ctx.analysis.scroll_mut(),
        );

        if let Some(side) = right_area {
            let side_pad = Rect::new(
                side.x + 1,
                side.y,
                side.width.saturating_sub(2),
                side.height,
            );
            if let Some(row) = ctx.analysis.selected_row() {
                let sections = build_analysis_detail_sections(row, ctx.analysis.totals());
                render_borderless_inspector(
                    f,
                    side_pad,
                    "INSPECTOR",
                    None,
                    &row.model,
                    &sections,
                    &mut self.inspector_scroll,
                );
            } else {
                render_borderless_empty(f, side_pad, "INSPECTOR", None);
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
            render_detail_panel(f, modal_area, &row.model, &sections);
        }
    }
}

fn render_editorial_header(
    f: &mut Frame,
    area: Rect,
    current: Tab,
    range: Option<TimeRange>,
    headline_accent: &str,
    headline_dim: &str,
) -> TopBarHits {
    let mut hits = TopBarHits {
        tabs: Tab::ALL.map(|tab| (tab, None)),
        ranges: RANGES.map(|(r, _)| (r, None)),
    };
    if area.width == 0 || area.height == 0 {
        return hits;
    }

    let theme = theme();
    let width = usize::from(area.width);
    let prefix = " vct ";
    let mut left = vec![Span::styled(prefix, Style::default().bold())];
    let mut cursor = prefix.chars().count();

    for (idx, tab) in Tab::ALL.into_iter().enumerate() {
        let text = format!(" {} ", tab.label());
        let len = text.chars().count();
        if cursor + len <= width {
            hits.tabs[idx].1 = Some(Rect::new(area.x + cursor as u16, area.y, len as u16, 1));
        }
        cursor += len;
        left.push(if tab == current {
            Span::styled(text, theme.accent().bold().underlined())
        } else {
            Span::styled(text, theme.dim())
        });
    }

    let mut range_spans = Vec::new();
    let mut range_offsets: Vec<(usize, usize)> = Vec::new();
    if let Some(active_range) = range {
        let mut r_cursor = 0usize;
        for (candidate, name) in RANGES {
            let text = format!(" {name} ");
            let len = text.chars().count();
            range_offsets.push((r_cursor, len));
            r_cursor += len;
            range_spans.push(if candidate == active_range {
                Span::styled(text, theme.active())
            } else {
                Span::styled(text, theme.dim())
            });
        }
    }

    let headline_spans = vec![
        Span::raw("  "),
        Span::styled(headline_accent.to_string(), theme.accent().bold()),
        Span::styled(format!("{headline_dim} "), theme.dim()),
    ];

    let span_w =
        |spans: &[Span]| -> usize { spans.iter().map(|s| s.content.chars().count()).sum() };
    let left_w = span_w(&left);
    let mut room = width.saturating_sub(left_w + 1);
    let mut show_ranges = false;
    let mut right = Vec::new();

    let range_w = span_w(&range_spans);
    if !range_spans.is_empty() && range_w <= room {
        show_ranges = true;
        room -= range_w;
        right.extend(range_spans);
        let head_w = span_w(&headline_spans);
        if head_w <= room {
            right.extend(headline_spans);
        }
    } else {
        let head_w = span_w(&headline_spans);
        if head_w <= room {
            right.extend(headline_spans);
        }
    }

    let right_w = span_w(&right);
    let gap = width.saturating_sub(left_w + right_w);
    let right_start = left_w + gap;

    if show_ranges {
        for (idx, (offset, len)) in range_offsets.into_iter().enumerate() {
            let x = right_start + offset;
            if x + len <= width {
                hits.ranges[idx].1 = Some(Rect::new(area.x + x as u16, area.y, len as u16, 1));
            }
        }
    }

    left.push(Span::raw(" ".repeat(gap)));
    left.extend(right);
    f.render_widget(Paragraph::new(Line::from(left)), area);
    hits
}

fn build_editorial_usage_row(
    row: &UsageRow,
    total_tokens: i64,
    cols: &UsageTableColumns,
    selected: bool,
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
    let mut cells: Vec<RatatuiCell<'static>> = Vec::with_capacity(cols.header.len());
    cells.push(cursor_model_cell(&row.model, selected));
    let numeric: Vec<String> = cols.kept.iter().map(|&i| all[i].clone()).collect();
    cells.extend(aligned_cells(numeric, 0));
    if cols.share_w > 0 && total_tokens > 0 && cols.share_w >= 6 {
        let share = (row.total as f64 / total_tokens as f64).clamp(0.0, 1.0);
        let bar_w = usize::from(cols.share_w - 6);
        let filled = (share * bar_w as f64).round() as usize;
        cells.push(RatatuiCell::from(Line::from(vec![
            Span::styled("━".repeat(filled.min(bar_w)), theme.accent()),
            Span::styled(
                format!(
                    "{}{:>5.1}%",
                    " ".repeat(bar_w.saturating_sub(filled)),
                    share * 100.0
                ),
                theme.dim(),
            ),
        ])));
    } else if cols.share_w > 0 {
        cells.push(RatatuiCell::from(""));
    }
    RatatuiRow::new(cells).style(style)
}

fn build_editorial_analysis_row(
    row: &AnalysisRow,
    cols: &AnalysisTableColumns,
    selected: bool,
    update_tracker: &UpdateTracker,
) -> RatatuiRow<'static> {
    let theme = theme();
    let style = if update_tracker.is_recently_updated(&row.model) {
        Style::default().bg(theme.flash_bg).bold()
    } else {
        Style::default()
    };
    let all = [
        format_compact(row.edit_lines as i64),
        format_compact(row.read_lines as i64),
        format_compact(row.write_lines as i64),
        format_compact(row.bash_count as i64),
        format_compact(row.edit_count as i64),
        format_compact(row.read_count as i64),
        format_compact(row.todo_write_count as i64),
        format_compact(row.write_count as i64),
    ];
    let mut cells: Vec<RatatuiCell<'static>> = Vec::with_capacity(cols.header.len());
    cells.push(cursor_model_cell(&row.model, selected));
    let numeric: Vec<String> = cols.kept.iter().map(|&i| all[i].clone()).collect();
    cells.extend(aligned_cells(numeric, 0));
    RatatuiRow::new(cells).style(style)
}

fn cursor_model_cell(model: &str, selected: bool) -> RatatuiCell<'static> {
    let theme = theme();
    if selected {
        RatatuiCell::from(Line::from(vec![
            Span::styled("▎ ", theme.accent().bold()),
            Span::raw(model.to_string()),
        ]))
    } else {
        RatatuiCell::from(Line::from(vec![
            Span::raw("  "),
            Span::raw(model.to_string()),
        ]))
    }
}

#[allow(clippy::too_many_arguments)]
fn render_borderless_table(
    f: &mut Frame,
    area: Rect,
    scrollbar_gutter: Rect,
    header: Vec<&str>,
    rows: Vec<RatatuiRow>,
    widths: &[Constraint],
    sorted: Option<usize>,
    row_count: usize,
    scroll: &mut ScrollState,
) {
    let theme = theme();
    let viewport = area.height.saturating_sub(2);
    let table = create_ratatui_table(rows, header, widths, sorted)
        .block(Block::default())
        .row_highlight_style(theme.selected());
    f.render_stateful_widget(table, area, &mut scroll.table);

    let selected = scroll.table.selected().unwrap_or(0);
    scroll.scrollbar = ScrollbarState::new(row_count)
        .viewport_content_length(usize::from(viewport))
        .position(selected);
    if row_count <= usize::from(viewport) || scrollbar_gutter.height == 0 {
        return;
    }
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("┃")
            .track_style(theme.border())
            .thumb_style(theme.dim()),
        scrollbar_gutter,
        &mut scroll.scrollbar,
    );
}

fn borderless_header_column_at(
    table_area: Rect,
    widths: &[Constraint],
    column: u16,
    row: u16,
) -> Option<usize> {
    if table_area.width == 0 || table_area.height == 0 || row != table_area.y {
        return None;
    }
    if column < table_area.x || column >= table_area.right() {
        return None;
    }
    let header_row = Rect::new(table_area.x, table_area.y, table_area.width, 1);
    let cols = Layout::horizontal(widths).spacing(1).split(header_row);
    cols.iter()
        .position(|cell| column >= cell.x && column < cell.right())
}

fn borderless_body_row_at(
    table_area: Rect,
    scroll: &ScrollState,
    row_count: usize,
    column: u16,
    row: u16,
) -> Option<usize> {
    if table_area.width == 0 || table_area.height < 3 {
        return None;
    }
    let body_top = table_area.y + 2;
    if column < table_area.x
        || column >= table_area.right()
        || row < body_top
        || row >= table_area.bottom()
    {
        return None;
    }
    let idx = scroll.table.offset() + usize::from(row - body_top);
    (idx < row_count).then_some(idx)
}

fn render_side_heading(f: &mut Frame, area: Rect, title: &str, right_note: Option<String>) {
    let theme = theme();
    let w = usize::from(area.width);
    let mut spans = vec![Span::styled(title.to_string(), theme.dim().bold())];
    if let Some(note) = right_note {
        let used = title.chars().count();
        let note_w = note.chars().count();
        if used + 1 + note_w <= w {
            spans.push(Span::raw(" ".repeat(w - used - note_w)));
            spans.push(Span::styled(note, theme.dim()));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_borderless_empty(f: &mut Frame, area: Rect, heading: &str, toggle_hint: Option<&str>) {
    if area.width < 8 || area.height < 2 {
        return;
    }
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    render_side_heading(f, head, heading, toggle_hint.map(str::to_string));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " No model selected",
            theme().dim(),
        ))),
        body,
    );
}

fn render_borderless_inspector(
    f: &mut Frame,
    area: Rect,
    heading: &str,
    toggle_hint: Option<&str>,
    name: &str,
    sections: &[crate::display::common::table::DetailSection],
    scroll_offset: &mut usize,
) {
    if area.width < 8 || area.height < 2 {
        return;
    }
    let [head, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let inner_w = usize::from(body.width.saturating_sub(2)).max(1);
    let lines = format_detail_lines(name, sections, inner_w);
    let line_count = lines.len();
    let visible = usize::from(body.height);
    let max_offset = line_count.saturating_sub(visible);
    *scroll_offset = (*scroll_offset).min(max_offset);
    let hidden_below = line_count.saturating_sub(*scroll_offset + visible);

    let note = match (hidden_below > 0, toggle_hint) {
        (true, Some(hint)) if usize::from(head.width) >= heading.len() + hint.len() + 10 => {
            Some(format!("+{hidden_below} ↓ · {hint}"))
        }
        (true, Some(_)) => Some(format!("+{hidden_below} ↓ · (p)")),
        (true, None) => Some(format!("+{hidden_below} ↓")),
        (false, Some(hint)) => Some(hint.to_string()),
        (false, None) => None,
    };
    render_side_heading(f, head, heading, note);

    f.render_widget(
        Paragraph::new(lines).scroll((*scroll_offset as u16, 0)),
        body,
    );
}

fn render_borderless_providers(
    f: &mut Frame,
    area: Rect,
    provider_rows: &[ProviderTotal<'_, ProviderStats>],
) {
    if area.width < 12 || area.height < 4 {
        return;
    }
    let [head, body, bar_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(2),
        Constraint::Length(1),
    ])
    .areas(area);
    let listed: Vec<_> = provider_rows
        .iter()
        .filter(|r| r.label != "All Providers")
        .collect();
    let capacity = usize::from(body.height.saturating_sub(2));
    let mut hidden = listed.len().saturating_sub(capacity);
    if hidden > 0 {
        hidden = listed.len().saturating_sub(capacity.saturating_sub(1));
    }
    let shown = listed.len() - hidden;

    let note = if hidden > 0 {
        format!("+{hidden} · p: model")
    } else {
        "p: model".to_string()
    };
    render_side_heading(f, head, "PROVIDERS", Some(note));

    let theme = theme();
    if listed.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No provider data yet",
                theme.dim(),
            ))),
            body,
        );
    } else {
        let mut rows: Vec<RatatuiRow> = listed
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
            rows.push(
                RatatuiRow::new(vec![
                    format!("+{hidden} more"),
                    String::new(),
                    String::new(),
                ])
                .style(theme.dim()),
            );
        }
        let widths = [
            Constraint::Min(9),
            Constraint::Length(9),
            Constraint::Length(9),
        ];
        let table = create_ratatui_table(rows, vec!["Provider", "Tokens", "Cost"], &widths, None)
            .block(Block::default());
        f.render_widget(table, body);
    }

    f.render_widget(
        Paragraph::new(provider_share_bar(provider_rows, bar_area.width)),
        bar_area,
    );
}

impl LayoutView for EditorialView {
    fn draw(&mut self, f: &mut Frame, ctx: &mut LayoutContext<'_>) {
        self.top_bar = TopBarHits::default();
        self.table_area = None;
        self.side_area = None;
        self.side_header_area = None;
        self.digest_area = None;
        self.split_active = false;

        if matches!(ctx.tab, Tab::Usage | Tab::Analysis) && !ctx.loading {
            self.draw_data_frame(f, ctx);
            return;
        }

        // Non-data tabs (`Quota`, `Settings`) and loading keep the editorial
        // underline bar + hairline above their own bottom-row controls footer
        // so `Star on GitHub` stays on `y = f.area().bottom() - 1`.
        let [bar, rule, body] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(f.area());
        let range = ctx.tab.scans().then_some(ctx.time_range);
        self.top_bar = render_top_bar_styled(f, bar, ctx.tab, range, ctx.metrics, true);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(usize::from(rule.width)),
                theme().border(),
            ))),
            rule,
        );
        render_standard_non_data(f, body, ctx, BorderType::Rounded);
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
                    if let Some(hdr) = self.side_header_area
                        && contains(hdr, column, row)
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
                        let cols = layout_usage_columns(table.width);
                        if let Some(col_idx) =
                            borderless_header_column_at(table, &cols.widths, column, row)
                            && let Some(sort) = cols.sort_for_header(col_idx)
                        {
                            ctx.usage.set_sort(sort);
                            return EventResult::consumed();
                        }
                        if let Some(row_idx) = borderless_body_row_at(
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
                    let cols = layout_analysis_columns(table.width);
                    if let Some(col_idx) =
                        borderless_header_column_at(table, &cols.widths, column, row)
                        && let Some(sort) = cols.sort_for_header(col_idx)
                    {
                        ctx.analysis.set_sort(sort);
                        return EventResult::consumed();
                    }
                    if let Some(row_idx) = borderless_body_row_at(
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
                r.push(("p", "toggle side column between Inspector and Providers"));
            }
            r.extend([
                ("tab", "next tab, shift+tab for the previous one"),
                ("esc", "return side column to Inspector"),
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
            input_tokens: 200_000,
            output_tokens: 40_000,
            total: 240_000,
            cost: 3.10,
            providers: vec![ModelProviderUsage {
                provider: Provider::Codex,
                total_tokens: 240_000,
                cost: 3.10,
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
    fn single_outer_frame_with_hairlines_cursor_and_clickable_borderless_table() {
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
        assert!(
            lines[0].starts_with('╭') && lines[0].ends_with('╮'),
            "{screen}"
        );
        assert!(screen.contains('┬') && screen.contains('│'), "{screen}");
        assert!(screen.contains("▎ claude-opus-4-8"), "{screen}");
        assert!(screen.contains("INSPECTOR"), "{screen}");
        assert!(lines.last().unwrap().contains("Star on GitHub"), "{screen}");

        // Click gpt-5.5 row and Total header in the borderless table.
        let gpt_y = lines.iter().position(|l| l.contains("gpt-5.5")).unwrap() as u16;
        let hdr_y = lines.iter().position(|l| l.contains("Cost (USD)")).unwrap() as u16;
        let total_byte = lines[hdr_y as usize].find("Total").unwrap();
        let total_x = lines[hdr_y as usize][..total_byte].chars().count() as u16;
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
                    row: gpt_y,
                },
                &mut ctx,
            );
            assert_eq!(ctx.usage.selected_row().unwrap().model, "gpt-5.5");
            view.on_mouse(
                MouseInput::Click {
                    column: total_x + 1,
                    row: hdr_y,
                },
                &mut ctx,
            );
            assert_eq!(ctx.usage.sort(), UsageSort::Total);
        }

        // Analysis tab omits any bottom Providers block.
        let a_screen = render(
            &mut view,
            98,
            26,
            Tab::Analysis,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(a_screen.contains("INSPECTOR"), "{a_screen}");
        assert!(!a_screen.contains("All Providers"), "{a_screen}");
        assert!(!a_screen.contains("Providers"), "{a_screen}");
    }

    #[test]
    fn providers_toggle_ignores_pagedown_and_widening_clears_stale_modals() {
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
        }
        let provs = render(
            &mut view,
            100,
            24,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(provs.contains("PROVIDERS"), "{provs}");

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
    fn scrollable_table_keeps_rightmost_digits_shows_dropped_cols_and_too_small_clears_hits() {
        let mut usage = UsageUiState::new(true);
        let mut rows = Vec::new();
        let mut totals = UsageTotals::default();
        for idx in 0..16 {
            let row = UsageRow {
                model: format!("model-{idx:02}"),
                input_tokens: 10_000,
                output_tokens: 2_000,
                total: 12_000,
                cost: 16.20 - idx as f64 * 0.5,
                ..UsageRow::default()
            };
            totals.accumulate(&row);
            rows.push(row);
        }
        usage.apply(UsageRefreshPayload {
            rows: rows.clone(),
            merged_rows: rows,
            totals,
            provider_totals: ProviderTotals::default(),
        });
        let mut analysis = AnalysisUiState::new();
        let mut settings = SettingsUiState::new(Config::default(), Box::new(|_, _| Ok(())));
        let mut view = LAYOUT.create();

        let lines = render(
            &mut view,
            104,
            18,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
        let screen = lines.join("\n");
        assert!(
            screen.contains("Cost (USD)") && screen.contains("$16.20"),
            "scrollbar overwrote rightmost column:\n{screen}"
        );
        assert!(
            screen.contains("cols") && screen.contains("sort: cost"),
            "missing dropped-cols / sort notice:\n{screen}"
        );

        // Shrink below minimum size -> stale header hits must not switch tabs.
        let _ = render(
            &mut view,
            50,
            12,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        );
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
        assert_eq!(
            view.on_mouse(MouseInput::Click { column: 16, row: 1 }, &mut ctx),
            EventResult::consumed()
        );
    }
}
