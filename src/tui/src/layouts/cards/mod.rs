//! `cards` layout: summary + clickable sort-pill toolbar over a responsive
//! multi-column visual model card grid (`1..=3` columns) with share bars,
//! key metrics, colored provider chips, and full `Details` popups on click or
//! `Enter`.

use crate::display::analysis::{AnalysisRow, AnalysisSort, build_analysis_detail_sections};
use crate::display::app::Tab;
use crate::display::common::provider_tui_color;
use crate::display::common::table::{
    create_controls_with_status, render_detail_panel, render_too_small,
};
use crate::display::common::theme::theme;
use crate::display::common::tui::{MouseInput, NavDelta};
use crate::display::usage::{
    UsageRow, UsageSort, UsageTotals, build_provider_total_rows, build_usage_detail_sections,
    provider_digest, render_provider_panel,
};
use crate::layouts::{
    EventResult, LayoutContext, LayoutMeta, LayoutView, TopBarHits, TuiLayout, bar_spans, contains,
    handle_settings_mouse, handle_top_bar_click, render_standard_non_data, render_top_bar,
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
pub(crate) static LAYOUT: CardsPlugin = CardsPlugin;

const CARD_H: u16 = 7;

// Exact `UsageSort::ALL` order so `←` / `→` always step along the drawn pills.
const USAGE_SORT_PILLS: [(UsageSort, &str); 7] = [
    (UsageSort::Model, "Model"),
    (UsageSort::Input, "In"),
    (UsageSort::Output, "Out"),
    (UsageSort::CacheRead, "C.Read"),
    (UsageSort::CacheWrite, "C.Write"),
    (UsageSort::Total, "Total"),
    (UsageSort::Cost, "Cost"),
];

// Exact `AnalysisSort::ALL` order so `←` / `→` always step along the drawn pills.
const ANALYSIS_SORT_PILLS: [(AnalysisSort, &str); 9] = [
    (AnalysisSort::Model, "Model"),
    (AnalysisSort::EditLines, "Edit L"),
    (AnalysisSort::ReadLines, "Read L"),
    (AnalysisSort::WriteLines, "Write L"),
    (AnalysisSort::Bash, "Bash"),
    (AnalysisSort::Edit, "Edit"),
    (AnalysisSort::Read, "Read"),
    (AnalysisSort::TodoWrite, "Todo"),
    (AnalysisSort::Write, "Write"),
];

pub(crate) struct CardsPlugin;

impl TuiLayout for CardsPlugin {
    fn meta(&self) -> LayoutMeta {
        LayoutMeta {
            id: "cards",
            title: "Cards",
            summary: "Responsive multi-column model card grid with sort toolbar and share bars",
        }
    }

    fn create(&self) -> Box<dyn LayoutView> {
        Box::new(CardsView::default())
    }
}

#[derive(Default)]
struct CardsView {
    top_bar: TopBarHits,
    detail_modal_open: bool,
    provider_modal_open: bool,
    usage_row_offset: usize,
    analysis_row_offset: usize,
    card_hits: Vec<(Rect, usize)>,
    usage_sort_hits: Vec<(Rect, UsageSort)>,
    analysis_sort_hits: Vec<(Rect, AnalysisSort)>,
    digest_area: Option<Rect>,
}

impl CardsView {
    fn draw_usage(&mut self, f: &mut Frame, area: Rect, ctx: &mut LayoutContext<'_>) {
        if area.width < 68 || area.height < 14 {
            self.top_bar = TopBarHits::default();
            render_too_small(f, 68, 14);
            return;
        }
        if ctx.usage.selected_row().is_none() {
            self.detail_modal_open = false;
        }
        let [work, controls] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

        let toolbar_h = if work.height >= 16 { 3 } else { 1 };
        let [toolbar_area, grid_area, digest_area] = Layout::vertical([
            Constraint::Length(toolbar_h),
            Constraint::Min(CARD_H),
            Constraint::Length(1),
        ])
        .areas(work);
        self.digest_area = Some(digest_area);

        let rows = ctx.usage.rows();
        let totals = ctx.usage.totals();
        let selected = ctx.usage.selected_index();
        let cols = usize::from((grid_area.width / 34).clamp(1, 3));
        let card_h = CARD_H.min(grid_area.height.max(4));
        let vis_rows = usize::from(grid_area.height / card_h).max(1);
        let total_grid_rows = rows.len().div_ceil(cols);
        sync_grid_offset(
            &mut self.usage_row_offset,
            selected.map(|s| s / cols),
            total_grid_rows,
            vis_rows,
        );

        let start_idx = self.usage_row_offset * cols;
        let capacity = vis_rows * cols;
        let end_idx = (start_idx + capacity).min(rows.len());
        let hidden_above = start_idx;
        let hidden_below = rows.len().saturating_sub(end_idx);

        let summary_left = format!(
            "{} · {} tokens · {} models",
            format_cost(totals.cost),
            format_compact(totals.total),
            rows.len(),
        );
        let scroll_badge = grid_scroll_badge(hidden_above, hidden_below);
        self.usage_sort_hits = render_sort_toolbar(
            f,
            toolbar_area,
            &summary_left,
            &scroll_badge,
            &USAGE_SORT_PILLS,
            ctx.usage.sort(),
        );

        if rows.is_empty() {
            render_empty_grid(f, grid_area);
        } else {
            let rects = layout_card_rects(grid_area, cols, vis_rows, card_h);
            for (slot, rect) in rects.into_iter().enumerate() {
                let idx = start_idx + slot;
                let Some(row) = rows.get(idx) else {
                    break;
                };
                render_usage_card(f, rect, row, totals, selected == Some(idx));
                self.card_hits.push((rect, idx));
            }
        }

        let provider_rows = build_provider_total_rows(ctx.usage.provider_totals());
        f.render_widget(
            Paragraph::new(provider_digest(&provider_rows, digest_area.width)).centered(),
            digest_area,
        );

        f.render_widget(
            create_controls_with_status(
                &[
                    ("↑↓", "select"),
                    ("⏎", "details"),
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
        if area.width < 68 || area.height < 14 {
            self.top_bar = TopBarHits::default();
            render_too_small(f, 68, 14);
            return;
        }
        if ctx.analysis.selected_row().is_none() {
            self.detail_modal_open = false;
        }
        let [work, controls] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(area);

        let toolbar_h = if work.height >= 16 { 3 } else { 1 };
        let [toolbar_area, grid_area] =
            Layout::vertical([Constraint::Length(toolbar_h), Constraint::Min(CARD_H)]).areas(work);

        let rows = ctx.analysis.rows();
        let totals = ctx.analysis.totals();
        let selected = ctx.analysis.selected_index();
        let cols = usize::from((grid_area.width / 34).clamp(1, 3));
        let card_h = CARD_H.min(grid_area.height.max(4));
        let vis_rows = usize::from(grid_area.height / card_h).max(1);
        let total_grid_rows = rows.len().div_ceil(cols);
        sync_grid_offset(
            &mut self.analysis_row_offset,
            selected.map(|s| s / cols),
            total_grid_rows,
            vis_rows,
        );

        let start_idx = self.analysis_row_offset * cols;
        let capacity = vis_rows * cols;
        let end_idx = (start_idx + capacity).min(rows.len());
        let hidden_above = start_idx;
        let hidden_below = rows.len().saturating_sub(end_idx);

        let summary_left = format!(
            "{} lines · {} calls · {} models",
            format_compact(totals.total_lines() as i64),
            format_compact(totals.total_calls() as i64),
            rows.len(),
        );
        let scroll_badge = grid_scroll_badge(hidden_above, hidden_below);
        self.analysis_sort_hits = render_sort_toolbar(
            f,
            toolbar_area,
            &summary_left,
            &scroll_badge,
            &ANALYSIS_SORT_PILLS,
            ctx.analysis.sort(),
        );

        if rows.is_empty() {
            render_empty_grid(f, grid_area);
        } else {
            let rects = layout_card_rects(grid_area, cols, vis_rows, card_h);
            for (slot, rect) in rects.into_iter().enumerate() {
                let idx = start_idx + slot;
                let Some(row) = rows.get(idx) else {
                    break;
                };
                render_analysis_card(f, rect, row, totals, selected == Some(idx));
                self.card_hits.push((rect, idx));
            }
        }

        f.render_widget(
            create_controls_with_status(
                &[
                    ("↑↓", "select"),
                    ("⏎", "details"),
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

fn sync_grid_offset(
    offset: &mut usize,
    selected_row: Option<usize>,
    total_rows: usize,
    vis_rows: usize,
) {
    let max_off = total_rows.saturating_sub(vis_rows);
    *offset = (*offset).min(max_off);
    if let Some(sel) = selected_row {
        if sel < *offset {
            *offset = sel;
        } else if sel >= *offset + vis_rows {
            *offset = sel + 1 - vis_rows;
        }
    }
}

fn grid_scroll_badge(hidden_above: usize, hidden_below: usize) -> String {
    match (hidden_above, hidden_below) {
        (0, 0) => String::new(),
        (a, 0) => format!(" · ↑{a}"),
        (0, b) => format!(" · +{b} more"),
        (a, b) => format!(" · ↑{a} +{b}↓"),
    }
}

fn layout_card_rects(area: Rect, cols: usize, vis_rows: usize, card_h: u16) -> Vec<Rect> {
    let row_constraints = vec![Constraint::Length(card_h); vis_rows];
    let row_rects = Layout::vertical(row_constraints).split(area);
    let col_constraints = vec![Constraint::Ratio(1, cols as u32); cols];
    let mut out = Vec::with_capacity(cols * vis_rows);
    for row_rect in row_rects.iter() {
        let cell_rects = Layout::horizontal(col_constraints.clone())
            .spacing(1)
            .split(*row_rect);
        out.extend(cell_rects.iter().copied());
    }
    out
}

fn render_empty_grid(f: &mut Frame, area: Rect) {
    let theme = theme();
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " No models in selected range",
            theme.dim(),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(theme.border()),
        ),
        area,
    );
}

fn render_sort_toolbar<S: Copy + PartialEq>(
    f: &mut Frame,
    area: Rect,
    summary: &str,
    scroll_badge: &str,
    pills: &[(S, &'static str)],
    active: S,
) -> Vec<(Rect, S)> {
    let theme = theme();
    let inner = if area.height >= 3 {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme.border());
        let inner = block.inner(area);
        f.render_widget(block, area);
        Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 1)
    } else {
        Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1)
    };
    if inner.width == 0 || inner.height == 0 {
        return Vec::new();
    }

    let total_w = usize::from(inner.width);
    let badge_w = scroll_badge.chars().count();
    let all_pills: Vec<(S, String, usize)> = pills
        .iter()
        .map(|&(sort, label)| {
            let text = format!(" {label} ");
            let len = text.chars().count();
            (sort, text, len)
        })
        .collect();

    // Always reserve full room for `scroll_badge` plus a readable summary prefix,
    // then fit the widest contiguous window of pills containing `active` so
    // `←` / `→` always move along the drawn pills and `active` never disappears.
    let prefix = "sort: ";
    let min_left = (16usize + badge_w).min(total_w);
    let max_right = total_w.saturating_sub(min_left + 1);
    let active_idx = all_pills
        .iter()
        .position(|(sort, _, _)| *sort == active)
        .unwrap_or(0);

    let window_w = |lo: usize, hi: usize| -> usize {
        let pills_sum: usize = all_pills[lo..hi].iter().map(|(_, _, l)| *l).sum();
        let left_arrow = usize::from(lo > 0) * 2;
        let right_arrow = usize::from(hi < all_pills.len()) * 2;
        prefix.len() + left_arrow + pills_sum + right_arrow
    };

    let (lo, hi) = if all_pills.is_empty() {
        (0, 0)
    } else if window_w(0, all_pills.len()) <= max_right {
        (0, all_pills.len())
    } else {
        let mut lo = active_idx;
        let mut hi = active_idx + 1;
        loop {
            let mut grew = false;
            if lo > 0 && window_w(lo - 1, hi) <= max_right {
                lo -= 1;
                grew = true;
            }
            if hi < all_pills.len() && window_w(lo, hi + 1) <= max_right {
                hi += 1;
                grew = true;
            }
            if !grew {
                break;
            }
        }
        if window_w(lo, hi) <= max_right {
            (lo, hi)
        } else {
            (active_idx, active_idx + 1)
        }
    };

    let visible_pills = &all_pills[lo..hi];
    let has_left_more = lo > 0;
    let has_right_more = hi < all_pills.len();
    let right_w = if visible_pills.is_empty() {
        0
    } else {
        window_w(lo, hi)
    };
    let left_total_budget = total_w.saturating_sub(if right_w > 0 { right_w + 1 } else { 0 });
    let summary_budget = left_total_budget.saturating_sub(badge_w);
    let fitted_summary = truncate_ellipsis(summary, summary_budget);
    let fitted_badge = if badge_w <= left_total_budget {
        scroll_badge.to_string()
    } else {
        truncate_ellipsis(scroll_badge, left_total_budget)
    };
    let left_w = fitted_summary.chars().count() + fitted_badge.chars().count();
    let gap = total_w.saturating_sub(left_w + right_w);

    let mut spans: Vec<Span<'static>> = Vec::new();
    // Emphasize the leading grand total before the first ` · `.
    if let Some((head, rest)) = fitted_summary.split_once(" · ") {
        spans.push(Span::styled(head.to_string(), theme.accent().bold()));
        spans.push(Span::styled(format!(" · {rest}"), theme.dim()));
    } else {
        spans.push(Span::styled(fitted_summary, theme.accent().bold()));
    }
    if !fitted_badge.is_empty() {
        spans.push(Span::styled(fitted_badge, theme.dim()));
    }
    spans.push(Span::raw(" ".repeat(gap)));

    let mut hits: Vec<(Rect, S)> = Vec::new();
    if !visible_pills.is_empty() {
        spans.push(Span::styled(prefix, theme.dim()));
        let mut x = inner.x + (left_w + gap + prefix.len()) as u16;
        if has_left_more {
            spans.push(Span::styled("‹ ", theme.dim()));
            x += 2;
        }
        for (sort, text, len) in visible_pills {
            let rect = Rect::new(x, inner.y, *len as u16, 1);
            hits.push((rect, *sort));
            spans.push(if *sort == active {
                Span::styled(text.clone(), theme.active())
            } else {
                Span::styled(text.clone(), theme.dim())
            });
            x += *len as u16;
        }
        if has_right_more {
            spans.push(Span::styled(" ›", theme.dim()));
        }
    }

    f.render_widget(Paragraph::new(Line::from(spans)), inner);
    hits
}

fn render_usage_card(
    f: &mut Frame,
    area: Rect,
    row: &UsageRow,
    totals: &UsageTotals,
    selected: bool,
) {
    let theme = theme();
    let cost_str = format!(" {} ", format_cost(row.cost));
    let prefix = if selected { " ▸ " } else { " " };
    let title_budget = usize::from(area.width)
        .saturating_sub(cost_str.chars().count() + prefix.chars().count() + 4)
        .max(6);
    let model_title = truncate_ellipsis(&row.model, title_budget);
    let left_title = Line::from(vec![
        Span::styled(
            prefix,
            if selected {
                theme.accent().bold()
            } else {
                Style::default()
            },
        ),
        Span::styled(
            format!("{model_title} "),
            if selected {
                theme.title()
            } else {
                Style::default().bold()
            },
        ),
    ]);
    let right_title = Line::from(Span::styled(cost_str, theme.accent().bold())).right_aligned();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if selected {
            theme.accent()
        } else {
            theme.border()
        })
        .title(left_title)
        .title(right_title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 8 || inner.height == 0 {
        return;
    }

    let padded = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let w = usize::from(padded.width);
    let ratio = if totals.cost > 0.0 {
        row.cost / totals.cost
    } else if totals.total > 0 {
        row.total as f64 / totals.total as f64
    } else {
        0.0
    };
    let pct_str = format!(" {:>4.1}%", (ratio * 100.0).clamp(0.0, 100.0));
    let bar_w = w.saturating_sub(pct_str.len()).max(4);
    let [filled, empty] = bar_spans(ratio, bar_w, '━', '─', theme.accent());
    let bar_line = Line::from(vec![filled, empty, Span::styled(pct_str, theme.dim())]);

    let tok_line = Line::from(Span::styled(
        truncate_ellipsis(
            &format!(
                "Tot {} · In {} · Out {}",
                format_compact(row.total),
                format_compact(row.input_tokens),
                format_compact(row.output_with_reasoning()),
            ),
            w,
        ),
        theme.dim(),
    ));

    let prompt = row.input_tokens + row.cache_read + row.cache_creation;
    let cache_line = if row.cache_read > 0 && prompt > 0 {
        let hit = (row.cache_read as f64 / prompt as f64 * 100.0).round() as u32;
        let head = format!("Cache {} (", format_compact(row.cache_read));
        let hit_s = format!("{hit}% hit");
        if head.len() + hit_s.len() < w {
            Line::from(vec![
                Span::styled(head, theme.dim()),
                Span::styled(hit_s, Style::default().fg(theme.good)),
                Span::styled(")", theme.dim()),
            ])
        } else {
            Line::from(Span::styled(
                truncate_ellipsis(
                    &format!("Cache {} ({hit}%)", format_compact(row.cache_read)),
                    w,
                ),
                theme.dim(),
            ))
        }
    } else if row.cache_creation > 0 {
        Line::from(Span::styled(
            truncate_ellipsis(
                &format!("Cache W {}", format_compact(row.cache_creation)),
                w,
            ),
            theme.dim(),
        ))
    } else {
        Line::from(Span::styled("Cache 0", theme.dim()))
    };

    let prov_line = provider_chips_line(
        row.providers.iter().map(|p| {
            (
                provider_tui_color(p.provider),
                p.provider.display_name(),
                format_cost_compact(p.cost),
            )
        }),
        w,
    );

    let mut eff_parts = Vec::new();
    if row.total > 0 && row.cost > 0.0 {
        let rate = row.cost / (row.total as f64 / 1_000_000.0);
        eff_parts.push(format!("eff ${rate:.2}/1M"));
    }
    if row.reasoning_tokens > 0 {
        eff_parts.push(format!("reason {}", format_compact(row.reasoning_tokens)));
    }
    if let Some(matched) = &row.matched_model {
        eff_parts.push(format!("as {matched}"));
    }
    let eff_line = Line::from(Span::styled(
        truncate_ellipsis(&eff_parts.join(" · "), w),
        theme.dim(),
    ));

    let mut lines = vec![bar_line, tok_line, cache_line, prov_line, eff_line];
    lines.truncate(usize::from(padded.height));
    let mut para = Paragraph::new(lines);
    if selected {
        para = para.style(Style::default().bg(theme.selected_bg));
    }
    f.render_widget(para, padded);
}

fn render_analysis_card(
    f: &mut Frame,
    area: Rect,
    row: &AnalysisRow,
    totals: &AnalysisRow,
    selected: bool,
) {
    let theme = theme();
    let lines_val = row.total_lines();
    let lines_str = format!(" {}L ", format_compact(lines_val as i64));
    let prefix = if selected { " ▸ " } else { " " };
    let title_budget = usize::from(area.width)
        .saturating_sub(lines_str.chars().count() + prefix.chars().count() + 4)
        .max(6);
    let model_title = truncate_ellipsis(&row.model, title_budget);
    let left_title = Line::from(vec![
        Span::styled(
            prefix,
            if selected {
                theme.accent().bold()
            } else {
                Style::default()
            },
        ),
        Span::styled(
            format!("{model_title} "),
            if selected {
                theme.title()
            } else {
                Style::default().bold()
            },
        ),
    ]);
    let right_title = Line::from(Span::styled(lines_str, theme.accent().bold())).right_aligned();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if selected {
            theme.accent()
        } else {
            theme.border()
        })
        .title(left_title)
        .title(right_title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 8 || inner.height == 0 {
        return;
    }

    let padded = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    let w = usize::from(padded.width);
    let ratio = if totals.total_lines() > 0 {
        lines_val as f64 / totals.total_lines() as f64
    } else if totals.total_calls() > 0 {
        row.total_calls() as f64 / totals.total_calls() as f64
    } else {
        0.0
    };
    let pct_str = format!(" {:>4.1}%", (ratio * 100.0).clamp(0.0, 100.0));
    let bar_w = w.saturating_sub(pct_str.len()).max(4);
    let [filled, empty] = bar_spans(ratio, bar_w, '━', '─', theme.accent());
    let bar_line = Line::from(vec![filled, empty, Span::styled(pct_str, theme.dim())]);

    let file_line = Line::from(Span::styled(
        truncate_ellipsis(
            &format!(
                "Edit {} · Read {} · Write {}",
                format_compact(row.edit_lines as i64),
                format_compact(row.read_lines as i64),
                format_compact(row.write_lines as i64),
            ),
            w,
        ),
        theme.dim(),
    ));

    let calls_line = Line::from(Span::styled(
        truncate_ellipsis(
            &format!(
                "Bash {} · Edit {} · Read {} · Write {}",
                format_compact(row.bash_count as i64),
                format_compact(row.edit_count as i64),
                format_compact(row.read_count as i64),
                format_compact(row.write_count as i64),
            ),
            w,
        ),
        theme.dim(),
    ));

    let prov_line = provider_chips_line(
        row.providers.iter().map(|p| {
            (
                provider_tui_color(p.provider),
                p.provider.display_name(),
                format!("{}L", format_compact(p.total_lines as i64)),
            )
        }),
        w,
    );

    let mut avg_parts = Vec::new();
    if row.edit_count > 0 && row.edit_lines > 0 {
        avg_parts.push(format!("edit ~{}/op", row.edit_lines / row.edit_count));
    }
    if row.read_count > 0 && row.read_lines > 0 {
        avg_parts.push(format!("read ~{}/op", row.read_lines / row.read_count));
    }
    let avg_line = Line::from(Span::styled(
        truncate_ellipsis(&avg_parts.join(" · "), w),
        theme.dim(),
    ));

    let mut lines = vec![bar_line, file_line, calls_line, prov_line, avg_line];
    lines.truncate(usize::from(padded.height));
    let mut para = Paragraph::new(lines);
    if selected {
        para = para.style(Style::default().bg(theme.selected_bg));
    }
    f.render_widget(para, padded);
}

fn provider_chips_line(
    items: impl Iterator<Item = (ratatui::style::Color, &'static str, String)>,
    max_w: usize,
) -> Line<'static> {
    let theme = theme();
    let entries: Vec<_> = items.collect();
    if entries.is_empty() {
        return Line::from(Span::styled("no provider breakdown", theme.dim()));
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let mut shown = 0usize;
    for (idx, (color, name, val)) in entries.iter().enumerate() {
        let text = format!("{name} {val}");
        let sep_w = if shown > 0 { 2 } else { 0 };
        let need = sep_w + 2 + text.chars().count();
        let remaining = entries.len().saturating_sub(idx + 1);
        let tail_reserve = if remaining > 0 { 4 } else { 0 };
        if shown > 0 && used + need + tail_reserve > max_w {
            break;
        }
        if shown == 0 && need > max_w {
            spans.push(Span::styled("● ", Style::default().fg(*color)));
            spans.push(Span::raw(truncate_ellipsis(&text, max_w.saturating_sub(2))));
            shown = 1;
            break;
        }
        if shown > 0 {
            spans.push(Span::raw("  "));
            used += 2;
        }
        spans.push(Span::styled("● ", Style::default().fg(*color)));
        spans.push(Span::raw(text.clone()));
        used += 2 + text.chars().count();
        shown += 1;
    }
    let hidden = entries.len().saturating_sub(shown);
    if hidden > 0 {
        let tail = format!(" +{hidden}");
        if used + tail.len() <= max_w {
            spans.push(Span::styled(tail, theme.dim()));
        }
    }
    Line::from(spans)
}

impl LayoutView for CardsView {
    fn draw(&mut self, f: &mut Frame, ctx: &mut LayoutContext<'_>) {
        self.card_hits.clear();
        self.usage_sort_hits.clear();
        self.analysis_sort_hits.clear();
        self.digest_area = None;

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
                    if let Some((_, sort)) = self
                        .usage_sort_hits
                        .iter()
                        .find(|(rect, _)| contains(*rect, column, row))
                    {
                        ctx.usage.set_sort(*sort);
                        return EventResult::consumed();
                    }
                    if let Some(digest) = self.digest_area
                        && contains(digest, column, row)
                    {
                        self.provider_modal_open = !self.provider_modal_open;
                        return EventResult::consumed();
                    }
                    if let Some((_, idx)) = self
                        .card_hits
                        .iter()
                        .find(|(rect, _)| contains(*rect, column, row))
                    {
                        let same = ctx.usage.selected_index() == Some(*idx);
                        ctx.usage.select_index(*idx);
                        if same {
                            self.detail_modal_open = true;
                        }
                        return EventResult::consumed();
                    }
                } else if ctx.tab == Tab::Analysis {
                    if let Some((_, sort)) = self
                        .analysis_sort_hits
                        .iter()
                        .find(|(rect, _)| contains(*rect, column, row))
                    {
                        ctx.analysis.set_sort(*sort);
                        return EventResult::consumed();
                    }
                    if let Some((_, idx)) = self
                        .card_hits
                        .iter()
                        .find(|(rect, _)| contains(*rect, column, row))
                    {
                        let same = ctx.analysis.selected_index() == Some(*idx);
                        ctx.analysis.select_index(*idx);
                        if same {
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
                }
                EventResult::consumed()
            }
            KeyCode::Enter | KeyCode::Char(' ') if ctx.tab.scans() && !ctx.loading => {
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
            ("↑ ↓ / wheel", "select the previous or next model card"),
            ("PgUp PgDn", "jump by 8 model cards"),
            ("⏎ / space / click", "open full model Details popup"),
            ("← → / click pill", "change card sort order"),
            ("[ ] / click", "change time range"),
            ("r", "scan again now"),
        ];
        if tab == Tab::Usage {
            rows.push(("p", "per-provider totals"));
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
    fn renders_model_cards_and_clicking_sort_pill_or_card_updates_state() {
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
        assert!(screen.contains("claude-opus-4-8"), "{screen}");
        assert!(screen.contains("gpt-5.5"), "{screen}");
        assert!(screen.contains("● Claude"), "{screen}");

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

        // Click "In" sort pill -> sorts by Input descending (gpt-5.5 first).
        let bar_y = wide.iter().position(|l| l.contains("sort:")).unwrap() as u16;
        let in_byte = wide[bar_y as usize].find(" In ").unwrap();
        let in_x = wide[bar_y as usize][..in_byte].chars().count() as u16;
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
                    column: in_x + 1,
                    row: bar_y,
                },
                &mut ctx,
            );
        }
        assert_eq!(usage.sort(), UsageSort::Input);
        assert_eq!(usage.rows()[0].model, "gpt-5.5");

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
    fn narrow_toolbar_keeps_scroll_badge_and_active_sort_in_step_order_and_ignores_empty_enter() {
        let mut usage = UsageUiState::new(true);
        let mut rows = Vec::new();
        let mut totals = UsageTotals::default();
        for idx in 0..10 {
            let row = UsageRow {
                model: format!("model-{idx:02}"),
                input_tokens: 10_000,
                output_tokens: 2_000,
                total: 12_000,
                cost: 10.0 - idx as f64 * 0.5,
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

        // Step sort left from default Cost -> Total -> CacheWrite -> CacheRead,
        // verifying the active pill is always drawn even at narrow 68 cols and
        // the `+N more` badge is never clipped off.
        usage.step_sort(-1);
        assert_eq!(usage.sort(), UsageSort::Total);
        usage.step_sort(-1);
        assert_eq!(usage.sort(), UsageSort::CacheWrite);
        let narrow = render(
            &mut view,
            68,
            20,
            Tab::Usage,
            &mut usage,
            &mut analysis,
            &mut settings,
        )
        .join("\n");
        assert!(narrow.contains("C.Write"), "active pill missing:\n{narrow}");
        assert!(
            narrow.contains("+6 more"),
            "scroll badge clipped:\n{narrow}"
        );

        // Empty usage state: Enter must not latch a ghost modal swallowing top-bar clicks.
        let mut empty_usage = UsageUiState::new(true);
        let lines = render(
            &mut view,
            96,
            24,
            Tab::Usage,
            &mut empty_usage,
            &mut analysis,
            &mut settings,
        );
        let analysis_x = lines[0].find(" Analysis ").unwrap() as u16;
        let mut ctx = LayoutContext {
            tab: Tab::Usage,
            time_range: TimeRange::All,
            loading: false,
            retry_pending: false,
            spinner: 0,
            metrics: (50.0, 0.2),
            data_status: None,
            update_status: None,
            usage: &mut empty_usage,
            analysis: &mut analysis,
            quota: None,
            settings: &mut settings,
        };
        view.on_key(KeyCode::Enter, &mut ctx);
        assert_eq!(
            view.on_mouse(
                MouseInput::Click {
                    column: analysis_x + 2,
                    row: 0,
                },
                &mut ctx,
            ),
            EventResult::Consumed(crate::layouts::LayoutCommand {
                switch_tab: Some(Tab::Analysis),
                ..crate::layouts::LayoutCommand::default()
            })
        );
    }
}
